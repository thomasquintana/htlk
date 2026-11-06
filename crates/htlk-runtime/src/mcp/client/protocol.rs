//! Pure client-service admission, including semantic constraints not expressed by
//! the generated MCP JSON Schema. No schema or URL is fetched during validation.
use super::*;
use crate::mcp::{as_map, as_text, get};
use htlk_analyzer::{
    McpMessageKind, NativeSchemaOptions, NativeSchemas, SchemaCatalog, validate_mcp_message,
};
use std::collections::BTreeSet;

/// A validated incoming client-service category. Capability gates are checked
/// separately from shape and cross-message semantic validity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpIncomingKind {
    /// Model generation, optionally using tools or host-provided context.
    Sampling,
    /// Schema-driven in-band form interaction.
    Form,
    /// Out-of-band URL interaction.
    Url,
    /// Host roots inquiry.
    Roots,
}

/// Redacted validation disposition suitable for a JSON-RPC error response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpClientValidationError {
    /// The method is not implemented or its base service is not installed.
    UnsupportedMethod,
    /// An extension/mode was not advertised, or request parameters are invalid.
    InvalidRequest,
    /// The host returned an invalid response or violated request-specific rules.
    InvalidResponse,
}
impl McpClientValidationError {
    /// Standard JSON-RPC error code; messages never contain raw request/result data.
    pub fn rpc_code(self) -> i64 {
        match self {
            Self::UnsupportedMethod => -32601,
            Self::InvalidRequest => -32602,
            Self::InvalidResponse => -32603,
        }
    }
}
impl fmt::Display for McpClientValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedMethod => "Unsupported client service.",
            Self::InvalidRequest => "Invalid client-service parameters.",
            Self::InvalidResponse => "Invalid host-service result.",
        })
    }
}
impl std::error::Error for McpClientValidationError {}

/// Independently checks a complete incoming request and installed capabilities.
/// Validation never dispatches handlers, reads roots, or acquires context/URLs.
///
/// # Errors
/// Returns unsupported service or invalid request/capability use.
pub fn validate_client_request(
    request: &JsonDocument,
    config: &McpClientConfig,
    limits: &Limits,
) -> Result<McpIncomingKind, McpClientValidationError> {
    // Task augmentation is validated by the installed durable receiver. It must
    // be ignored for otherwise supported methods when that service is absent.
    let underlying = mcp_without_task_augmentation(request, limits)
        .map_err(|_| McpClientValidationError::InvalidRequest)?;
    let request = &underlying;
    let invalid = McpClientValidationError::InvalidRequest;
    let fields = as_map(request.value()).map_err(|_| invalid)?;
    let method = fields
        .get("method")
        .and_then(|v| as_text(v).ok())
        .ok_or(invalid)?;
    let installed = |service| config.handler(service).is_some();
    let kind = match method {
        "sampling/createMessage" if installed(McpClientService::Sampling) => {
            McpIncomingKind::Sampling
        }
        "elicitation/create"
            if installed(McpClientService::ElicitationForm)
                || installed(McpClientService::ElicitationUrl) =>
        {
            match fields
                .get("params")
                .and_then(|v| as_map(v).ok())
                .and_then(|p| p.get("mode"))
            {
                None => McpIncomingKind::Form,
                Some(Value::Text(mode)) if mode == "form" => McpIncomingKind::Form,
                Some(Value::Text(mode)) if mode == "url" => McpIncomingKind::Url,
                _ => return Err(invalid),
            }
        }
        "roots/list" if installed(McpClientService::Roots) => McpIncomingKind::Roots,
        _ => return Err(McpClientValidationError::UnsupportedMethod),
    };
    let shape = match kind {
        McpIncomingKind::Sampling => McpMessageKind::SamplingRequest,
        McpIncomingKind::Form | McpIncomingKind::Url => McpMessageKind::ElicitationRequest,
        McpIncomingKind::Roots => McpMessageKind::RootsRequest,
    };
    validate_mcp_message(shape, request, limits).map_err(|_| invalid)?;
    let params = fields
        .get("params")
        .map(as_map)
        .transpose()
        .map_err(|_| invalid)?;
    match kind {
        McpIncomingKind::Sampling => {
            sampling_request(params.ok_or(invalid)?, config, limits).map_err(|_| invalid)?
        }
        McpIncomingKind::Form => {
            if !installed(McpClientService::ElicitationForm) {
                return Err(invalid);
            }
            prepare_schema(
                params
                    .ok_or(invalid)?
                    .get("requestedSchema")
                    .ok_or(invalid)?,
                limits,
            )
            .map_err(|_| invalid)?;
        }
        McpIncomingKind::Url => {
            if !installed(McpClientService::ElicitationUrl) {
                return Err(invalid);
            }
            let params = params.ok_or(invalid)?;
            let url = params
                .get("url")
                .and_then(|v| as_text(v).ok())
                .ok_or(invalid)?;
            // Syntax validity is distinct from host consent/navigation policy.
            // Parsing never fetches the URL or invokes a scheme handler.
            reqwest::Url::parse(url).map_err(|_| invalid)?;
        }
        _ => {}
    }
    Ok(kind)
}

/// Checks an outgoing host result against the admitted request and negotiated
/// service. Protocol success does not imply runtime acceptance or delivery.
///
/// # Errors
/// Returns invalid request or result shape, schema, modality, or tool-message rules.
pub fn validate_client_result(
    request: &JsonDocument,
    result: &JsonDocument,
    config: &McpClientConfig,
    limits: &Limits,
) -> Result<(), McpClientValidationError> {
    let kind = validate_client_request(request, config, limits)?;
    let checked = (|| -> Result<(), RuntimeFailure> {
        let request = as_map(request.value())?;
        match kind {
            McpIncomingKind::Sampling => {
                validate_mcp_message(McpMessageKind::SamplingResult, result, limits)
                    .map_err(|_| fail(FailureCode::McpProtocol))?;
                sampling_result(
                    as_map(get(request, "params")?)?,
                    as_map(result.value())?,
                    config,
                    limits,
                )
            }
            McpIncomingKind::Form | McpIncomingKind::Url => elicitation_result(
                as_map(get(request, "params")?)?,
                as_map(result.value())?,
                kind,
                limits,
            ),
            McpIncomingKind::Roots => {
                validate_mcp_message(McpMessageKind::RootsResult, result, limits)
                    .map_err(|_| fail(FailureCode::McpProtocol))?;
                let Value::Array(roots) = get(as_map(result.value())?, "roots")? else {
                    return Err(fail(FailureCode::McpProtocol));
                };
                for root in roots {
                    let uri = as_text(get(as_map(root)?, "uri")?)?;
                    if !uri.starts_with("file://") || reqwest::Url::parse(uri).is_err() {
                        return Err(fail(FailureCode::McpProtocol));
                    }
                }
                Ok(())
            }
        }
    })();
    checked.map_err(|_| McpClientValidationError::InvalidResponse)
}

fn blocks(content: &Value) -> &[Value] {
    match content {
        Value::Array(blocks) => blocks,
        block => std::slice::from_ref(block),
    }
}
fn binary(content: &Value, limits: &Limits) -> Result<(), RuntimeFailure> {
    for block in blocks(content) {
        let block = as_map(block)?;
        crate::mcp_protocol::content_binary(block, limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        if block.get("type") == Some(&Value::Text("tool_result".into())) {
            binary(get(block, "content")?, limits)?;
        }
    }
    Ok(())
}
fn sampling_request(
    params: &Map,
    config: &McpClientConfig,
    limits: &Limits,
) -> Result<(), RuntimeFailure> {
    let tools = config.handler(McpClientService::SamplingTools).is_some();
    if !tools && (params.get("tools").is_some() || params.get("toolChoice").is_some()) {
        return Err(fail(FailureCode::McpProtocol));
    }
    if params
        .get("includeContext")
        .is_some_and(|v| v != &Value::Text("none".into()))
        && config.handler(McpClientService::SamplingContext).is_none()
    {
        return Err(fail(FailureCode::McpProtocol));
    }
    if !matches!(params.get("maxTokens"), Some(Value::Integer(n)) if *n > 0) {
        return Err(fail(FailureCode::McpProtocol));
    }
    let Value::Array(messages) = get(params, "messages")? else {
        return Err(fail(FailureCode::McpProtocol));
    };
    let mut pending = BTreeSet::new();
    let mut used = BTreeSet::new();
    for message in messages {
        let message = as_map(message)?;
        let role = as_text(get(message, "role")?)?;
        let content = get(message, "content")?;
        binary(content, limits)?;
        let parts = blocks(content);
        let results = parts
            .iter()
            .filter(|b| {
                as_map(b).is_ok_and(|b| b.get("type") == Some(&Value::Text("tool_result".into())))
            })
            .count();
        if !pending.is_empty() || results != 0 {
            if !tools || role != "user" || results != parts.len() || pending.len() != results {
                return Err(fail(FailureCode::McpProtocol));
            }
            for part in parts {
                if !pending.remove(as_text(get(as_map(part)?, "toolUseId")?)?) {
                    return Err(fail(FailureCode::McpProtocol));
                }
            }
            continue;
        }
        for part in parts {
            let part = as_map(part)?;
            if part.get("type") == Some(&Value::Text("tool_use".into())) {
                if !tools || role != "assistant" {
                    return Err(fail(FailureCode::McpProtocol));
                }
                let id = as_text(get(part, "id")?)?;
                if !used.insert(id) {
                    return Err(fail(FailureCode::McpProtocol));
                }
                pending.insert(id);
            }
        }
    }
    if !pending.is_empty() {
        return Err(fail(FailureCode::McpProtocol));
    }
    Ok(())
}
fn sampling_result(
    params: &Map,
    result: &Map,
    config: &McpClientConfig,
    limits: &Limits,
) -> Result<(), RuntimeFailure> {
    let content = get(result, "content")?;
    binary(content, limits)?;
    let mut uses = BTreeSet::new();
    for block in blocks(content) {
        let block = as_map(block)?;
        match as_text(get(block, "type")?)? {
            "tool_result" => return Err(fail(FailureCode::McpProtocol)),
            "tool_use" => {
                if config.handler(McpClientService::SamplingTools).is_none()
                    || get(result, "role")? != &Value::Text("assistant".into())
                    || !uses.insert(as_text(get(block, "id")?)?)
                {
                    return Err(fail(FailureCode::McpProtocol));
                }
                let Value::Array(tools) = get(params, "tools")? else {
                    return Err(fail(FailureCode::McpProtocol));
                };
                let name = get(block, "name")?;
                let tool = tools
                    .iter()
                    .find(|tool| as_map(tool).is_ok_and(|fields| fields.get("name") == Some(name)))
                    .ok_or_else(|| fail(FailureCode::McpProtocol))?;
                schema(
                    get(as_map(tool)?, "inputSchema")?,
                    get(block, "input")?,
                    limits,
                )?;
            }
            _ => {}
        }
    }
    let choice = params
        .get("toolChoice")
        .and_then(|v| as_map(v).ok())
        .and_then(|v| v.get("mode"));
    if (choice == Some(&Value::Text("required".into())) && uses.is_empty())
        || (choice == Some(&Value::Text("none".into())) && !uses.is_empty())
    {
        return Err(fail(FailureCode::McpProtocol));
    }
    Ok(())
}
fn elicitation_result(
    params: &Map,
    result: &Map,
    kind: McpIncomingKind,
    limits: &Limits,
) -> Result<(), RuntimeFailure> {
    // The pinned generated ElicitResult schema incorrectly restricts scalar numbers
    // to integers. Its authoritative TypeScript content union includes `number`.
    // Validate that union explicitly, then the actual requested form schema.
    if result
        .get("_meta")
        .is_some_and(|value| !matches!(value, Value::Map(_)))
    {
        return Err(fail(FailureCode::McpProtocol));
    }
    match as_text(get(result, "action")?)? {
        "accept" if kind == McpIncomingKind::Form => {
            let content = get(result, "content")?;
            for (_, value) in as_map(content)?.iter() {
                if !matches!(
                    value,
                    Value::Text(_) | Value::Integer(_) | Value::Float(_) | Value::Bool(_)
                ) && !matches!(value, Value::Array(values) if values.iter().all(|v| matches!(v, Value::Text(_))))
                {
                    return Err(fail(FailureCode::McpProtocol));
                }
            }
            schema(get(params, "requestedSchema")?, content, limits)
        }
        "accept" | "decline" | "cancel" if result.get("content").is_none() => Ok(()),
        _ => Err(fail(FailureCode::McpProtocol)),
    }
}
fn schema(schema: &Value, value: &Value, limits: &Limits) -> Result<(), RuntimeFailure> {
    let uri = "urn:htlk:mcp:client-value";
    let value =
        JsonDocument::from_value(value, limits).map_err(|_| fail(FailureCode::McpProtocol))?;
    let validator = prepare_schema(schema, limits)?;
    if validator
        .validate(uri, &value, limits)
        .map_err(|_| fail(FailureCode::McpProtocol))?
    {
        Ok(())
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn prepare_schema(schema: &Value, limits: &Limits) -> Result<NativeSchemas, RuntimeFailure> {
    let schema =
        JsonDocument::from_value(schema, limits).map_err(|_| fail(FailureCode::McpProtocol))?;
    let catalog = SchemaCatalog::new(vec![("urn:htlk:mcp:client-value".into(), schema)], limits)
        .map_err(|_| fail(FailureCode::McpProtocol))?;
    NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), limits)
        .map_err(|_| fail(FailureCode::McpProtocol))
}
