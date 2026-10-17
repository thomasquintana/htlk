#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

/// Shared semantic analysis and structured compiler-facing diagnostics.
pub use htlk_analyzer as analyzer;
/// Canonical executable records and bounded wire construction.
pub use htlk_executable as executable;

mod ast;
mod bundle;
mod catalogs;
mod compiler;
mod diagnostic;
mod input_value;
mod inspect;
mod join;
pub use join::{JoinEdge, JoinEndpoint, JoinInput, JoinOperand, JoinOutput, JoinSpec};
mod lexer;
mod lower;
mod modules;
mod parser;
mod provenance;

pub use bundle::{ModuleRef, SourceBundle, SourcePackage, SourcePackageManifest};
pub use inspect::{ExportIndexEntry, ExportKind, ExportSummary, ModuleInspectionSuccess};

pub use catalogs::MCPCatalogInputs;
pub use compiler::{
    CompilationInput, CompilationSuccess, Compiler, CompilerCapabilities, CompilerConfig,
    FrontendLimits,
};
pub use diagnostic::{
    CompilationFailure, Diagnostic, DiagnosticLocation, SourceLocation, SourceMapEntry, SourceSpan,
};
