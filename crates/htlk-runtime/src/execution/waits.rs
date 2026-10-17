//! Durable lease-free typed waits, with replies proposed through ordinary acceptance.
use super::{
    model::*,
    step::{self, OutputProposal, ScopeState},
};
use crate::{
    Runtime, RuntimeError,
    failure::FailureCode,
    identity::{InvocationId, WaitId},
    store::{RecordFamily, RunLease, StoredRecord},
};
use htlk_executable::{
    Node, Operation, TypeContext,
    cbor::{self, Value},
};
use std::sync::Arc;
pub(super) async fn operate(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    node: &Node,
    record: &StoredRecord,
) -> Result<bool, RuntimeError> {
    let invocation: InvocationId = record.key.id.parse().map_err(|_| err())?;
    let wait_id = WaitId::input(invocation, &runtime.config.limits).map_err(|_| err())?;
    let wait = match runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Wait, wait_id))
        .await
    {
        Ok(wait) => wait,
        Err(crate::store::StoreError::Missing) => {
            let Operation::Wait { topic, timeout_ms } = &node.fields().operation else {
                return Err(err());
            };
            let now = runtime
                .store
                .coordinator_time(runtime.clock.now_ms()?)
                .await?;
            let expiry = now
                .checked_add(i64::try_from(*timeout_ms).map_err(|_| err())?)
                .ok_or_else(err)?
                .min(number(&record.payload, "deadline_ms")?);
            let request = entry(
                map(field(&record.payload, "inputs")?)?
                    .get("request")
                    .ok_or_else(err)?,
            )?
            .ok_or_else(err)?;
            let payload = object([
                ("record_version", Value::Text("0.1".into())),
                ("run_id", Value::Text(lease.run_id().to_string())),
                ("invocation_id", Value::Text(invocation.to_string())),
                ("scope_instance_id", Value::Text(scope.id.to_string())),
                ("purpose", Value::Text("input".into())),
                ("generation", Value::Integer(0)),
                ("topic", Value::Text(topic.clone())),
                ("request_artifact_id", Value::Text(request.to_string())),
                (
                    "response_type",
                    node.fields()
                        .outputs
                        .get("value")
                        .ok_or_else(err)?
                        .value_type()
                        .to_value(TypeContext::Value, &runtime.config.limits)
                        .map_err(|_| err())?,
                ),
                ("expires_at", Value::Integer(expiry)),
                (
                    "influence_labels",
                    field(&record.payload, "influence_labels")?.clone(),
                ),
                ("state", Value::Text("open".into())),
                ("accepted_reply_digest", Value::Null),
                ("reply_value", Value::Null),
            ])?;
            step::commit(
                runtime,
                lease,
                format!("wait-open:{wait_id}"),
                vec![
                    insert(RecordFamily::Wait, wait_id, payload, false),
                    update(
                        record,
                        replace(
                            &record.payload,
                            [
                                ("wait_id", Value::Text(wait_id.to_string())),
                                ("reason", Value::Text("waiting_reply".into())),
                            ],
                        )?,
                        false,
                    ),
                ],
                None,
            )
            .await?;
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    };
    match string(&wait.payload, "state")?.as_str() {
        "open" => {
            let now = runtime
                .store
                .coordinator_time(runtime.clock.now_ms()?)
                .await?;
            if now >= number(&wait.payload, "expires_at")? {
                let mut writes = vec![update(
                    &wait,
                    replace(&wait.payload, [("state", Value::Text("expired".into()))])?,
                    true,
                )];
                let failed = replace(
                    &record.payload,
                    [
                        ("status", Value::Text("failed".into())),
                        (
                            "failure",
                            crate::failure::RuntimeFailure::new(FailureCode::WaitExpired)
                                .to_value(&runtime.config.limits)
                                .map_err(|_| err())?,
                        ),
                        ("reason", Value::Null),
                    ],
                )?;
                writes.push(update(record, failed, true));
                step::commit(
                    runtime,
                    lease,
                    format!("wait-expire:{wait_id}"),
                    writes,
                    None,
                )
                .await?;
                return Ok(true);
            }
            Ok(false)
        }
        "accepted" => {
            let Value::Bytes(raw) = field(&wait.payload, "reply_value")? else {
                return Err(err());
            };
            let value = cbor::decode(raw, &runtime.config.limits)?;
            let proposed = object([(
                "value",
                object([("state", Value::Text("value".into())), ("value", value)])?,
            )])?;
            step::accept_node(
                runtime,
                lease,
                scope,
                node,
                record,
                OutputProposal {
                    values: &proposed,
                    influence: labels(&record.payload, &runtime.config.limits)?,
                    parents: vec![],
                },
            )
            .await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
