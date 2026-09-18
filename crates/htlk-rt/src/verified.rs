//! Composed executable admission and immutable checked execution access.
use crate::{
    CanonicalDocument, EvaluationContext, EvaluationError, EvaluationResult, ExecutableEnvelope,
    Expression, ExpressionContext as C, ExpressionSite, GraphVerification, NativeRegistry,
    NativeSchemaOptions, NativeSchemas, Operation, PolicyDocument, Scope, ScopeUse, digest::Digest,
};
use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;

/// An immutable executable admitted through the shipped schema, MCP, expression,
/// graph and exact linked-profile stages. Construction is private. The borrowed
/// registry cannot be mutated while the verified executable is in use.
pub struct VerifiedExecutable<'a> {
    analysis: crate::AnalyzedDocument,
    fingerprint: Digest,
    registry: &'a NativeRegistry,
}
impl VerifiedExecutable<'_> {
    /// Semantic result recomputed during this runtime admission, tied to the
    /// authoritative immutable document and including structural bounds.
    pub fn analysis(&self) -> &crate::AnalyzedDocument {
        &self.analysis
    }
    /// Fingerprint verified from the original canonical envelope.
    pub fn fingerprint(&self) -> Digest {
        self.fingerprint
    }
    /// Authoritative immutable canonical graph document.
    pub fn document(&self) -> &CanonicalDocument {
        self.analysis.document()
    }
    /// Exact admitted policy and evaluator ceilings.
    pub fn policy(&self) -> &PolicyDocument {
        self.analysis.policy()
    }
    /// Immutable offline native validators linked from the document's closure.
    pub fn schemas(&self) -> &NativeSchemas {
        self.analysis.schemas()
    }
    /// Derived scope-use and checked-boundary plans.
    pub fn graphs(&self) -> &GraphVerification {
        self.analysis.graphs()
    }
    /// Evaluates an admitted authored expression with frozen runtime data. The
    /// verified policy, schema closure and linked registry cannot be substituted.
    /// Guards/contracts return strict Booleans when settled; pending remains a
    /// separate result for the coordinator to handle at the appropriate boundary.
    ///
    /// # Errors
    /// Returns unknown use/site, actual-value/execution failures or evaluator limits.
    pub fn evaluate(
        &self,
        scope_use: &ScopeUse,
        site: &ExpressionSite,
        frame: &impl EvaluationContext,
    ) -> Result<EvaluationResult, EvaluationError> {
        let plan = self
            .graphs()
            .plan(scope_use)
            .ok_or(EvaluationError::UnknownExpression)?;
        let analysis = plan
            .expressions()
            .get(site)
            .ok_or(EvaluationError::UnknownExpression)?;
        let env = plan
            .environment(site)
            .ok_or(EvaluationError::UnknownExpression)?;
        let (scope, until) = self.scope_use(scope_use)?;
        let role = until.is_some();
        let (expression, context) = expression_at(scope, until, site, role)?;
        let checked = crate::CheckedExpression::borrow_admitted(
            expression,
            context,
            env,
            analysis,
            self.schemas(),
            self.analysis.limits(),
        );
        self.registry.evaluate_admitted(
            &checked,
            frame,
            Some(self.schemas()),
            &self.policy().fields().evaluator_limits,
        )
    }
    /// Validates a proposed destination value against its admitted edge boundary.
    /// Absence/selection/error ordering remains the runtime binding reducer's job.
    ///
    /// # Errors
    /// Returns unknown use/edge, value/schema mismatch, or evaluator limits.
    pub fn validate_edge_value(
        &self,
        scope_use: &ScopeUse,
        edge: &crate::Identifier,
        value: &Value,
    ) -> Result<crate::EvaluationUsage, EvaluationError> {
        let plan = self
            .graphs()
            .plan(scope_use)
            .ok_or(EvaluationError::UnknownExpression)?;
        let boundary = plan
            .boundaries()
            .iter()
            .find(|p| p.edge() == edge)
            .ok_or(EvaluationError::UnknownExpression)?;
        crate::validate_typed_value(
            value,
            boundary.destination().value_type(),
            Some(self.schemas()),
            self.analysis.limits(),
            &self.policy().fields().evaluator_limits,
        )
    }
    fn scope_use(&self, site: &ScopeUse) -> Result<(&Scope, Option<&Expression>), EvaluationError> {
        let fields = self.document().fields();
        let (digest, until) = match site {
            ScopeUse::Root => (fields.root_scope, None),
            ScopeUse::Node { scope, node } => {
                let owner = fields
                    .scopes
                    .get(scope)
                    .ok_or(EvaluationError::UnknownExpression)?;
                let node = owner
                    .fields()
                    .nodes
                    .iter()
                    .find(|n| n.id() == node)
                    .ok_or(EvaluationError::UnknownExpression)?;
                match &node.fields().operation {
                    Operation::Scope(scope) => (*scope, None),
                    Operation::Loop { body, until, .. } => (*body, Some(until)),
                    _ => return Err(EvaluationError::UnknownExpression),
                }
            }
        };
        Ok((
            fields
                .scopes
                .get(&digest)
                .ok_or(EvaluationError::UnknownExpression)?,
            until,
        ))
    }
}
fn expression_at<'a>(
    scope: &'a Scope,
    until: Option<&'a Expression>,
    site: &ExpressionSite,
    loop_body: bool,
) -> Result<(&'a Expression, C), EvaluationError> {
    let fields = scope.fields();
    match site {
        ExpressionSite::Preconditions => Ok((&fields.preconditions, C::Preconditions)),
        ExpressionSite::Postconditions => Ok((&fields.postconditions, C::ScopePostconditions)),
        ExpressionSite::Until => Ok((
            until.ok_or(EvaluationError::UnknownExpression)?,
            C::LoopUntil,
        )),
        ExpressionSite::EdgeGuard(id) => Ok((
            fields
                .edges
                .iter()
                .find(|e| e.id() == id)
                .ok_or(EvaluationError::UnknownExpression)?
                .guard(),
            C::Guard { loop_body },
        )),
        ExpressionSite::NodeGuard(id)
        | ExpressionSite::NodePreconditions(id)
        | ExpressionSite::NodePostconditions(id)
        | ExpressionSite::Eval(id) => {
            let node = fields
                .nodes
                .iter()
                .find(|n| n.id() == id)
                .ok_or(EvaluationError::UnknownExpression)?
                .fields();
            match site {
                ExpressionSite::NodeGuard(_) => Ok((&node.guard, C::Guard { loop_body })),
                ExpressionSite::NodePreconditions(_) => Ok((&node.preconditions, C::Preconditions)),
                ExpressionSite::NodePostconditions(_) => Ok((
                    &node.postconditions,
                    match node.operation {
                        Operation::Scope(_) => C::WrapperPostconditions,
                        Operation::Loop { .. } => C::LoopPostconditions,
                        _ => C::PrimitivePostconditions,
                    },
                )),
                ExpressionSite::Eval(_) => match &node.operation {
                    Operation::Eval(expression) => Ok((expression, C::Eval)),
                    _ => Err(EvaluationError::UnknownExpression),
                },
                _ => Err(EvaluationError::UnknownExpression),
            }
        }
        ExpressionSite::EdgeBoundary(_) => Err(EvaluationError::UnknownExpression),
    }
}

/// Verifies a complete bounded canonical envelope against exact linked native
/// implementations. Opaque envelope payloads must pass document admission before
/// schema/MCP/graph checks run. No partial verified object is published on failure.
///
/// # Errors
/// Returns a structured stage failure with its underlying redacted cause.
pub fn verify_executable<'a>(
    bytes: &[u8],
    registry: &'a NativeRegistry,
    limits: &Limits,
) -> Result<VerifiedExecutable<'a>, ExecutableVerificationError> {
    let envelope = ExecutableEnvelope::decode(bytes, limits)?;
    let fingerprint = envelope.fingerprint();
    let document = CanonicalDocument::from_envelope(&envelope, limits)?;
    drop(envelope);
    let fields = document.fields();
    let policy = fields
        .documents
        .get(&fields.profile.policy_document())
        .ok_or(crate::LinkageError::MissingRecord("policy document"))
        .map_err(crate::DocumentAnalysisError::from)?;
    let policy = PolicyDocument::from_document(policy.clone(), limits)?;
    registry.verify_profile(&fields.profile, &policy)?;
    for library in fields.libraries.values() {
        registry.verify_library(library)?;
    }
    let analysis = crate::analyze_document(document, NativeSchemaOptions::default(), limits)?;
    Ok(VerifiedExecutable {
        analysis,
        fingerprint,
        registry,
    })
}
/// Stage-specific composed verification failure; graph errors retain scope/use/site
/// locations. Submitted value bytes and native validator messages are not retained.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecutableVerificationError {
    /// Envelope format/canonical encoding/fingerprint failure.
    Envelope(crate::EnvelopeError),
    /// Canonical document representation failure.
    Document(Box<crate::DocumentError>),
    /// Exact pinned policy failure.
    Policy(crate::PolicyError),
    /// Linked implementation/profile admission failure.
    Registry(crate::NativeRegistryError),
    /// Semantic linkage, schema/MCP, expression, graph or structural failure.
    Analysis(Box<crate::DocumentAnalysisError>),
}
macro_rules! convert {
    ($ty:ty,$variant:ident) => {
        impl From<$ty> for ExecutableVerificationError {
            fn from(error: $ty) -> Self {
                Self::$variant(error)
            }
        }
    };
}
convert!(crate::EnvelopeError, Envelope);
convert!(crate::PolicyError, Policy);
convert!(crate::NativeRegistryError, Registry);
impl From<crate::DocumentError> for ExecutableVerificationError {
    fn from(e: crate::DocumentError) -> Self {
        Self::Document(Box::new(e))
    }
}
impl From<crate::DocumentAnalysisError> for ExecutableVerificationError {
    fn from(e: crate::DocumentAnalysisError) -> Self {
        Self::Analysis(Box::new(e))
    }
}
impl std::fmt::Display for ExecutableVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "executable verification: {self:?}")
    }
}
impl std::error::Error for ExecutableVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Envelope(e) => e,
            Self::Document(e) => e.as_ref(),
            Self::Policy(e) => e,
            Self::Registry(e) => e,
            Self::Analysis(e) => e.as_ref(),
        })
    }
}
