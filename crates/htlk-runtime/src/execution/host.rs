//! Authenticated mutating commands routed to the local fenced transactional owner.
use super::{Coordinator, Supervisor, control, model::*, step};
use crate::{
    Authorization, HostContext, Runtime, RuntimeError,
    failure::FailureCode,
    identity::{RunId, WaitId, host_request_digest},
    records::RunStatus,
    store::{RecordFamily, RecordWrite, RunLease},
};
use actix::prelude::*;
use htlk_executable::{
    TypeContext, ValueType,
    cbor::{self, Value},
    digest::Digest,
};
use std::sync::Arc;

/// Immutable input-wait submission receipt; acceptance is not a policy grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyReceipt {
    /// Exact addressed wait.
    pub wait_id: WaitId,
    /// Durable reply state, independently of node postcondition.
    pub disposition: String,
}
/// Stable cancellation result; already committed terminal outcomes win.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelReceipt {
    /// Target run.
    pub run_id: RunId,
    /// Immutable result if terminal.
    pub status: RunStatus,
}
pub(crate) enum Operation {
    Reply { wait: WaitId, value: Value },
    Cancel { reason: String },
}
pub(crate) struct Command {
    pub run: RunId,
    pub context: HostContext,
    pub method: String,
    pub target: String,
    pub key: String,
    pub digest: Digest,
    pub operation: Operation,
}
impl Message for Command {
    type Result = Result<Value, RuntimeError>;
}
impl Handler<Command> for Supervisor {
    type Result = ResponseFuture<Result<Value, RuntimeError>>;
    fn handle(&mut self, msg: Command, _: &mut Self::Context) -> Self::Result {
        let actor = self.active.get(&msg.run).cloned();
        Box::pin(async move {
            let actor = actor.ok_or_else(|| RuntimeError::failure(FailureCode::Fenced))?;
            actor.send(msg).await.map_err(|_| err())?
        })
    }
}
impl Handler<Command> for Coordinator {
    type Result = AtomicResponse<Self, Result<Value, RuntimeError>>;
    fn handle(&mut self, msg: Command, _: &mut Self::Context) -> Self::Result {
        let runtime = self.runtime.clone();
        let lease = self.lease.clone();
        AtomicResponse::new(Box::pin(
            async move { apply(runtime, lease.ok_or_else(err)?, msg).await }.into_actor(self),
        ))
    }
}
fn receipt_key(
    context: &HostContext,
    method: &str,
    target: &str,
    key: &str,
    limits: &cbor::Limits,
) -> Result<String, RuntimeError> {
    Ok(crate::identity::hash(
        "host_receipt",
        &[
            &Value::Text(context.principal().into()),
            &Value::Text(method.into()),
            &Value::Text(target.into()),
            &Value::Text(key.into()),
        ],
        limits,
    )
    .map_err(|_| err())?
    .to_string())
}
async fn recorded(runtime: &Runtime, command: &Command) -> Result<Option<Value>, RuntimeError> {
    let id = receipt_key(
        &command.context,
        &command.method,
        &command.target,
        &command.key,
        &runtime.config.limits,
    )?;
    match runtime
        .store
        .record(&command.run, &key(RecordFamily::Reply, id))
        .await
    {
        Ok(record) => {
            if string(&record.payload, "request_digest")? != command.digest.to_string() {
                return Err(RuntimeError::failure(FailureCode::IdempotencyConflict));
            }
            Ok(Some(field(&record.payload, "response")?.clone()))
        }
        Err(crate::store::StoreError::Missing) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn receipt(
    runtime: &Runtime,
    command: &Command,
    response: Value,
) -> Result<RecordWrite, RuntimeError> {
    Ok(insert(
        RecordFamily::Reply,
        receipt_key(
            &command.context,
            &command.method,
            &command.target,
            &command.key,
            &runtime.config.limits,
        )?,
        object([
            ("request_digest", Value::Text(command.digest.to_string())),
            ("principal", Value::Text(command.context.principal().into())),
            ("method", Value::Text(command.method.clone())),
            ("target", Value::Text(command.target.clone())),
            ("response", response),
        ])?,
        true,
    ))
}
async fn route(runtime: &Runtime, command: Command) -> Result<Value, RuntimeError> {
    if let Some(response) = recorded(runtime, &command).await? {
        return Ok(response);
    }
    let address = runtime
        .execution
        .lock()
        .map_err(|_| err())?
        .as_ref()
        .and_then(actix::WeakAddr::upgrade)
        .ok_or_else(|| RuntimeError::failure(FailureCode::Fenced))?;
    address.send(command).await.map_err(|_| err())?
}
impl Runtime {
    /// Submits an authenticated typed reply to an explicit input wait. Exact-action
    /// approval waits are resolved only by the separate approval API.
    ///
    /// # Errors
    /// Returns policy/purpose/type/deadline/key conflicts or unavailable ownership.
    pub async fn submit_reply(
        &self,
        context: &HostContext,
        wait_id: WaitId,
        value: Value,
        idempotency_key: &str,
    ) -> Result<ReplyReceipt, RuntimeError> {
        let (run, wait) = self
            .store
            .find_record(RecordFamily::Wait, &wait_id.to_string())
            .await?;
        if string(&wait.payload, "purpose")? != "input" {
            return Err(RuntimeError::failure(FailureCode::Policy));
        }
        let protection = labels(&wait.payload, &self.config.limits)?;
        self.policy.authorize(
            context,
            Authorization::SubmitReply {
                run_id: &run,
                wait_id,
                labels: &protection,
            },
        )?;
        if idempotency_key.is_empty() {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let digest = host_request_digest(
            context.principal(),
            "submit_reply",
            &wait_id.to_string(),
            &value,
            &self.config.limits,
        )
        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
        let command = Command {
            run,
            context: context.clone(),
            method: "submit_reply".into(),
            target: wait_id.to_string(),
            key: idempotency_key.into(),
            digest,
            operation: Operation::Reply {
                wait: wait_id,
                value,
            },
        };
        if let Some(response) = recorded(self, &command).await? {
            return Ok(ReplyReceipt {
                wait_id,
                disposition: string(&response, "disposition")?,
            });
        }
        if string(&wait.payload, "state")? == "accepted" {
            let Operation::Reply { value, .. } = &command.operation else {
                return Err(err());
            };
            let ty = ValueType::from_value(
                field(&wait.payload, "response_type")?,
                TypeContext::Value,
                &self.config.limits,
            )
            .map_err(|_| err())?;
            let digest = crate::identity::ValueDigest::new(&ty, value, &self.config.limits)
                .map_err(|_| err())?;
            if string(&wait.payload, "accepted_reply_digest")? != digest.to_string() {
                return Err(RuntimeError::failure(FailureCode::IdempotencyConflict));
            }
            return Ok(ReplyReceipt {
                wait_id,
                disposition: "accepted".into(),
            });
        }
        if record_terminal_wait(&wait.payload)? {
            return Err(RuntimeError::failure(FailureCode::Cancelled));
        }
        let response = route(self, command).await?;
        Ok(ReplyReceipt {
            wait_id,
            disposition: string(&response, "disposition")?,
        })
    }
    /// Cancels future work/acceptance through the current fenced owner. Cancellation
    /// does not undo already sent effects or replace committed terminal outcomes.
    ///
    /// # Errors
    /// Returns authorization, conflicting key or unavailable current owner.
    pub async fn cancel_run(
        &self,
        context: &HostContext,
        run: &RunId,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<CancelReceipt, RuntimeError> {
        self.policy
            .authorize(context, Authorization::CancelRun { run_id: run })?;
        if idempotency_key.is_empty() {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let digest = host_request_digest(
            context.principal(),
            "cancel_run",
            run.as_str(),
            &Value::Text(reason.into()),
            &self.config.limits,
        )
        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
        let command = Command {
            run: run.clone(),
            context: context.clone(),
            method: "cancel_run".into(),
            target: run.to_string(),
            key: idempotency_key.into(),
            digest,
            operation: Operation::Cancel {
                reason: reason.into(),
            },
        };
        if recorded(self, &command).await?.is_none()
            && !self.store.run_header(run).await?.status.is_terminal()
        {
            route(self, command).await?;
        }
        Ok(CancelReceipt {
            run_id: run.clone(),
            status: self.store.run_header(run).await?.status,
        })
    }
}
async fn apply(
    runtime: Arc<Runtime>,
    lease: RunLease,
    command: Command,
) -> Result<Value, RuntimeError> {
    if let Some(response) = recorded(&runtime, &command).await? {
        return Ok(response);
    }
    match &command.operation {
        Operation::Cancel { reason } => {
            let response = object([("status", Value::Text("cancelled".into()))])?;
            let mut record = receipt(&runtime, &command, response.clone())?;
            record.payload = replace(&record.payload, [("reason", Value::Text(reason.clone()))])?;
            control::stop(
                &runtime,
                &lease,
                None,
                None,
                FailureCode::Cancelled,
                true,
                vec![record],
            )
            .await?;
            Ok(response)
        }
        Operation::Reply { wait: id, value } => {
            let record = runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Wait, id))
                .await?;
            if string(&record.payload, "purpose")? != "input" {
                return Err(RuntimeError::failure(FailureCode::Policy));
            }
            let encoded = cbor::encode(value, &runtime.config.limits)?;
            let digest = crate::identity::ValueDigest::new(
                &ValueType::from_value(
                    field(&record.payload, "response_type")?,
                    TypeContext::Value,
                    &runtime.config.limits,
                )
                .map_err(|_| err())?,
                value,
                &runtime.config.limits,
            )
            .map_err(|_| err())?;
            let response = object([("disposition", Value::Text("accepted".into()))])?;
            if string(&record.payload, "state")? == "accepted" {
                if string(&record.payload, "accepted_reply_digest")? == digest.to_string() {
                    return Ok(response);
                }
                return Err(RuntimeError::failure(FailureCode::IdempotencyConflict));
            }
            let header = runtime.store.run_header(lease.run_id()).await?;
            if header.status.is_terminal() || record.final_record {
                return Err(RuntimeError::failure(FailureCode::Cancelled));
            }
            let scope = runtime
                .store
                .record(
                    lease.run_id(),
                    &key(
                        RecordFamily::Scope,
                        string(&record.payload, "scope_instance_id")?,
                    ),
                )
                .await?;
            let executable = runtime.admitted(digest_value(&scope.payload)?).await?;
            let ty = ValueType::from_value(
                field(&record.payload, "response_type")?,
                TypeContext::Value,
                &runtime.config.limits,
            )
            .map_err(|_| err())?;
            crate::validate_typed_value(
                value,
                &ty,
                Some(executable.schemas()),
                &runtime.config.limits,
                &executable.policy().fields().evaluator_limits,
            )
            .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
            let now = runtime
                .store
                .coordinator_time(runtime.clock.now_ms()?)
                .await?;
            if now >= number(&record.payload, "expires_at")? {
                return Err(RuntimeError::failure(FailureCode::WaitExpired));
            }
            let payload = replace(
                &record.payload,
                [
                    ("state", Value::Text("accepted".into())),
                    ("accepted_reply_digest", Value::Text(digest.to_string())),
                    ("reply_value", Value::Bytes(encoded)),
                    (
                        "reply_principal",
                        Value::Text(command.context.principal().into()),
                    ),
                ],
            )?;
            step::commit(
                &runtime,
                &lease,
                format!("reply:{}:{}", id, record.revision),
                vec![
                    update(&record, payload, true),
                    receipt(&runtime, &command, response.clone())?,
                ],
                None,
            )
            .await?;
            Ok(response)
        }
    }
}
fn digest_value(value: &Value) -> Result<Digest, RuntimeError> {
    digest(value, "origin_executable")
}
fn record_terminal_wait(value: &Value) -> Result<bool, RuntimeError> {
    Ok(matches!(
        string(value, "state")?.as_str(),
        "cancelled" | "expired"
    ))
}
