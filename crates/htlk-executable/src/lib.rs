#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod digest;

mod binding_validation;
mod document;
mod envelope;
mod expression;
mod graph;
mod identifier;
mod json;
mod metadata;
mod options;
mod policy;
mod record_accounting;
mod structure;
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
pub use json::{JsonDocument, JsonError};
pub use metadata::{
    CORE_VERSION, EngineIdentity, ExecutionProfile, FunctionSignature, Library,
    MCP_PROTOCOL_VERSION, McpBinding, McpBindingKind, McpTransport, MetadataError, ServerIdentity,
};
pub use options::{ExecutionLimits, ExecutionOptionsError, RetryPolicy};
pub use policy::{EvaluatorLimits, PolicyDocument, PolicyError, PolicyFields};
pub use structure::{StructuralLimit, StructuralSummary};
pub use types::{Port, PrimitiveType, TypeContext, TypeError, ValueType, ValueTypeKind};
