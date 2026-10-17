//! Actual-read protection tracking around a checked evaluation context. Metadata
//! is trusted coordinator input; this adapter grants no access from ordinary data.
use crate::{
    EvaluationArgument, EvaluationContext, EvaluationError, EvaluationMeter, EvaluationOutcome,
    EvaluationResult, EvaluationValue, Expression, ExpressionCallType, ExpressionCallbackType,
    VerifiedExecutable,
    failure::{FailureCause, FailureCode, RuntimeFailure},
    records::{RuntimeRecordError, SecurityLabels},
};
use htlk_analyzer::{ExpressionSite, ScopeUse};
use htlk_executable::{
    Identifier, PathStep, PromptTemplate, ValueReference,
    cbor::{Limits, Value},
    digest::Digest,
    record_accounting::RecordAccounting,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Protection and optional causal metadata for one admitted reference/outcome.
#[derive(Clone, Debug, Default)]
pub struct ReferenceInfluence {
    /// Immutable trusted source/boundary labels.
    pub labels: SecurityLabels,
    /// Protected causal invocation/location, when available.
    pub cause: Option<FailureCause>,
}
/// Frozen coordinator metadata accompanying a data context. Base labels cover
/// enclosing admission/control influence even when an expression reads no value.
#[derive(Clone, Debug, Default)]
pub struct InfluenceEnvironment {
    /// Enclosing scope/invocation protection.
    pub base: SecurityLabels,
    /// Every potentially evaluated admitted value reference.
    pub references: BTreeMap<ValueReference, ReferenceInfluence>,
    /// Every potentially evaluated direct-child outcome.
    pub outcomes: BTreeMap<Identifier, ReferenceInfluence>,
}
/// Checked value/error together with immutable actual-read influence and causes.
#[derive(Debug)]
pub struct InfluencedEvaluation {
    /// Value, absence, pending or native evaluation error. A denied read never
    /// appears as a successful Boolean false.
    pub result: Result<EvaluationResult, InfluencedError>,
    /// Conservative base plus actually evaluated source/decision labels.
    pub influence: SecurityLabels,
    /// Protected causal links read by this evaluation.
    pub causes: Vec<FailureCause>,
}
/// Native evaluation error or the exact stored authorization/invariant failure.
#[derive(Debug)]
pub enum InfluencedError {
    /// Checked native expression failed.
    Evaluation(EvaluationError),
    /// Trusted policy denied an actual read, or required metadata was missing.
    Failure(RuntimeFailure),
}
impl InfluencedError {
    /// Stable public classification, preserving operational errors as errors.
    pub fn code(&self) -> FailureCode {
        match self {
            Self::Evaluation(e) => {
                FailureCode::for_evaluation(e, crate::failure::FailureBoundary::Expression)
            }
            Self::Failure(f) => f.code(),
        }
    }
}
impl fmt::Display for InfluencedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evaluation(e) => fmt::Display::fmt(e, f),
            Self::Failure(e) => fmt::Display::fmt(e, f),
        }
    }
}
impl std::error::Error for InfluencedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Evaluation(e) => Some(e),
            Self::Failure(e) => Some(e),
        }
    }
}
#[derive(Default)]
struct Trace {
    references: BTreeSet<ValueReference>,
    outcomes: BTreeSet<Identifier>,
    labels: Vec<SecurityLabels>,
    causes: Vec<FailureCause>,
    failure: Option<RuntimeFailure>,
}
/// An actual-read adapter. Use `evaluate` to retain policy failures and protection
/// on both successful and failed results. It forwards schema/callback mechanisms
/// and does not alter lazy expression ordering or expose new capabilities.
pub struct InfluenceContext<'a, C> {
    inner: &'a C,
    environment: &'a InfluenceEnvironment,
    authorize: &'a dyn Fn(&SecurityLabels) -> Result<(), RuntimeFailure>,
    limits: &'a Limits,
    trace: RefCell<Trace>,
}
impl<'a, C: EvaluationContext> InfluenceContext<'a, C> {
    /// Binds trusted frozen protection and read authorization to an existing data
    /// context. Per-reference metadata is touched only when that reference is read.
    ///
    /// # Errors
    /// Returns configured base/key metadata limits. This establishes no admission.
    pub fn new(
        inner: &'a C,
        environment: &'a InfluenceEnvironment,
        authorize: &'a dyn Fn(&SecurityLabels) -> Result<(), RuntimeFailure>,
        limits: &'a Limits,
    ) -> Result<Self, RuntimeRecordError> {
        let mut budget = RecordAccounting::new(limits)?;
        budget.value(&environment.base.to_value(limits)?, 0)?;
        budget.collection(environment.references.len(), 0)?;
        for reference in environment.references.keys() {
            match reference {
                ValueReference::Input(n)
                | ValueReference::ScopeOutput(n)
                | ValueReference::Carried(n)
                | ValueReference::Next(n) => budget.text(n.as_str(), 1)?,
                ValueReference::Output { node, port } => {
                    budget.text(node.as_str(), 1)?;
                    budget.text(port.as_str(), 1)?;
                }
            }
        }
        budget.collection(environment.outcomes.len(), 0)?;
        for node in environment.outcomes.keys() {
            budget.text(node.as_str(), 1)?;
        }
        Ok(Self {
            inner,
            environment,
            authorize,
            limits,
            trace: RefCell::new(Trace::default()),
        })
    }
    /// Evaluates an admitted authored site, retaining actual-read protection even
    /// on failure/pending/absence. Each call starts a fresh influence trace.
    ///
    /// # Errors
    /// Returns metadata limits rather than publishing an incompletely labeled result.
    pub fn evaluate(
        &self,
        executable: &VerifiedExecutable<'_>,
        scope_use: &ScopeUse,
        site: &ExpressionSite,
    ) -> Result<InfluencedEvaluation, RuntimeRecordError> {
        self.trace.replace(Trace::default());
        let result = executable.evaluate(scope_use, site, self);
        let trace = self.trace.borrow();
        let influence = SecurityLabels::combine(
            std::iter::once(&self.environment.base).chain(trace.labels.iter()),
            self.limits,
        )?;
        let result = if let Some(failure) = &trace.failure {
            Err(InfluencedError::Failure(failure.clone()))
        } else {
            result.map_err(InfluencedError::Evaluation)
        };
        Ok(InfluencedEvaluation {
            result,
            influence,
            causes: trace.causes.clone(),
        })
    }
    fn read(&self, influence: Option<&ReferenceInfluence>) -> bool {
        let Some(influence) = influence else {
            self.fail(RuntimeFailure::new(FailureCode::Invariant));
            return false;
        };
        self.trace
            .borrow_mut()
            .labels
            .push(influence.labels.clone());
        if let Some(cause) = &influence.cause {
            self.trace.borrow_mut().causes.push(cause.clone());
        }
        let labels = match self.environment.base.union(&influence.labels, self.limits) {
            Ok(labels) => labels,
            Err(_) => {
                self.fail(RuntimeFailure::new(FailureCode::ExpressionLimit));
                return false;
            }
        };
        if let Err(failure) = (self.authorize)(&labels) {
            self.fail(failure);
            return false;
        }
        true
    }
    fn fail(&self, failure: RuntimeFailure) {
        let mut trace = self.trace.borrow_mut();
        if trace.failure.is_none() {
            trace.failure = Some(failure);
        }
    }
}
impl<C: EvaluationContext> EvaluationContext for InfluenceContext<'_, C> {
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        let first = self.trace.borrow_mut().references.insert(reference.clone());
        if first && !self.read(self.environment.references.get(reference)) {
            return Err(EvaluationError::UnknownReference);
        }
        if self.trace.borrow().failure.is_some() {
            return Err(EvaluationError::UnknownReference);
        }
        self.inner.resolve(reference, path, meter)
    }
    fn outcome(&self, node: &Identifier) -> Option<&EvaluationOutcome> {
        let first = self.trace.borrow_mut().outcomes.insert(node.clone());
        if first && !self.read(self.environment.outcomes.get(node)) {
            return None;
        }
        if self.trace.borrow().failure.is_some() {
            return None;
        }
        self.inner.outcome(node)
    }
    fn template(&self, digest: &Digest) -> Option<&PromptTemplate> {
        self.inner.template(digest)
    }
    fn check_result(
        &self,
        expression: &Expression,
        value: &EvaluationValue,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<(), EvaluationError> {
        self.inner.check_result(expression, value, meter)
    }
    fn project(
        &self,
        origin: &Expression,
        value: &Value,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.inner.project(origin, value, path, meter)
    }
    fn call(
        &self,
        library: Digest,
        name: &Identifier,
        args: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.inner.call(library, name, args, meter)
    }
    fn call_at(
        &self,
        expression: &Expression,
        library: Digest,
        name: &Identifier,
        args: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.inner.call_at(expression, library, name, args, meter)
    }
    fn call_typed(
        &self,
        expression: &Expression,
        boundary: &ExpressionCallType,
        args: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.inner.call_typed(expression, boundary, args, meter)
    }
    fn call_callback(
        &self,
        callback: &ExpressionCallbackType,
        args: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.inner.call_callback(callback, args, meter)
    }
    fn call_callback_typed(
        &self,
        callback: &ExpressionCallbackType,
        boundary: &ExpressionCallType,
        args: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.inner
            .call_callback_typed(callback, boundary, args, meter)
    }
}
