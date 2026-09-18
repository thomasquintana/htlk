//! Offline preparation and descriptor validation for the pinned MCP protocol.
use crate::{NativeSchemaError, NativeSchemaOptions, NativeSchemas, SchemaCatalog};
use htlk_executable::{JsonDocument, JsonError, MetadataError, cbor};
use std::{fmt, sync::OnceLock};

/// Retrieval base for the pinned MCP schema snapshot.
pub const MCP_SCHEMA_URI: &str = "urn:htlk:mcp:2025-11-25";
static VALIDATORS: OnceLock<Result<NativeSchemas, McpValidationError>> = OnceLock::new();

/// Closed protocol descriptor categories.
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
/// Borrows immutable offline validators for descriptors and runtime protocol data.
///
/// # Errors
/// Returns a failure preparing the pinned embedded schema snapshot.
pub fn mcp_schema_validators() -> Result<&'static NativeSchemas, McpValidationError> {
    VALIDATORS
        .get_or_init(|| {
            let limits = cbor::Limits::default();
            let mut documents = vec![(
                MCP_SCHEMA_URI.to_owned(),
                JsonDocument::new(
                    include_bytes!("../assets/mcp-2025-11-25.schema.json"),
                    &limits,
                )?,
            )];
            for root in [
                "Tool",
                "Resource",
                "ResourceTemplate",
                "Prompt",
                "GetPromptResult",
            ] {
                documents.push((
                    format!("{MCP_SCHEMA_URI}:{root}"),
                    JsonDocument::new(
                        format!(r#"{{"$ref":"{MCP_SCHEMA_URI}#/$defs/{root}"}}"#).as_bytes(),
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
/// Validates a complete descriptor against the pinned MCP 2025-11-25 schema.
///
/// # Errors
/// Returns protocol conformance, input limits or native schema failures.
pub fn validate_mcp_descriptor(
    kind: McpDescriptorKind,
    document: &JsonDocument,
    limits: &cbor::Limits,
) -> Result<(), McpValidationError> {
    if mcp_schema_validators()?.validate(
        &format!("{MCP_SCHEMA_URI}:{}", kind.root()),
        document,
        limits,
    )? {
        Ok(())
    } else {
        Err(McpValidationError::Descriptor)
    }
}
/// Redacted protocol validation diagnostics shared with runtime integrations.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpValidationError {
    /// Invalid base64 protocol content.
    BinaryEncoding,
    /// Decoded content exceeds its byte-string ceiling.
    BinaryLimit,
    /// Codec failure.
    Codec(cbor::Error),
    /// JSON representation failure.
    Json(JsonError),
    /// Native protocol schema failure.
    Native(NativeSchemaError),
    /// Binding identity encoding failed.
    Metadata(MetadataError),
    /// Descriptor fails its protocol schema.
    Descriptor,
    /// Prompt result fails its protocol schema.
    PromptResult,
    /// Malformed normalized snapshot.
    SnapshotShape,
    /// Snapshot differs from its frozen binding/request.
    SnapshotIdentity,
}
macro_rules! convert {
    ($ty:ty,$variant:ident) => {
        impl From<$ty> for McpValidationError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}
convert!(cbor::Error, Codec);
convert!(JsonError, Json);
convert!(NativeSchemaError, Native);
convert!(MetadataError, Metadata);
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
