//! Versioned exact-action approval generations, never graph-visible booleans.
use super::{
    model::*,
    step::{self, ScopeState},
};
use crate::{
    Runtime, RuntimeError,
    failure::FailureCode,
    identity::{InvocationId, WaitId},
    mcp::McpPolicy,
    store::{RecordFamily, RunLease, StoredRecord},
};
use htlk_executable::{McpBinding, Node, cbor::Value};
use std::sync::Arc;
pub(super) async fn gate(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    binding: &McpBinding,
    _node: &Node,
    record: &StoredRecord,
    policy: &McpPolicy,
) -> Result<bool, RuntimeError> {
    let Some(authority) = &policy.approval_authority else {
        return Ok(true);
    };
    let invocation: InvocationId = record.key.id.parse().map_err(|_| err())?;
    let request = object([
        (
            "origin_executable",
            field(&record.payload, "origin_executable")?.clone(),
        ),
        (
            "node_definition",
            field(&record.payload, "definition_digest")?.clone(),
        ),
        ("input_hash", field(&record.payload, "input_hash")?.clone()),
        ("invocation_id", Value::Text(invocation.to_string())),
        (
            "operation",
            Value::Text(
                binding
                    .digest(&runtime.config.limits)
                    .map_err(|_| err())?
                    .to_string(),
            ),
        ),
        ("authority", Value::Text(authority.clone())),
        ("principal", Value::Text(scope.context.principal().into())),
        ("policy_version", Value::Text(policy.policy_version.clone())),
        ("expires_at", field(&record.payload, "deadline_ms")?.clone()),
    ])?;
    let digest = crate::identity::hash("approval_request", &[&request], &runtime.config.limits)
        .map_err(|_| err())?;
    let mut generation = 0i64;
    let mut writes = Vec::new();
    if let Some(Value::Text(id)) = map(&record.payload)?.get("approval_wait") {
        let old = runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Wait, id))
            .await?;
        generation = number(&old.payload, "generation")?;
        if string(&old.payload, "request_digest")? == digest.to_string() {
            let now = runtime
                .store
                .coordinator_time(runtime.clock.now_ms()?)
                .await?;
            if now >= number(&old.payload, "expires_at")? {
                let writes = if old.final_record {
                    vec![]
                } else {
                    vec![update(
                        &old,
                        replace(&old.payload, [("state", Value::Text("expired".into()))])?,
                        true,
                    )]
                };
                step::finish_node(
                    runtime,
                    lease,
                    record,
                    "failed",
                    Some(crate::failure::RuntimeFailure::new(
                        FailureCode::ApprovalDenied,
                    )),
                    labels(&record.payload, &runtime.config.limits)?.union(
                        &labels(&old.payload, &runtime.config.limits)?,
                        &runtime.config.limits,
                    )?,
                    writes,
                )
                .await?;
                return Ok(false);
            }
            match string(&old.payload, "state")?.as_str() {
                "open" => return Ok(false),
                "accepted" => {
                    if string(&old.payload, "decision")? == "allow" {
                        return Ok(true);
                    } else {
                        step::finish_failure(
                            runtime,
                            lease,
                            record,
                            FailureCode::ApprovalDenied,
                            labels(&record.payload, &runtime.config.limits)?,
                        )
                        .await?;
                        return Ok(false);
                    }
                }
                _ => {}
            }
        }
        if !old.final_record {
            writes.push(update(
                &old,
                replace(&old.payload, [("state", Value::Text("cancelled".into()))])?,
                true,
            ));
        }
        generation = generation.checked_add(1).ok_or_else(err)?;
    }
    let id = WaitId::approval(invocation, generation as u64, &runtime.config.limits)
        .map_err(|_| err())?;
    if runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?
        >= number(&record.payload, "deadline_ms")?
    {
        step::finish_node(
            runtime,
            lease,
            record,
            "failed",
            Some(crate::failure::RuntimeFailure::new(
                FailureCode::ApprovalDenied,
            )),
            labels(&record.payload, &runtime.config.limits)?,
            writes,
        )
        .await?;
        return Ok(false);
    }
    let payload = object([
        ("run_id", Value::Text(lease.run_id().to_string())),
        ("scope_instance_id", Value::Text(scope.id.to_string())),
        ("invocation_id", Value::Text(invocation.to_string())),
        ("purpose", Value::Text("approval".into())),
        ("generation", Value::Integer(generation)),
        ("topic", Value::Text("approval".into())),
        ("request_digest", Value::Text(digest.to_string())),
        ("approval_request", request),
        ("expires_at", field(&record.payload, "deadline_ms")?.clone()),
        ("state", Value::Text("open".into())),
        ("decision", Value::Null),
        (
            "influence_labels",
            field(&record.payload, "influence_labels")?.clone(),
        ),
    ])?;
    writes.push(insert(RecordFamily::Wait, id, payload, false));
    writes.push(update(
        record,
        replace(
            &record.payload,
            [
                ("approval_wait", Value::Text(id.to_string())),
                ("reason", Value::Text("awaiting_approval".into())),
            ],
        )?,
        false,
    ));
    step::commit(
        runtime,
        lease,
        format!("approval:{invocation}:{generation}"),
        writes,
        None,
    )
    .await?;
    Ok(false)
}
