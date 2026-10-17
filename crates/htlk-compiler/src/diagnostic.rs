//! Portable compiler reports, independent of terminal rendering.
use crate::analyzer::{AnalysisDiagnostic, DiagnosticSeverity};
use crate::executable::digest::Digest;

/// Identity of one immutable source string.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceLocation {
    /// Standalone source text.
    Inline,
    /// Exact module in a supplied package snapshot.
    Module {
        /// Source package identity.
        package_digest: Digest,
        /// Logical module key.
        module_path: String,
        /// Manifest-mapped source file key.
        source_path: String,
    },
}

/// Exclusive UTF-8 byte range within one source string.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceSpan {
    /// Source identity.
    pub source: SourceLocation,
    /// Inclusive first byte.
    pub start_byte: usize,
    /// Exclusive last byte.
    pub end_byte: usize,
}

/// Stable report location. Structured pointers use RFC 6901 spelling.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticLocation {
    /// Authored text.
    SourceSpan(SourceSpan),
    /// Manifest or source bundle input.
    SourceBundlePath(String),
    /// Submitted or proposed canonical document.
    CanonicalRecordPath(String),
}

/// Structured compilation error or warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Specification-defined diagnostic family.
    pub code: String,
    /// Error or warning.
    pub severity: DiagnosticSeverity,
    /// Human-readable explanation.
    pub message: String,
    /// Primary location.
    pub location: DiagnosticLocation,
    /// Other relevant declaration/use locations.
    pub related_locations: Vec<DiagnosticLocation>,
}

/// A failed request; contains no partial artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilationFailure {
    /// Deterministically ordered errors and warnings.
    pub diagnostics: Vec<Diagnostic>,
}
impl std::fmt::Display for CompilationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, diagnostic) in self.diagnostics.iter().enumerate() {
            if i != 0 {
                writeln!(f)?;
            }
            write!(f, "{}: {}", diagnostic.code, diagnostic.message)?;
        }
        Ok(())
    }
}
impl std::error::Error for CompilationFailure {}

/// Association of a normalized record with an authored span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapEntry {
    /// RFC 6901 path into the canonical document.
    pub canonical_path: String,
    /// Source location and byte offsets.
    pub span: SourceSpan,
    /// Original declaration, when applicable.
    pub declaration_name: Option<String>,
}

pub(crate) type Result<T> = std::result::Result<T, CompilationFailure>;
pub(crate) fn fail(code: &str, message: impl ToString) -> CompilationFailure {
    CompilationFailure {
        diagnostics: vec![Diagnostic {
            code: code.into(),
            severity: DiagnosticSeverity::Error,
            message: message.to_string(),
            location: DiagnosticLocation::CanonicalRecordPath(String::new()),
            related_locations: vec![],
        }],
    }
}
pub(crate) fn analysis(d: AnalysisDiagnostic) -> Diagnostic {
    Diagnostic {
        code: d.code.into(),
        severity: d.severity,
        message: d.message.into(),
        location: DiagnosticLocation::CanonicalRecordPath(d.location.pointer.to_string()),
        related_locations: d
            .related_locations
            .into_iter()
            .map(|l| DiagnosticLocation::CanonicalRecordPath(l.pointer.to_string()))
            .collect(),
    }
}
