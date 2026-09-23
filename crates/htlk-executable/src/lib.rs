#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod cbor;
pub mod digest;

mod document;
mod envelope;
mod expression;
mod graph;
mod identifier;
mod implementation;
mod json;
mod json_pointer;
mod metadata;
mod options;
mod policy;
pub mod record_accounting;
mod types;
mod uri_template;

pub use document::{CanonicalDocument, DocumentError, DocumentFields};
pub use envelope::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};
pub use expression::{
    BinaryOperator, CoreFunction, Expression, ExpressionContext, ExpressionError, ExpressionKind,
    FunctionId, PathStep, PromptTemplate, ScalarLiteral, TemplatePart, ValueReference,
};
pub use graph::{
    Edge, EdgeDestination, EdgeSource, GraphRecordError, Node, NodeFields, Operation, PortTable,
    Scope, ScopeContext, ScopeFields,
};
pub use identifier::{Identifier, ParseIdentifierError};
pub use implementation::implementation_digest;
pub use json::{JsonDocument, JsonError};
pub use json_pointer::{JsonPointer, JsonPointerError};
pub use metadata::{
    CORE_VERSION, EngineIdentity, ExecutionProfile, FunctionSignature, Library,
    MCP_PROTOCOL_VERSION, McpBinding, McpBindingKind, McpTransport, MetadataError, ServerIdentity,
};
pub use options::{ExecutionLimits, ExecutionOptionsError, RetryPolicy};
pub use policy::{EvaluatorLimits, PolicyDocument, PolicyError, PolicyFields};
pub use types::builtin_record_type;
pub use types::{BuiltinType, Port, TypeContext, TypeError, ValueType, ValueTypeKind};
pub use uri_template::variables as uri_template_variables;
