//! Native evaluation of canonical expressions, with no embedded language runtime.

use crate::digest::Digest;
use crate::{
    BinaryOperator as B, CoreFunction, EvaluatorLimits, Expression, ExpressionContext,
    ExpressionError, ExpressionKind as E, Identifier, PathStep, PrimitiveType, PromptTemplate,
    ScalarLiteral as L, TemplatePart, ValueReference, ValueTypeKind,
};
use htlk_cbor::{Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use std::{cmp::Ordering, collections::BTreeMap, fmt};

/// A legitimate result or an execution state. Unavailable sources are errors,
/// never absence; pending is never handed to a native library as ordinary data.
#[derive(Clone, Debug, PartialEq)]
pub enum EvaluationValue {
    /// Present native value, including a present null.
    Present(Value),
    /// Legitimate settled absence.
    Absent,
    /// A required dependency has not settled.
    Pending,
}
/// Static callable metadata can occur only as a direct native-call argument.
#[derive(Clone, Debug, PartialEq)]
pub enum EvaluationArgument {
    /// Present value or legitimate absence; never pending.
    Value(EvaluationValue),
    /// Library callable metadata, not an application value.
    Function {
        /// Exact library implementation identity.
        library: Digest,
        /// Local function name.
        name: Identifier,
    },
}
/// A child's immutable outcome as observed by one evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluationOutcome {
    /// Not yet terminal.
    Pending,
    /// Successful completion.
    Succeeded,
    /// Failed with the published redacted error record.
    Failed {
        /// Stable error code.
        code: String,
        /// Published message.
        message: String,
    },
    /// Skipped by admission/binding rules.
    Skipped,
    /// Cancelled terminally.
    Cancelled,
}

/// Runtime data and linked Rust functions consumed by the evaluator. Implementors
/// must preserve verified scope/type/projection rules and charge native work via
/// the supplied meter. This is a context interface, not a replacement evaluator.
pub trait EvaluationContext {
    /// Checks an evaluated node against a verified type/boundary plan. Raw contexts
    /// have no plan; checked contexts override this without changing evaluation order.
    fn check_result(
        &self,
        _expression: &Expression,
        _value: &EvaluationValue,
        _meter: &mut EvaluationMeter<'_>,
    ) -> Result<(), EvaluationError> {
        Ok(())
    }
    /// Resolves a reference and its verified projection, charging copies/work.
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError>;
    /// Borrows a child's frozen outcome.
    fn outcome(&self, node: &Identifier) -> Option<&EvaluationOutcome>;
    /// Borrows a content-addressed prompt template.
    fn template(&self, digest: &Digest) -> Option<&PromptTemplate>;
    /// Invokes an exactly linked native function. Call-site typing, optional
    /// parameters, and callback compatibility must be checked by the registry.
    fn call(
        &self,
        _library: Digest,
        _name: &Identifier,
        _arguments: &[EvaluationArgument],
        _meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        Err(EvaluationError::UnknownFunction)
    }
    /// Call-site-aware dispatch for contexts enforcing instantiated signatures.
    fn call_at(
        &self,
        _expression: &Expression,
        library: Digest,
        name: &Identifier,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.call(library, name, arguments, meter)
    }
    /// Dispatches an admitted native call with concrete parameter/result constraints,
    /// including instantiated callback signatures. Checked evaluation validates
    /// ordinary arguments before dispatch and the result afterward. The native
    /// implementation remains responsible for callback invocations.
    fn call_typed(
        &self,
        expression: &Expression,
        boundary: &crate::ExpressionCallType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.call_at(
            expression,
            boundary.library,
            &boundary.name,
            arguments,
            meter,
        )
    }
    /// Dispatches a checked callback by its admitted identity and actual signature.
    /// Called by ExpressionCallType::invoke_callback after argument validation.
    fn call_callback(
        &self,
        callback: &crate::ExpressionCallbackType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.call(callback.library, &callback.name, arguments, meter)
    }
    /// Dispatches a checked callback with its concrete forwarding boundary. Hosts
    /// implementing higher-order callbacks retain this boundary for nested calls.
    fn call_callback_typed(
        &self,
        callback: &crate::ExpressionCallbackType,
        _boundary: &crate::ExpressionCallType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.call_callback(callback, arguments, meter)
    }
    /// Projects a computed value. The default supports literal record/list
    /// declarations and strict dynamic lookup. A verified execution context may
    /// use richer projection plans for library/schema-constrained results.
    fn project(
        &self,
        origin: &Expression,
        value: &Value,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        project(value, path, Some(origin), meter)
    }
}

/// A native context for frozen values, projected bindings, outcomes and templates.
/// Exact projected bindings represent verified optional-field results; fallback
/// map lookup is strict and never guesses that an undeclared field is optional.
#[derive(Default)]
pub struct EvaluationFrame {
    values:
        BTreeMap<ValueReference, BTreeMap<Vec<PathStep>, Result<EvaluationValue, EvaluationError>>>,
    outcomes: BTreeMap<Identifier, EvaluationOutcome>,
    templates: BTreeMap<Digest, PromptTemplate>,
}
impl EvaluationFrame {
    /// Binds a frozen reference or an explicitly resolved projected path.
    ///
    /// # Errors
    /// Returns invalid reference/path/value representation or codec limits.
    /// Graph types and producer trust are validated by the surrounding verifier.
    pub fn bind(
        &mut self,
        reference: ValueReference,
        path: Vec<PathStep>,
        value: Result<EvaluationValue, EvaluationError>,
        limits: &Limits,
    ) -> Result<(), EvaluationError> {
        let expr = Expression::new(
            E::Ref {
                source: reference,
                path,
            },
            ExpressionContext::LoopUntil,
            limits,
        )?;
        let E::Ref { source, path } = expr.kind() else {
            return Err(EvaluationError::InvalidProjection);
        };
        if let Ok(EvaluationValue::Present(v)) = &value {
            htlk_cbor::encode(v, limits)?;
        }
        self.values
            .entry(source.clone())
            .or_default()
            .insert(path.clone(), value);
        Ok(())
    }
    /// Sets a child's outcome; error text is bounded before later evaluation copies.
    ///
    /// # Errors
    /// Returns codec limits on published error text.
    pub fn set_outcome(
        &mut self,
        node: Identifier,
        outcome: EvaluationOutcome,
        limits: &Limits,
    ) -> Result<(), EvaluationError> {
        if let EvaluationOutcome::Failed { code, message } = &outcome {
            let mut accounting = crate::record_accounting::RecordAccounting::new(limits)?;
            accounting
                .collection(2, 0)
                .map_err(|_| EvaluationError::Limit("outcome metadata"))?;
            for (text, depth) in [
                ("code", 1),
                (code.as_str(), 1),
                ("message", 1),
                (message.as_str(), 1),
            ] {
                accounting
                    .text(text, depth)
                    .map_err(|_| EvaluationError::Limit("outcome metadata"))?;
            }
        }
        self.outcomes.insert(node, outcome);
        Ok(())
    }
    /// Installs an immutable template under its checked content digest.
    ///
    /// # Errors
    /// Returns template/codec limits.
    pub fn insert_template(
        &mut self,
        template: PromptTemplate,
        limits: &Limits,
    ) -> Result<Digest, EvaluationError> {
        let digest = template.digest(limits)?;
        htlk_analyzer::check_prompt_template(&template, limits)?;
        self.templates.insert(digest, template);
        Ok(digest)
    }
}
impl EvaluationContext for EvaluationFrame {
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        meter.visit(1)?;
        let reference_bytes = match reference {
            ValueReference::Output { node, port } => node.as_str().len() + port.as_str().len(),
            ValueReference::Input(n)
            | ValueReference::ScopeOutput(n)
            | ValueReference::Carried(n)
            | ValueReference::Next(n) => n.as_str().len(),
        };
        let levels = usize::BITS - self.values.len().leading_zeros();
        meter.charge((reference_bytes as u64).saturating_mul(u64::from(levels.max(1))))?;
        let values = self
            .values
            .get(reference)
            .ok_or(EvaluationError::UnknownReference)?;
        let levels = usize::BITS - values.len().leading_zeros();
        for step in path {
            let bytes = match step {
                PathStep::Field(s) => s.len(),
                PathStep::Index(_) => 8,
            };
            meter.charge((bytes as u64).saturating_mul(u64::from(levels.max(1))))?;
        }
        if let Some(state) = values.get(path) {
            return copy_state(state.as_ref().map_err(Clone::clone)?, meter);
        }
        let state = values
            .get(&[][..])
            .ok_or(EvaluationError::UnknownReference)?
            .as_ref()
            .map_err(Clone::clone)?;
        match state {
            EvaluationValue::Present(value) => {
                meter.inspect(value)?;
                project(value, path, None, meter)
            }
            EvaluationValue::Pending => Ok(EvaluationValue::Pending),
            EvaluationValue::Absent => Err(EvaluationError::AbsentOperand),
        }
    }
    fn outcome(&self, node: &Identifier) -> Option<&EvaluationOutcome> {
        self.outcomes.get(node)
    }
    fn template(&self, digest: &Digest) -> Option<&PromptTemplate> {
        self.templates.get(digest)
    }
}

/// Per-evaluation usage, separate from immutable policy configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvaluationUsage {
    /// Logical evaluator/native-function work charged.
    pub steps: u64,
    /// Collection elements/bindings/path steps inspected.
    pub collection_visits: u64,
    /// Encoded native input/intermediate bytes inspected.
    pub value_bytes: u64,
}
/// Ephemeral meter passed to trusted native operations. Each evaluate call starts
/// fresh accounting; limits remain immutable and exhaustion returns an error.
pub struct EvaluationMeter<'a> {
    policy: &'a EvaluatorLimits,
    codec: Limits,
    program_codec: Limits,
    usage: EvaluationUsage,
    depth: u64,
    native_depth: u64,
    expression_path: Vec<usize>,
}
impl<'a> EvaluationMeter<'a> {
    pub(crate) fn new(
        policy: &'a EvaluatorLimits,
        codec: &Limits,
    ) -> Result<Self, EvaluationError> {
        codec.validate()?;
        if [
            policy.max_expression_depth,
            policy.max_value_bytes,
            policy.max_collection_visits,
            policy.max_regex_bytes,
            policy.max_regex_compiled_bytes,
            policy.max_output_bytes,
            policy.max_steps,
        ]
        .contains(&0)
        {
            return Err(EvaluationError::InvalidLimits);
        }
        let mut limits = codec.clone();
        limits.max_document_bytes = limits
            .max_document_bytes
            .min(usize::try_from(policy.max_value_bytes).unwrap_or(usize::MAX));
        Ok(Self {
            policy,
            codec: limits,
            program_codec: codec.clone(),
            usage: EvaluationUsage::default(),
            depth: 0,
            native_depth: 0,
            expression_path: Vec::new(),
        })
    }
    pub(crate) fn usage(&self) -> EvaluationUsage {
        self.usage
    }
    pub(crate) fn remaining_steps(&self) -> u64 {
        self.policy.max_steps.saturating_sub(self.usage.steps)
    }
    /// Current canonical child-index path during AST execution. Native callback
    /// origins are supplied separately through their admitted call metadata.
    pub fn expression_path(&self) -> &[usize] {
        &self.expression_path
    }
    pub(crate) fn program_limits(&self) -> &Limits {
        &self.program_codec
    }
    pub(crate) fn intermediate<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, EvaluationError>,
    ) -> Result<T, EvaluationError> {
        let depth = self.depth;
        self.depth = self.depth.max(2);
        let result = operation(self);
        self.depth = depth;
        result
    }
    pub(crate) fn callback<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, EvaluationError>,
    ) -> Result<T, EvaluationError> {
        self.charge(1)?;
        let depth = self
            .native_depth
            .checked_add(1)
            .ok_or(EvaluationError::Limit("callback depth"))?;
        let maximum = self
            .policy
            .max_expression_depth
            .min(self.program_codec.max_depth as u64);
        if self.depth.checked_add(depth).is_none_or(|n| n > maximum) {
            return Err(EvaluationError::Limit("callback depth"));
        }
        self.native_depth = depth;
        let result = self.intermediate(operation);
        self.native_depth -= 1;
        result
    }
    /// Charges deterministic work before a native operation performs it.
    ///
    /// # Errors
    /// Returns E_EXPRESSION_LIMIT on exhaustion or arithmetic overflow.
    pub fn charge(&mut self, count: u64) -> Result<(), EvaluationError> {
        self.usage.steps = self
            .usage
            .steps
            .checked_add(count)
            .ok_or(EvaluationError::Limit("steps"))?;
        if self.usage.steps > self.policy.max_steps {
            return Err(EvaluationError::Limit("steps"));
        }
        Ok(())
    }
    /// Charges collection traversal plus its logical work.
    ///
    /// # Errors
    /// Returns E_EXPRESSION_LIMIT when either ceiling is exceeded.
    pub fn visit(&mut self, count: u64) -> Result<(), EvaluationError> {
        self.usage.collection_visits = self
            .usage
            .collection_visits
            .checked_add(count)
            .ok_or(EvaluationError::Limit("collection visits"))?;
        if self.usage.collection_visits > self.policy.max_collection_visits {
            return Err(EvaluationError::Limit("collection visits"));
        }
        self.charge(count)
    }
    /// Borrows codec limits tightened to the policy's maximum value size.
    pub fn codec_limits(&self) -> &Limits {
        &self.codec
    }
    /// Bounds and charges a value before copying it into evaluator storage.
    ///
    /// # Errors
    /// Returns codec/value/work limit failures.
    pub fn copy_value(&mut self, value: &Value) -> Result<Value, EvaluationError> {
        let size = self.inspect(value)?;
        self.size(size)?;
        Ok(value.clone())
    }
    pub(crate) fn inspect(&mut self, value: &Value) -> Result<usize, EvaluationError> {
        let size = htlk_cbor::encode(value, &self.codec)?.len();
        if size as u64 > self.policy.max_value_bytes {
            return Err(EvaluationError::Limit("value bytes"));
        }
        self.usage.value_bytes = self
            .usage
            .value_bytes
            .checked_add(size as u64)
            .ok_or(EvaluationError::Limit("value bytes"))?;
        self.charge(size as u64)?;
        Ok(size)
    }
    fn size(&self, size: usize) -> Result<(), EvaluationError> {
        if size as u64 > self.policy.max_value_bytes {
            return Err(EvaluationError::Limit("value bytes"));
        }
        if self.depth <= 1 && size as u64 > self.policy.max_output_bytes {
            return Err(EvaluationError::Limit("output bytes"));
        }
        Ok(())
    }
    fn text(&mut self, s: &str) -> Result<String, EvaluationError> {
        self.size(
            s.len()
                .checked_add(header(s.len()))
                .ok_or(EvaluationError::Limit("value bytes"))?,
        )?;
        self.charge(s.len() as u64)?;
        owned(s)
    }
}
/// Result plus the deterministic work used by a successful evaluation attempt.
#[derive(Clone, Debug, PartialEq)]
pub struct EvaluationResult {
    /// Value, legitimate absence, or pending.
    pub value: EvaluationValue,
    /// Work used in this attempt.
    pub usage: EvaluationUsage,
}
/// A Boolean condition result or an unresolved dependency, never truthiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionValue {
    /// An actual Boolean result. The coordinator interprets false by condition site.
    Ready(bool),
    /// Evaluation awaits a dependency.
    Pending,
}
/// Boolean-condition outcome with the work used by this attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConditionResult {
    /// Strict Boolean or pending.
    pub value: ConditionValue,
    /// Deterministic evaluator usage.
    pub usage: EvaluationUsage,
}
/// Evaluates a guard/contract/until expression and requires an actual Boolean.
/// The coordinator owns skip/contract-failure handling and rejects pending at
/// boundaries where all inputs/body outcomes must already have settled.
///
/// # Errors
/// Returns evaluation errors, absence, or a non-Boolean result.
pub fn evaluate_condition(
    expression: &Expression,
    context: ExpressionContext,
    environment: &impl EvaluationContext,
    codec: &Limits,
    policy: &EvaluatorLimits,
) -> Result<ConditionResult, EvaluationError> {
    let result = evaluate(expression, context, environment, codec, policy)?;
    let value = match result.value {
        EvaluationValue::Present(Value::Bool(value)) => ConditionValue::Ready(value),
        EvaluationValue::Pending => ConditionValue::Pending,
        EvaluationValue::Absent => return Err(EvaluationError::AbsentOperand),
        _ => return Err(EvaluationError::OperandType),
    };
    Ok(ConditionResult {
        value,
        usage: result.usage,
    })
}

/// Executes an expression using native Rust. The caller chooses its schema-owned
/// context and supplies frozen data/linked functions. This does not replace static
/// graph/type verification or give expressions ambient capabilities.
///
/// # Errors
/// Returns context, operand, lookup, regex, native-call, or limit failures.
pub fn evaluate(
    expression: &Expression,
    context: ExpressionContext,
    environment: &impl EvaluationContext,
    codec: &Limits,
    policy: &EvaluatorLimits,
) -> Result<EvaluationResult, EvaluationError> {
    let mut meter = EvaluationMeter::new(policy, codec)?;
    htlk_analyzer::check_expression_context(expression, context, codec).map_err(|diagnostic| {
        match diagnostic.error {
            crate::ExpressionTypeError::Expression(e) => EvaluationError::from(e),
            e => EvaluationError::from(e),
        }
    })?;
    let value = run(expression, environment, &mut meter, 1, None)?;
    Ok(EvaluationResult {
        value,
        usage: meter.usage,
    })
}
pub(crate) fn evaluate_admitted(
    expression: &Expression,
    environment: &impl EvaluationContext,
    codec: &Limits,
    policy: &EvaluatorLimits,
) -> Result<EvaluationResult, EvaluationError> {
    let mut meter = EvaluationMeter::new(policy, codec)?;
    let value = run(expression, environment, &mut meter, 1, None)?;
    Ok(EvaluationResult {
        value,
        usage: meter.usage,
    })
}
fn run(
    expr: &Expression,
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
    child: Option<usize>,
) -> Result<EvaluationValue, EvaluationError> {
    if depth > meter.policy.max_expression_depth {
        return Err(EvaluationError::Limit("expression depth"));
    }
    meter.charge(1)?;
    if let Some(index) = child {
        meter.expression_path.try_reserve(1).map_err(allocation)?;
        meter.expression_path.push(index);
    }
    let previous = meter.depth;
    meter.depth = depth;
    let result = run_inner(expr, env, meter, depth).and_then(|value| {
        if let EvaluationValue::Present(v) = &value {
            let size = meter.inspect(v)?;
            meter.size(size)?;
        }
        env.check_result(expr, &value, meter)?;
        Ok(value)
    });
    meter.depth = previous;
    if child.is_some() {
        meter.expression_path.pop();
    }
    result
}
fn run_inner(
    expr: &Expression,
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
) -> Result<EvaluationValue, EvaluationError> {
    match expr.kind() {
        E::Literal(literal) => {
            let value = match literal {
                L::String(s) => Value::Text(meter.text(s)?),
                L::Bytes(b) => {
                    meter.size(b.len() + header(b.len()))?;
                    meter.charge(b.len() as u64)?;
                    Value::Bytes(b.clone())
                }
                L::Integer(n) => Value::Integer(*n),
                L::Float(n) => Value::Float(*n),
                L::Boolean(v) => Value::Bool(*v),
                L::Null => Value::Null,
            };
            Ok(EvaluationValue::Present(value))
        }
        E::Ref { source, path } => env.resolve(source, path, meter),
        E::Get { value, path } => match run(value, env, meter, depth + 1, Some(0))? {
            EvaluationValue::Present(result) => env.project(value, &result, path, meter),
            EvaluationValue::Pending => Ok(EvaluationValue::Pending),
            EvaluationValue::Absent => Err(EvaluationError::AbsentOperand),
        },
        E::List(values) => sequence(values, env, meter, depth),
        E::Record(fields) => record(fields, env, meter, depth),
        E::Binary {
            operator,
            left,
            right,
        } => binary(*operator, left, right, env, meter, depth),
        E::Not(value) => match run(value, env, meter, depth + 1, Some(0))? {
            EvaluationValue::Present(Value::Bool(v)) => {
                Ok(EvaluationValue::Present(Value::Bool(!v)))
            }
            EvaluationValue::Pending => Ok(EvaluationValue::Pending),
            EvaluationValue::Absent => Err(EvaluationError::AbsentOperand),
            _ => Err(EvaluationError::OperandType),
        },
        E::Call {
            function,
            arguments,
        } => call(expr, function, arguments, env, meter, depth),
        E::FunctionRef { .. } => Err(EvaluationError::CallableAsValue),
        E::Render {
            template,
            arguments,
        } => render(template, arguments, env, meter, depth),
        E::Status(node) | E::Error(node) => {
            outcome(node, matches!(expr.kind(), E::Error(_)), env, meter)
        }
        E::Regex { pattern, flags } => regex_literal(pattern, flags, meter),
    }
}
#[inline(never)]
fn regex_literal(
    pattern: &str,
    flags: &str,
    meter: &mut EvaluationMeter<'_>,
) -> Result<EvaluationValue, EvaluationError> {
    meter.size(
        1 + 8 + header(pattern.len()) + pattern.len() + 6 + header(flags.len()) + flags.len(),
    )?;
    compile_regex(pattern, flags, meter)?;
    let value = Value::Map(Map::try_from_entries([
        ("pattern".into(), Value::Text(meter.text(pattern)?)),
        ("flags".into(), Value::Text(meter.text(flags)?)),
    ])?);
    meter.inspect(&value)?;
    Ok(EvaluationValue::Present(value))
}
pub(crate) fn compile_regex(
    pattern: &str,
    flags: &str,
    meter: &mut EvaluationMeter<'_>,
) -> Result<(), EvaluationError> {
    if pattern.len() as u64 > meter.policy.max_regex_bytes {
        return Err(EvaluationError::Limit("regex bytes"));
    }
    if !matches!(flags, "" | "i" | "m" | "s" | "im" | "is" | "ms" | "ims") {
        return Err(EvaluationError::Regex);
    }
    meter.charge(pattern.len() as u64)?;
    let compiled_limit =
        usize::try_from(meter.policy.max_regex_compiled_bytes).unwrap_or(usize::MAX);
    // Reserve the configured compiler-size allowance before native work;
    // this deterministic upper-bound charge is independent of caches.
    meter.charge(meter.policy.max_regex_compiled_bytes)?;
    regex::RegexBuilder::new(pattern)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .size_limit(compiled_limit)
        .dfa_size_limit(compiled_limit)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => EvaluationError::Limit("regex compiled bytes"),
            _ => EvaluationError::Regex,
        })?;
    Ok(())
}
#[inline(never)]
fn sequence(
    values: &[Expression],
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
) -> Result<EvaluationValue, EvaluationError> {
    let mut output = Vec::new();
    let mut size = header(values.len());
    meter.size(size)?;
    for (index, expr) in values.iter().enumerate() {
        meter.visit(1)?;
        match run(expr, env, meter, depth + 1, Some(index))? {
            EvaluationValue::Present(value) => {
                size = size
                    .checked_add(meter.inspect(&value)?)
                    .ok_or(EvaluationError::Limit("output bytes"))?;
                meter.size(size)?;
                output.try_reserve(1).map_err(allocation)?;
                output.push(value);
            }
            EvaluationValue::Pending => return Ok(EvaluationValue::Pending),
            EvaluationValue::Absent => return Err(EvaluationError::AbsentOperand),
        }
    }
    Ok(EvaluationValue::Present(Value::Array(output)))
}
#[inline(never)]
fn record(
    fields: &[(String, Expression)],
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
) -> Result<EvaluationValue, EvaluationError> {
    let mut output = Vec::new();
    let mut payload = 0usize;
    for (index, (key, expr)) in fields.iter().enumerate() {
        meter.visit(1)?;
        match run(expr, env, meter, depth + 1, Some(index))? {
            EvaluationValue::Present(value) => {
                for n in [key.len(), header(key.len()), meter.inspect(&value)?] {
                    payload = payload
                        .checked_add(n)
                        .ok_or(EvaluationError::Limit("output bytes"))?;
                }
                meter.size(
                    payload
                        .checked_add(header(output.len() + 1))
                        .ok_or(EvaluationError::Limit("output bytes"))?,
                )?;
                output.try_reserve(1).map_err(allocation)?;
                output.push((meter.text(key)?, value));
            }
            EvaluationValue::Pending => return Ok(EvaluationValue::Pending),
            EvaluationValue::Absent => (),
        }
    }
    Ok(EvaluationValue::Present(Value::Map(Map::try_from_entries(
        output,
    )?)))
}
#[inline(never)]
fn binary(
    op: B,
    left: &Expression,
    right: &Expression,
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
) -> Result<EvaluationValue, EvaluationError> {
    let left = match run(left, env, meter, depth + 1, Some(0))? {
        EvaluationValue::Present(v) => v,
        EvaluationValue::Pending => return Ok(EvaluationValue::Pending),
        EvaluationValue::Absent => return Err(EvaluationError::AbsentOperand),
    };
    if matches!(op, B::And | B::Or) {
        let Value::Bool(value) = left else {
            return Err(EvaluationError::OperandType);
        };
        if op == B::And && !value || op == B::Or && value {
            return Ok(EvaluationValue::Present(Value::Bool(value)));
        }
    }
    let right = match run(right, env, meter, depth + 1, Some(1))? {
        EvaluationValue::Present(v) => v,
        EvaluationValue::Pending => return Ok(EvaluationValue::Pending),
        EvaluationValue::Absent => return Err(EvaluationError::AbsentOperand),
    };
    let equal_op = matches!(op, B::Eq | B::Ne);
    let order = match (&left, &right) {
        (Value::Null, Value::Null) if equal_op => Some(Ordering::Equal),
        (Value::Null, _) | (_, Value::Null) if equal_op => None,
        (Value::Text(a), Value::Text(b)) => {
            meter.charge(a.len().min(b.len()) as u64)?;
            Some(a.cmp(b))
        }
        (Value::Integer(a), Value::Integer(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.get().partial_cmp(&b.get()),
        (Value::Bool(a), Value::Bool(b)) if matches!(op, B::And | B::Or | B::Eq | B::Ne) => {
            Some(a.cmp(b))
        }
        _ => return Err(EvaluationError::OperandType),
    };
    let result = match op {
        B::And | B::Or => {
            let Value::Bool(v) = right else {
                return Err(EvaluationError::OperandType);
            };
            v
        }
        B::Eq => order == Some(Ordering::Equal),
        B::Ne => order != Some(Ordering::Equal),
        B::Lt => order == Some(Ordering::Less),
        B::Le => matches!(order, Some(Ordering::Less | Ordering::Equal)),
        B::Gt => order == Some(Ordering::Greater),
        B::Ge => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
    };
    Ok(EvaluationValue::Present(Value::Bool(result)))
}
#[inline(never)]
fn call(
    origin: &Expression,
    function: &crate::FunctionId,
    expressions: &[Expression],
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
) -> Result<EvaluationValue, EvaluationError> {
    if let crate::FunctionId::Core(core) = function {
        let value = run(&expressions[0], env, meter, depth + 1, Some(0))?;
        if value == EvaluationValue::Pending {
            return Ok(value);
        }
        if *core == CoreFunction::Present {
            return Ok(EvaluationValue::Present(Value::Bool(matches!(
                value,
                EvaluationValue::Present(_)
            ))));
        }
        let EvaluationValue::Present(value) = value else {
            return Err(EvaluationError::AbsentOperand);
        };
        let count = match &value {
            Value::Text(s) => {
                meter.charge(s.len() as u64)?;
                s.chars().count()
            }
            Value::Bytes(b) => b.len(),
            Value::Array(v) => v.len(),
            Value::Map(m) => m.len(),
            _ => return Err(EvaluationError::OperandType),
        };
        return Ok(EvaluationValue::Present(Value::Integer(
            i64::try_from(count).map_err(|_| EvaluationError::Limit("length"))?,
        )));
    }
    let crate::FunctionId::Library { library, name } = function else {
        return Err(EvaluationError::UnknownFunction);
    };
    let mut args = Vec::new();
    for (index, expr) in expressions.iter().enumerate() {
        meter.visit(1)?;
        let arg = if let E::FunctionRef { library, name } = expr.kind() {
            if depth + 1 > meter.policy.max_expression_depth {
                return Err(EvaluationError::Limit("expression depth"));
            }
            meter.charge(1 + name.as_str().len() as u64)?;
            EvaluationArgument::Function {
                library: *library,
                name: name.clone(),
            }
        } else {
            let value = run(expr, env, meter, depth + 1, Some(index))?;
            if value == EvaluationValue::Pending {
                return Ok(value);
            }
            EvaluationArgument::Value(value)
        };
        args.try_reserve(1).map_err(allocation)?;
        args.push(arg);
    }
    let result = env.call_at(origin, *library, name, &args, meter)?;
    if result == EvaluationValue::Pending {
        return Err(EvaluationError::OperandType);
    }
    if let EvaluationValue::Present(value) = &result {
        meter.inspect(value)?;
    }
    Ok(result)
}
#[inline(never)]
fn render(
    template: &Digest,
    expressions: &[(Identifier, Expression)],
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
    depth: u64,
) -> Result<EvaluationValue, EvaluationError> {
    let template = env
        .template(template)
        .ok_or(EvaluationError::UnknownTemplate)?;
    htlk_analyzer::check_prompt_template(template, &meter.program_codec)?;
    if template.parameters().len() != expressions.len()
        || template
            .parameters()
            .iter()
            .zip(expressions)
            .any(|((p, _), (a, _))| p != a)
    {
        return Err(EvaluationError::TemplateArguments);
    }
    let mut args = BTreeMap::new();
    for (index, ((name, port), (_, expr))) in
        template.parameters().iter().zip(expressions).enumerate()
    {
        meter.visit(1)?;
        let value = match run(expr, env, meter, depth + 1, Some(index))? {
            EvaluationValue::Present(v) => v,
            EvaluationValue::Pending => return Ok(EvaluationValue::Pending),
            EvaluationValue::Absent => return Err(EvaluationError::AbsentOperand),
        };
        let text = match (port.value_type().kind(), value) {
            (ValueTypeKind::Primitive(PrimitiveType::String), Value::Text(s)) => s,
            (ValueTypeKind::Primitive(PrimitiveType::Integer), Value::Integer(n)) => n.to_string(),
            (ValueTypeKind::Primitive(PrimitiveType::Boolean), Value::Bool(v)) => v.to_string(),
            _ => return Err(EvaluationError::OperandType),
        };
        args.insert(name, text);
    }
    let mut output = String::new();
    for part in template.parts() {
        meter.visit(1)?;
        let text = match part {
            TemplatePart::Text(s) => s,
            TemplatePart::Slot(name) => args.get(name).ok_or(EvaluationError::TemplateArguments)?,
        };
        let size = output
            .len()
            .checked_add(text.len())
            .ok_or(EvaluationError::Limit("output bytes"))?;
        meter.size(
            size.checked_add(header(size))
                .ok_or(EvaluationError::Limit("output bytes"))?,
        )?;
        meter.charge(text.len() as u64)?;
        output.try_reserve(text.len()).map_err(allocation)?;
        output.push_str(text);
    }
    Ok(EvaluationValue::Present(Value::Text(output)))
}
fn outcome(
    node: &Identifier,
    error: bool,
    env: &impl EvaluationContext,
    meter: &mut EvaluationMeter<'_>,
) -> Result<EvaluationValue, EvaluationError> {
    let outcome = env.outcome(node).ok_or(EvaluationError::UnknownOutcome)?;
    if *outcome == EvaluationOutcome::Pending {
        return Ok(EvaluationValue::Pending);
    }
    if error {
        if let EvaluationOutcome::Failed { code, message } = outcome {
            meter.size(
                1 + 5 + header(code.len()) + code.len() + 8 + header(message.len()) + message.len(),
            )?;
            return Ok(EvaluationValue::Present(Value::Map(Map::try_from_entries(
                [
                    ("code".into(), Value::Text(meter.text(code)?)),
                    ("message".into(), Value::Text(meter.text(message)?)),
                ],
            )?)));
        }
        return Ok(EvaluationValue::Present(Value::Null));
    }
    let text = match outcome {
        EvaluationOutcome::Succeeded => "succeeded",
        EvaluationOutcome::Failed { .. } => "failed",
        EvaluationOutcome::Skipped => "skipped",
        EvaluationOutcome::Cancelled => "cancelled",
        EvaluationOutcome::Pending => return Ok(EvaluationValue::Pending),
    };
    Ok(EvaluationValue::Present(Value::Text(meter.text(text)?)))
}
fn project(
    value: &Value,
    path: &[PathStep],
    mut origin: Option<&Expression>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<EvaluationValue, EvaluationError> {
    let mut value = value;
    for (index, step) in path.iter().enumerate() {
        meter.visit(1)?;
        let declared = match (origin.map(Expression::kind), step) {
            (Some(E::Record(fields)), PathStep::Field(name)) => {
                let levels = u64::from((usize::BITS - fields.len().leading_zeros()).max(1));
                meter.charge((name.len() as u64).saturating_mul(levels))?;
                fields
                    .binary_search_by(|(key, _)| key.cmp(name))
                    .ok()
                    .map(|i| &fields[i].1)
            }
            (Some(E::List(items)), PathStep::Index(i)) => {
                usize::try_from(*i).ok().and_then(|i| items.get(i))
            }
            _ => None,
        };
        value = match (value, step) {
            (Value::Map(m), PathStep::Field(key)) => {
                meter
                    .charge((key.len() as u64).saturating_mul(u64::from(
                        (usize::BITS - m.len().leading_zeros()).max(1),
                    )))?;
                match m.get(key) {
                    Some(v) => v,
                    None if declared.is_some() && index + 1 == path.len() => {
                        return Ok(EvaluationValue::Absent);
                    }
                    None if declared.is_some() => return Err(EvaluationError::AbsentOperand),
                    None => return Err(EvaluationError::InvalidProjection),
                }
            }
            (Value::Array(items), PathStep::Index(i)) => usize::try_from(*i)
                .ok()
                .and_then(|i| items.get(i))
                .ok_or(EvaluationError::InvalidProjection)?,
            _ => return Err(EvaluationError::InvalidProjection),
        };
        origin = declared;
    }
    Ok(EvaluationValue::Present(meter.copy_value(value)?))
}
fn copy_state(
    value: &EvaluationValue,
    meter: &mut EvaluationMeter<'_>,
) -> Result<EvaluationValue, EvaluationError> {
    match value {
        EvaluationValue::Present(v) => Ok(EvaluationValue::Present(meter.copy_value(v)?)),
        EvaluationValue::Absent => Ok(EvaluationValue::Absent),
        EvaluationValue::Pending => Ok(EvaluationValue::Pending),
    }
}
fn header(n: usize) -> usize {
    match n {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=0xffff_ffff => 5,
        _ => 9,
    }
}
fn owned(s: &str) -> Result<String, EvaluationError> {
    let mut result = String::new();
    result.try_reserve_exact(s.len()).map_err(allocation)?;
    result.push_str(s);
    Ok(result)
}
fn allocation(_: std::collections::TryReserveError) -> EvaluationError {
    EvaluationError::AllocationFailed
}

/// Static evaluator errors. Limit exhaustion and unavailable data are never false.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvaluationError {
    /// Requested scope-use/expression site is not part of the admitted executable.
    UnknownExpression,
    /// Exact host-linked library admission failed.
    Registry(Box<crate::NativeRegistryError>),
    /// Static expression admission failed.
    Analysis(crate::ExpressionTypeError),
    /// Invalid ordinary value type metadata.
    Type(crate::TypeError),
    /// Native schema validation failed operationally.
    NativeSchema(crate::NativeSchemaError),
    /// A required schema or projection plan is unavailable.
    UnresolvedType,
    /// Codec failure.
    Codec(htlk_cbor::Error),
    /// Expression/context failure.
    Expression(ExpressionError),
    /// Invalid zero policy ceiling.
    InvalidLimits,
    /// Evaluator resource exhausted.
    Limit(&'static str),
    /// An operation consumed legitimate absence without accepting it.
    AbsentOperand,
    /// A failed/cancelled producer cannot supply a requested value.
    UnavailableSource,
    /// Unknown reference in the supplied frame.
    UnknownReference,
    /// Unknown child outcome.
    UnknownOutcome,
    /// Invalid field/index projection.
    InvalidProjection,
    /// Incompatible actual operand representation.
    OperandType,
    /// Native function not linked in this context.
    UnknownFunction,
    /// Static callable appeared as an application value.
    CallableAsValue,
    /// Template not present.
    UnknownTemplate,
    /// Incorrect render argument coverage.
    TemplateArguments,
    /// Regex is invalid or exceeds compiled-size constraints.
    Regex,
    /// Storage reservation failed.
    AllocationFailed,
}
impl EvaluationError {
    /// Stable runtime error code, without untrusted payload contents.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Limit(_) => "E_EXPRESSION_LIMIT",
            Self::Codec(e) if matches!(e.kind(), htlk_cbor::ErrorKind::LimitExceeded { .. }) => {
                "E_EXPRESSION_LIMIT"
            }
            Self::Expression(ExpressionError::LimitExceeded { .. }) => "E_EXPRESSION_LIMIT",
            Self::AbsentOperand => "E_EXPRESSION_ABSENT",
            _ => "E_EXPRESSION",
        }
    }
}
impl From<htlk_cbor::Error> for EvaluationError {
    fn from(e: htlk_cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<ExpressionError> for EvaluationError {
    fn from(e: ExpressionError) -> Self {
        Self::Expression(e)
    }
}
impl From<crate::TypeError> for EvaluationError {
    fn from(e: crate::TypeError) -> Self {
        Self::Type(e)
    }
}
impl From<crate::NativeSchemaError> for EvaluationError {
    fn from(e: crate::NativeSchemaError) -> Self {
        Self::NativeSchema(e)
    }
}
impl From<crate::ExpressionTypeError> for EvaluationError {
    fn from(e: crate::ExpressionTypeError) -> Self {
        Self::Analysis(e)
    }
}
impl From<crate::NativeRegistryError> for EvaluationError {
    fn from(e: crate::NativeRegistryError) -> Self {
        Self::Registry(Box::new(e))
    }
}
impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {self:?}", self.code())
    }
}
impl std::error::Error for EvaluationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Expression(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Analysis(e) => Some(e),
            Self::Registry(e) => Some(e.as_ref()),
            Self::NativeSchema(e) => Some(e),
            _ => None,
        }
    }
}
