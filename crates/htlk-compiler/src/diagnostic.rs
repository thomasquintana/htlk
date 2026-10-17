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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticLocation {
    /// Authored text.
    SourceSpan(SourceSpan),
    /// Manifest or source bundle input.
    SourceBundlePath(String),
    /// Submitted or proposed canonical document.
    CanonicalRecordPath(String),
}
impl Ord for DiagnosticLocation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let kind = |v: &Self| match v {
            Self::CanonicalRecordPath(_) => 0,
            Self::SourceBundlePath(_) => 1,
            Self::SourceSpan(_) => 2,
        };
        kind(self)
            .cmp(&kind(other))
            .then_with(|| match (self, other) {
                (Self::CanonicalRecordPath(a), Self::CanonicalRecordPath(b))
                | (Self::SourceBundlePath(a), Self::SourceBundlePath(b)) => a.cmp(b),
                (Self::SourceSpan(a), Self::SourceSpan(b)) => a.cmp(b),
                _ => std::cmp::Ordering::Equal,
            })
    }
}
impl PartialOrd for DiagnosticLocation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
pub(crate) fn sort(diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.sort_by(|a, b| {
        a.location
            .cmp(&b.location)
            .then(a.code.cmp(&b.code))
            .then(a.message.cmp(&b.message))
            .then(a.related_locations.cmp(&b.related_locations))
    });
    diagnostics.dedup();
}
pub(crate) fn related(code: &str, message: &str, span: &SourceSpan) -> CompilationFailure {
    let mut e = fail(code, message);
    e.diagnostics[0]
        .related_locations
        .push(DiagnosticLocation::SourceSpan(span.clone()));
    e
}
fn source_size(s: &SourceLocation) -> usize {
    match s {
        SourceLocation::Inline => 0,
        SourceLocation::Module {
            module_path,
            source_path,
            ..
        } => module_path.len().saturating_add(source_path.len()),
    }
}
pub(crate) fn map_size(e: &SourceMapEntry) -> usize {
    size_of::<SourceMapEntry>()
        .saturating_add(e.canonical_path.len())
        .saturating_add(source_size(&e.span.source))
        .saturating_add(e.declaration_name.as_ref().map_or(0, String::len))
}
pub(crate) fn diagnostic_size(d: &Diagnostic) -> usize {
    let location = |l: &DiagnosticLocation| match l {
        DiagnosticLocation::SourceSpan(s) => {
            size_of::<SourceSpan>().saturating_add(source_size(&s.source))
        }
        DiagnosticLocation::CanonicalRecordPath(p) | DiagnosticLocation::SourceBundlePath(p) => {
            p.len()
        }
    };
    d.related_locations.iter().fold(
        size_of::<Diagnostic>()
            .saturating_add(d.code.len())
            .saturating_add(d.message.len())
            .saturating_add(location(&d.location)),
        |n, l| n.saturating_add(location(l)),
    )
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
