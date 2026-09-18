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
    let (policy, structure) = crate::linkage::verify(&document, limits)?;
    document.validate_mcp_descriptors(limits)?;
    let schemas = document.native_schemas(options, limits)?;
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
    Ok(AnalyzedDocument {
        document,
        fingerprint,
        policy,
        structure,
        schemas,
        graphs,
        limits: limits.clone(),
    })
}

/// Structured stage failure from whole-document semantic analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentAnalysisError {
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
            Self::Linkage(e) => e.as_ref(),
            Self::Graph(e) => e.as_ref(),
        })
    }
}
