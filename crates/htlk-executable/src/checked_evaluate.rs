//! Static admission connected to actual-value expression enforcement.
use crate::{
    EvaluationArgument, EvaluationContext, EvaluationError as Error, EvaluationMeter,
    EvaluationOutcome, EvaluationResult, EvaluationValue as V, EvaluatorLimits, Expression,
    ExpressionAnalysis, ExpressionContext, ExpressionKind as E, ExpressionTypeEnvironment,
    Identifier, NativeSchemas, PathStep, Port, PromptTemplate, RuntimeTypeCheckKind as Check,
    ValueReference, digest::Digest,
};
use htlk_cbor::{Limits, Value};
use std::collections::BTreeMap;

/// A statically admitted expression with enforced actual-value constraints.
/// Borrows immutable syntax/declarations and rejects unresolved schema projections.
/// Native function and callback execution still require a trusted linked context;
/// this adapter is not whole-graph admission or a native function registry.
pub struct CheckedExpression<'a> {
    expression: &'a Expression,
    context: ExpressionContext,
    environment: &'a ExpressionTypeEnvironment,
    analysis: ExpressionAnalysis,
    nodes: BTreeMap<usize, usize>,
    checks: BTreeMap<usize, Vec<usize>>,
    calls: BTreeMap<usize, usize>,
    limits: Limits,
}
impl<'a> CheckedExpression<'a> {
    /// Analyzes all branches and prepares node-local runtime enforcement.
    ///
    /// # Errors
    /// Returns static typing errors, limits, or unresolved schema projections.
    pub fn new(
        expression: &'a Expression,
        context: ExpressionContext,
        environment: &'a ExpressionTypeEnvironment,
        expected: Option<&Port>,
        limits: &Limits,
    ) -> Result<Self, Error> {
        let analysis = crate::check_expression(expression, context, environment, expected, limits)?;
        let mut nodes = BTreeMap::new();
        let mut checks: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (index, node) in analysis.nodes().iter().enumerate() {
            nodes.insert(key(at(expression, &node.expression_path)?), index);
        }
        for (index, check) in analysis.runtime_checks().iter().enumerate() {
            if matches!(check.kind, Check::SchemaProjection { .. }) {
                return Err(Error::UnresolvedType);
            }
            checks
                .entry(key(at(expression, &check.expression_path)?))
                .or_default()
                .push(index);
        }
        let mut calls = BTreeMap::new();
        for (index, call) in analysis.calls().iter().enumerate() {
            calls.insert(key(at(expression, &call.expression_path)?), index);
        }
        Ok(Self {
            expression,
            context,
            environment,
            analysis,
            nodes,
            checks,
            calls,
            limits: limits.clone(),
        })
    }
    /// The complete static analysis used by this adapter.
    pub fn analysis(&self) -> &ExpressionAnalysis {
        &self.analysis
    }
    /// Evaluates lazily, validating every evaluated result and reference root.
    /// Schema checks use the supplied offline native validators.
    ///
    /// # Errors
    /// Returns evaluation, actual-value type, schema, projection, or limit errors.
    pub fn evaluate(
        &self,
        context: &impl EvaluationContext,
        schemas: Option<&NativeSchemas>,
        policy: &EvaluatorLimits,
    ) -> Result<EvaluationResult, Error> {
        crate::evaluate(
            self.expression,
            self.context,
            &CheckedContext {
                plan: self,
                inner: context,
                schemas,
            },
            &self.limits,
            policy,
        )
    }
}
fn key(expression: &Expression) -> usize {
    std::ptr::from_ref(expression) as usize
}
fn at<'a>(mut expression: &'a Expression, path: &[usize]) -> Result<&'a Expression, Error> {
    for &index in path {
        expression = match expression.kind() {
            E::Not(value) | E::Get { value, .. } if index == 0 => value,
            E::Binary { left, .. } if index == 0 => left,
            E::Binary { right, .. } if index == 1 => right,
            E::Call { arguments, .. } | E::List(arguments) => {
                arguments.get(index).ok_or(Error::UnresolvedType)?
            }
            E::Record(fields) => &fields.get(index).ok_or(Error::UnresolvedType)?.1,
            E::Render {
                arguments: fields, ..
            } => &fields.get(index).ok_or(Error::UnresolvedType)?.1,
            _ => return Err(Error::UnresolvedType),
        };
    }
    Ok(expression)
}
struct CheckedContext<'a, 'b, C> {
    plan: &'a CheckedExpression<'b>,
    inner: &'a C,
    schemas: Option<&'a NativeSchemas>,
}
fn port(
    value: &V,
    port: &Port,
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<(), Error> {
    match value {
        V::Present(value) => crate::runtime_type::require(value, port.value_type(), schemas, meter),
        V::Absent if port.required() => Err(Error::AbsentOperand),
        _ => Ok(()),
    }
}
impl<C: EvaluationContext> EvaluationContext for CheckedContext<'_, '_, C> {
    fn check_result(
        &self,
        expression: &Expression,
        value: &V,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<(), Error> {
        self.inner.check_result(expression, value, meter)?;
        meter.charge(u64::from(
            (usize::BITS - self.plan.nodes.len().leading_zeros()).max(1),
        ))?;
        let index = self
            .plan
            .nodes
            .get(&key(expression))
            .ok_or(Error::UnresolvedType)?;
        let node = &self.plan.analysis.nodes()[*index];
        if !node.callable {
            port(value, &node.port, self.schemas, meter)?;
        }
        if let Some(checks) = self.plan.checks.get(&key(expression)) {
            for index in checks {
                meter.charge(1)?;
                match &self.plan.analysis.runtime_checks()[*index].kind {
                    Check::Present if matches!(value, V::Absent) => {
                        return Err(Error::AbsentOperand);
                    }
                    Check::Value(expected) => port(value, expected, self.schemas, meter)?,
                    Check::SchemaProjection { .. } => return Err(Error::UnresolvedType),
                    // Operators enforce comparison/length; projections use the
                    // declared source types through resolve/project below.
                    _ => (),
                }
            }
        }
        Ok(())
    }
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        let declared = self
            .plan
            .environment
            .references
            .get(reference)
            .ok_or(Error::UnknownReference)?;
        let value = if path.is_empty() {
            self.inner.resolve(reference, &[], meter)?
        } else {
            meter.intermediate(|meter| self.inner.resolve(reference, &[], meter))?
        };
        port(&value, declared, self.schemas, meter)?;
        if path.is_empty() {
            return Ok(value);
        }
        match value {
            V::Present(value) => crate::runtime_type::project(
                &value,
                declared.value_type(),
                path,
                self.schemas,
                meter,
            ),
            V::Absent => Err(Error::AbsentOperand),
            V::Pending => Ok(V::Pending),
        }
    }
    fn project(
        &self,
        origin: &Expression,
        value: &Value,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        let index = self
            .plan
            .nodes
            .get(&key(origin))
            .ok_or(Error::UnresolvedType)?;
        crate::runtime_type::project(
            value,
            self.plan.analysis.nodes()[*index].port.value_type(),
            path,
            self.schemas,
            meter,
        )
    }
    fn outcome(&self, node: &Identifier) -> Option<&EvaluationOutcome> {
        self.inner.outcome(node)
    }
    fn template(&self, digest: &Digest) -> Option<&PromptTemplate> {
        self.plan.environment.templates.get(digest)
    }
    fn call_at(
        &self,
        expression: &Expression,
        library: Digest,
        name: &Identifier,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        meter.charge(u64::from(
            (usize::BITS - self.plan.calls.len().leading_zeros()).max(1),
        ))?;
        let index = self
            .plan
            .calls
            .get(&key(expression))
            .ok_or(Error::UnresolvedType)?;
        let boundary = &self.plan.analysis.calls()[*index];
        if boundary.library != library
            || &boundary.name != name
            || boundary.parameters.len() != arguments.len()
        {
            return Err(Error::UnresolvedType);
        }
        for (argument, expected) in arguments.iter().zip(&boundary.parameters) {
            meter.visit(1)?;
            match (argument, expected.value_type().kind()) {
                (EvaluationArgument::Function { .. }, crate::ValueTypeKind::Function { .. }) => (),
                (EvaluationArgument::Value(V::Pending), _) => return Err(Error::OperandType),
                (EvaluationArgument::Value(value), _) => {
                    port(value, expected, self.schemas, meter)?
                }
                _ => return Err(Error::CallableAsValue),
            }
        }
        let value = self
            .inner
            .call_typed(expression, boundary, arguments, meter)?;
        if matches!(value, V::Pending) {
            return Err(Error::OperandType);
        }
        port(&value, &boundary.returns, self.schemas, meter)?;
        Ok(value)
    }
}
