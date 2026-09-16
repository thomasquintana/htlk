#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod digest;

mod binding_validation;
mod checked_evaluate;
mod document;
mod envelope;
mod evaluate;
mod expression;
mod graph;
mod graph_verify;
mod identifier;
mod json;
mod json_pointer;
mod mcp_protocol;
mod metadata;
mod native_profile;
mod native_registry;
mod native_schema;
mod options;
mod policy;
mod record_accounting;
mod runtime_type;
mod schema_catalog;
mod schema_hints;
mod schema_locations;
mod schema_projection;
mod schema_resources;
mod structure;
mod type_check;
mod types;
mod uri_template;
mod verified;

pub use checked_evaluate::{CallbackArgument, CheckedExpression};
pub use document::{CanonicalDocument, DocumentError, DocumentFields};
pub use envelope::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};
pub use evaluate::{
    ConditionResult, ConditionValue, EvaluationArgument, EvaluationContext, EvaluationError,
    EvaluationFrame, EvaluationMeter, EvaluationOutcome, EvaluationResult, EvaluationUsage,
    EvaluationValue, evaluate, evaluate_condition,
};
pub use expression::{
    BinaryOperator, CoreFunction, Expression, ExpressionContext, ExpressionError, ExpressionKind,
    FunctionId, PathStep, PromptTemplate, ScalarLiteral, TemplatePart, ValueReference,
};
pub use graph::{
    Edge, EdgeDestination, EdgeSource, GraphRecordError, Node, NodeFields, Operation, PortTable,
    Scope, ScopeContext, ScopeFields,
};
pub use graph_verify::{
    BindingPlan, EdgeBoundaryPlan, ExpressionSite, ScopeGraphPlan, ScopeVerificationError,
    ScopeVerificationErrorKind, WaitVertex, verify_scope_graph,
};
pub use graph_verify::{GraphVerification, GraphVerificationError, ScopeUse, verify_graphs};
pub use identifier::{Identifier, ParseIdentifierError};
pub use json::{JsonDocument, JsonError};
pub use json_pointer::{JsonPointer, JsonPointerError};
pub use mcp_protocol::{
    McpDescriptorKind, McpValidationError, validate_mcp_descriptor, validate_mcp_prompt_result,
    validate_resource_snapshot,
};
pub use metadata::{
    CORE_VERSION, EngineIdentity, ExecutionProfile, FunctionSignature, Library,
    MCP_PROTOCOL_VERSION, McpBinding, McpBindingKind, McpTransport, MetadataError, ServerIdentity,
};
pub use native_profile::native_profile;
pub use native_registry::{
    NativeCallContext, NativeFunction, NativeFunctionImpl, NativeRegistry, NativeRegistryError,
};
pub use native_schema::{
    NativeSchemaDiagnostic, NativeSchemaError, NativeSchemaOptions, NativeSchemas,
    SchemaDiagnosticLocation,
};
pub use options::{ExecutionLimits, ExecutionOptionsError, RetryPolicy};
pub use policy::{EvaluatorLimits, PolicyDocument, PolicyError, PolicyFields};
pub use runtime_type::{project_typed_value, validate_typed_value};
pub use schema_catalog::{
    ResolvedSchema, SchemaCatalog, SchemaClosure, SchemaReference, SchemaReferenceKind,
};
pub use schema_locations::{JSON_SCHEMA_DIALECT, SchemaLocationError, SchemaLocations};
pub use schema_projection::SchemaProjection;
pub use schema_resources::{SchemaResourceError, SchemaResources, embedded_schema_base};
pub use structure::{StructuralLimit, StructuralSummary};
pub use type_check::check_expression_with_schemas;
pub use type_check::{
    ExpressionAnalysis, ExpressionCallType, ExpressionCallbackType, ExpressionNodeType,
    ExpressionTypeEnvironment, ExpressionTypeError, RuntimeTypeCheck, RuntimeTypeCheckKind,
    check_condition, check_expression,
};
pub use type_check::{ExpressionDiagnostic, check_expression_diagnostic};
pub use types::{Port, PrimitiveType, TypeContext, TypeError, ValueType, ValueTypeKind};
pub use uri_template::expand_uri_template;
pub use verified::{ExecutableVerificationError, VerifiedExecutable, verify_executable};
