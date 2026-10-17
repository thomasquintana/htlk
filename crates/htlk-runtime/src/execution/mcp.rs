//! Durable MCP attempts, intent-before-send, response-before-acceptance and safe retry.
use super::{
    model::*,
    step::{self, OutputProposal, ScopeState},
};
use crate::{
    Authorization, Runtime, RuntimeError,
    failure::FailureCode,
    identity::{AttemptId, InvocationId},
    mcp::{McpDeduplication, McpPolicy, McpRequest},
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
    let mut policy = runtime.policy.mcp_policy(&scope.context, binding)?;
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
                            .to_value(&runtime.config.limits)?,
                    )],
                )?;
                let record = &protected_record;
                if let Some(Value::Text(code)) = map(&response.payload)?.get("failure_code") {
                    let code: FailureCode = code.parse().map_err(|_| err())?;
                    return fail_attempt(runtime, lease, node, record, &attempt, code, &policy)
                        .await;
                }
                let Value::Bytes(raw) = field(&response.payload, "raw")? else {
                    return Err(err());
                };
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
                let (verified, _) = deduplication(
                    runtime,
                    binding,
                    invocation,
                    field(&attempt.payload, "params")?,
                )?;
                if verified != *field(&attempt.payload, "deduplication")? {
                    return Err(RuntimeError::failure(FailureCode::Policy));
                }
                policy.replay_safe |= verified != Value::Null;
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
        let advertised = match descriptor(
            runtime,
            binding,
            effective.attempt_timeout_ms().ok_or_else(err)?,
        )
        .await
        {
            Ok(v) => v,
            Err(error) => {
                step::finish_failure(
                    runtime,
                    lease,
                    record,
                    error.code(),
                    labels(&record.payload, &runtime.config.limits)?,
                )
                .await?;
                return Ok(true);
            }
        };
        if advertised.digest() != binding.descriptor() {
            step::finish_failure(
                runtime,
                lease,
                record,
                FailureCode::McpDescriptor,
                labels(&record.payload, &runtime.config.limits)?,
            )
            .await?;
            return Ok(true);
        }
        let number = map(&record.payload)?
            .get("attempt_number")
            .and_then(|v| {
                if let Value::Integer(n) = v {
                    Some(*n as u64 + 1)
                } else {
                    None
                }
            })
            .unwrap_or(1);
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
        let payload = object([
            ("dispatch_state", Value::Text("prepared".into())),
            ("invocation_id", Value::Text(invocation.to_string())),
            ("number", Value::Integer(number as i64)),
            ("method", Value::Text(method)),
            ("params", params),
            ("deduplication", dedup),
            (
                "protocol_key",
                protocol_key.map_or(Value::Null, Value::Text),
            ),
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
    let advertised = descriptor(
        runtime,
        binding,
        effective.attempt_timeout_ms().ok_or_else(err)?,
    )
    .await?;
    if advertised.digest() != binding.descriptor() {
        return Err(RuntimeError::failure(FailureCode::McpDescriptor));
    }
    // Discovery is asynchronous. Check the current policy and exact disposition
    // after it, immediately before the durable dispatch fence.
    let mut policy = runtime.policy.mcp_policy(&scope.context, binding)?;
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
    runtime.policy.authorize(
        &scope.context,
        Authorization::DispatchMcp {
            binding,
            invocation,
            input_hash: digest(&record.payload, "input_hash")?,
            labels: &influence,
            resource_uri: uri,
        },
    )?;
    if !super::approval::gate(runtime, lease, scope, binding, node, record, &policy).await? {
        return Ok(false);
    }
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    if now >= number(&record.payload, "deadline_ms")? {
        return Err(RuntimeError::failure(FailureCode::Timeout));
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
            [("dispatch_state", Value::Text("possibly_sent".into()))],
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
    let adapter = runtime
        .mcp
        .as_ref()
        .ok_or_else(|| RuntimeError::failure(FailureCode::McpIdentity))?;
    let reply = tokio::time::timeout(
        std::time::Duration::from_millis(timeout),
        adapter.invoke(
            binding,
            McpRequest {
                method: string(&attempt.payload, "method")?,
                params: field(&attempt.payload, "params")?.clone(),
                timeout_ms: timeout,
                max_response_bytes: runtime.config.limits.max_document_bytes,
                idempotency_key: protocol_key,
            },
        ),
    )
    .await
    .unwrap_or_else(|_| Err(crate::failure::RuntimeFailure::new(FailureCode::McpTimeout)));
    let response = match reply {
        Ok(raw) if raw.len() <= runtime.config.limits.max_document_bytes => {
            object([("raw", Value::Bytes(raw))])?
        }
        Ok(_) => object([(
            "failure_code",
            Value::Text(FailureCode::McpProtocol.to_string()),
        )])?,
        Err(error) => {
            let code = if matches!(
                error.code(),
                FailureCode::McpTransport | FailureCode::McpTimeout
            ) && !policy.replay_safe
            {
                FailureCode::EffectUncertain
            } else {
                error.code()
            };
            object([("failure_code", Value::Text(code.to_string()))])?
        }
    };
    let response_labels = influence.union(&policy.result_labels, &runtime.config.limits)?;
    let response = replace(
        &response,
        [(
            "influence_labels",
            response_labels.to_value(&runtime.config.limits)?,
        )],
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
        insert(RecordFamily::Response, &attempt_id, response.clone(), true),
    ];
    if !matches!(map(&response)?.get("failure_code"), Some(Value::Text(code)) if code == FailureCode::EffectUncertain.as_str() || code == FailureCode::McpTransport.as_str() || code == FailureCode::McpTimeout.as_str())
    {
        writes.extend(release(runtime, lease, &attempt_id).await?);
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
                .save_late_response(lease.run_id(), &attempt_id, &response, &response_labels)
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
async fn descriptor(
    runtime: &Runtime,
    binding: &McpBinding,
    timeout_ms: u64,
) -> Result<JsonDocument, crate::failure::RuntimeFailure> {
    let adapter = runtime
        .mcp
        .as_ref()
        .ok_or_else(|| crate::failure::RuntimeFailure::new(FailureCode::McpIdentity))?;
    tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        adapter.descriptor(binding, &runtime.config.limits),
    )
    .await
    .unwrap_or_else(|_| Err(crate::failure::RuntimeFailure::new(FailureCode::McpTimeout)))
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
            let uri = crate::expand_uri_template(uri_template, &args, &runtime.config.limits)
                .map_err(|_| err())?;
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
            for content in contents {
                let uri = string(content, "uri")?;
                runtime.policy.authorize(
                    &scope.context,
                    Authorization::ReadResource { binding, uri: &uri },
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
    let number = number(&attempt.payload, "number")? as u64;
    if code != FailureCode::EffectUncertain
        && policy.replay_safe
        && number < retry.max_attempts()
        && retry.on().iter().any(|c| c == code.as_str())
    {
        let now = runtime
            .store
            .coordinator_time(runtime.clock.now_ms()?)
            .await?;
        let delay = retry.backoff_ms()[number as usize - 1];
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
    let mut current = Some(scope.record.clone());
    let mut seen = BTreeSet::new();
    keys.push((
        format!("node:{}", node.key.id),
        Some(field(&node.payload, "effective_limits")?.clone()),
    ));
    while let Some(record) = current {
        if !seen.insert(record.key.id.clone()) {
            return Err(err());
        }
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
    let mut writes = Vec::new();
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
