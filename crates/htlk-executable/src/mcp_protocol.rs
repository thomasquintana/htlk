//! Native validation against the pinned MCP protocol schema snapshot.

use crate::digest::Digest;
use crate::{
    JsonDocument, JsonError, McpBinding, McpBindingKind, MetadataError, NativeSchemaError,
    NativeSchemaOptions, NativeSchemas, SchemaCatalog,
};
use htlk_cbor::{Limits, Map, Value};
use std::{fmt, sync::OnceLock};

const URI: &str = "urn:htlk:mcp:2025-11-25";
const ROOTS: [&str; 5] = [
    "Tool",
    "Resource",
    "ResourceTemplate",
    "Prompt",
    "GetPromptResult",
];
static VALIDATORS: OnceLock<Result<NativeSchemas, McpValidationError>> = OnceLock::new();

/// Closed protocol descriptor categories, distinct from application value types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpDescriptorKind {
    /// Tool descriptor.
    Tool,
    /// Fixed resource descriptor.
    Resource,
    /// Resource template descriptor.
    ResourceTemplate,
    /// Prompt descriptor.
    Prompt,
}
impl McpDescriptorKind {
    fn root(self) -> &'static str {
        match self {
            Self::Tool => "Tool",
            Self::Resource => "Resource",
            Self::ResourceTemplate => "ResourceTemplate",
            Self::Prompt => "Prompt",
        }
    }
}
fn validators() -> Result<&'static NativeSchemas, McpValidationError> {
    VALIDATORS
        .get_or_init(|| {
            let limits = Limits::default();
            let mut documents = vec![(
                URI.to_owned(),
                JsonDocument::new(
                    include_bytes!("../assets/mcp-2025-11-25.schema.json"),
                    &limits,
                )?,
            )];
            for root in ROOTS {
                documents.push((
                    format!("{URI}:{root}"),
                    JsonDocument::new(
                        format!(r#"{{"$ref":"{URI}#/$defs/{root}"}}"#).as_bytes(),
                        &limits,
                    )?,
                ));
            }
            let catalog =
                SchemaCatalog::new(documents, &limits).map_err(NativeSchemaError::from)?;
            Ok(NativeSchemas::compile(
                &catalog,
                NativeSchemaOptions::default(),
                &limits,
            )?)
        })
        .as_ref()
        .map_err(Clone::clone)
}
/// Validates a complete MCP 2025-11-25 descriptor with the shipped native backend.
/// Original descriptions, extension metadata, and external names are preserved.
///
/// # Errors
/// Returns input limits, protocol-shape failures, or native validator failures.
pub fn validate_mcp_descriptor(
    kind: McpDescriptorKind,
    document: &JsonDocument,
    limits: &Limits,
) -> Result<(), McpValidationError> {
    if validators()?.validate(&format!("{URI}:{}", kind.root()), document, limits)? {
        Ok(())
    } else {
        Err(McpValidationError::Descriptor)
    }
}
/// Validates the complete protocol prompt result, including content-block kinds,
/// roles, optional metadata, and annotations. It remains data for graph execution.
///
/// # Errors
/// Returns input limits or invalid protocol result shape.
pub fn validate_mcp_prompt_result(
    document: &JsonDocument,
    limits: &Limits,
) -> Result<(), McpValidationError> {
    if !validators()?.validate(&format!("{URI}:GetPromptResult"), document, limits)? {
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
/// Checks the normalized native ResourceSnapshot shape and frozen request identity.
/// Byte contents are native bytes; no base64 conversion or timestamp is introduced.
///
/// # Errors
/// Returns malformed fields/content, mismatched identity, or codec limits.
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
    htlk_cbor::encode(value, limits)?;
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

/// Protocol/value errors with static diagnostics rather than submitted content.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpValidationError {
    /// Invalid base64 protocol content.
    BinaryEncoding,
    /// Decoded protocol content exceeds the byte-string ceiling.
    BinaryLimit,
    /// Codec failure.
    Codec(htlk_cbor::Error),
    /// JSON profile failure.
    Json(JsonError),
    /// Native protocol-schema failure.
    Native(NativeSchemaError),
    /// Binding identity encoding failed.
    Metadata(MetadataError),
    /// Descriptor does not conform to the pinned protocol schema.
    Descriptor,
    /// Prompt result does not conform to its protocol schema.
    PromptResult,
    /// Malformed normalized snapshot.
    SnapshotShape,
    /// Snapshot does not match its frozen binding/request.
    SnapshotIdentity,
}
impl From<htlk_cbor::Error> for McpValidationError {
    fn from(e: htlk_cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<JsonError> for McpValidationError {
    fn from(e: JsonError) -> Self {
        Self::Json(e)
    }
}
impl From<NativeSchemaError> for McpValidationError {
    fn from(e: NativeSchemaError) -> Self {
        Self::Native(e)
    }
}
impl From<MetadataError> for McpValidationError {
    fn from(e: MetadataError) -> Self {
        Self::Metadata(e)
    }
}
impl fmt::Display for McpValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP validation: {self:?}")
    }
}
impl std::error::Error for McpValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Native(e) => Some(e),
            Self::Metadata(e) => Some(e),
            _ => None,
        }
    }
}
