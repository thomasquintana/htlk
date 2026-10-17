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
#[derive(Clone)]
pub(crate) enum Operation {
    Extend {
        request: super::extension::ExtensionRequest,
    },
    Approval {
        wait: WaitId,
        request: Digest,
        allow: bool,
    },
    Reply {
        wait: WaitId,
        value: Value,
    },
    Cancel {
        reason: String,
    },
}
#[derive(Clone)]
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
        if !self.recovered {
            return AtomicResponse::new(Box::pin(
                async { Err(RuntimeError::failure(FailureCode::Fenced)) }.into_actor(self),
            ));
        }
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
pub(super) async fn recorded(
    runtime: &Runtime,
    command: &Command,
) -> Result<Option<Value>, RuntimeError> {
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
pub(super) fn receipt(
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
pub(super) async fn route(runtime: &Runtime, command: Command) -> Result<Value, RuntimeError> {
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
    /// Resolves exact-action policy approval. Ordinary replies cannot call this
    /// disposition path or produce a graph-visible authority Boolean.
    ///
    /// # Errors
    /// Returns purpose/request/generation/deadline conflicts or approver denial.
    pub async fn submit_approval(
        &self,
        context: &HostContext,
        wait_id: WaitId,
        request_digest: Digest,
        allow: bool,
        idempotency_key: &str,
    ) -> Result<ReplyReceipt, RuntimeError> {
        let (run, wait) = self
            .store
            .find_record(RecordFamily::Wait, &wait_id.to_string())
            .await?;
        if string(&wait.payload, "purpose")? != "approval" {
            return Err(RuntimeError::failure(FailureCode::Policy));
        }
        authorize_approval(self, context, &run, &wait, wait_id, request_digest)?;
        if string(&wait.payload, "request_digest")? != request_digest.to_string() {
            return Err(RuntimeError::failure(FailureCode::Policy));
        }
        if idempotency_key.is_empty() {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let args = Value::Array(vec![
            Value::Text(request_digest.to_string()),
            Value::Bool(allow),
        ]);
        let digest = host_request_digest(
            context.principal(),
            "submit_approval",
            &wait_id.to_string(),
            &args,
            &self.config.limits,
        )
        .map_err(|_| err())?;
        let command = Command {
            run,
            context: context.clone(),
            method: "submit_approval".into(),
            target: wait_id.to_string(),
            key: idempotency_key.into(),
            digest,
            operation: Operation::Approval {
                wait: wait_id,
                request: request_digest,
                allow,
            },
        };
        if let Some(response) = recorded(self, &command).await? {
            return Ok(ReplyReceipt {
                wait_id,
                disposition: string(&response, "disposition")?,
            });
        }
        let state = string(&wait.payload, "state")?;
        if state == "accepted" {
            let decision = if allow { "allow" } else { "deny" };
            if string(&wait.payload, "decision")? != decision {
                return Err(RuntimeError::failure(FailureCode::IdempotencyConflict));
            }
            return Ok(ReplyReceipt {
                wait_id,
                disposition: decision.into(),
            });
        }
        if state != "open"
            || self.store.coordinator_time(self.clock.now_ms()?).await?
                >= number(&wait.payload, "expires_at")?
        {
            return Err(RuntimeError::failure(FailureCode::ApprovalDenied));
        }
        let response = route(self, command).await?;
        Ok(ReplyReceipt {
            wait_id,
            disposition: string(&response, "disposition")?,
        })
    }
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
        authorize_reply(self, context, &run, &wait, wait_id).await?;
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
            return Err(RuntimeError::failure(
                if string(&wait.payload, "state")? == "expired" {
                    FailureCode::WaitExpired
                } else {
                    FailureCode::Cancelled
                },
            ));
        }
        if self.store.coordinator_time(self.clock.now_ms()?).await?
            >= number(&wait.payload, "expires_at")?
        {
            return Err(RuntimeError::failure(FailureCode::WaitExpired));
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
        if recorded(self, &command).await?.is_none() {
            if !self.store.run_header(run).await?.status.is_terminal() {
                let result = route(self, command.clone()).await;
                if result.is_err() && !self.store.run_header(run).await?.status.is_terminal() {
                    result?;
                }
            }
            if recorded(self, &command).await?.is_none() {
                let status = self.store.run_header(run).await?.status;
                let response = object([("status", Value::Text(status.as_str().into()))])?;
                let mut write = receipt(self, &command, response)?;
                write.payload = replace(&write.payload, [("reason", Value::Text(reason.into()))])?;
                let now = self.store.coordinator_time(self.clock.now_ms()?).await?;
                self.store.terminal_receipt(run, write, now).await?;
            }
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
        Operation::Extend { request } => {
            super::extension::install(&runtime, &lease, &command, request).await
        }
        Operation::Approval {
            wait: id,
            request,
            allow,
        } => {
            let wait = runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Wait, id))
                .await?;
            authorize_approval(
                &runtime,
                &command.context,
                lease.run_id(),
                &wait,
                *id,
                *request,
            )?;
            if string(&wait.payload, "purpose")? != "approval"
                || string(&wait.payload, "request_digest")? != request.to_string()
            {
                return Err(RuntimeError::failure(FailureCode::Policy));
            }
            let decision = if *allow { "allow" } else { "deny" };
            let response = object([("disposition", Value::Text(decision.into()))])?;
            if string(&wait.payload, "state")? == "accepted" {
                if string(&wait.payload, "decision")? == decision {
                    return Ok(response);
                }
                return Err(RuntimeError::failure(FailureCode::IdempotencyConflict));
            }
            let invocation = runtime
                .store
                .record(
                    lease.run_id(),
                    &key(
                        RecordFamily::Invocation,
                        string(&wait.payload, "invocation_id")?,
                    ),
                )
                .await?;
            if invocation.final_record
                || string(&invocation.payload, "approval_wait")? != id.to_string()
            {
                return Err(RuntimeError::failure(FailureCode::Policy));
            }
            let now = runtime
                .store
                .coordinator_time(runtime.clock.now_ms()?)
                .await?;
            if wait.final_record || now >= number(&wait.payload, "expires_at")? {
                return Err(RuntimeError::failure(FailureCode::ApprovalDenied));
            }
            let disposition_labels = runtime.policy.approval_labels(
                &command.context,
                lease.run_id(),
                *id,
                field(&wait.payload, "approval_request")?,
                *allow,
            )?;
            let protection = labels(&wait.payload, &runtime.config.limits)?
                .union(&disposition_labels, &runtime.config.limits)?;
            let influence = labels(&invocation.payload, &runtime.config.limits)?
                .union(&protection, &runtime.config.limits)?;
            let payload = replace(
                &wait.payload,
                [
                    ("state", Value::Text("accepted".into())),
                    ("decision", Value::Text(decision.into())),
                    ("approver", Value::Text(command.context.principal().into())),
                    (
                        "influence_labels",
                        protection.to_value(&runtime.config.limits)?,
                    ),
                    (
                        "disposition_labels",
                        disposition_labels.to_value(&runtime.config.limits)?,
                    ),
                ],
            )?;
            step::commit(
                &runtime,
                &lease,
                format!("approval-disposition:{id}"),
                vec![
                    update(&wait, payload, true),
                    update(
                        &invocation,
                        replace(
                            &invocation.payload,
                            [
                                (
                                    "influence_labels",
                                    influence.to_value(&runtime.config.limits)?,
                                ),
                                ("reason", Value::Text("approval_resolved".into())),
                            ],
                        )?,
                        false,
                    ),
                    receipt(&runtime, &command, response.clone())?,
                ],
                None,
            )
            .await?;
            Ok(response)
        }
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
            let (response, writes) = reply_writes(&runtime, &lease, &command, *id, value).await?;
            if !writes.is_empty() {
                step::commit(
                    &runtime,
                    &lease,
                    format!("reply:{id}:{}", command.digest),
                    writes,
                    None,
                )
                .await?;
            }
            Ok(response)
        }
    }
}
pub(super) async fn reply_writes(
    runtime: &Runtime,
    lease: &RunLease,
    command: &Command,
    id: WaitId,
    value: &Value,
) -> Result<(Value, Vec<RecordWrite>), RuntimeError> {
    let record = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Wait, id))
        .await?;
    if string(&record.payload, "purpose")? != "input" {
        return Err(RuntimeError::failure(FailureCode::Policy));
    }
    authorize_reply(runtime, &command.context, lease.run_id(), &record, id).await?;
    if let Some(response) = recorded(runtime, command).await? {
        return Ok((response, vec![]));
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
            return Ok((response, vec![]));
        }
        return Err(RuntimeError::failure(FailureCode::IdempotencyConflict));
    }
    let header = runtime.store.run_header(lease.run_id()).await?;
    if header.status.is_terminal() || record.final_record {
        return Err(RuntimeError::failure(
            if string(&record.payload, "state")? == "expired" {
                FailureCode::WaitExpired
            } else {
                FailureCode::Cancelled
            },
        ));
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
    let reply_labels = runtime
        .policy
        .reply_labels(&command.context, lease.run_id(), id, value)?;
    let protection = labels(&record.payload, &runtime.config.limits)?
        .union(&reply_labels, &runtime.config.limits)?;
    let invocation = runtime
        .store
        .record(
            lease.run_id(),
            &key(
                RecordFamily::Invocation,
                string(&record.payload, "invocation_id")?,
            ),
        )
        .await?;
    if invocation.final_record {
        return Err(RuntimeError::failure(FailureCode::Cancelled));
    }
    let influence = labels(&invocation.payload, &runtime.config.limits)?
        .union(&protection, &runtime.config.limits)?;
    let payload = replace(
        &record.payload,
        [
            ("state", Value::Text("accepted".into())),
            ("accepted_reply_digest", Value::Text(digest.to_string())),
            ("reply_value", Value::Bytes(encoded)),
            (
                "influence_labels",
                protection.to_value(&runtime.config.limits)?,
            ),
            (
                "reply_labels",
                reply_labels.to_value(&runtime.config.limits)?,
            ),
            (
                "reply_principal",
                Value::Text(command.context.principal().into()),
            ),
        ],
    )?;
    Ok((
        response.clone(),
        vec![
            update(&record, payload, true),
            update(
                &invocation,
                replace(
                    &invocation.payload,
                    [
                        (
                            "influence_labels",
                            influence.to_value(&runtime.config.limits)?,
                        ),
                        ("reason", Value::Text("reply_received".into())),
                    ],
                )?,
                false,
            ),
            receipt(runtime, command, response)?,
        ],
    ))
}
fn authorize_approval(
    runtime: &Runtime,
    context: &HostContext,
    run: &RunId,
    wait: &crate::store::StoredRecord,
    id: WaitId,
    digest: Digest,
) -> Result<(), RuntimeError> {
    let request = field(&wait.payload, "approval_request")?;
    let authority = string(request, "authority")?;
    let principal = string(request, "principal")?;
    let protection = labels(&wait.payload, &runtime.config.limits)?;
    runtime.policy.authorize(
        context,
        Authorization::Approve {
            run_id: run,
            wait_id: id,
            request_digest: digest,
            authority: &authority,
            requesting_principal: &principal,
            labels: &protection,
        },
    )?;
    Ok(())
}
async fn authorize_reply(
    runtime: &Runtime,
    context: &HostContext,
    run: &RunId,
    wait: &crate::store::StoredRecord,
    id: WaitId,
) -> Result<(), RuntimeError> {
    let (authority, version, requester) =
        if let Some(policy) = map(&wait.payload)?.get("authorization_policy") {
            (
                string(policy, "authority")?,
                string(policy, "version")?,
                string(policy, "requester")?,
            )
        } else {
            // Earlier immutable receipts remain inspectable without their old native
            // implementation. Current host policy may explicitly reject legacy routing.
            let scope = runtime
                .store
                .record(
                    run,
                    &key(
                        RecordFamily::Scope,
                        string(&wait.payload, "scope_instance_id")?,
                    ),
                )
                .await?;
            (
                "host".into(),
                "legacy".into(),
                string(&scope.payload, "principal")?,
            )
        };
    let topic = string(&wait.payload, "topic")?;
    let protection = labels(&wait.payload, &runtime.config.limits)?;
    runtime.policy.authorize(
        context,
        Authorization::SubmitReply {
            run_id: run,
            wait_id: id,
            authority: &authority,
            policy_version: &version,
            requesting_principal: &requester,
            topic: &topic,
            labels: &protection,
        },
    )?;
    Ok(())
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
