//! Durable MCP attempts, intent-before-send, response-before-acceptance and safe retry.
use super::{
    model::*,
    step::{self, OutputProposal, ScopeState},
};
use crate::{
    Authorization, Runtime, RuntimeError,
    failure::FailureCode,
    identity::{AttemptId, InvocationId},
    mcp::{McpCancellation, McpDeduplication, McpDelivery, McpOutcome, McpPolicy, McpRequest},
    store::{RecordFamily, RunLease, StoredRecord},
};
use htlk_executable::{
    ExecutionLimits, JsonDocument, McpBinding, McpBindingKind, Node, Operation,
    cbor::{Map, Value},
};
use std::{collections::BTreeSet, sync::Arc};
pub(super) async fn operate(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    node: &Node,
    record: &StoredRecord,
) -> Result<bool, RuntimeError> {
    match operate_inner(runtime, lease, scope, node, record).await {
        Err(RuntimeError::Failure(failure)) => {
            step::finish_failure(
                runtime,
                lease,
                record,
                failure.code(),
                labels(&record.payload, &runtime.config.limits)?,
            )
            .await?;
            Ok(true)
        }
        result => result,
    }
}
async fn operate_inner(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    node: &Node,
    record: &StoredRecord,
) -> Result<bool, RuntimeError> {
    let Operation::Mcp {
        binding: binding_id,
        retry,
    } = &node.fields().operation
    else {
        return Err(err());
    };
    let binding = scope
        .executable
        .document()
        .fields()
        .bindings
        .get(binding_id)
        .ok_or_else(err)?;
    let invocation: InvocationId = record.key.id.parse().map_err(|_| err())?;
    let parameters = if let Some(Value::Text(id)) = map(&record.payload)?.get("attempt_id") {
        field(
            &runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Attempt, id))
                .await?
                .payload,
            "params",
        )?
        .clone()
    } else {
        request(runtime, scope, binding, record).await?.1
    };
    let mut policy = runtime
        .policy
        .mcp_policy_for_request(&scope.context, binding, &parameters)?;
    let effective = ExecutionLimits::from_value(
        field(&record.payload, "effective_limits")?,
        &runtime.config.limits,
    )
    .map_err(|_| err())?;
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    if let Some(Value::Integer(due)) = map(&record.payload)?.get("retry_at")
        && now < *due
    {
        return Ok(false);
    }
    if let Some(Value::Text(attempt_id)) = map(&record.payload)?.get("attempt_id") {
        let attempt = runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Attempt, attempt_id))
            .await?;
        match string(&attempt.payload, "dispatch_state")?.as_str() {
            "response_recorded" => {
                if !policy.replay_safe
                    && runtime.mcp.is_some()
                    && let Ok((verified, _)) = deduplication(
                        runtime,
                        binding,
                        invocation,
                        field(&attempt.payload, "params")?,
                    )
                    && verified != Value::Null
                    && verified == *field(&attempt.payload, "deduplication")?
                {
                    policy.replay_safe = true;
                }
                let response = runtime
                    .store
                    .record(lease.run_id(), &key(RecordFamily::Response, attempt_id))
                    .await?;
                let delivery =
                    McpDelivery::parse(&string(&response.payload, "delivery")?).ok_or_else(err)?;
                policy.replay_safe |= delivery == McpDelivery::NotSent;
                let mut protected_record = record.clone();
                protected_record.payload = replace(
                    &record.payload,
                    [(
                        "influence_labels",
                        labels(&record.payload, &runtime.config.limits)?
                            .union(
                                &labels(&response.payload, &runtime.config.limits)?,
                                &runtime.config.limits,
                            )?
                            .union(&policy.result_labels, &runtime.config.limits)?
                            .to_value(&runtime.config.limits)?,
                    )],
                )?;
                let record = &protected_record;
                if let Some(Value::Text(code)) = map(&response.payload)?.get("failure_code") {
                    let observed = map(&response.payload)?
                        .get("observed_failure_code")
                        .unwrap_or(&Value::Null)
                        .clone();
                    let code: FailureCode = if let Value::Text(observed) = observed {
                        observed.parse().map_err(|_| err())?
                    } else {
                        code.parse().map_err(|_| err())?
                    };
                    let code = if delivery == McpDelivery::PossiblySent && !policy.replay_safe {
                        FailureCode::EffectUncertain
                    } else {
                        code
                    };
                    return fail_attempt(runtime, lease, node, record, &attempt, code, &policy)
                        .await;
                }
                let Value::Bytes(raw) = field(&response.payload, "raw")? else {
                    return Err(err());
                };
                if digest(&response.payload, "raw_digest")?
                    != htlk_executable::digest::hash_bytes(raw)
                {
                    return Err(RuntimeError::failure(FailureCode::Digest));
                }
                let result = match normalize(runtime, scope, binding, raw, &attempt.payload).await {
                    Ok(result) => result,
                    Err(error) => {
                        return fail_attempt(
                            runtime,
                            lease,
                            node,
                            record,
                            &attempt,
                            error.code(),
                            &policy,
                        )
                        .await;
                    }
                };
                let proposed = object([(
                    "value",
                    object([("state", Value::Text("value".into())), ("value", result)])?,
                )])?;
                let influence = labels(&record.payload, &runtime.config.limits)?
                    .union(
                        &labels(&response.payload, &runtime.config.limits)?,
                        &runtime.config.limits,
                    )?
                    .union(&policy.result_labels, &runtime.config.limits)?;
                step::accept_node(
                    runtime,
                    lease,
                    scope,
                    node,
                    record,
                    OutputProposal {
                        values: &proposed,
                        influence,
                        parents: vec![],
                    },
                )
                .await?;
                return Ok(true);
            }
            "possibly_sent" => {
                if runtime.mcp.is_some()
                    && let Ok((verified, _)) = deduplication(
                        runtime,
                        binding,
                        invocation,
                        field(&attempt.payload, "params")?,
                    )
                    && verified == *field(&attempt.payload, "deduplication")?
                {
                    policy.replay_safe |= verified != Value::Null;
                }
                let code = if policy.replay_safe {
                    FailureCode::McpTransport
                } else {
                    FailureCode::EffectUncertain
                };
                return fail_attempt(runtime, lease, node, record, &attempt, code, &policy).await;
            }
            "prepared" => {}
            _ => return Err(err()),
        }
    } else {
        let number = match map(&record.payload)?.get("attempt_number") {
            None | Some(Value::Null) => 1,
            Some(Value::Integer(n)) if *n > 0 => u64::try_from(*n)
                .map_err(|_| err())?
                .checked_add(1)
                .ok_or_else(err)?,
            _ => return Err(err()),
        };
        if number > retry.max_attempts() {
            step::finish_failure(
                runtime,
                lease,
                record,
                FailureCode::McpTransport,
                labels(&record.payload, &runtime.config.limits)?,
            )
            .await?;
            return Ok(true);
        }
        let attempt =
            AttemptId::new(invocation, number, &runtime.config.limits).map_err(|_| err())?;
        let (method, params, uri) = request(runtime, scope, binding, record).await?;
        let encoded = JsonDocument::from_value(&params, &runtime.config.limits)
            .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
        let (dedup, protocol_key) = deduplication(runtime, binding, invocation, &params)?;
        let protocol_key = protocol_key.map_or(Value::Null, Value::Text);
        if let Some(previous) = map(&record.payload)?.get("dispatch_deduplication")
            && (previous != &dedup
                || field(&record.payload, "dispatch_protocol_key")? != &protocol_key)
        {
            return Err(RuntimeError::failure(FailureCode::Policy));
        }
        let payload = object([
            ("dispatch_state", Value::Text("prepared".into())),
            ("invocation_id", Value::Text(invocation.to_string())),
            ("number", Value::Integer(number as i64)),
            ("method", Value::Text(method)),
            ("params", params),
            ("deduplication", dedup.clone()),
            ("protocol_key", protocol_key.clone()),
            ("encoded_request", Value::Bytes(encoded.as_bytes().to_vec())),
            ("requested_uri", uri.map_or(Value::Null, Value::Text)),
            ("owner_fence", Value::Integer(lease.fence())),
            (
                "timeout_ms",
                Value::Integer(
                    i64::try_from(effective.attempt_timeout_ms().ok_or_else(err)?)
                        .map_err(|_| err())?,
                ),
            ),
        ])?;
        let payload_node = replace(
            &record.payload,
            [
                ("attempt_id", Value::Text(attempt.to_string())),
                ("attempt_number", Value::Integer(number as i64)),
                ("reason", Value::Text("prepared".into())),
                ("dispatch_deduplication", dedup),
                ("dispatch_protocol_key", protocol_key),
            ],
        )?;
        step::commit(
            runtime,
            lease,
            format!("prepare:{attempt}"),
            vec![
                insert(RecordFamily::Attempt, attempt, payload, false),
                update(record, payload_node, false),
            ],
            None,
        )
        .await?;
        return Ok(true);
    }
    let attempt_id = string(&record.payload, "attempt_id")?;
    let attempt = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Attempt, &attempt_id))
        .await?;
    let uri = map(&attempt.payload)?.get("requested_uri").and_then(|v| {
        if let Value::Text(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    });
    let influence = labels(&record.payload, &runtime.config.limits)?;
    let (expected_method, expected_params, expected_uri) =
        request(runtime, scope, binding, record).await?;
    if expected_method != string(&attempt.payload, "method")?
        || expected_params != *field(&attempt.payload, "params")?
        || parameters != expected_params
        || expected_uri.map_or(Value::Null, Value::Text)
            != *field(&attempt.payload, "requested_uri")?
    {
        return Err(RuntimeError::failure(FailureCode::Digest));
    }
    let input_hash = digest(&record.payload, "input_hash")?;
    let method = string(&attempt.payload, "method")?;
    let Value::Bytes(encoded_parameters) = field(&attempt.payload, "encoded_request")? else {
        return Err(err());
    };
    if JsonDocument::from_value(&parameters, &runtime.config.limits)
        .map_err(|_| err())?
        .as_bytes()
        != encoded_parameters
    {
        return Err(RuntimeError::failure(FailureCode::Digest));
    }
    let idempotency_key = map(&attempt.payload)?.get("protocol_key").and_then(|v| {
        if let Value::Text(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    });
    let authorize = |protection: &crate::records::SecurityLabels| {
        runtime.policy.authorize(
            &scope.context,
            Authorization::DispatchMcp {
                method: &method,
                parameters: &parameters,
                encoded_parameters,
                idempotency_key,
                binding,
                invocation,
                input_hash,
                labels: protection,
                resource_uri: uri,
            },
        )
    };
    authorize(&influence)?;
    if !super::approval::gate(runtime, lease, scope, binding, node, record, &policy).await? {
        return Ok(runtime
            .store
            .record(lease.run_id(), &record.key)
            .await?
            .revision
            != record.revision);
    }
    let adapter = runtime
        .mcp
        .as_ref()
        .ok_or_else(|| RuntimeError::failure(FailureCode::McpIdentity))?;
    let cancellation =
        runtime
            .mcp_control
            .attempt(lease, invocation, attempt_id.parse().map_err(|_| err())?)?;
    if cancellation.token.is_cancelled() {
        return Err(crate::store::StoreError::Fenced.into());
    }
    let latest = runtime.store.record(lease.run_id(), &record.key).await?;
    if latest.final_record {
        return Err(crate::store::StoreError::Terminal.into());
    }
    if latest.revision != record.revision {
        return Err(crate::store::StoreError::Conflict.into());
    }
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    if now >= number(&record.payload, "deadline_ms")? {
        return Err(RuntimeError::failure(FailureCode::Timeout));
    }
    let connect_timeout = effective
        .attempt_timeout_ms()
        .ok_or_else(err)?
        .min((number(&record.payload, "deadline_ms")? - now) as u64);
    let connected = bounded(
        &cancellation.token,
        connect_timeout,
        runtime.config.cancellation_grace_ms,
        adapter.connect(binding, &scope.context, &runtime.config.limits),
    )
    .await;
    if cancellation.token.is_cancelled() {
        if let Ok(Ok(mut connection)) = connected {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(runtime.config.cancellation_grace_ms),
                connection.close(),
            )
            .await;
        }
        return Err(crate::store::StoreError::Fenced.into());
    }
    let mut connection = match connected {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            return record_outcome(
                runtime,
                lease,
                &attempt,
                McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(error),
                },
                &policy,
                influence,
                false,
            )
            .await;
        }
        Err(_) => {
            return record_outcome(
                runtime,
                lease,
                &attempt,
                McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(crate::failure::RuntimeFailure::new(FailureCode::McpTimeout)),
                },
                &policy,
                influence,
                false,
            )
            .await;
        }
    };
    let result = async {
        if connection.descriptor().digest() != binding.descriptor() {
            return record_outcome(
                runtime,
                lease,
                &attempt,
                McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(crate::failure::RuntimeFailure::new(
                        FailureCode::McpDescriptor,
                    )),
                },
                &policy,
                influence,
                false,
            )
            .await;
        }
        // No connection negotiation remains after this policy/approval decision.
        let mut policy =
            runtime
                .policy
                .mcp_policy_for_request(&scope.context, binding, &parameters)?;
        let (verified, protocol_key) = deduplication(
            runtime,
            binding,
            invocation,
            field(&attempt.payload, "params")?,
        )?;
        if verified != *field(&attempt.payload, "deduplication")? {
            return Err(RuntimeError::failure(FailureCode::Policy));
        }
        policy.replay_safe |= verified != Value::Null;
        authorize(&influence)?;
        if !super::approval::gate(runtime, lease, scope, binding, node, record, &policy).await? {
            return Ok(runtime
                .store
                .record(lease.run_id(), &record.key)
                .await?
                .revision
                != record.revision);
        }
        let now = runtime
            .store
            .coordinator_time(runtime.clock.now_ms()?)
            .await?;
        if now >= number(&record.payload, "deadline_ms")? {
            return Err(RuntimeError::failure(FailureCode::Timeout));
        }
        if cancellation.token.is_cancelled() {
            return Err(crate::store::StoreError::Fenced.into());
        }
        let mut usage = match reserve(runtime, lease, scope, record, &attempt_id).await {
            Ok(writes) => writes,
            Err(error) => {
                step::finish_failure(runtime, lease, record, error.code(), influence).await?;
                return Ok(true);
            }
        };
        usage.push(update(
            &attempt,
            replace(
                &attempt.payload,
                [
                    ("dispatch_state", Value::Text("possibly_sent".into())),
                    ("owner_fence", Value::Integer(lease.fence())),
                    ("policy_version", Value::Text(policy.policy_version.clone())),
                    ("replay_authorized", Value::Bool(policy.replay_safe)),
                    (
                        "result_labels",
                        policy.result_labels.to_value(&runtime.config.limits)?,
                    ),
                    ("input_hash", field(&record.payload, "input_hash")?.clone()),
                    (
                        "influence_labels",
                        influence.to_value(&runtime.config.limits)?,
                    ),
                    (
                        "approval_wait",
                        if policy.approval_authority.is_some() {
                            map(&record.payload)?
                                .get("approval_wait")
                                .cloned()
                                .unwrap_or(Value::Null)
                        } else {
                            Value::Null
                        },
                    ),
                ],
            )?,
            false,
        ));
        step::commit(
            runtime,
            lease,
            format!("dispatch:{attempt_id}"),
            usage,
            None,
        )
        .await?;
        let timeout = effective.attempt_timeout_ms().ok_or_else(err)?.min(
            u64::try_from(number(&record.payload, "deadline_ms")?.saturating_sub(now))
                .unwrap_or(1)
                .max(1),
        );
        let outcome = match bounded(
            &cancellation.token,
            timeout,
            runtime.config.cancellation_grace_ms,
            connection.invoke(McpRequest {
                response_limits: runtime.config.limits.clone(),
                cancellation: cancellation.token.clone(),
                cancellation_grace_ms: runtime.config.cancellation_grace_ms,
                method: string(&attempt.payload, "method")?,
                params: field(&attempt.payload, "params")?.clone(),
                timeout_ms: timeout,
                max_response_bytes: response_limit(runtime),
                idempotency_key: protocol_key,
            }),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(delivery) => McpOutcome {
                delivery,
                result: Err(crate::failure::RuntimeFailure::new(FailureCode::McpTimeout)),
            },
        };
        record_outcome(runtime, lease, &attempt, outcome, &policy, influence, true).await
    }
    .await;
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(runtime.config.cancellation_grace_ms),
        connection.close(),
    )
    .await;
    result
}
fn response_limit(runtime: &Runtime) -> usize {
    runtime
        .config
        .limits
        .max_document_bytes
        .min(runtime.store.config().limits.max_document_bytes)
        / 8
}
async fn bounded<F: Future>(
    signal: &McpCancellation,
    timeout: u64,
    grace: u64,
    future: F,
) -> Result<F::Output, McpDelivery> {
    if signal.is_cancelled() {
        return Err(McpDelivery::NotSent);
    }
    let Some(deadline) =
        tokio::time::Instant::now().checked_add(std::time::Duration::from_millis(timeout))
    else {
        return Err(McpDelivery::NotSent);
    };
    tokio::pin!(future);
    tokio::time::timeout_at(deadline,async{
        tokio::select!{
            biased;
            _=signal.cancelled()=>{let until=tokio::time::Instant::now().checked_add(std::time::Duration::from_millis(grace)).unwrap_or(deadline).min(deadline);tokio::time::timeout_at(until,future.as_mut()).await.map_err(|_|McpDelivery::PossiblySent)},
            result=future.as_mut()=>Ok(result),
        }
    }).await.unwrap_or(Err(McpDelivery::PossiblySent))
}
async fn record_outcome(
    runtime: &Runtime,
    lease: &RunLease,
    attempt: &StoredRecord,
    outcome: McpOutcome,
    policy: &McpPolicy,
    influence: crate::records::SecurityLabels,
    reserved: bool,
) -> Result<bool, RuntimeError> {
    let attempt_id = &attempt.key.id;
    let delivery = outcome.delivery;
    let response = match outcome.result {
        Ok(raw)
            if delivery == McpDelivery::ResponseReceived
                && raw.len() <= response_limit(runtime) =>
        {
            object([
                (
                    "raw_digest",
                    Value::Text(htlk_executable::digest::hash_bytes(&raw).to_string()),
                ),
                ("raw", Value::Bytes(raw)),
            ])?
        }
        Ok(_) => object([
            (
                "failure_code",
                Value::Text(
                    if delivery == McpDelivery::PossiblySent && !policy.replay_safe {
                        FailureCode::EffectUncertain
                    } else {
                        FailureCode::McpProtocol
                    }
                    .to_string(),
                ),
            ),
            (
                "observed_failure_code",
                Value::Text(FailureCode::McpProtocol.to_string()),
            ),
        ])?,
        Err(error) => {
            let code = if delivery == McpDelivery::PossiblySent && !policy.replay_safe {
                FailureCode::EffectUncertain
            } else {
                error.code()
            };
            object([
                ("failure_code", Value::Text(code.to_string())),
                (
                    "observed_failure_code",
                    Value::Text(error.code().to_string()),
                ),
            ])?
        }
    };
    let response_labels = influence.union(&policy.result_labels, &runtime.config.limits)?;
    let response = replace(
        &response,
        [
            (
                "influence_labels",
                response_labels.to_value(&runtime.config.limits)?,
            ),
            ("delivery", Value::Text(delivery.as_str().into())),
        ],
    )?;
    let current = runtime.store.record(lease.run_id(), &attempt.key).await?;
    let mut writes = vec![
        update(
            &current,
            replace(
                &current.payload,
                [("dispatch_state", Value::Text("response_recorded".into()))],
            )?,
            true,
        ),
        insert(RecordFamily::Response, attempt_id, response.clone(), true),
    ];
    if reserved && delivery != McpDelivery::PossiblySent {
        writes.extend(release(runtime, lease, attempt_id).await?);
    }
    match step::commit(
        runtime,
        lease,
        format!("response:{attempt_id}"),
        writes,
        None,
    )
    .await
    {
        Ok(()) => {}
        Err(RuntimeError::Storage(
            crate::store::StoreError::Terminal | crate::store::StoreError::Fenced,
        )) => {
            runtime
                .store
                .save_late_response(lease.run_id(), attempt_id, &response, &response_labels)
                .await?;
        }
        Err(error) => return Err(error),
    }
    Ok(true)
}
fn deduplication(
    runtime: &Runtime,
    binding: &McpBinding,
    invocation: InvocationId,
    params: &Value,
) -> Result<(Value, Option<String>), RuntimeError> {
    let adapter = runtime
        .mcp
        .as_ref()
        .ok_or_else(|| RuntimeError::failure(FailureCode::McpIdentity))?;
    Ok(
        match adapter.deduplication(binding, invocation, params, &runtime.config.limits)? {
            None => (Value::Null, None),
            Some(McpDeduplication::Protocol) => {
                let key = invocation
                    .effect_key(&runtime.config.limits)
                    .map_err(|_| err())?
                    .to_string();
                (
                    object([
                        ("mechanism", Value::Text("protocol".into())),
                        ("key", Value::Text(key.clone())),
                    ])?,
                    Some(key),
                )
            }
            Some(McpDeduplication::Argument { name, value }) => {
                if map(field(params, "arguments")?)?.get(&name) != Some(&value) {
                    return Err(RuntimeError::failure(FailureCode::Policy));
                }
                (
                    object([
                        ("mechanism", Value::Text("argument".into())),
                        ("name", Value::Text(name)),
                        ("value", value),
                    ])?,
                    None,
                )
            }
        },
    )
}
async fn request(
    runtime: &Runtime,
    scope: &ScopeState,
    binding: &McpBinding,
    record: &StoredRecord,
) -> Result<(String, Value, Option<String>), RuntimeError> {
    let args = if let Some(input) = map(field(&record.payload, "inputs")?)?.get("arguments") {
        runtime
            .read_artifact(&scope.context, entry(input)?.ok_or_else(err)?)
            .await?
            .value
    } else {
        object([])?
    };
    encode_request(binding, args, &runtime.config.limits)
}
pub(super) fn encode_request(
    binding: &McpBinding,
    args: Value,
    limits: &htlk_executable::cbor::Limits,
) -> Result<(String, Value, Option<String>), RuntimeError> {
    Ok(match binding.kind() {
        McpBindingKind::Tool { name, .. } => (
            "tools/call".into(),
            object([("name", Value::Text(name.clone())), ("arguments", args)])?,
            None,
        ),
        McpBindingKind::Prompt { name } => (
            "prompts/get".into(),
            object([("name", Value::Text(name.clone())), ("arguments", args)])?,
            None,
        ),
        McpBindingKind::Resource { uri } => (
            "resources/read".into(),
            object([("uri", Value::Text(uri.clone()))])?,
            Some(uri.clone()),
        ),
        McpBindingKind::Template { uri_template } => {
            let uri = crate::expand_uri_template(uri_template, &args, limits).map_err(|_| err())?;
            (
                "resources/read".into(),
                object([("uri", Value::Text(uri.clone()))])?,
                Some(uri),
            )
        }
    })
}
async fn normalize(
    runtime: &Runtime,
    scope: &ScopeState,
    binding: &McpBinding,
    raw: &[u8],
    attempt: &Value,
) -> Result<Value, RuntimeError> {
    let document = JsonDocument::new(raw, &runtime.config.limits)
        .map_err(|_| RuntimeError::failure(FailureCode::McpProtocol))?;
    match binding.kind() {
        McpBindingKind::Tool { output_schema, .. } => {
            if !htlk_analyzer::mcp_schema_validators()
                .map_err(|_| err())?
                .validate(
                    &format!("{}:CallToolResult", htlk_analyzer::MCP_SCHEMA_URI),
                    &document,
                    &runtime.config.limits,
                )
                .map_err(|_| err())?
            {
                return Err(RuntimeError::failure(FailureCode::McpProtocol));
            }
            if map(document.value())?.get("isError") == Some(&Value::Bool(true)) {
                return Err(RuntimeError::failure(FailureCode::McpToolError));
            }
            let value = map(document.value())?
                .get("structuredContent")
                .ok_or_else(|| RuntimeError::failure(FailureCode::McpProtocol))?;
            if !matches!(value, Value::Map(_)) {
                return Err(RuntimeError::failure(FailureCode::McpProtocol));
            }
            let ty = htlk_executable::ValueType::new(
                htlk_executable::ValueTypeKind::Schema(*output_schema),
                htlk_executable::TypeContext::Value,
                &runtime.config.limits,
            )
            .map_err(|_| err())?;
            crate::validate_typed_value(
                value,
                &ty,
                Some(scope.executable.schemas()),
                &runtime.config.limits,
                &scope.executable.policy().fields().evaluator_limits,
            )
            .map_err(|_| RuntimeError::failure(FailureCode::OutputSchema))?;
            Ok(value.clone())
        }
        McpBindingKind::Prompt { .. } => {
            crate::validate_mcp_prompt_result(&document, &runtime.config.limits)
                .map_err(|_| RuntimeError::failure(FailureCode::McpProtocol))?;
            Ok(document.value().clone())
        }
        _ => {
            if !htlk_analyzer::mcp_schema_validators()
                .map_err(|_| err())?
                .validate(
                    &format!("{}:ReadResourceResult", htlk_analyzer::MCP_SCHEMA_URI),
                    &document,
                    &runtime.config.limits,
                )
                .map_err(|_| err())?
            {
                return Err(RuntimeError::failure(FailureCode::McpProtocol));
            }
            let Value::Array(contents) = field(document.value(), "contents")? else {
                return Err(err());
            };
            let mut result = Vec::new();
            let requested_uri = string(attempt, "requested_uri")?;
            for content in contents {
                let uri = string(content, "uri")?;
                runtime.policy.authorize(
                    &scope.context,
                    Authorization::ReadResource {
                        binding,
                        requested_uri: &requested_uri,
                        uri: &uri,
                    },
                )?;
                let mut fields = vec![("uri".into(), Value::Text(uri))];
                if let Some(mime) = map(content)?.get("mimeType") {
                    fields.push(("mime_type".into(), mime.clone()));
                }
                if let Some(value) = map(content)?.get("text") {
                    fields.push(("kind".into(), Value::Text("text".into())));
                    fields.push(("text".into(), value.clone()));
                } else {
                    use base64::Engine as _;
                    let blob = string(content, "blob")?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(blob)
                        .map_err(|_| RuntimeError::failure(FailureCode::McpProtocol))?;
                    if bytes.len() > runtime.config.limits.max_document_bytes {
                        return Err(RuntimeError::failure(FailureCode::McpProtocol));
                    }
                    fields.push(("kind".into(), Value::Text("bytes".into())));
                    fields.push(("data".into(), Value::Bytes(bytes)));
                }
                result.push(Value::Map(Map::try_from_entries(fields)?));
            }
            let result = object([
                (
                    "server_identity",
                    Value::Text(
                        binding
                            .server()
                            .digest(&runtime.config.limits)
                            .map_err(|_| err())?
                            .to_string(),
                    ),
                ),
                (
                    "descriptor_digest",
                    Value::Text(binding.descriptor().to_string()),
                ),
                ("requested_uri", field(attempt, "requested_uri")?.clone()),
                ("contents", Value::Array(result)),
            ])?;
            crate::validate_resource_snapshot(
                &result,
                binding,
                &string(attempt, "requested_uri")?,
                &runtime.config.limits,
            )
            .map_err(|_| RuntimeError::failure(FailureCode::McpProtocol))?;
            Ok(result)
        }
    }
}
async fn fail_attempt(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    node: &Node,
    record: &StoredRecord,
    attempt: &StoredRecord,
    code: FailureCode,
    policy: &McpPolicy,
) -> Result<bool, RuntimeError> {
    let Operation::Mcp { retry, .. } = &node.fields().operation else {
        return Err(err());
    };
    let number = u64::try_from(number(&attempt.payload, "number")?).map_err(|_| err())?;
    if number == 0 || number > retry.max_attempts() {
        return Err(err());
    }
    if matches!(
        code,
        FailureCode::McpTransport
            | FailureCode::McpTimeout
            | FailureCode::McpProtocol
            | FailureCode::McpToolError
            | FailureCode::OutputSchema
    ) && policy.replay_safe
        && number < retry.max_attempts()
        && retry.on().iter().any(|c| c == code.as_str())
    {
        let now = runtime
            .store
            .coordinator_time(runtime.clock.now_ms()?)
            .await?;
        let delay = *retry
            .backoff_ms()
            .get(number as usize - 1)
            .ok_or_else(err)?;
        let due = now.saturating_add(i64::try_from(delay).map_err(|_| err())?);
        if due < number_value(&record.payload, "deadline_ms")? {
            let payload = replace(
                &record.payload,
                [
                    ("attempt_id", Value::Null),
                    ("retry_at", Value::Integer(due)),
                    ("reason", Value::Text("retry_delay".into())),
                ],
            )?;
            step::commit(
                runtime,
                lease,
                format!("retry:{}:{number}", record.key.id),
                vec![update(record, payload, false)],
                None,
            )
            .await?;
            return Ok(true);
        }
    }
    step::finish_failure(
        runtime,
        lease,
        record,
        code,
        labels(&record.payload, &runtime.config.limits)?,
    )
    .await?;
    Ok(true)
}
fn number_value(v: &Value, name: &str) -> Result<i64, RuntimeError> {
    number(v, name)
}
async fn reserve(
    runtime: &Runtime,
    lease: &RunLease,
    scope: &ScopeState,
    node: &StoredRecord,
    attempt: &str,
) -> Result<Vec<crate::store::RecordWrite>, RuntimeError> {
    let mut keys = vec![("run".to_string(), None)];
    let mut current = Some(
        runtime
            .store
            .record(lease.run_id(), &scope.record.key)
            .await?,
    );
    let mut authority_checks = vec![update(
        node,
        replace(
            &node.payload,
            [("reason", Value::Text("possibly_sent".into()))],
        )?,
        false,
    )];
    let mut seen = BTreeSet::new();
    keys.push((
        format!("node:{}", node.key.id),
        Some(field(&node.payload, "effective_limits")?.clone()),
    ));
    while let Some(record) = current {
        if !seen.insert(record.key.id.clone()) {
            return Err(err());
        }
        // Scope mounting and descendant stops revise these records. The dispatch
        // transaction must conflict with a stale ancestry or cancellation snapshot.
        authority_checks.push(update(&record, record.payload.clone(), false));
        keys.push((
            format!("scope:{}", record.key.id),
            Some(field(&record.payload, "effective_limits")?.clone()),
        ));
        if let Some(Value::Text(parent)) = map(&record.payload)?.get("parent_invocation") {
            let parent = runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Invocation, parent))
                .await?;
            keys.push((
                format!("node:{}", parent.key.id),
                Some(field(&parent.payload, "effective_limits")?.clone()),
            ));
            current = Some(
                runtime
                    .store
                    .record(
                        lease.run_id(),
                        &key(
                            RecordFamily::Scope,
                            string(&parent.payload, "scope_instance_id")?,
                        ),
                    )
                    .await?,
            );
        } else {
            current = None
        }
    }
    let mut writes = authority_checks;
    let mut reserved_keys = Vec::new();
    for (id, limits) in keys {
        let old = runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Usage, &id))
            .await;
        let (record, value) = match old {
            Ok(record) => {
                let value = record.payload.clone();
                (Some(record), value)
            }
            Err(crate::store::StoreError::Missing) => (
                None,
                object([
                    ("limits", limits.ok_or_else(err)?),
                    ("calls_spent", Value::Integer(0)),
                    ("concurrency_reserved", Value::Integer(0)),
                ])?,
            ),
            Err(error) => return Err(error.into()),
        };
        let limits = ExecutionLimits::from_value(field(&value, "limits")?, &runtime.config.limits)
            .map_err(|_| err())?;
        let spent = number(&value, "calls_spent")?;
        let concurrent = number(&value, "concurrency_reserved")?;
        if limits
            .max_mcp_calls()
            .is_some_and(|max| spent as u64 >= max)
        {
            return Err(RuntimeError::failure(FailureCode::Budget));
        }
        if limits
            .max_concurrency()
            .is_some_and(|max| concurrent as u64 >= max)
        {
            return Err(RuntimeError::failure(FailureCode::Budget));
        }
        let value = replace(
            &value,
            [
                (
                    "calls_spent",
                    Value::Integer(spent.checked_add(1).ok_or_else(err)?),
                ),
                (
                    "concurrency_reserved",
                    Value::Integer(concurrent.checked_add(1).ok_or_else(err)?),
                ),
            ],
        )?;
        reserved_keys.push(Value::Text(id.clone()));
        writes.push(if let Some(record) = record {
            update(&record, value, false)
        } else {
            insert(RecordFamily::Usage, id, value, false)
        });
    }
    writes.push(insert(
        RecordFamily::Reservation,
        attempt,
        object([
            ("state", Value::Text("outstanding".into())),
            ("usage_keys", Value::Array(reserved_keys)),
            ("invocation_id", Value::Text(node.key.id.clone())),
            ("calls_spent", Value::Integer(1)),
        ])?,
        false,
    ));
    Ok(writes)
}
async fn release(
    runtime: &Runtime,
    lease: &RunLease,
    attempt: &str,
) -> Result<Vec<crate::store::RecordWrite>, RuntimeError> {
    let reservation = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Reservation, attempt))
        .await?;
    if reservation.final_record {
        return Ok(vec![]);
    }
    let Value::Array(keys) = field(&reservation.payload, "usage_keys")? else {
        return Err(err());
    };
    let mut writes = Vec::new();
    for id in keys {
        let Value::Text(id) = id else {
            return Err(err());
        };
        let usage = runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Usage, id))
            .await?;
        let concurrent = number(&usage.payload, "concurrency_reserved")?;
        if concurrent <= 0 {
            return Err(err());
        }
        writes.push(update(
            &usage,
            replace(
                &usage.payload,
                [("concurrency_reserved", Value::Integer(concurrent - 1))],
            )?,
            false,
        ));
    }
    writes.push(update(
        &reservation,
        replace(
            &reservation.payload,
            [("state", Value::Text("released".into()))],
        )?,
        true,
    ));
    Ok(writes)
}
