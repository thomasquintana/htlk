#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use error::LinkageError as DocumentError;
use htlk_executable::*;

mod analyzed;
mod binding_validation;
mod context;
mod error;
mod graph_verify;
mod implementation;
mod linkage;
mod linked;
mod mcp_protocol;
mod native_schema;
mod schema_catalog;
mod schema_hints;
mod schema_locations;
mod schema_projection;
mod schema_resources;
mod structure;
mod type_check;

pub use analyzed::{AnalyzedDocument, DocumentAnalysisError, analyze_document};
pub use context::check_expression_context;
pub use context::check_function_signature;
pub use context::check_prompt_template;
pub use error::LinkageError;
pub use graph_verify::{
    BindingPlan, EdgeBoundaryPlan, ExpressionSite, GraphVerification, GraphVerificationError,
    ScopeGraphPlan, ScopeUse, ScopeVerificationError, ScopeVerificationErrorKind, WaitVertex,
    verify_graphs, verify_scope_graph,
};
pub use implementation::implementation_digest;
pub use linkage::DocumentAnalysis;
pub use linked::LinkedDocument;
pub use mcp_protocol::{MCP_SCHEMA_URI, mcp_schema_validators};
pub use mcp_protocol::{McpDescriptorKind, McpValidationError, validate_mcp_descriptor};
pub use native_schema::{
    NativeSchemaDiagnostic, NativeSchemaError, NativeSchemaOptions, NativeSchemas,
    SchemaDiagnosticLocation,
};
pub use schema_catalog::{
    ResolvedSchema, SchemaCatalog, SchemaClosure, SchemaReference, SchemaReferenceKind,
};
pub use schema_locations::{JSON_SCHEMA_DIALECT, SchemaLocationError, SchemaLocations};
pub use schema_projection::DECLARED_FIELDS;
pub use schema_resources::{SchemaResourceError, SchemaResources, embedded_schema_base};
pub use structure::{StructuralLimit, StructuralSummary};
pub use type_check::CallbackCompatibility;
pub use type_check::callback_compatibility;
pub use type_check::{
    ExpressionAnalysis, ExpressionCallType, ExpressionCallbackType, ExpressionDiagnostic,
    ExpressionNodeType, ExpressionTypeEnvironment, ExpressionTypeError, RuntimeTypeCheck,
    RuntimeTypeCheckKind, check_condition, check_expression, check_expression_diagnostic,
    check_expression_with_schemas,
};
