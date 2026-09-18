//! Focused document linkage before schema and expression/graph analysis.
use crate::{LinkageError, StructuralSummary};
use htlk_executable::{
    CanonicalDocument, DocumentFields, ExecutableEnvelope, McpBindingKind, PolicyDocument,
    cbor::{self, Limits, Value},
};

/// Immutable document with semantic references/interfaces and structural bounds
/// checked. This focused result does not claim schema, expression or full graph
/// verification; use analyze_document for those additional stages.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkedDocument {
    document: CanonicalDocument,
    policy: PolicyDocument,
    structure: StructuralSummary,
}
impl LinkedDocument {
    /// Constructs the canonical model and verifies its semantic linkage.
    ///
    /// # Errors
    /// Returns representation, linkage or structural-limit failures.
    pub fn new(fields: DocumentFields, limits: &Limits) -> Result<Self, LinkageError> {
        Self::from_document(CanonicalDocument::new(fields, limits)?, limits)
    }
    /// Analyzes an existing immutable canonical document.
    ///
    /// # Errors
    /// Returns linkage or structural-limit failures under the supplied limits.
    pub fn from_document(
        document: CanonicalDocument,
        limits: &Limits,
    ) -> Result<Self, LinkageError> {
        let (policy, structure) = crate::linkage::verify(&document, limits)?;
        Ok(Self {
            document,
            policy,
            structure,
        })
    }
    /// Original immutable canonical representation.
    pub fn document(&self) -> &CanonicalDocument {
        &self.document
    }
    /// Parsed linked policy declaration, without host authorization.
    pub fn policy(&self) -> &PolicyDocument {
        &self.policy
    }
    /// Policy-checked derived structural bounds; never serialized.
    pub fn structural_summary(&self) -> StructuralSummary {
        self.structure
    }
    /// Produces canonical model data, reapplying representation/derived syntax limits.
    ///
    /// # Errors
    /// Returns tighter codec or URI-template discovery limits.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, LinkageError> {
        let value = self.document.to_value(limits)?;
        for b in self.document.fields().bindings.values() {
            if let McpBindingKind::Template { uri_template } = b.kind() {
                htlk_executable::uri_template_variables(uri_template, limits)?;
            }
        }
        Ok(value)
    }
    /// Encodes the canonical model, without serializing analysis data.
    ///
    /// # Errors
    /// Returns representation or resource failures.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, LinkageError> {
        Ok(cbor::encode(&self.to_value(limits)?, limits)?)
    }
    /// Decodes canonical input and checks its semantic document linkage.
    ///
    /// # Errors
    /// Returns representation, linkage or resource failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, LinkageError> {
        Self::from_document(CanonicalDocument::decode(bytes, limits)?, limits)
    }
    /// Checks canonical model data and then its semantic linkage.
    ///
    /// # Errors
    /// Returns representation, linkage or resource failures.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, LinkageError> {
        Self::from_document(CanonicalDocument::from_value(value, limits)?, limits)
    }
    /// Checks the nested model and semantic linkage of a canonical envelope.
    ///
    /// # Errors
    /// Returns representation, linkage or resource failures.
    pub fn from_envelope(
        envelope: &ExecutableEnvelope,
        limits: &Limits,
    ) -> Result<Self, LinkageError> {
        Self::decode(envelope.payload(), limits)
    }
}
impl std::ops::Deref for LinkedDocument {
    type Target = CanonicalDocument;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}
