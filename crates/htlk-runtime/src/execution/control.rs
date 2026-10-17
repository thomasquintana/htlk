//! Cancellation/deadline transitions remain responsive while calculations run.
use super::{model::*, step};
use crate::{
    Runtime, RuntimeError,
    failure::{FailureCode, RuntimeFailure},
    records::{RunStatus, SecurityLabels},
    store::{RecordFamily, RunLease, StoredRecord},
};
use htlk_executable::cbor::Value;
use std::{collections::BTreeSet, sync::Arc};

pub(super) async fn all(
    runtime: &Runtime,
    lease: &RunLease,
    family: RecordFamily,
) -> Result<Vec<StoredRecord>, RuntimeError> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let page = runtime
            .store
            .records(
                lease.run_id(),
                family,
                cursor.as_deref(),
                runtime.store.config().max_page_size,
            )
            .await?;
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|r| r.key.id.clone());
        rows.extend(page);
    }
    Ok(rows)
}
pub(super) async fn enforce(runtime: Arc<Runtime>, lease: RunLease) -> Result<(), RuntimeError> {
    let header = runtime.store.run_header(lease.run_id()).await?;
    if header.status.is_terminal() {
        return Ok(());
    }
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    let usage = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Usage, "run"))
        .await?;
    if due(&usage.payload, now)? {
        return stop(
            runtime.as_ref(),
            &lease,
            None,
            None,
            FailureCode::Timeout,
            false,
        )
        .await;
    }
    for scope in all(&runtime, &lease, RecordFamily::Scope).await? {
        if !scope.final_record && due(&scope.payload, now)? {
            return stop(
                &runtime,
                &lease,
                Some(&scope.key.id),
                None,
                FailureCode::Timeout,
                false,
            )
            .await;
        }
    }
    for node in all(&runtime, &lease, RecordFamily::Invocation).await? {
        if !node.final_record && due(&node.payload, now)? {
            return stop(
                &runtime,
                &lease,
                None,
                Some(&node.key.id),
                FailureCode::Timeout,
                false,
            )
            .await;
        }
    }
    Ok(())
}
fn due(value: &Value, now: i64) -> Result<bool, RuntimeError> {
    Ok(matches!(map(value)?.get("deadline_ms"),Some(Value::Integer(n))if now>=*n))
}
pub(super) async fn stop(
    runtime: &Runtime,
    lease: &RunLease,
    scope_target: Option<&str>,
    node_target: Option<&str>,
    code: FailureCode,
    cancel: bool,
) -> Result<(), RuntimeError> {
    let header = runtime.store.run_header(lease.run_id()).await?;
    if header.status.is_terminal() {
        return Ok(());
    }
    let scopes = all(runtime, lease, RecordFamily::Scope).await?;
    let nodes = all(runtime, lease, RecordFamily::Invocation).await?;
    let whole = scope_target.is_none() && node_target.is_none();
    let mut affected = BTreeSet::new();
    if whole {
        affected.extend(scopes.iter().map(|r| r.key.id.clone()));
    } else if let Some(scope) = scope_target {
        affected.insert(scope.to_string());
    }
    loop {
        let mut changed = false;
        for scope in &scopes {
            if affected.contains(&scope.key.id) {
                continue;
            }
            if let Some(Value::Text(parent)) = map(&scope.payload)?.get("parent_invocation") {
                let inside = node_target.is_some_and(|n| n == parent)
                    || nodes.iter().any(|node| {
                        node.key.id == *parent
                            && string(&node.payload, "scope_instance_id")
                                .is_ok_and(|id| affected.contains(&id))
                    });
                if inside {
                    affected.insert(scope.key.id.clone());
                    changed = true
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut writes = Vec::new();
    let mut protection = Vec::new();
    for node in &nodes {
        if node.final_record {
            continue;
        }
        if whole
            || node_target == Some(node.key.id.as_str())
            || affected.contains(&string(&node.payload, "scope_instance_id")?)
        {
            protection.push(labels(&node.payload, &runtime.config.limits)?);
            let direct = node_target == Some(node.key.id.as_str());
            let status = if direct && !cancel {
                "failed"
            } else {
                "cancelled"
            };
            let error = RuntimeFailure::new(if direct { code } else { FailureCode::Cancelled });
            writes.push(update(
                node,
                replace(
                    &node.payload,
                    [
                        ("status", Value::Text(status.into())),
                        (
                            "failure",
                            error.to_value(&runtime.config.limits).map_err(|_| err())?,
                        ),
                        ("reason", Value::Null),
                    ],
                )?,
                true,
            ));
        }
    }
    for scope in &scopes {
        if scope.final_record || !affected.contains(&scope.key.id) {
            continue;
        }
        protection.push(labels(&scope.payload, &runtime.config.limits)?);
    }
    let influence = SecurityLabels::combine(protection.iter(), &runtime.config.limits)?;
    for scope in &scopes {
        if scope.final_record || !affected.contains(&scope.key.id) {
            continue;
        }
        let primary = scope_target == Some(scope.key.id.as_str())
            || (whole && scope.key.id == header.root_scope.to_string());
        let status = if primary && !cancel {
            "failed"
        } else {
            "cancelled"
        };
        let error = RuntimeFailure::new(if primary {
            code
        } else {
            FailureCode::Cancelled
        });
        writes.push(update(
            scope,
            replace(
                &scope.payload,
                [
                    ("status", Value::Text(status.into())),
                    (
                        "failure",
                        error.to_value(&runtime.config.limits).map_err(|_| err())?,
                    ),
                    (
                        "influence_labels",
                        influence.to_value(&runtime.config.limits)?,
                    ),
                ],
            )?,
            true,
        ));
    }
    for wait in all(runtime, lease, RecordFamily::Wait).await? {
        if wait.final_record {
            continue;
        }
        let invocation = string(&wait.payload, "invocation_id")?;
        if whole
            || node_target == Some(invocation.as_str())
            || nodes.iter().any(|n| {
                n.key.id == invocation
                    && string(&n.payload, "scope_instance_id")
                        .is_ok_and(|id| affected.contains(&id))
            })
        {
            writes.push(update(
                &wait,
                replace(&wait.payload, [("state", Value::Text("cancelled".into()))])?,
                true,
            ));
        }
    }
    let root_affected = whole || affected.contains(&header.root_scope.to_string());
    let status = if root_affected {
        Some(if cancel {
            RunStatus::Cancelled
        } else {
            RunStatus::Failed
        })
    } else {
        None
    };
    if !writes.is_empty() {
        step::commit(
            runtime,
            lease,
            format!(
                "stop:{}:{}:{}",
                scope_target.unwrap_or("run"),
                node_target.unwrap_or("none"),
                header.sequence
            ),
            writes,
            status,
        )
        .await?;
    }
    Ok(())
}
