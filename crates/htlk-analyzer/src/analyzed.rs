//! Whole-document analysis with immutable document ownership.
use crate::{
    DocumentAnalysis, GraphVerification, GraphVerificationError, LinkageError, NativeSchemaOptions,
    NativeSchemas, StructuralSummary,
};
use htlk_executable::{CanonicalDocument, PolicyDocument, cbor::Limits, digest::Digest};

/// Complete semantic analysis tied to the exact immutable document it describes.
/// Construction is private; inferred plans are neither deserialized nor accepted
/// as caller-provided proof. Host policy and native implementation admission are
/// separate runtime responsibilities.
pub struct AnalyzedDocument {
    document: std::sync::Arc<CanonicalDocument>,
    fingerprint: Digest,
    policy: PolicyDocument,
    structure: StructuralSummary,
    schemas: NativeSchemas,
    graphs: GraphVerification,
    limits: Limits,
    diagnostics: Vec<crate::AnalysisDiagnostic>,
}
impl AnalyzedDocument {
    /// Authoritative immutable canonical document.
    pub fn document(&self) -> &CanonicalDocument {
        &self.document
    }
    /// Fingerprint of the exact canonical document envelope.
    pub fn fingerprint(&self) -> Digest {
        self.fingerprint
    }
    /// Parsed linked policy declaration, without host authorization.
    pub fn policy(&self) -> &PolicyDocument {
        &self.policy
    }
    /// Derived policy-checked structural bounds.
    pub fn structural_summary(&self) -> StructuralSummary {
        self.structure
    }
    /// Prepared immutable offline schemas used for inference and obligations.
    pub fn schemas(&self) -> &NativeSchemas {
        &self.schemas
    }
    /// Read-only scope-use and expression plans.
    pub fn graphs(&self) -> &GraphVerification {
        &self.graphs
    }
    /// Codec and analysis ceilings used for this result.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    /// Deterministic semantic warnings, outside executable identity.
    pub fn diagnostics(&self) -> &[crate::AnalysisDiagnostic] {
        &self.diagnostics
    }
}

/// Verifies linkage, schema/MCP semantics, expression types and every graph use.
/// Retains ownership of the immutable input so plans cannot be attached to a
/// different document after verification.
///
/// # Errors
/// Returns structured linkage/schema or scope/use/expression diagnostics.
pub fn analyze_document(
    document: CanonicalDocument,
    options: NativeSchemaOptions,
    limits: &Limits,
) -> Result<AnalyzedDocument, DocumentAnalysisError> {
    let (policy, structure) = crate::linkage::verify_located(&document, limits)?;
    crate::linkage::mcp_located(&document, limits)?;
    let schemas = document.native_schemas(options, limits).map_err(|error| {
        if let LinkageError::SchemaDiagnostic(cause) = &error
            && let Some(location) = &cause.location
            && let Ok(location) = crate::diagnostic::location(
                &format!("/documents/{}", location.document),
                None,
                limits,
            )
        {
            return DocumentAnalysisError::LocatedLinkage(Box::new(crate::LinkageDiagnostic {
                location,
                related_locations: Vec::new(),
                error,
            }));
        }
        error.into()
    })?;
    let document = std::sync::Arc::new(document);
    let graphs = crate::graph_verify::verify_linked_graphs(
        std::sync::Arc::clone(&document),
        &schemas,
        limits,
    )?;
    let fingerprint = document
        .envelope(limits)
        .map_err(LinkageError::from)?
        .fingerprint();
    let mut diagnostics = Vec::new();
    let mut report_bytes = 0usize;
    for (scope_use, scope, plan) in graphs.plans() {
        for binding in plan.bindings().iter().filter(|b| b.may_overlap()) {
            let mut locations = Vec::new();
            for edge in binding.candidates() {
                let location = crate::diagnostic::expression_location(
                    &document,
                    scope,
                    scope_use,
                    &crate::ExpressionSite::EdgeBoundary(edge.clone()),
                    &[],
                    limits,
                )?;
                report_bytes = report_bytes
                    .checked_add(location.stored_size() + size_of::<crate::AnalysisDiagnostic>())
                    .ok_or(LinkageError::AllocationFailed)?;
                if report_bytes > limits.max_document_bytes {
                    return Err(LinkageError::LimitExceeded {
                        limit: htlk_executable::cbor::LimitKind::DocumentBytes,
                        maximum: limits.max_document_bytes,
                    }
                    .into());
                }
                locations.push(location);
            }
            let location = locations.remove(0);
            diagnostics.push(crate::AnalysisDiagnostic {
                code: "W_CONDITIONAL_WRITERS",
                severity: crate::DiagnosticSeverity::Warning,
                message: "Candidate guards may overlap; runtime enforces unique selection.",
                location,
                related_locations: locations,
            });
        }
    }
    Ok(AnalyzedDocument {
        document,
        fingerprint,
        policy,
        structure,
        schemas,
        graphs,
        limits: limits.clone(),
        diagnostics,
    })
}

/// Structured stage failure from whole-document semantic analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentAnalysisError {
    /// Located linkage failure, preserving its original structured category.
    LocatedLinkage(Box<crate::LinkageDiagnostic>),
    /// Document linkage, schema, protocol or structural failure.
    Linkage(Box<LinkageError>),
    /// Scope-use, expression, dependency or observability failure.
    Graph(Box<GraphVerificationError>),
}
impl From<LinkageError> for DocumentAnalysisError {
    fn from(e: LinkageError) -> Self {
        Self::Linkage(Box::new(e))
    }
}
impl From<GraphVerificationError> for DocumentAnalysisError {
    fn from(e: GraphVerificationError) -> Self {
        Self::Graph(Box::new(e))
    }
}
impl std::fmt::Display for DocumentAnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document analysis: {self:?}")
    }
}
impl std::error::Error for DocumentAnalysisError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::LocatedLinkage(e) => e.as_ref(),
            Self::Linkage(e) => e.as_ref(),
            Self::Graph(e) => e.as_ref(),
        })
    }
}

impl DocumentAnalysisError {
    /// Converts a stage failure to the specification's source-independent report.
    /// Native schema causes retain their additional original JSON pointer.
    ///
    /// # Errors
    /// Returns a missing canonical location or a diagnostic-construction limit.
    pub fn diagnostic(
        &self,
        document: &CanonicalDocument,
        limits: &Limits,
    ) -> Result<crate::AnalysisDiagnostic, LinkageError> {
        use crate::{DiagnosticSeverity as S, ScopeVerificationErrorKind as K};
        let (code, location, related_locations) = match self {
            Self::LocatedLinkage(e) => (
                e.error.diagnostic_code(),
                e.location.clone(),
                e.related_locations.clone(),
            ),
            Self::Linkage(e) => (
                e.diagnostic_code(),
                crate::diagnostic::location("", None, limits)?,
                Vec::new(),
            ),
            Self::Graph(e) => {
                let code = match &e.cause.kind {
                    K::Expression(error) => error.diagnostic_code(),
                    K::Cycle => "C_DEPENDENCY_CYCLE",
                    K::Unobservable(_) => "C_UNUSED_NODE",
                    K::MissingCandidate(_) | K::UnconditionalWriters(_) => "C_BINDING",
                    K::Limit => "C_POLICY",
                    _ => "C_SCOPE",
                };
                let (location, related) = if let (Some(scope), Some(scope_use), Some(site)) =
                    (e.scope, &e.scope_use, &e.cause.site)
                {
                    (
                        crate::diagnostic::expression_location(
                            document,
                            scope,
                            scope_use,
                            site,
                            e.cause.expression_path.as_deref().unwrap_or_default(),
                            limits,
                        )?,
                        Vec::new(),
                    )
                } else {
                    crate::diagnostic::graph_location(document, e, limits)?
                };
                (code, location, related)
            }
        };
        let diagnostic = crate::AnalysisDiagnostic {
            code,
            severity: S::Error,
            message: "The executable does not satisfy semantic analysis requirements.",
            location,
            related_locations,
        };
        diagnostic.check_limits(limits)?;
        Ok(diagnostic)
    }
}
