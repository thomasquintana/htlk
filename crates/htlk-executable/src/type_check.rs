//! Static expression analysis, separate from evaluation and graph admission.

use crate::digest::Digest;
use crate::{
    BinaryOperator as B, CoreFunction as C, Expression, ExpressionContext, ExpressionKind as E,
    FunctionId, FunctionSignature, Identifier, Library, PathStep, Port, PrimitiveType as P,
    PromptTemplate, ScalarLiteral as L, TypeContext, ValueReference, ValueType as T,
    ValueTypeKind as K,
};
use htlk_cbor::{LimitKind, Limits};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Declarations visible at one expression location. The graph verifier constructs
/// this environment from the owning scope/node; source expressions cannot extend it.
#[derive(Clone, Debug, Default)]
pub struct ExpressionTypeEnvironment {
    /// Exact value roots available in this location.
    pub references: BTreeMap<ValueReference, Port>,
    /// Permitted child outcome names.
    pub outcomes: BTreeSet<Identifier>,
    /// Complete library manifests keyed by implementation identity.
    pub libraries: BTreeMap<Digest, Library>,
    /// Local templates keyed by content identity.
    pub templates: BTreeMap<Digest, PromptTemplate>,
}

/// Actual-value checks required where static types cannot establish compatibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeTypeCheckKind {
    /// An optional expression must produce a present value at this use site.
    Present,
    /// Validate the actual value against this ordinary port constraint.
    Value(Port),
    /// Validate actual scalar operand representations for this comparison.
    Comparison(B),
    /// Validate the operand accepted by core length.
    Length,
    /// Apply a typed field/index projection, including union-variant presence.
    Projection {
        /// Static source type.
        source: T,
        /// Literal field or index.
        step: PathStep,
    },
    /// Schema-aware field typing still needs a location-aware schema projection plan.
    SchemaProjection {
        /// Original schema root identity.
        schema: Digest,
        /// Literal projection step.
        step: PathStep,
    },
}
/// A runtime obligation attached to a canonical AST location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeTypeCheck {
    /// Child-index path: call/list/record/render arguments use their canonical order;
    /// binary operands are 0/1, unary/get operands are 0. Empty means the root.
    pub expression_path: Vec<usize>,
    /// Required actual-value behavior.
    pub kind: RuntimeTypeCheckKind,
}
/// Inferred metadata for a canonical AST node. Callable metadata may contain a
/// function type; it is never an ordinary application value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpressionNodeType {
    /// Canonical child-index path.
    pub expression_path: Vec<usize>,
    /// Inferred type and whether a successful expression can be absent.
    pub port: Port,
    /// True only for a direct static function-reference argument.
    pub callable: bool,
}
/// Static analysis result, not a verified executable or an automatically enforced
/// runtime plan. Schema projection obligations must be resolved by the schema layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpressionAnalysis {
    result: Port,
    nodes: Vec<ExpressionNodeType>,
    checks: Vec<RuntimeTypeCheck>,
}
impl ExpressionAnalysis {
    /// Inferred root type and presence, independent of an expected boundary type.
    pub fn result(&self) -> &Port {
        &self.result
    }
    /// Node metadata sorted by canonical child-index path.
    pub fn nodes(&self) -> &[ExpressionNodeType] {
        &self.nodes
    }
    /// Runtime obligations in stable analysis order.
    pub fn runtime_checks(&self) -> &[RuntimeTypeCheck] {
        &self.checks
    }
}

/// Resolves all branches and infers expression/call types under the supplied
/// declarations. `expected` supplies an optional result-boundary constraint and
/// may resolve otherwise unconstrained generic/empty-list element types.
///
/// # Errors
/// Rejects unknown names, invalid operands/projections, disjoint argument types,
/// ambiguous/unresolved/recursive generic inference, incompatible callbacks,
/// callable values outside argument positions, or resource failures.
pub fn check_expression(
    expression: &Expression,
    context: ExpressionContext,
    environment: &ExpressionTypeEnvironment,
    expected: Option<&Port>,
    limits: &Limits,
) -> Result<ExpressionAnalysis, ExpressionTypeError> {
    expression.to_value(context, limits)?;
    validate_environment(environment, limits)?;
    if let Some(port) = expected {
        port.to_value(TypeContext::Value, limits)?;
    }
    let mut checker = Checker {
        environment,
        limits,
        work: 0,
        bytes: 0,
        serial: 0,
        bindings: BTreeMap::new(),
        mandatory: BTreeSet::new(),
        holes: BTreeSet::new(),
        nodes: Vec::new(),
        checks: Vec::new(),
        constraints: Vec::new(),
        operations: Vec::new(),
        deferred: Vec::new(),
    };
    let result = checker.infer(expression, &[], 0)?;
    if let Some(expected) = expected {
        checker.unify(result.value_type(), expected.value_type(), 0)?;
        checker.constraint(&result, expected, &[], false)?;
    }
    checker.solve_deferred()?;
    for name in checker.mandatory.clone() {
        let resolved =
            checker.substitute(&T::new(K::Var(name), TypeContext::Signature, limits)?, 0)?;
        if has_variable(&resolved) {
            return Err(ExpressionTypeError::UnresolvedGeneric);
        }
    }
    for name in checker.holes.clone() {
        checker
            .bindings
            .entry(name)
            .or_insert_with(|| T::primitive(P::Json));
    }
    for operation in std::mem::take(&mut checker.operations) {
        match operation {
            OperationConstraint::Compare(path, op, left, right) => {
                let left = checker.substitute(&left, 0)?;
                let right = checker.substitute(&right, 0)?;
                match comparison(op, &left, &right) {
                    Match::Never => return Err(ExpressionTypeError::OperandType),
                    Match::Runtime => {
                        checker.add_check(&path, RuntimeTypeCheckKind::Comparison(op))?
                    }
                    Match::Yes => (),
                }
            }
            OperationConstraint::Length(path, ty) => {
                let ty = checker.substitute(&ty, 0)?;
                match length_match(&ty) {
                    Match::Never => return Err(ExpressionTypeError::OperandType),
                    Match::Runtime => checker.add_check(&path, RuntimeTypeCheckKind::Length)?,
                    Match::Yes => (),
                }
            }
        }
    }
    for constraint in std::mem::take(&mut checker.constraints) {
        let actual = checker.substitute_port(&constraint.actual, 0)?;
        let expected = checker.substitute_port(&constraint.expected, 0)?;
        let relation = checker.compatible(actual.value_type(), expected.value_type(), 0)?;
        if relation == Match::Never
            || constraint.callback
                && (relation != Match::Yes || expected.required() && !actual.required())
        {
            return Err(if constraint.callback {
                ExpressionTypeError::CallbackMismatch
            } else {
                ExpressionTypeError::TypeMismatch
            });
        }
        if expected.required() && !actual.required() {
            checker.add_check(&constraint.path, RuntimeTypeCheckKind::Present)?;
        }
        if relation == Match::Runtime {
            checker.add_check(&constraint.path, RuntimeTypeCheckKind::Value(expected))?;
        }
    }
    let result = checker.substitute_port(&result, 0)?;
    result.to_value(TypeContext::Value, limits)?;
    let mut nodes = std::mem::take(&mut checker.nodes);
    for node in &mut nodes {
        node.port = checker.substitute_port(&node.port, 0)?;
        node.port.to_value(
            if node.callable {
                TypeContext::Signature
            } else {
                TypeContext::Value
            },
            limits,
        )?;
    }
    nodes.sort_unstable_by(|a, b| a.expression_path.cmp(&b.expression_path));
    let mut checks = std::mem::take(&mut checker.checks);
    for check in &mut checks {
        match &mut check.kind {
            RuntimeTypeCheckKind::Projection { source, .. } => {
                *source = checker.substitute(source, 0)?;
                source.to_value(TypeContext::Value, limits)?;
            }
            RuntimeTypeCheckKind::Value(port) => {
                *port = checker.substitute_port(port, 0)?;
                port.to_value(TypeContext::Value, limits)?;
            }
            _ => (),
        }
    }
    Ok(ExpressionAnalysis {
        result,
        nodes,
        checks,
    })
}
/// Checks a guard/contract as Boolean while retaining any required presence checks.
///
/// # Errors
/// Returns the same errors as check_expression, including non-Boolean conditions.
pub fn check_condition(
    expression: &Expression,
    context: ExpressionContext,
    environment: &ExpressionTypeEnvironment,
    limits: &Limits,
) -> Result<ExpressionAnalysis, ExpressionTypeError> {
    check_expression(
        expression,
        context,
        environment,
        Some(&Port::new(T::primitive(P::Boolean), true)),
        limits,
    )
}

fn validate_environment(
    env: &ExpressionTypeEnvironment,
    limits: &Limits,
) -> Result<(), ExpressionTypeError> {
    use crate::record_accounting::RecordAccounting;
    let mut a = RecordAccounting::new(limits)?;
    a.collection(4, 0).map_err(bound)?;
    a.collection(env.references.len(), 1).map_err(bound)?;
    for (reference, port) in &env.references {
        match reference {
            ValueReference::Output { node, port } => {
                a.text(node.as_str(), 2).map_err(bound)?;
                a.text(port.as_str(), 2).map_err(bound)?;
            }
            ValueReference::Input(n)
            | ValueReference::ScopeOutput(n)
            | ValueReference::Carried(n)
            | ValueReference::Next(n) => a.text(n.as_str(), 2).map_err(bound)?,
        }
        a.value(&port.to_value(TypeContext::Value, limits)?, 2)
            .map_err(bound)?;
    }
    a.collection(env.outcomes.len(), 1).map_err(bound)?;
    for name in &env.outcomes {
        a.text(name.as_str(), 2).map_err(bound)?;
    }
    a.collection(env.libraries.len(), 1).map_err(bound)?;
    for (id, lib) in &env.libraries {
        if *id != lib.implementation_digest() {
            return Err(ExpressionTypeError::IdentityMismatch);
        }
        a.text(&id.to_string(), 2).map_err(bound)?;
        a.value(&lib.to_value(limits)?, 2).map_err(bound)?;
    }
    a.collection(env.templates.len(), 1).map_err(bound)?;
    for (id, template) in &env.templates {
        if *id != template.digest(limits)? {
            return Err(ExpressionTypeError::IdentityMismatch);
        }
        a.text(&id.to_string(), 2).map_err(bound)?;
        a.value(&template.to_value(limits)?, 2).map_err(bound)?;
    }
    Ok(())
}
fn bound(e: crate::record_accounting::EncodingLimitError) -> ExpressionTypeError {
    ExpressionTypeError::LimitExceeded {
        limit: e.limit,
        maximum: e.maximum,
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    Yes,
    Runtime,
    Never,
}
struct Constraint {
    path: Vec<usize>,
    actual: Port,
    expected: Port,
    callback: bool,
}
enum OperationConstraint {
    Compare(Vec<usize>, B, T, T),
    Length(Vec<usize>, T),
}
struct Checker<'a> {
    environment: &'a ExpressionTypeEnvironment,
    limits: &'a Limits,
    work: usize,
    bytes: usize,
    serial: usize,
    bindings: BTreeMap<Identifier, T>,
    mandatory: BTreeSet<Identifier>,
    holes: BTreeSet<Identifier>,
    nodes: Vec<ExpressionNodeType>,
    checks: Vec<RuntimeTypeCheck>,
    constraints: Vec<Constraint>,
    operations: Vec<OperationConstraint>,
    deferred: Vec<(T, T, usize)>,
}
impl Checker<'_> {
    fn solve_deferred(&mut self) -> Result<(), ExpressionTypeError> {
        while !self.deferred.is_empty() {
            let before = self.bindings.len();
            let constraints = std::mem::take(&mut self.deferred);
            for (formal, actual, depth) in constraints {
                self.unify(&formal, &actual, depth)?;
            }
            if !self.deferred.is_empty() && self.bindings.len() == before {
                return Err(ExpressionTypeError::AmbiguousGeneric);
            }
        }
        Ok(())
    }
    fn lookup_cost(&mut self, key_bytes: usize, count: usize) -> Result<(), ExpressionTypeError> {
        self.step(0)?;
        let levels = (usize::BITS - count.leading_zeros()).max(1) as usize;
        self.account(
            key_bytes
                .checked_add(1)
                .and_then(|n| n.checked_mul(levels))
                .ok_or(ExpressionTypeError::InferenceLimit)?,
        )
    }
    fn field<'b>(
        &mut self,
        fields: &'b [(String, Port)],
        key: &str,
    ) -> Result<Option<&'b Port>, ExpressionTypeError> {
        self.lookup_cost(key.len(), fields.len())?;
        Ok(fields
            .binary_search_by(|(name, _)| {
                name.len()
                    .cmp(&key.len())
                    .then_with(|| name.as_str().cmp(key))
            })
            .ok()
            .map(|i| &fields[i].1))
    }
    fn account(&mut self, bytes: usize) -> Result<(), ExpressionTypeError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(ExpressionTypeError::InferenceLimit)?;
        if self.bytes > self.limits.max_total_payload_bytes {
            return Err(ExpressionTypeError::InferenceLimit);
        }
        Ok(())
    }
    fn step(&mut self, depth: usize) -> Result<(), ExpressionTypeError> {
        if depth > self.limits.max_depth {
            return Err(ExpressionTypeError::LimitExceeded {
                limit: LimitKind::Depth,
                maximum: self.limits.max_depth,
            });
        }
        self.work = self
            .work
            .checked_add(1)
            .ok_or(ExpressionTypeError::InferenceLimit)?;
        if self.work > self.limits.max_total_values {
            return Err(ExpressionTypeError::InferenceLimit);
        }
        Ok(())
    }
    fn copy_type(&mut self, ty: &T) -> Result<T, ExpressionTypeError> {
        self.charge_type(ty)?;
        Ok(ty.clone())
    }
    fn charge_type(&mut self, ty: &T) -> Result<(), ExpressionTypeError> {
        let size = ty.encode(TypeContext::Signature, self.limits)?.len();
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or(ExpressionTypeError::InferenceLimit)?;
        if self.bytes > self.limits.max_total_payload_bytes {
            return Err(ExpressionTypeError::InferenceLimit);
        }
        Ok(())
    }
    fn copy_port(&mut self, p: &Port) -> Result<Port, ExpressionTypeError> {
        Ok(Port::new(self.copy_type(p.value_type())?, p.required()))
    }
    fn make(&mut self, kind: K) -> Result<T, ExpressionTypeError> {
        self.step(0)?;
        Ok(T::new(kind, TypeContext::Signature, self.limits)?)
    }
    fn child(&mut self, path: &[usize], index: usize) -> Result<Vec<usize>, ExpressionTypeError> {
        self.step(path.len() + 1)?;
        let mut p = Vec::new();
        p.try_reserve_exact(path.len() + 1).map_err(allocation)?;
        p.extend_from_slice(path);
        p.push(index);
        Ok(p)
    }
    fn fresh(&mut self, mandatory: bool) -> Result<T, ExpressionTypeError> {
        self.step(0)?;
        let name = Identifier::new(format!("t{}", self.serial))
            .map_err(|_| ExpressionTypeError::InferenceLimit)?;
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or(ExpressionTypeError::InferenceLimit)?;
        if mandatory {
            self.mandatory.insert(name.clone());
        } else {
            self.holes.insert(name.clone());
        }
        self.make(K::Var(name))
    }
    fn add_check(
        &mut self,
        path: &[usize],
        kind: RuntimeTypeCheckKind,
    ) -> Result<(), ExpressionTypeError> {
        self.step(path.len())?;
        self.account(size_of_val(path))?;
        if let RuntimeTypeCheckKind::Projection {
            step: PathStep::Field(name),
            ..
        }
        | RuntimeTypeCheckKind::SchemaProjection {
            step: PathStep::Field(name),
            ..
        } = &kind
        {
            self.account(name.len())?;
        }
        if self.checks.len() >= self.limits.max_collection_entries {
            return Err(ExpressionTypeError::InferenceLimit);
        }
        self.checks.try_reserve(1).map_err(allocation)?;
        self.checks.push(RuntimeTypeCheck {
            expression_path: path.to_vec(),
            kind,
        });
        Ok(())
    }
    fn constraint(
        &mut self,
        actual: &Port,
        expected: &Port,
        path: &[usize],
        callback: bool,
    ) -> Result<(), ExpressionTypeError> {
        self.step(path.len())?;
        self.account(size_of_val(path))?;
        let actual = self.copy_port(actual)?;
        let expected = self.copy_port(expected)?;
        self.constraints.try_reserve(1).map_err(allocation)?;
        self.constraints.push(Constraint {
            path: path.to_vec(),
            actual,
            expected,
            callback,
        });
        Ok(())
    }
    fn require(
        &mut self,
        actual: &Port,
        expected: T,
        path: &[usize],
    ) -> Result<(), ExpressionTypeError> {
        self.unify(&expected, actual.value_type(), 0)?;
        self.constraint(actual, &Port::new(expected, true), path, false)
    }
    fn infer(
        &mut self,
        expr: &Expression,
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        self.step(depth)?;
        let result = match expr.kind() {
            E::Not(value) => self.infer_not(value, path, depth),
            E::Get { value, path: steps } => self.infer_get(value, steps, path, depth),
            E::Binary {
                operator,
                left,
                right,
            } => self.infer_binary(*operator, left, right, path, depth),
            E::List(items) => self.infer_list(items, path, depth),
            E::Record(fields) => self.infer_record(fields, path, depth),
            E::Call {
                function: FunctionId::Core(core),
                arguments,
            } => self.infer_core(*core, &arguments[0], path, depth),
            E::Call {
                function: FunctionId::Library { library, name },
                arguments,
            } => self.call(*library, name, arguments, path, depth),
            E::Render {
                template,
                arguments,
            } => self.infer_render(template, arguments, path, depth),
            _ => self.infer_leaf(expr, path, depth),
        }?;
        let port = self.copy_port(&result)?;
        self.account(size_of_val(path))?;
        if self.nodes.len() >= self.limits.max_collection_entries {
            return Err(ExpressionTypeError::InferenceLimit);
        }
        self.nodes.try_reserve(1).map_err(allocation)?;
        self.nodes.push(ExpressionNodeType {
            expression_path: path.to_vec(),
            port,
            callable: false,
        });
        Ok(result)
    }
    #[inline(never)]
    fn infer_leaf(
        &mut self,
        expr: &Expression,
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let required = |p| Port::new(T::primitive(p), true);
        match expr.kind() {
            E::Literal(literal) => Ok(required(match literal {
                L::String(_) => P::String,
                L::Integer(_) => P::Integer,
                L::Float(_) => P::Float,
                L::Boolean(_) => P::Boolean,
                L::Null => P::Null,
                L::Bytes(_) => P::Bytes,
            })),
            E::Regex { .. } => Ok(required(P::Regex)),
            E::Ref {
                source,
                path: steps,
            } => {
                let port = self
                    .environment
                    .references
                    .get(source)
                    .ok_or(ExpressionTypeError::UnknownReference)?;
                let mut port = self.copy_port(port)?;
                for step in steps {
                    port = self.project(&port, step, path, depth + 1)?;
                }
                Ok(port)
            }
            E::Status(node) | E::Error(node) => {
                if !self.environment.outcomes.contains(node) {
                    return Err(ExpressionTypeError::UnknownOutcome);
                }
                let ty = if matches!(expr.kind(), E::Status(_)) {
                    self.make(K::Enum(
                        ["succeeded", "failed", "skipped", "cancelled"]
                            .map(String::from)
                            .to_vec(),
                    ))?
                } else {
                    self.union(vec![T::primitive(P::Error), T::primitive(P::Null)])?
                };
                Ok(Port::new(ty, true))
            }
            E::FunctionRef { .. } => Err(ExpressionTypeError::CallableAsValue),
            _ => unreachable!("recursive expressions use separate dispatch helpers"),
        }
    }
    #[inline(never)]
    fn infer_get(
        &mut self,
        value: &Expression,
        steps: &[PathStep],
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let child = self.child(path, 0)?;
        let mut port = self.infer(value, &child, depth + 1)?;
        for step in steps {
            port = self.project(&port, step, path, depth + 1)?;
        }
        Ok(port)
    }
    #[inline(never)]
    fn infer_not(
        &mut self,
        value: &Expression,
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let child = self.child(path, 0)?;
        let port = self.infer(value, &child, depth + 1)?;
        self.require(&port, T::primitive(P::Boolean), &child)?;
        Ok(Port::new(T::primitive(P::Boolean), true))
    }
    #[inline(never)]
    fn infer_binary(
        &mut self,
        operator: B,
        left: &Expression,
        right: &Expression,
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let lp = self.child(path, 0)?;
        let rp = self.child(path, 1)?;
        let left = self.infer(left, &lp, depth + 1)?;
        let right = self.infer(right, &rp, depth + 1)?;
        self.binary_constraints(operator, &left, &right, [&lp, &rp], path)?;
        Ok(Port::new(T::primitive(P::Boolean), true))
    }
    #[inline(never)]
    fn binary_constraints(
        &mut self,
        operator: B,
        left: &Port,
        right: &Port,
        operands: [&[usize]; 2],
        path: &[usize],
    ) -> Result<(), ExpressionTypeError> {
        let [lp, rp] = operands;
        if matches!(operator, B::And | B::Or) {
            self.require(left, T::primitive(P::Boolean), lp)?;
            self.require(right, T::primitive(P::Boolean), rp)?;
        } else {
            if !left.required() {
                self.add_check(lp, RuntimeTypeCheckKind::Present)?;
            }
            if !right.required() {
                self.add_check(rp, RuntimeTypeCheckKind::Present)?;
            }
            let left = self.copy_type(left.value_type())?;
            let right = self.copy_type(right.value_type())?;
            if matches!(left.kind(), K::Var(_)) || matches!(right.kind(), K::Var(_)) {
                self.unify(&left, &right, 0)?;
            }
            self.account(size_of_val(path))?;
            self.operations.push(OperationConstraint::Compare(
                path.to_vec(),
                operator,
                left,
                right,
            ));
        }
        Ok(())
    }
    #[inline(never)]
    fn infer_list(
        &mut self,
        items: &[Expression],
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let mut types = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let child = self.child(path, i)?;
            let item = self.infer(item, &child, depth + 1)?;
            if !item.required() {
                self.add_check(&child, RuntimeTypeCheckKind::Present)?;
            }
            types.try_reserve(1).map_err(allocation)?;
            types.push(self.copy_type(item.value_type())?);
        }
        let item = if types.is_empty() {
            self.fresh(false)?
        } else {
            self.union(types)?
        };
        Ok(Port::new(self.make(K::List(Box::new(item)))?, true))
    }
    #[inline(never)]
    fn infer_record(
        &mut self,
        fields: &[(String, Expression)],
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let mut result = Vec::new();
        for (i, (name, value)) in fields.iter().enumerate() {
            let child = self.child(path, i)?;
            let value = self.infer(value, &child, depth + 1)?;
            result.try_reserve(1).map_err(allocation)?;
            result.push((name.clone(), value));
        }
        Ok(Port::new(self.make(K::Record(result))?, true))
    }
    #[inline(never)]
    fn infer_core(
        &mut self,
        core: C,
        argument: &Expression,
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let child = self.child(path, 0)?;
        let actual = self.infer(argument, &child, depth + 1)?;
        if core == C::Present {
            return Ok(Port::new(T::primitive(P::Boolean), true));
        }
        let ty = self.copy_type(actual.value_type())?;
        self.account(size_of_val(child.as_slice()))?;
        self.operations
            .push(OperationConstraint::Length(child.clone(), ty));
        if !actual.required() {
            self.add_check(&child, RuntimeTypeCheckKind::Present)?;
        }
        Ok(Port::new(T::primitive(P::Integer), true))
    }
    #[inline(never)]
    fn infer_render(
        &mut self,
        template: &Digest,
        arguments: &[(Identifier, Expression)],
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let template = self
            .environment
            .templates
            .get(template)
            .ok_or(ExpressionTypeError::UnknownTemplate)?;
        if template.parameters().len() != arguments.len() {
            return Err(ExpressionTypeError::Arity);
        }
        for (i, ((name, expected), (actual_name, value))) in
            template.parameters().iter().zip(arguments).enumerate()
        {
            if name != actual_name {
                return Err(ExpressionTypeError::Arity);
            }
            let child = self.child(path, i)?;
            let actual = self.infer(value, &child, depth + 1)?;
            let expected = Port::new(self.copy_type(expected.value_type())?, true);
            self.constraint(&actual, &expected, &child, false)?;
        }
        Ok(Port::new(T::primitive(P::String), true))
    }
    #[inline(never)]
    fn call(
        &mut self,
        library: Digest,
        name: &Identifier,
        arguments: &[Expression],
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        let sig = self
            .environment
            .libraries
            .get(&library)
            .ok_or(ExpressionTypeError::UnknownLibrary)?
            .function(name.as_str())
            .ok_or(ExpressionTypeError::UnknownFunction)?;
        if sig.parameters().len() != arguments.len() {
            return Err(ExpressionTypeError::Arity);
        }
        let (parameters, returns) = self.instantiate(sig)?;
        for (i, (expected, argument)) in parameters.iter().zip(arguments).enumerate() {
            let child = self.child(path, i)?;
            if let E::FunctionRef { library, name } = argument.kind() {
                let expected_type = self.deref(expected.value_type())?;
                if !matches!(expected_type.kind(), K::Function { .. }) {
                    return Err(ExpressionTypeError::CallableAsValue);
                }
                let sig = self
                    .environment
                    .libraries
                    .get(library)
                    .ok_or(ExpressionTypeError::UnknownLibrary)?
                    .function(name.as_str())
                    .ok_or(ExpressionTypeError::UnknownFunction)?;
                let (params, ret) = self.instantiate(sig)?;
                let actual = Port::new(
                    self.make(K::Function {
                        parameters: params,
                        returns: Box::new(ret),
                    })?,
                    true,
                );
                self.unify(&expected_type, actual.value_type(), 0)?;
                self.constraint(&actual, expected, &child, true)?;
                let port = self.copy_port(&actual)?;
                self.account(size_of_val(child.as_slice()))?;
                if self.nodes.len() >= self.limits.max_collection_entries {
                    return Err(ExpressionTypeError::InferenceLimit);
                }
                self.nodes.try_reserve(1).map_err(allocation)?;
                self.nodes.push(ExpressionNodeType {
                    expression_path: child,
                    port,
                    callable: true,
                });
            } else {
                let actual = self.infer(argument, &child, depth + 1)?;
                self.unify(expected.value_type(), actual.value_type(), 0)?;
                self.constraint(&actual, expected, &child, false)?;
            }
        }
        Ok(returns)
    }
    fn instantiate(
        &mut self,
        signature: &FunctionSignature,
    ) -> Result<(Vec<Port>, Port), ExpressionTypeError> {
        let mut names = BTreeMap::new();
        for name in signature.type_parameters() {
            names.insert(name.clone(), self.fresh(true)?);
        }
        let params = signature
            .parameters()
            .iter()
            .map(|p| self.rename_port(p, &names, 0))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((params, self.rename_port(signature.returns(), &names, 0)?))
    }
    fn rename_port(
        &mut self,
        port: &Port,
        names: &BTreeMap<Identifier, T>,
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        Ok(Port::new(
            self.rename(port.value_type(), names, depth)?,
            port.required(),
        ))
    }
    fn rename(
        &mut self,
        ty: &T,
        names: &BTreeMap<Identifier, T>,
        depth: usize,
    ) -> Result<T, ExpressionTypeError> {
        self.step(depth)?;
        if let K::Var(name) = ty.kind() {
            return self.copy_type(
                names
                    .get(name)
                    .ok_or(ExpressionTypeError::UnresolvedGeneric)?,
            );
        }
        self.map_children(ty, depth, |s, t, d| s.rename(t, names, d))
    }
    fn map_children(
        &mut self,
        ty: &T,
        depth: usize,
        mut map: impl FnMut(&mut Self, &T, usize) -> Result<T, ExpressionTypeError>,
    ) -> Result<T, ExpressionTypeError> {
        let kind = match ty.kind() {
            K::List(t) => K::List(Box::new(map(self, t, depth + 1)?)),
            K::Map(t) => K::Map(Box::new(map(self, t, depth + 1)?)),
            K::Record(fields) => K::Record(
                fields
                    .iter()
                    .map(|(n, p)| {
                        Ok((
                            n.clone(),
                            Port::new(map(self, p.value_type(), depth + 1)?, p.required()),
                        ))
                    })
                    .collect::<Result<_, ExpressionTypeError>>()?,
            ),
            K::Union(types) => {
                let types = types
                    .iter()
                    .map(|t| map(self, t, depth + 1))
                    .collect::<Result<_, _>>()?;
                return self.union(types);
            }
            K::Function {
                parameters,
                returns,
            } => K::Function {
                parameters: parameters
                    .iter()
                    .map(|p| {
                        Ok(Port::new(
                            map(self, p.value_type(), depth + 1)?,
                            p.required(),
                        ))
                    })
                    .collect::<Result<_, ExpressionTypeError>>()?,
                returns: Box::new(Port::new(
                    map(self, returns.value_type(), depth + 1)?,
                    returns.required(),
                )),
            },
            _ => return self.copy_type(ty),
        };
        self.make(kind)
    }
    fn deref(&mut self, ty: &T) -> Result<T, ExpressionTypeError> {
        let mut current = ty;
        while let K::Var(name) = current.kind() {
            let Some(next) = self.bindings.get(name) else {
                break;
            };
            self.work = self
                .work
                .checked_add(1)
                .ok_or(ExpressionTypeError::InferenceLimit)?;
            if self.work > self.limits.max_total_values {
                return Err(ExpressionTypeError::InferenceLimit);
            }
            current = next;
        }
        let size = current.encode(TypeContext::Signature, self.limits)?.len();
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or(ExpressionTypeError::InferenceLimit)?;
        if self.bytes > self.limits.max_total_payload_bytes {
            return Err(ExpressionTypeError::InferenceLimit);
        }
        Ok(current.clone())
    }
    fn substitute_port(&mut self, p: &Port, depth: usize) -> Result<Port, ExpressionTypeError> {
        Ok(Port::new(
            self.substitute(p.value_type(), depth)?,
            p.required(),
        ))
    }
    fn substitute(&mut self, ty: &T, depth: usize) -> Result<T, ExpressionTypeError> {
        self.step(depth)?;
        let ty = self.deref(ty)?;
        self.map_children(&ty, depth, |s, t, d| s.substitute(t, d))
    }
    fn occurs(
        &mut self,
        name: &Identifier,
        ty: &T,
        depth: usize,
    ) -> Result<bool, ExpressionTypeError> {
        self.step(depth)?;
        let ty = self.deref(ty)?;
        if matches!(ty.kind(), K::Var(n) if n == name) {
            return Ok(true);
        }
        for child in children(&ty) {
            if self.occurs(name, child, depth + 1)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn unify(&mut self, formal: &T, actual: &T, depth: usize) -> Result<(), ExpressionTypeError> {
        self.step(depth)?;
        let formal = self.deref(formal)?;
        let actual = self.deref(actual)?;
        if formal == actual {
            return Ok(());
        }
        if let K::Var(name) = formal.kind() {
            if self.occurs(name, &actual, depth + 1)? {
                return Err(ExpressionTypeError::RecursiveGeneric);
            }
            self.bindings.insert(name.clone(), actual);
            return Ok(());
        }
        if matches!(actual.kind(), K::Var(_)) {
            return self.unify(&actual, &formal, depth + 1);
        }
        match (formal.kind(), actual.kind()) {
            (K::List(a), K::List(b)) | (K::Map(a), K::Map(b)) => self.unify(a, b, depth + 1)?,
            (K::Record(a), K::Record(b)) => {
                for (name, expected) in a {
                    if let Some(value) = self.field(b, name)? {
                        self.unify(expected.value_type(), value.value_type(), depth + 1)?;
                    }
                }
            }
            (K::Map(a), K::Record(b)) => {
                for (_, p) in b {
                    self.unify(a, p.value_type(), depth + 1)?;
                }
            }
            (
                K::Function {
                    parameters: a,
                    returns: ar,
                },
                K::Function {
                    parameters: b,
                    returns: br,
                },
            ) => {
                if a.len() != b.len() {
                    return Err(ExpressionTypeError::CallbackMismatch);
                }
                for (a, b) in a.iter().zip(b) {
                    self.unify(a.value_type(), b.value_type(), depth + 1)?;
                }
                self.unify(ar.value_type(), br.value_type(), depth + 1)?;
            }
            (K::Union(members), _) => {
                let members = members
                    .iter()
                    .map(|t| self.substitute(t, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                let values = if let K::Union(values) = actual.kind() {
                    values.as_slice()
                } else {
                    std::slice::from_ref(&actual)
                };
                let mut remaining = Vec::new();
                let mut known = BTreeSet::new();
                let mut known_types = Vec::new();
                let mut candidates = Vec::new();
                for member in &members {
                    self.step(depth)?;
                    if has_variable(member) {
                        candidates.push(member);
                    } else {
                        known_types.push(member);
                        let encoded = member.encode(TypeContext::Signature, self.limits)?;
                        self.account(encoded.len())?;
                        known.insert(encoded);
                    }
                }
                for value in values {
                    let encoded = value.encode(TypeContext::Signature, self.limits)?;
                    self.lookup_cost(encoded.len(), known.len())?;
                    if known.contains(&encoded) {
                        continue;
                    }
                    let mut covered = false;
                    for member in &known_types {
                        if self.compatible(value, member, depth + 1)? == Match::Yes {
                            covered = true;
                            break;
                        }
                    }
                    if covered {
                        continue;
                    }
                    remaining.push(self.copy_type(value)?);
                }
                if !remaining.is_empty() {
                    if candidates.len() > 1 {
                        let formal = self.copy_type(&formal)?;
                        let actual = self.copy_type(&actual)?;
                        self.deferred.try_reserve(1).map_err(allocation)?;
                        self.deferred.push((formal, actual, depth));
                        return Ok(());
                    }
                    if let Some(candidate) = candidates.first() {
                        let combined = self.union(remaining)?;
                        self.unify(candidate, &combined, depth + 1)?;
                    }
                }
            }
            _ => (), // Concrete compatibility is checked after all constraints resolve.
        }
        Ok(())
    }
    fn union(&mut self, types: Vec<T>) -> Result<T, ExpressionTypeError> {
        let mut unique = BTreeMap::new();
        let mut pending = types;
        while let Some(ty) = pending.pop() {
            self.step(0)?;
            if let K::Union(children) = ty.kind() {
                for t in children {
                    pending.push(self.copy_type(t)?);
                }
            } else {
                unique.insert(ty.encode(TypeContext::Signature, self.limits)?, ty);
            }
        }
        if unique.len() == 1 {
            return Ok(unique.into_values().next().expect("one member"));
        }
        self.make(K::Union(unique.into_values().collect()))
    }
    fn project(
        &mut self,
        port: &Port,
        step: &PathStep,
        path: &[usize],
        depth: usize,
    ) -> Result<Port, ExpressionTypeError> {
        self.step(depth)?;
        if !port.required() {
            self.add_check(path, RuntimeTypeCheckKind::Present)?;
        }
        let ty = self.deref(port.value_type())?;
        let mut optional = false;
        let mut projected = Vec::new();
        let members = if let K::Union(types) = ty.kind() {
            types.as_slice()
        } else {
            std::slice::from_ref(&ty)
        };
        for member in members {
            self.step(depth)?;
            match (member.kind(), step) {
                (K::Record(fields), PathStep::Field(key)) => {
                    if let Some(p) = self.field(fields, key)? {
                        projected.push(self.copy_type(p.value_type())?);
                        optional |= !p.required();
                    } else {
                        optional = true;
                    }
                }
                (K::Map(t), PathStep::Field(_)) | (K::List(t), PathStep::Index(_)) => {
                    projected.push(self.copy_type(t)?)
                }
                (
                    K::Primitive(
                        p @ (P::Error | P::Regex | P::ResourceSnapshot | P::McpPromptResult),
                    ),
                    PathStep::Field(key),
                ) => {
                    let shape = self.builtin_record(*p)?;
                    let K::Record(fields) = shape.kind() else {
                        unreachable!("builtin record")
                    };
                    if let Some(port) = self.field(fields, key)? {
                        projected.push(self.copy_type(port.value_type())?);
                        optional |= !port.required();
                    } else {
                        optional = true;
                    }
                }
                (K::Primitive(P::Json), _) => projected.push(T::primitive(P::Json)),
                (K::Schema(id), _) => {
                    self.add_check(
                        path,
                        RuntimeTypeCheckKind::SchemaProjection {
                            schema: *id,
                            step: step.clone(),
                        },
                    )?;
                    projected.push(T::primitive(P::Json));
                    optional = true;
                }
                _ => (),
            }
        }
        if projected.is_empty() {
            return Err(ExpressionTypeError::InvalidProjection);
        }
        let source = self.copy_type(&ty)?;
        self.add_check(
            path,
            RuntimeTypeCheckKind::Projection {
                source,
                step: step.clone(),
            },
        )?;
        Ok(Port::new(self.union(projected)?, !optional))
    }
    fn builtin_record(&mut self, primitive: P) -> Result<T, ExpressionTypeError> {
        let string = || Port::new(T::primitive(P::String), true);
        let fields = match primitive {
            P::Error => vec![("code".into(), string()), ("message".into(), string())],
            P::Regex => vec![("pattern".into(), string()), ("flags".into(), string())],
            P::ResourceSnapshot => {
                let mut variants = Vec::new();
                for (kind, field, ty) in [("text", "text", P::String), ("bytes", "data", P::Bytes)]
                {
                    let tag = self.make(K::Enum(vec![kind.into()]))?;
                    variants.push(self.make(K::Record(vec![
                        ("kind".into(), Port::new(tag, true)),
                        ("uri".into(), string()),
                        (
                            "mime_type".into(),
                            Port::new(T::primitive(P::String), false),
                        ),
                        (field.into(), Port::new(T::primitive(ty), true)),
                    ]))?);
                }
                let content = self.union(variants)?;
                let contents = self.make(K::List(Box::new(content)))?;
                vec![
                    ("server_identity".into(), string()),
                    ("descriptor_digest".into(), string()),
                    ("requested_uri".into(), string()),
                    ("contents".into(), Port::new(contents, true)),
                ]
            }
            P::McpPromptResult => {
                let role = self.make(K::Enum(vec!["user".into(), "assistant".into()]))?;
                let message = self.make(K::Record(vec![
                    ("role".into(), Port::new(role, true)),
                    ("content".into(), Port::new(T::primitive(P::Json), true)),
                ]))?;
                let messages = self.make(K::List(Box::new(message)))?;
                let meta = self.make(K::Map(Box::new(T::primitive(P::Json))))?;
                vec![
                    ("messages".into(), Port::new(messages, true)),
                    (
                        "description".into(),
                        Port::new(T::primitive(P::String), false),
                    ),
                    ("_meta".into(), Port::new(meta, false)),
                ]
            }
            _ => return Err(ExpressionTypeError::InvalidProjection),
        };
        self.make(K::Record(fields))
    }
    fn compatible(
        &mut self,
        actual: &T,
        expected: &T,
        depth: usize,
    ) -> Result<Match, ExpressionTypeError> {
        self.step(depth)?;
        self.charge_type(actual)?;
        self.charge_type(expected)?;
        if actual == expected {
            return Ok(Match::Yes);
        }
        if let K::Union(members) = actual.kind() {
            let results = members
                .iter()
                .map(|m| self.compatible(m, expected, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(if results.iter().all(|r| *r == Match::Yes) {
                Match::Yes
            } else if results.iter().all(|r| *r == Match::Never) {
                Match::Never
            } else {
                Match::Runtime
            });
        }
        if let K::Union(members) = expected.kind() {
            let results = members
                .iter()
                .map(|m| self.compatible(actual, m, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(if results.contains(&Match::Yes) {
                Match::Yes
            } else if results.contains(&Match::Runtime) {
                Match::Runtime
            } else {
                Match::Never
            });
        }
        if families(actual) & families(expected) == 0 {
            return Ok(Match::Never);
        }
        match (actual.kind(), expected.kind()) {
            (K::Enum(a), K::Enum(b)) => {
                let mut present = 0;
                for value in a {
                    self.lookup_cost(value.len(), b.len())?;
                    present += usize::from(b.binary_search(value).is_ok());
                }
                Ok(if present == a.len() {
                    Match::Yes
                } else if present != 0 {
                    Match::Runtime
                } else {
                    Match::Never
                })
            }
            (K::Enum(_), K::Primitive(P::String)) => Ok(Match::Yes),
            (_, K::Primitive(P::Json)) => Ok(match actual.kind() {
                K::Primitive(
                    P::String | P::Boolean | P::Null | P::Regex | P::Error | P::McpPromptResult,
                )
                | K::Schema(_) => Match::Yes,
                _ => Match::Runtime,
            }),
            (K::Primitive(P::Json), _) | (K::Schema(_), _) | (_, K::Schema(_)) => {
                Ok(Match::Runtime)
            }
            (K::List(a), K::List(b)) | (K::Map(a), K::Map(b)) => self.compatible(a, b, depth + 1),
            (K::Record(a), K::Record(b)) => {
                let mut result = Match::Yes;
                for (name, wanted) in b {
                    let Some(supplied) = self.field(a, name)? else {
                        if wanted.required() {
                            return Ok(Match::Never);
                        }
                        result = Match::Runtime;
                        continue;
                    };
                    let relation =
                        self.compatible(supplied.value_type(), wanted.value_type(), depth + 1)?;
                    if relation == Match::Never && supplied.required() {
                        return Ok(Match::Never);
                    }
                    if relation != Match::Yes || wanted.required() && !supplied.required() {
                        result = Match::Runtime;
                    }
                }
                Ok(result)
            }
            (K::Record(fields), K::Map(item)) => {
                for (_, port) in fields {
                    if self.compatible(port.value_type(), item, depth + 1)? == Match::Never
                        && port.required()
                    {
                        return Ok(Match::Never);
                    }
                }
                Ok(Match::Runtime) // Record values may contain undeclared extra fields.
            }
            (K::Map(item), K::Record(fields)) => {
                let mut result = Match::Yes;
                for (_, port) in fields {
                    let relation = self.compatible(item, port.value_type(), depth + 1)?;
                    if relation == Match::Never && port.required() {
                        return Ok(Match::Never);
                    }
                    if relation != Match::Yes || port.required() {
                        result = Match::Runtime;
                    }
                }
                Ok(result)
            }
            (K::Primitive(P::Error | P::Regex), K::Record(_)) => {
                let names = if matches!(actual.kind(), K::Primitive(P::Error)) {
                    ["code", "message"]
                } else {
                    ["pattern", "flags"]
                };
                let record = self.make(K::Record(
                    names
                        .into_iter()
                        .map(|n| (n.into(), Port::new(T::primitive(P::String), true)))
                        .collect(),
                ))?;
                self.compatible(&record, expected, depth + 1)
            }
            (K::Record(_), K::Primitive(P::Error)) => {
                let record = self.make(K::Record(
                    ["code", "message"]
                        .into_iter()
                        .map(|n| (n.into(), Port::new(T::primitive(P::String), true)))
                        .collect(),
                ))?;
                self.compatible(actual, &record, depth + 1)
            }
            (
                K::Function {
                    parameters: a,
                    returns: ar,
                },
                K::Function {
                    parameters: b,
                    returns: br,
                },
            ) => {
                if a.len() != b.len() || br.required() && !ar.required() {
                    return Ok(Match::Never);
                }
                for (actual, expected) in a.iter().zip(b) {
                    if !expected.required() && actual.required()
                        || self.compatible(expected.value_type(), actual.value_type(), depth + 1)?
                            != Match::Yes
                    {
                        return Ok(Match::Never);
                    }
                }
                self.compatible(ar.value_type(), br.value_type(), depth + 1)
            }
            _ => Ok(Match::Runtime),
        }
    }
}

fn children(ty: &T) -> Vec<&T> {
    match ty.kind() {
        K::List(t) | K::Map(t) => vec![t],
        K::Record(f) => f.iter().map(|(_, p)| p.value_type()).collect(),
        K::Union(v) => v.iter().collect(),
        K::Function {
            parameters,
            returns,
        } => parameters
            .iter()
            .map(Port::value_type)
            .chain(std::iter::once(returns.value_type()))
            .collect(),
        _ => vec![],
    }
}
fn has_variable(ty: &T) -> bool {
    matches!(ty.kind(), K::Var(_)) || children(ty).into_iter().any(has_variable)
}
const NULL: u16 = 1;
const BOOL: u16 = 2;
const INT: u16 = 4;
const FLOAT: u16 = 8;
const TEXT: u16 = 16;
const BYTES: u16 = 32;
const ARRAY: u16 = 64;
const OBJECT: u16 = 128;
const JSON: u16 = NULL | BOOL | INT | FLOAT | TEXT | ARRAY | OBJECT;
fn families(ty: &T) -> u16 {
    match ty.kind() {
        K::Primitive(p) => match p {
            P::Null => NULL,
            P::Boolean => BOOL,
            P::Integer => INT,
            P::Float => FLOAT,
            P::String => TEXT,
            P::Bytes => BYTES,
            P::Json => JSON,
            _ => OBJECT,
        },
        K::Enum(_) => TEXT,
        K::Schema(_) => JSON,
        K::List(_) => ARRAY,
        K::Map(_) | K::Record(_) => OBJECT,
        K::Union(types) => types.iter().fold(0, |mask, t| mask | families(t)),
        K::Var(_) => u16::MAX,
        K::Function { .. } => 256,
    }
}
fn comparison(op: B, a: &T, b: &T) -> Match {
    let a = families(a);
    let b = families(b);
    let equality = matches!(op, B::Eq | B::Ne);
    if equality && (a == NULL && b & NULL != 0 || b == NULL && a & NULL != 0) {
        return Match::Yes;
    }
    let allowed = if equality {
        NULL | BOOL | INT | FLOAT | TEXT
    } else {
        INT | FLOAT | TEXT
    };
    let common = a & b & allowed;
    if common == 0 {
        Match::Never
    } else if a == b && a.count_ones() == 1 {
        Match::Yes
    } else {
        Match::Runtime
    }
}
fn length_match(ty: &T) -> Match {
    match ty.kind() {
        K::Primitive(
            P::String | P::Bytes | P::Regex | P::Error | P::ResourceSnapshot | P::McpPromptResult,
        )
        | K::Enum(_)
        | K::List(_)
        | K::Map(_)
        | K::Record(_) => Match::Yes,
        K::Primitive(P::Json) | K::Schema(_) | K::Var(_) => Match::Runtime,
        K::Union(types) => {
            let results: Vec<_> = types.iter().map(length_match).collect();
            if results.iter().all(|r| *r == Match::Yes) {
                Match::Yes
            } else if results.iter().all(|r| *r == Match::Never) {
                Match::Never
            } else {
                Match::Runtime
            }
        }
        _ => Match::Never,
    }
}
fn allocation(_: std::collections::TryReserveError) -> ExpressionTypeError {
    ExpressionTypeError::AllocationFailed
}

/// Static, input-free expression typing failures.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpressionTypeError {
    /// Codec failure.
    Codec(htlk_cbor::Error),
    /// Expression representation/context failure.
    Expression(crate::ExpressionError),
    /// Type construction failure.
    Type(crate::TypeError),
    /// Manifest validation failure.
    Metadata(crate::MetadataError),
    /// A supplied manifest/template key disagrees with its identity.
    IdentityMismatch,
    /// Referenced root is not declared in this environment.
    UnknownReference,
    /// Referenced child outcome is not declared.
    UnknownOutcome,
    /// Referenced library is absent.
    UnknownLibrary,
    /// Referenced function is absent.
    UnknownFunction,
    /// Referenced template is absent.
    UnknownTemplate,
    /// Positional count or named render coverage differs.
    Arity,
    /// Actual and expected types are statically incompatible.
    TypeMismatch,
    /// Operator cannot accept these static operand representations.
    OperandType,
    /// No union variant declares/supports the requested projection.
    InvalidProjection,
    /// A static callable appears outside a declared function argument position.
    CallableAsValue,
    /// Callback type or presence variance is incompatible.
    CallbackMismatch,
    /// Generic union constraints do not select a unique inference path.
    AmbiguousGeneric,
    /// A declared call-site variable remains unresolved.
    UnresolvedGeneric,
    /// Unification would create an infinite type.
    RecursiveGeneric,
    /// Type analysis work or derived storage exceeds its allowance.
    InferenceLimit,
    /// Environment/type depth or encoding limit exceeded.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// A storage reservation failed.
    AllocationFailed,
}
macro_rules! from_error {
    ($t:ty, $v:ident) => {
        impl From<$t> for ExpressionTypeError {
            fn from(e: $t) -> Self {
                Self::$v(e)
            }
        }
    };
}
from_error!(htlk_cbor::Error, Codec);
from_error!(crate::ExpressionError, Expression);
from_error!(crate::TypeError, Type);
from_error!(crate::MetadataError, Metadata);
impl fmt::Display for ExpressionTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expression type error: {self:?}")
    }
}
impl std::error::Error for ExpressionTypeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Expression(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Metadata(e) => Some(e),
            _ => None,
        }
    }
}
