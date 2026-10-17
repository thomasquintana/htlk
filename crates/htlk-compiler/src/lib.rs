#![doc = include_str!("../README.md")]

/// Shared semantic analysis and structured compiler-facing diagnostics.
pub use htlk_analyzer as analyzer;
/// Canonical executable records and bounded wire construction.
pub use htlk_executable as executable;

mod catalogs;
mod compiler;
mod diagnostic;
mod input_value;

pub use catalogs::MCPCatalogInputs;
pub use compiler::{CompilationSuccess, Compiler, CompilerConfig, FrontendLimits};
pub use diagnostic::{
    CompilationFailure, Diagnostic, DiagnosticLocation, SourceLocation, SourceMapEntry, SourceSpan,
};
