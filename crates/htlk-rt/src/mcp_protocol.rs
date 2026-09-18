//! Actual-value checking of pinned MCP results and normalized resource snapshots.
use htlk_analyzer::{MCP_SCHEMA_URI, McpValidationError, mcp_schema_validators};
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind,
    cbor::{self, Limits, Map, Value},
    digest::Digest,
};

/// Validates protocol prompt results, including base64 content under value limits.
///
/// # Errors
/// Returns malformed protocol data, invalid base64 or resource failures.
pub fn validate_mcp_prompt_result(
    document: &JsonDocument,
    limits: &Limits,
) -> Result<(), McpValidationError> {
    if !mcp_schema_validators()?.validate(
        &format!("{MCP_SCHEMA_URI}:GetPromptResult"),
        document,
        limits,
    )? {
        return Err(McpValidationError::PromptResult);
    }
    let Value::Map(root) = document.value() else {
        return Err(McpValidationError::PromptResult);
    };
    let Some(Value::Array(messages)) = root.get("messages") else {
        return Err(McpValidationError::PromptResult);
    };
    for message in messages {
        let Value::Map(message) = message else {
            return Err(McpValidationError::PromptResult);
        };
        let Some(Value::Map(content)) = message.get("content") else {
            return Err(McpValidationError::PromptResult);
        };
        match content.get("type") {
            Some(Value::Text(kind)) if kind == "image" || kind == "audio" => {
                if let Some(Value::Text(data)) = content.get("data") {
                    base64_data(data, limits)?;
                }
            }
            Some(Value::Text(kind)) if kind == "resource" => {
                if let Some(Value::Map(resource)) = content.get("resource")
                    && !matches!(resource.get("text"), Some(Value::Text(_)))
                    && let Some(Value::Text(data)) = resource.get("blob")
                {
                    base64_data(data, limits)?;
                }
            }
            _ => (),
        }
    }
    Ok(())
}
fn base64_data(data: &str, limits: &Limits) -> Result<(), McpValidationError> {
    use std::io::Read;
    let mut decoder = base64::read::DecoderReader::new(
        data.as_bytes(),
        &base64::engine::general_purpose::STANDARD,
    );
    let mut buffer = [0u8; 1024];
    let mut total = 0usize;
    loop {
        let n = decoder
            .read(&mut buffer)
            .map_err(|_| McpValidationError::BinaryEncoding)?;
        if n == 0 {
            return Ok(());
        }
        total = total
            .checked_add(n)
            .ok_or(McpValidationError::BinaryLimit)?;
        if total > limits.max_byte_string_bytes {
            return Err(McpValidationError::BinaryLimit);
        }
    }
}
/// Checks a normalized ResourceSnapshot against its frozen binding and URI.
///
/// # Errors
/// Returns invalid shape/content, identity mismatch or resource failures.
pub fn validate_resource_snapshot(
    value: &Value,
    binding: &McpBinding,
    requested_uri: &str,
    limits: &Limits,
) -> Result<(), McpValidationError> {
    match binding.kind() {
        McpBindingKind::Resource { uri } if uri == requested_uri => (),
        McpBindingKind::Template { .. } => (),
        _ => return Err(McpValidationError::SnapshotIdentity),
    }
    let m = snapshot_shape(value, limits)?;
    if digest(m.get("server_identity"))? != binding.server().digest(limits)?
        || digest(m.get("descriptor_digest"))? != binding.descriptor()
        || text(m.get("requested_uri"))? != requested_uri
    {
        return Err(McpValidationError::SnapshotIdentity);
    }
    Ok(())
}
pub(crate) fn snapshot_shape<'a>(
    value: &'a Value,
    limits: &Limits,
) -> Result<&'a Map, McpValidationError> {
    cbor::encode(value, limits)?;
    let m = closed(
        value,
        &[
            "server_identity",
            "requested_uri",
            "descriptor_digest",
            "contents",
        ],
        &[],
    )?;
    digest(m.get("server_identity"))?;
    digest(m.get("descriptor_digest"))?;
    text(m.get("requested_uri"))?;
    let Some(Value::Array(contents)) = m.get("contents") else {
        return Err(McpValidationError::SnapshotShape);
    };
    for content in contents {
        let Value::Map(m) = content else {
            return Err(McpValidationError::SnapshotShape);
        };
        let key = match text(m.get("kind"))? {
            "text" => "text",
            "bytes" => "data",
            _ => return Err(McpValidationError::SnapshotShape),
        };
        let m = closed(content, &["kind", "uri", key], &["mime_type"])?;
        text(m.get("uri"))?;
        if m.get("mime_type").is_some() {
            text(m.get("mime_type"))?;
        }
        match (key, m.get(key)) {
            ("text", Some(Value::Text(_))) | ("data", Some(Value::Bytes(_))) => (),
            _ => return Err(McpValidationError::SnapshotShape),
        }
    }
    Ok(m)
}
fn closed<'a>(
    v: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map, McpValidationError> {
    let Value::Map(m) = v else {
        return Err(McpValidationError::SnapshotShape);
    };
    if required.iter().any(|k| m.get(k).is_none())
        || m.iter()
            .any(|(k, _)| !required.contains(&k) && !optional.contains(&k))
    {
        return Err(McpValidationError::SnapshotShape);
    }
    Ok(m)
}
fn text(v: Option<&Value>) -> Result<&str, McpValidationError> {
    if let Some(Value::Text(v)) = v {
        Ok(v)
    } else {
        Err(McpValidationError::SnapshotShape)
    }
}
fn digest(v: Option<&Value>) -> Result<Digest, McpValidationError> {
    text(v)?
        .parse()
        .map_err(|_| McpValidationError::SnapshotIdentity)
}
