//! Static admission connected to actual-value expression enforcement.
use crate::{
    EvaluationArgument, EvaluationContext, EvaluationError as Error, EvaluationMeter,
    EvaluationOutcome, EvaluationResult, EvaluationValue as V, EvaluatorLimits, Expression,
    ExpressionAnalysis, ExpressionContext, ExpressionTypeEnvironment, Identifier, NativeSchemas,
    PathStep, Port, PromptTemplate, RuntimeTypeCheckKind as Check, ValueReference, digest::Digest,
};
use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use std::borrow::Cow;
use std::{cell::RefCell, collections::BTreeMap};

/// A statically admitted expression with enforced actual-value constraints.
/// Borrows immutable syntax/declarations and rejects unresolved schema projections.
/// Native function and callback execution still require a trusted linked context;
/// this adapter is not whole-graph admission or a native function registry.
pub struct CheckedExpression<'a> {
    expression: &'a Expression,
    context: ExpressionContext,
    pub(crate) environment: &'a ExpressionTypeEnvironment,
    analysis: Cow<'a, ExpressionAnalysis>,
    limits: Limits,
    pub(crate) schemas: Option<&'a NativeSchemas>,
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
        Self::prepare(expression, context, environment, expected, None, limits)
    }
    /// Admits schema projections against immutable offline validators. The admitted
    /// validators remain pinned for evaluation, including native callback checks.
    ///
    /// # Errors
    /// Returns static typing, missing prescribed schema roots, or resource errors.
    pub fn with_schemas(
        expression: &'a Expression,
        context: ExpressionContext,
        environment: &'a ExpressionTypeEnvironment,
        expected: Option<&Port>,
        schemas: &'a NativeSchemas,
        limits: &Limits,
    ) -> Result<Self, Error> {
        Self::prepare(
            expression,
            context,
            environment,
            expected,
            Some(schemas),
            limits,
        )
    }
    fn prepare(
        expression: &'a Expression,
        context: ExpressionContext,
        environment: &'a ExpressionTypeEnvironment,
        expected: Option<&Port>,
        schemas: Option<&'a NativeSchemas>,
        limits: &Limits,
    ) -> Result<Self, Error> {
        let analysis = match schemas {
            Some(schemas) => crate::check_expression_with_schemas(
                expression,
                context,
                environment,
                expected,
                schemas,
                limits,
            )?,
            None => crate::check_expression(expression, context, environment, expected, limits)?,
        };
        for check in analysis.runtime_checks() {
            if let Check::SchemaProjection { schema, .. } = check.kind
                && !schemas.is_some_and(|schemas| schemas.has_schema_type(&schema))
            {
                return Err(Error::UnresolvedType);
            }
        }
        Ok(Self {
            expression,
            context,
            environment,
            analysis: Cow::Owned(analysis),
            limits: limits.clone(),
            schemas,
        })
    }
    pub(crate) fn borrow_admitted(
        expression: &'a Expression,
        context: ExpressionContext,
        environment: &'a ExpressionTypeEnvironment,
        analysis: &'a ExpressionAnalysis,
        schemas: &'a NativeSchemas,
        limits: &Limits,
    ) -> Self {
        Self {
            expression,
            context,
            environment,
            analysis: Cow::Borrowed(analysis),
            schemas: Some(schemas),
            limits: limits.clone(),
        }
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
        crate::evaluate::evaluate_admitted(
            self.expression,
            &CheckedContext {
                plan: self,
                inner: context,
                schemas: self.schemas.or(schemas),
                trace: RefCell::new(SchemaTrace::default()),
            },
            &self.limits,
            policy,
        )
    }
    /// Context admitted during construction; evaluation cannot change it.
    pub fn context(&self) -> ExpressionContext {
        self.context
    }
}
/// Runtime callback dispatch for analyzer-inferred call signatures.
pub trait CallbackInvocation {
    /// Invokes an admitted callback, allowing statically identified forwarding.
    ///
    /// # Errors
    /// Returns invalid arguments, actual-value failures or evaluator limits.
    fn invoke_callback_with(
        &self,
        index: usize,
        arguments: &[CallbackArgument<'_>],
        context: &(impl EvaluationContext + ?Sized),
        schemas: Option<&NativeSchemas>,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error>;
    /// Invokes an admitted callback with ordinary value arguments.
    ///
    /// # Errors
    /// Returns invalid arguments, actual-value failures or evaluator limits.
    fn invoke_callback(
        &self,
        index: usize,
        arguments: &[V],
        context: &(impl EvaluationContext + ?Sized),
        schemas: Option<&NativeSchemas>,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error>;
}
impl CallbackInvocation for crate::ExpressionCallType {
    /// Invokes a callback with values and/or other already admitted static
    /// callbacks. Function arguments name slots of this enclosing call, never
    /// function identities read from application values. Forwarded signatures are
    /// checked against both actual and receiving higher-order parameter types.
    ///
    /// # Errors
    /// Returns invalid slots/arity/types/presence, pending state, or shared limits.
    fn invoke_callback_with(
        &self,
        index: usize,
        arguments: &[CallbackArgument<'_>],
        context: &(impl EvaluationContext + ?Sized),
        schemas: Option<&NativeSchemas>,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        meter.callback(|meter| {
            let invocation = prepare_forwarded_callback(self, index, arguments, schemas, meter)?;
            let result = context.call_callback_typed(
                invocation.callback,
                &invocation.boundary,
                &invocation.arguments,
                meter,
            )?;
            if matches!(result, V::Pending) {
                return Err(Error::OperandType);
            }
            port(&result, &invocation.boundary.returns, schemas, meter)?;
            port(&result, invocation.expected_return, schemas, meter)?;
            Ok(result)
        })
    }
    /// Invokes an admitted direct callback with ordinary value arguments. Enforces
    /// both the receiving parameter's function type and the callback's actual
    /// instantiated signature, including presence and variance-sensitive results.
    /// Shares the caller's meter and bounds recursive callback depth by both the
    /// expression policy and codec depth ceiling. Callback
    /// intermediates use the value ceiling; the enclosing expression owns the
    /// final output ceiling. Exact native implementation linkage remains the
    /// supplied context's responsibility.
    ///
    /// # Errors
    /// Returns unknown callback/arity, invalid or pending arguments/results,
    /// schema/type errors, or exhausted evaluator limits. This value-argument
    /// entry point does not accept higher-order callable arguments.
    fn invoke_callback(
        &self,
        argument_index: usize,
        arguments: &[V],
        context: &(impl EvaluationContext + ?Sized),
        schemas: Option<&NativeSchemas>,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        meter.callback(|meter| {
            let invocation = prepare_callback(self, argument_index, arguments, schemas, meter)?;
            let result =
                context.call_callback(invocation.callback, &invocation.arguments, meter)?;
            if matches!(result, V::Pending) {
                return Err(Error::OperandType);
            }
            port(&result, invocation.actual_return, schemas, meter)?;
            port(&result, invocation.expected_return, schemas, meter)?;
            Ok(result)
        })
    }
}
/// Arguments to checked callback invocation; callable capabilities stay separate
/// from native application values and refer only to statically admitted slots.
pub enum CallbackArgument<'a> {
    /// Borrowed native value or settled absence; pending is rejected.
    Value(&'a V),
    /// Positional function-reference argument of the enclosing native call.
    Function(usize),
}
struct ForwardedInvocation<'a> {
    callback: &'a crate::ExpressionCallbackType,
    boundary: crate::ExpressionCallType,
    arguments: Vec<EvaluationArgument>,
    expected_return: &'a Port,
}
#[inline(never)]
fn prepare_forwarded_callback<'a>(
    boundary: &'a crate::ExpressionCallType,
    index: usize,
    arguments: &[CallbackArgument<'_>],
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<ForwardedInvocation<'a>, Error> {
    use crate::ValueTypeKind as K;
    meter.visit(1)?;
    let lookup = |index| {
        boundary
            .callbacks
            .binary_search_by_key(&index, |c| c.argument_index)
            .ok()
            .and_then(|i| boundary.callbacks.get(i))
            .ok_or(Error::UnknownFunction)
    };
    let callback = lookup(index)?;
    let expected = boundary
        .parameters
        .get(index)
        .ok_or(Error::UnresolvedType)?;
    for ty in [&callback.signature, expected.value_type()] {
        meter.charge(
            ty.encode(crate::TypeContext::Signature, meter.program_limits())?
                .len() as u64,
        )?;
    }
    let K::Function {
        parameters: actual,
        returns,
    } = callback.signature.kind()
    else {
        return Err(Error::UnresolvedType);
    };
    let K::Function {
        parameters: expected,
        returns: expected_return,
    } = expected.value_type().kind()
    else {
        return Err(Error::UnresolvedType);
    };
    if arguments.len() != actual.len() || arguments.len() != expected.len() {
        return Err(Error::OperandType);
    }
    meter.charge(
        callback.name.as_str().len() as u64
            + (callback.expression_path.len() as u64).saturating_mul(size_of::<usize>() as u64),
    )?;
    let mut call = crate::ExpressionCallType {
        expression_path: callback.expression_path.clone(),
        library: callback.library,
        name: callback.name.clone(),
        parameters: actual.clone(),
        returns: returns.as_ref().clone(),
        callbacks: Vec::new(),
    };
    let mut native = Vec::new();
    for (position, ((argument, actual), expected)) in
        arguments.iter().zip(actual).zip(expected).enumerate()
    {
        meter.visit(1)?;
        let argument = match argument {
            CallbackArgument::Value(value) => {
                if matches!(value, V::Pending) {
                    return Err(Error::OperandType);
                }
                actual.to_value(crate::TypeContext::Value, meter.program_limits())?;
                expected.to_value(crate::TypeContext::Value, meter.program_limits())?;
                port(value, actual, schemas, meter)?;
                port(value, expected, schemas, meter)?;
                EvaluationArgument::Value(match value {
                    V::Present(v) => V::Present(meter.copy_value(v)?),
                    V::Absent => V::Absent,
                    V::Pending => return Err(Error::OperandType),
                })
            }
            CallbackArgument::Function(slot) => {
                if !matches!(actual.value_type().kind(), K::Function { .. })
                    || !matches!(expected.value_type().kind(), K::Function { .. })
                {
                    return Err(Error::CallableAsValue);
                }
                let source = lookup(*slot)?;
                require_callback_compatibility(&source.signature, actual.value_type(), meter)?;
                require_callback_compatibility(&source.signature, expected.value_type(), meter)?;
                meter.charge(
                    (source.name.as_str().len() as u64)
                        .saturating_mul(2)
                        .saturating_add(
                            (source.expression_path.len() as u64)
                                .saturating_mul(size_of::<usize>() as u64),
                        ),
                )?;
                let mut metadata = source.clone();
                metadata.argument_index = position;
                call.callbacks
                    .try_reserve(1)
                    .map_err(|_| Error::AllocationFailed)?;
                call.callbacks.push(metadata);
                EvaluationArgument::Function {
                    library: source.library,
                    name: source.name.clone(),
                }
            }
        };
        native.try_reserve(1).map_err(|_| Error::AllocationFailed)?;
        native.push(argument);
    }
    Ok(ForwardedInvocation {
        callback,
        boundary: call,
        arguments: native,
        expected_return,
    })
}
struct PreparedCallback<'a> {
    callback: &'a crate::ExpressionCallbackType,
    arguments: Vec<EvaluationArgument>,
    actual_return: &'a Port,
    expected_return: &'a Port,
}
// Admission temporaries must not remain on the recursive native invocation stack.
#[inline(never)]
fn prepare_callback<'a>(
    boundary: &'a crate::ExpressionCallType,
    index: usize,
    arguments: &[V],
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<PreparedCallback<'a>, Error> {
    meter.charge(u64::from(
        (usize::BITS - boundary.callbacks.len().leading_zeros()).max(1),
    ))?;
    let callback = boundary
        .callbacks
        .binary_search_by_key(&index, |c| c.argument_index)
        .ok()
        .and_then(|i| boundary.callbacks.get(i))
        .ok_or(Error::UnknownFunction)?;
    let expected = boundary
        .parameters
        .get(index)
        .ok_or(Error::UnresolvedType)?;
    for ty in [&callback.signature, expected.value_type()] {
        let bytes = ty.encode(crate::TypeContext::Signature, meter.program_limits())?;
        meter.charge(bytes.len() as u64)?;
    }
    let crate::ValueTypeKind::Function {
        parameters: actual,
        returns: actual_return,
    } = callback.signature.kind()
    else {
        return Err(Error::UnresolvedType);
    };
    let crate::ValueTypeKind::Function {
        parameters: expected,
        returns: expected_return,
    } = expected.value_type().kind()
    else {
        return Err(Error::UnresolvedType);
    };
    if arguments.len() != actual.len() || arguments.len() != expected.len() {
        return Err(Error::OperandType);
    }
    let mut native = Vec::new();
    for ((value, actual), expected) in arguments.iter().zip(actual).zip(expected) {
        meter.visit(1)?;
        if matches!(value, V::Pending) {
            return Err(Error::OperandType);
        }
        actual.to_value(crate::TypeContext::Value, meter.program_limits())?;
        expected.to_value(crate::TypeContext::Value, meter.program_limits())?;
        port(value, actual, schemas, meter)?;
        port(value, expected, schemas, meter)?;
        native.try_reserve(1).map_err(|_| Error::AllocationFailed)?;
        native.push(EvaluationArgument::Value(match value {
            V::Present(value) => V::Present(meter.copy_value(value)?),
            V::Absent => V::Absent,
            V::Pending => return Err(Error::OperandType),
        }));
    }
    Ok(PreparedCallback {
        callback,
        arguments: native,
        actual_return,
        expected_return,
    })
}
fn require_callback_compatibility(
    actual: &crate::ValueType,
    expected: &crate::ValueType,
    meter: &mut EvaluationMeter<'_>,
) -> Result<(), Error> {
    let report = htlk_analyzer::callback_compatibility(
        actual,
        expected,
        meter.program_limits(),
        meter.remaining_steps(),
    );
    for work in report.input_work() {
        meter.charge(work)?;
    }
    meter.charge(report.analysis_work())?;
    match report.into_result() {
        Ok(()) => Ok(()),
        Err(crate::ExpressionTypeError::UnresolvedGeneric) => Err(Error::UnresolvedType),
        Err(crate::ExpressionTypeError::CallbackMismatch) => Err(Error::OperandType),
        Err(
            crate::ExpressionTypeError::InferenceLimit
            | crate::ExpressionTypeError::LimitExceeded { .. },
        ) => Err(Error::Limit("callback signature work")),
        Err(e) => Err(e.into()),
    }
}
fn lookup_work(meter: &mut EvaluationMeter<'_>, count: usize, extra: usize) -> Result<(), Error> {
    let levels = u64::from((usize::BITS - count.leading_zeros()).max(1));
    meter.charge(
        (meter
            .expression_path()
            .len()
            .saturating_add(extra)
            .saturating_add(1) as u64)
            .saturating_mul(levels),
    )
}
struct CheckedContext<'a, 'b, C> {
    plan: &'a CheckedExpression<'b>,
    inner: &'a C,
    schemas: Option<&'a NativeSchemas>,
    trace: RefCell<SchemaTrace>,
}
#[derive(Default)]
struct SchemaTrace {
    values: BTreeMap<Vec<usize>, (crate::ValueType, Value)>,
    bytes: usize,
}
impl<C: EvaluationContext> CheckedContext<'_, '_, C> {
    fn retain_schema_root(
        &self,
        expression: &Expression,
        node: &crate::ExpressionNodeType,
        value: &V,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<(), Error> {
        use crate::{ExpressionKind as E, ValueTypeKind as K};
        if !self
            .plan
            .analysis
            .retains_schema_root(meter.expression_path())
        {
            return Ok(());
        }
        if matches!(expression.kind(), E::Ref { .. }) {
            return Ok(());
        }
        let ty = node.port.value_type();
        let root = matches!(ty.kind(), K::Schema(_))
            || matches!(ty.kind(),K::Union(members) if members.iter().any(|member|matches!(member.kind(),K::Schema(_))));
        if !root {
            return Ok(());
        }
        let V::Present(value) = value else {
            return Ok(());
        };
        let size = meter.inspect(value)?;
        let metadata = ty
            .encode(crate::TypeContext::Value, meter.program_limits())?
            .len()
            .saturating_add(size_of_val(meter.expression_path()));
        meter.charge(metadata as u64)?;
        let mut trace = self.trace.borrow_mut();
        let bytes = trace
            .bytes
            .checked_add(size)
            .and_then(|n| n.checked_add(metadata))
            .ok_or(Error::Limit("schema provenance"))?;
        if bytes > meter.program_limits().max_document_bytes {
            return Err(Error::Limit("schema provenance"));
        }
        trace.values.insert(
            meter.expression_path().to_vec(),
            (ty.clone(), value.clone()),
        );
        trace.bytes = bytes;
        Ok(())
    }
    fn schema_origin(
        &self,
        expression: &Expression,
        position: &mut Vec<usize>,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<Option<V>, Error> {
        use crate::ExpressionKind as E;
        meter.charge(
            (position.len().saturating_add(1) as u64).saturating_mul(u64::from(
                (usize::BITS - self.plan.analysis.nodes().len().leading_zeros()).max(1),
            )),
        )?;
        let index = self
            .plan
            .analysis
            .nodes()
            .binary_search_by(|node| node.expression_path.cmp(position))
            .map_err(|_| Error::UnresolvedType)?;
        if !self.plan.analysis.nodes()[index].schema_derived {
            return Ok(None);
        }
        if let Some((ty, value)) = self.trace.borrow().values.get(position.as_slice()) {
            return crate::runtime_type::project(value, ty, path, self.schemas, meter).map(Some);
        }
        match expression.kind() {
            E::Ref {
                source,
                path: prefix,
            } => {
                let path = joined_path(prefix, path, meter)?;
                self.resolve(source, &path, meter).map(Some)
            }
            E::Get {
                value,
                path: prefix,
            } => {
                let path = joined_path(prefix, path, meter)?;
                position.push(0);
                let result = self.schema_origin(value, position, &path, meter);
                position.pop();
                result
            }
            E::Record(fields) => {
                let Some((PathStep::Field(name), rest)) = path.split_first() else {
                    return Ok(None);
                };
                meter.charge((name.len() as u64).saturating_mul(u64::from(
                    (usize::BITS - fields.len().leading_zeros()).max(1),
                )))?;
                let Ok(index) = fields.binary_search_by(|(key, _)| key.cmp(name)) else {
                    return Ok(None);
                };
                position.push(index);
                let result = self.schema_origin(&fields[index].1, position, rest, meter);
                position.pop();
                result
            }
            E::List(items) => {
                let Some((PathStep::Index(index), rest)) = path.split_first() else {
                    return Ok(None);
                };
                let Some((index, value)) = usize::try_from(*index)
                    .ok()
                    .and_then(|i| items.get(i).map(|value| (i, value)))
                else {
                    return Ok(None);
                };
                position.push(index);
                let result = self.schema_origin(value, position, rest, meter);
                position.pop();
                result
            }
            _ => Ok(None),
        }
    }
}
fn joined_path(
    first: &[PathStep],
    second: &[PathStep],
    meter: &mut EvaluationMeter<'_>,
) -> Result<Vec<PathStep>, Error> {
    let count = first
        .len()
        .checked_add(second.len())
        .ok_or(Error::Limit("schema projection path"))?;
    meter.visit(count as u64)?;
    let mut size = count.saturating_mul(size_of::<PathStep>());
    for step in first.iter().chain(second) {
        if let PathStep::Field(name) = step {
            size = size
                .checked_add(name.len())
                .ok_or(Error::Limit("schema projection path"))?;
        }
    }
    if size > meter.program_limits().max_document_bytes {
        return Err(Error::Limit("schema projection path"));
    }
    meter.charge(size as u64)?;
    let mut path = Vec::new();
    path.try_reserve_exact(count)
        .map_err(|_| Error::AllocationFailed)?;
    path.extend_from_slice(first);
    path.extend_from_slice(second);
    Ok(path)
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
        lookup_work(meter, self.plan.analysis.nodes().len(), 0)?;
        let index = self
            .plan
            .analysis
            .nodes()
            .binary_search_by(|node| node.expression_path.as_slice().cmp(meter.expression_path()))
            .map_err(|_| Error::UnresolvedType)?;
        let node = &self.plan.analysis.nodes()[index];
        if !node.callable {
            port(value, &node.port, self.schemas, meter)?;
        }
        if node.expression_path.is_empty()
            && let Some(boundary) = self.plan.analysis.boundary()
        {
            port(value, boundary, self.schemas, meter)?;
        }
        lookup_work(meter, self.plan.analysis.runtime_checks().len(), 0)?;
        for check in self.plan.analysis.checks_at(meter.expression_path()) {
            meter.charge(1)?;
            match &check.kind {
                Check::Present if matches!(value, V::Absent) => {
                    return Err(Error::AbsentOperand);
                }
                Check::Value(expected) => port(value, expected, self.schemas, meter)?,
                Check::SchemaProjection { schema, .. }
                    if !self
                        .schemas
                        .is_some_and(|schemas| schemas.has_schema_type(schema)) =>
                {
                    return Err(Error::UnresolvedType);
                }
                // Operators enforce comparison/length; projections use the
                // declared source types through resolve/project below.
                _ => (),
            }
        }
        if self
            .plan
            .analysis
            .retains_schema_root(meter.expression_path())
        {
            self.retain_schema_root(expression, node, value, meter)?;
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
        lookup_work(meter, self.plan.analysis.nodes().len(), 1)?;
        let mut child = meter.expression_path().to_vec();
        child.push(0);
        if let Some(result) = self.schema_origin(origin, &mut child, path, meter)? {
            return Ok(result);
        }
        let index = self
            .plan
            .analysis
            .nodes()
            .binary_search_by(|node| node.expression_path.cmp(&child))
            .map_err(|_| Error::UnresolvedType)?;
        crate::runtime_type::project(
            value,
            self.plan.analysis.nodes()[index].port.value_type(),
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
        lookup_work(meter, self.plan.analysis.calls().len(), 0)?;
        let index = self
            .plan
            .analysis
            .calls()
            .binary_search_by(|call| call.expression_path.as_slice().cmp(meter.expression_path()))
            .map_err(|_| Error::UnresolvedType)?;
        let boundary = &self.plan.analysis.calls()[index];
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_input_budget_failures_preserve_ordered_meter_charges() {
        let limits = Limits::default();
        let port = Port::new(
            crate::ValueType::primitive(crate::PrimitiveType::Boolean),
            true,
        );
        let signature = crate::ValueType::new(
            crate::ValueTypeKind::Function {
                parameters: vec![port.clone()],
                returns: Box::new(port),
            },
            crate::TypeContext::Signature,
            &limits,
        )
        .unwrap();
        let cost = signature
            .encode(crate::TypeContext::Signature, &limits)
            .unwrap()
            .len() as u64;
        for inputs in [1, 2] {
            let policy = EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 1024,
                max_collection_visits: 1024,
                max_regex_bytes: 1024,
                max_regex_compiled_bytes: 1024,
                max_output_bytes: 1024,
                max_steps: inputs * cost - 1,
            };
            let mut meter = EvaluationMeter::new(&policy, &limits).unwrap();
            assert_eq!(
                require_callback_compatibility(&signature, &signature, &mut meter),
                Err(Error::Limit("steps"))
            );
            assert_eq!(meter.usage().steps, inputs * cost);
        }
    }
}
