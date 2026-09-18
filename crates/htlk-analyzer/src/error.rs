//! Semantic document linkage errors.
use htlk_executable::{self as model, cbor, digest::ParseDigestError};

/// Document linkage or offline schema failure, with redacted structured causes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkageError {
    /// Canonical model representation failed under the requested limits.
    Representation(Box<model::DocumentError>),
    /// Native schema failure with its original document and pointer.
    SchemaDiagnostic(Box<crate::NativeSchemaDiagnostic>),
    /// Full MCP protocol conformance failure.
    Mcp(crate::McpValidationError),
    /// Conflicting descriptors for the same compound MCP selection.
    ConflictingMcpSelection,
    /// Offline native schema preparation failed.
    NativeSchema(crate::NativeSchemaError),
    /// Offline schema resources or references failed.
    Schema(crate::SchemaResourceError),
    /// A tool schema is absent at its prescribed retrieval base.
    SchemaRootMismatch,
    /// Codec failure.
    Codec(cbor::Error),
    /// Graph record or use-context failure.
    Graph(model::GraphRecordError),
    /// Metadata or generic declaration failure.
    Metadata(model::MetadataError),
    /// Expression syntax or use-context failure.
    Expression(model::ExpressionError),
    /// Type representation failure.
    Type(model::TypeError),
    /// Invalid digest spelling.
    Digest(ParseDigestError),
    /// External JSON failure.
    Json(model::JsonError),
    /// Invalid linked policy document.
    Policy(model::PolicyError),
    /// Missing referenced record or function.
    MissingRecord(&'static str),
    /// Unreachable entry in a definition table.
    UnreachableRecord(&'static str),
    /// Cyclic scope definitions, distinct from loop feedback.
    ScopeCycle,
    /// Scope interface or initializer mismatch.
    ScopeInterfaceMismatch,
    /// Invalid binding descriptor selection or shape.
    InvalidDescriptor(&'static str),
    /// Malformed RFC 6570 syntax.
    InvalidUriTemplate {
        /// Zero-based byte offset.
        offset: usize,
    },
    /// Tool schema differs from the descriptor's exact subdocument.
    ToolSchemaMismatch(&'static str),
    /// MCP ports differ from the binding interface.
    McpInterfaceMismatch,
    /// Schema type does not identify a reached tool schema root.
    UnreachedSchemaType,
    /// Structural policy ceiling exceeded.
    StructuralLimitExceeded {
        /// Exhausted resource.
        limit: crate::StructuralLimit,
        /// Exact positive policy ceiling.
        maximum: u64,
    },
    /// Library call arity differs from its declaration.
    FunctionArity,
    /// Render arguments do not cover template parameters exactly.
    TemplateArguments,
    /// Analysis input or derived-data resource ceiling exceeded.
    LimitExceeded {
        /// Exhausted resource.
        limit: cbor::LimitKind,
        /// Configured maximum.
        maximum: usize,
    },
    /// Fallible allocation failed.
    AllocationFailed,
}
macro_rules! convert {
    ($ty:ty,$variant:ident) => {
        impl From<$ty> for LinkageError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}
convert!(cbor::Error, Codec);
convert!(model::GraphRecordError, Graph);
convert!(model::MetadataError, Metadata);
convert!(model::ExpressionError, Expression);
convert!(model::TypeError, Type);
convert!(ParseDigestError, Digest);
convert!(model::JsonError, Json);
convert!(model::PolicyError, Policy);
convert!(crate::SchemaResourceError, Schema);
convert!(crate::NativeSchemaError, NativeSchema);
convert!(crate::McpValidationError, Mcp);
impl From<model::DocumentError> for LinkageError {
    fn from(e: model::DocumentError) -> Self {
        match e {
            model::DocumentError::Codec(e) => Self::Codec(e),
            model::DocumentError::Graph(e) => Self::Graph(e),
            model::DocumentError::Metadata(e) => Self::Metadata(e),
            model::DocumentError::Expression(e) => Self::Expression(e),
            model::DocumentError::Type(e) => Self::Type(e),
            model::DocumentError::Digest(e) => Self::Digest(e),
            model::DocumentError::Json(e) => Self::Json(e),
            model::DocumentError::InvalidUriTemplate { offset } => {
                Self::InvalidUriTemplate { offset }
            }
            model::DocumentError::LimitExceeded { limit, maximum } => {
                Self::LimitExceeded { limit, maximum }
            }
            model::DocumentError::AllocationFailed => Self::AllocationFailed,
            e => Self::Representation(Box::new(e)),
        }
    }
}
impl std::fmt::Display for LinkageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document linkage: {self:?}")
    }
}
impl std::error::Error for LinkageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Representation(e) => Some(e.as_ref()),
            Self::SchemaDiagnostic(e) => Some(e.as_ref()),
            Self::Mcp(e) => Some(e),
            Self::NativeSchema(e) => Some(e),
            Self::Schema(e) => Some(e),
            Self::Codec(e) => Some(e),
            Self::Graph(e) => Some(e),
            Self::Metadata(e) => Some(e),
            Self::Expression(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Digest(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Policy(e) => Some(e),
            _ => None,
        }
    }
}
