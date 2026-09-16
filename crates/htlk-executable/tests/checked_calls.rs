//! Native-call admission, concrete generic boundaries, and dispatch ordering.
use htlk_cbor::{Limits, Value};
use htlk_executable::{
    CheckedExpression, EvaluationArgument, EvaluationContext, EvaluationError as Error,
    EvaluationFrame, EvaluationMeter, EvaluationOutcome, EvaluationValue as V, EvaluatorLimits,
    Expression, ExpressionCallType, ExpressionContext as C, ExpressionKind as E,
    ExpressionTypeEnvironment, FunctionId, FunctionSignature, Identifier, Library, PathStep, Port,
    PrimitiveType as P, PromptTemplate, TypeContext, ValueReference, ValueType as T,
    ValueTypeKind as K, digest::Digest,
};
use std::cell::{Cell, RefCell};

fn policy() -> EvaluatorLimits {
    EvaluatorLimits {
        max_expression_depth: 128,
        max_value_bytes: 65536,
        max_collection_visits: 10000,
        max_regex_bytes: 1024,
        max_regex_compiled_bytes: 4096,
        max_output_bytes: 65536,
        max_steps: 100000,
    }
}
fn id() -> Digest {
    Digest::from_bytes([5; 32])
}
fn port(p: P, required: bool) -> Port {
    Port::new(T::primitive(p), required)
}
fn variable() -> T {
    T::new(
        K::Var("t".parse().unwrap()),
        TypeContext::Signature,
        &Limits::default(),
    )
    .unwrap()
}
fn signature(generic: bool, parameters: Vec<Port>, returns: Port) -> FunctionSignature {
    FunctionSignature::new(
        if generic {
            vec!["t".parse().unwrap()]
        } else {
            vec![]
        },
        parameters,
        returns,
        &Limits::default(),
    )
    .unwrap()
}
fn environment(functions: Vec<(&str, FunctionSignature)>) -> ExpressionTypeEnvironment {
    let mut env = ExpressionTypeEnvironment::default();
    env.libraries.insert(
        id(),
        Library::new(
            "test".into(),
            "1".into(),
            id(),
            functions
                .into_iter()
                .map(|(n, s)| (n.parse().unwrap(), s))
                .collect(),
            &Limits::default(),
        )
        .unwrap(),
    );
    env
}
fn call(name: &str, arguments: Vec<Expression>) -> Expression {
    Expression::new(
        E::Call {
            function: FunctionId::Library {
                library: id(),
                name: name.parse().unwrap(),
            },
            arguments,
        },
        C::Eval,
        &Limits::default(),
    )
    .unwrap()
}
struct Native {
    frame: EvaluationFrame,
    result: V,
    calls: Cell<usize>,
    boundary: RefCell<Option<ExpressionCallType>>,
}
impl Native {
    fn new(result: V) -> Self {
        Self {
            frame: EvaluationFrame::default(),
            result,
            calls: Cell::new(0),
            boundary: RefCell::new(None),
        }
    }
}
impl EvaluationContext for Native {
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        self.frame.resolve(reference, path, meter)
    }
    fn outcome(&self, _: &Identifier) -> Option<&EvaluationOutcome> {
        None
    }
    fn template(&self, _: &Digest) -> Option<&PromptTemplate> {
        None
    }
    fn call_typed(
        &self,
        _: &Expression,
        boundary: &ExpressionCallType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        assert_eq!(boundary.library, id());
        assert_eq!(boundary.parameters.len(), arguments.len());
        self.calls.set(self.calls.get() + 1);
        self.boundary.replace(Some(boundary.clone()));
        match &self.result {
            V::Present(v) => Ok(V::Present(meter.copy_value(v)?)),
            state => Ok(state.clone()),
        }
    }
}

#[test]
fn invalid_arguments_precede_dispatch_and_results_are_checked_afterward() {
    let codec = Limits::default();
    let mut env = environment(vec![(
        "accept",
        signature(false, vec![port(P::Integer, true)], port(P::Boolean, true)),
    )]);
    let source = ValueReference::Input("value".parse().unwrap());
    env.references.insert(source.clone(), port(P::Json, false));
    let reference = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![],
        },
        C::Eval,
        &codec,
    )
    .unwrap();
    let expression = call("accept", vec![reference]);
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    let mut native = Native::new(V::Present(Value::Bool(true)));
    for (value, expected) in [
        (V::Present(Value::Text("wrong".into())), Error::OperandType),
        (V::Absent, Error::AbsentOperand),
    ] {
        native
            .frame
            .bind(source.clone(), vec![], Ok(value), &codec)
            .unwrap();
        assert_eq!(checked.evaluate(&native, None, &policy()), Err(expected));
        assert_eq!(native.calls.get(), 0);
    }
    native
        .frame
        .bind(source.clone(), vec![], Ok(V::Pending), &codec)
        .unwrap();
    assert_eq!(
        checked.evaluate(&native, None, &policy()).unwrap().value,
        V::Pending
    );
    assert_eq!(native.calls.get(), 0);
    native
        .frame
        .bind(source, vec![], Ok(V::Present(Value::Integer(1))), &codec)
        .unwrap();
    assert_eq!(
        checked.evaluate(&native, None, &policy()).unwrap().value,
        V::Present(Value::Bool(true))
    );
    for result in [V::Present(Value::Integer(1)), V::Absent, V::Pending] {
        native.result = result.clone();
        let expected = if result == V::Absent {
            Error::AbsentOperand
        } else {
            Error::OperandType
        };
        assert_eq!(checked.evaluate(&native, None, &policy()), Err(expected));
    }
    assert_eq!(native.calls.get(), 4);
}

#[test]
fn expected_results_instantiate_the_boundary_supplied_to_native_code() {
    let env = environment(vec![(
        "generate",
        signature(true, vec![], Port::new(variable(), true)),
    )]);
    let expression = call("generate", vec![]);
    let expected = port(P::String, true);
    let checked = CheckedExpression::new(
        &expression,
        C::Eval,
        &env,
        Some(&expected),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(checked.analysis().calls()[0].returns, expected);
    let mut native = Native::new(V::Present(Value::Text("value".into())));
    checked.evaluate(&native, None, &policy()).unwrap();
    assert_eq!(native.boundary.borrow().as_ref().unwrap().returns, expected);
    native.result = V::Present(Value::Integer(1));
    assert_eq!(
        checked.evaluate(&native, None, &policy()),
        Err(Error::OperandType)
    );
}

#[test]
fn optional_values_and_instantiated_callback_signatures_reach_native_code() {
    let codec = Limits::default();
    let callback = T::new(
        K::Function {
            parameters: vec![port(P::Integer, true)],
            returns: Box::new(port(P::Integer, true)),
        },
        TypeContext::Signature,
        &codec,
    )
    .unwrap();
    let mut env = environment(vec![
        (
            "apply",
            signature(
                false,
                vec![port(P::Integer, false), Port::new(callback.clone(), true)],
                port(P::Integer, false),
            ),
        ),
        (
            "identity",
            signature(
                true,
                vec![Port::new(variable(), true)],
                Port::new(variable(), true),
            ),
        ),
    ]);
    let source = ValueReference::Input("value".parse().unwrap());
    env.references
        .insert(source.clone(), port(P::Integer, false));
    let reference = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![],
        },
        C::Eval,
        &codec,
    )
    .unwrap();
    let function = Expression::new(
        E::FunctionRef {
            library: id(),
            name: "identity".parse().unwrap(),
        },
        C::Eval,
        &codec,
    )
    .unwrap();
    let expression = call("apply", vec![reference, function]);
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    let mut native = Native::new(V::Absent);
    native
        .frame
        .bind(source, vec![], Ok(V::Absent), &codec)
        .unwrap();
    assert_eq!(
        checked.evaluate(&native, None, &policy()).unwrap().value,
        V::Absent
    );
    let boundary = native.boundary.borrow();
    let boundary = boundary.as_ref().unwrap();
    assert_eq!(boundary.parameters[0], port(P::Integer, false));
    assert_eq!(boundary.parameters[1].value_type(), &callback);
    assert_eq!(boundary.returns, port(P::Integer, false));
}

#[test]
fn generic_boundaries_are_fresh_and_sorted_by_call_site() {
    use htlk_executable::ScalarLiteral;
    let env = environment(vec![(
        "identity",
        signature(
            true,
            vec![Port::new(variable(), true)],
            Port::new(variable(), true),
        ),
    )]);
    let expression = Expression::new(
        E::List(vec![
            call(
                "identity",
                vec![call(
                    "identity",
                    vec![Expression::literal(ScalarLiteral::Integer(1))],
                )],
            ),
            call(
                "identity",
                vec![Expression::literal(ScalarLiteral::String("text".into()))],
            ),
        ]),
        C::Eval,
        &Limits::default(),
    )
    .unwrap();
    let checked =
        CheckedExpression::new(&expression, C::Eval, &env, None, &Limits::default()).unwrap();
    let calls = checked.analysis().calls();
    assert_eq!(calls.len(), 3);
    for (call, (path, ty)) in calls.iter().zip([
        (vec![0], P::Integer),
        (vec![0, 0], P::Integer),
        (vec![1], P::String),
    ]) {
        assert_eq!(call.expression_path, path);
        assert_eq!(call.parameters, vec![port(ty, true)]);
        assert_eq!(call.returns, port(ty, true));
    }
}

struct CallbackNative {
    arguments: Vec<V>,
    result: V,
    calls: Cell<usize>,
    recurse: bool,
    boundary: RefCell<Option<ExpressionCallType>>,
}
impl EvaluationContext for CallbackNative {
    fn resolve(
        &self,
        _: &ValueReference,
        _: &[PathStep],
        _: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        Err(Error::UnknownReference)
    }
    fn outcome(&self, _: &Identifier) -> Option<&EvaluationOutcome> {
        None
    }
    fn template(&self, _: &Digest) -> Option<&PromptTemplate> {
        None
    }
    fn call_typed(
        &self,
        _: &Expression,
        boundary: &ExpressionCallType,
        _: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        self.boundary.replace(Some(boundary.clone()));
        boundary.invoke_callback(0, &self.arguments, self, None, meter)?;
        Ok(V::Present(Value::Bool(true)))
    }
    fn call_callback(
        &self,
        callback: &htlk_executable::ExpressionCallbackType,
        _: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        assert_eq!(callback.name.as_str(), "callback");
        self.calls.set(self.calls.get() + 1);
        if self.recurse {
            return self.boundary.borrow().as_ref().unwrap().invoke_callback(
                0,
                &self.arguments,
                self,
                None,
                meter,
            );
        }
        match &self.result {
            V::Present(value) => Ok(V::Present(meter.copy_value(value)?)),
            other => Ok(other.clone()),
        }
    }
}
fn callback_fixture() -> (ExpressionTypeEnvironment, Expression) {
    let broad = T::new(
        K::Union(vec![T::primitive(P::Integer), T::primitive(P::String)]),
        TypeContext::Value,
        &Limits::default(),
    )
    .unwrap();
    let expected = T::new(
        K::Function {
            parameters: vec![port(P::Integer, true)],
            returns: Box::new(Port::new(broad.clone(), false)),
        },
        TypeContext::Signature,
        &Limits::default(),
    )
    .unwrap();
    let env = environment(vec![
        (
            "apply",
            signature(
                false,
                vec![Port::new(expected, true)],
                port(P::Boolean, true),
            ),
        ),
        (
            "callback",
            signature(false, vec![Port::new(broad, false)], port(P::Integer, true)),
        ),
    ]);
    let function = Expression::new(
        E::FunctionRef {
            library: id(),
            name: "callback".parse().unwrap(),
        },
        C::Eval,
        &Limits::default(),
    )
    .unwrap();
    (env, call("apply", vec![function]))
}
#[test]
fn callback_invocation_enforces_actual_and_expected_variance_boundaries() {
    let (env, expression) = callback_fixture();
    let checked =
        CheckedExpression::new(&expression, C::Eval, &env, None, &Limits::default()).unwrap();
    let mut native = CallbackNative {
        arguments: vec![V::Present(Value::Integer(1))],
        result: V::Present(Value::Integer(1000)),
        calls: Cell::new(0),
        recurse: false,
        boundary: RefCell::new(None),
    };
    let mut budget = policy();
    budget.max_output_bytes = 1;
    // The callback's three-byte integer is an intermediate; the outer Boolean fits.
    assert_eq!(
        checked.evaluate(&native, None, &budget).unwrap().value,
        V::Present(Value::Bool(true))
    );
    for result in [
        V::Present(Value::Text(
            "valid expected union, wrong actual return".into(),
        )),
        V::Absent,
        V::Pending,
    ] {
        native.result = result.clone();
        let error = if result == V::Absent {
            Error::AbsentOperand
        } else {
            Error::OperandType
        };
        assert_eq!(checked.evaluate(&native, None, &policy()), Err(error));
    }
    assert_eq!(native.calls.get(), 4);
    native.result = V::Present(Value::Integer(1));
    for argument in [
        V::Present(Value::Text(
            "actual accepts strings; caller requires integer".into(),
        )),
        V::Absent,
        V::Pending,
    ] {
        native.arguments = vec![argument.clone()];
        let error = if argument == V::Absent {
            Error::AbsentOperand
        } else {
            Error::OperandType
        };
        assert_eq!(checked.evaluate(&native, None, &policy()), Err(error));
        assert_eq!(native.calls.get(), 4);
    }
    native.arguments.clear();
    assert_eq!(
        checked.evaluate(&native, None, &policy()),
        Err(Error::OperandType)
    );
    assert_eq!(native.calls.get(), 4);
}
#[test]
fn callback_recursion_shares_the_expression_depth_budget() {
    let (env, expression) = callback_fixture();
    let checked =
        CheckedExpression::new(&expression, C::Eval, &env, None, &Limits::default()).unwrap();
    let native = CallbackNative {
        arguments: vec![V::Present(Value::Integer(1))],
        result: V::Absent,
        calls: Cell::new(0),
        recurse: true,
        boundary: RefCell::new(None),
    };
    let mut budget = policy();
    budget.max_expression_depth = 8;
    let error = checked.evaluate(&native, None, &budget).unwrap_err();
    assert_eq!(error, Error::Limit("callback depth"));
    assert_eq!(error.code(), "E_EXPRESSION_LIMIT");
    assert!(native.calls.get() > 0 && native.calls.get() < 8);
}

#[test]
fn callback_depth_on_controlled_stacks() {
    const CHILD: &str = "HTLK_CALLBACK_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(|| {
                let (env, expression) = callback_fixture();
                let codec = Limits {
                    max_depth: 128,
                    ..Limits::default()
                };
                let checked =
                    CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
                let native = CallbackNative {
                    arguments: vec![V::Present(Value::Integer(1))],
                    result: V::Absent,
                    calls: Cell::new(0),
                    recurse: true,
                    boundary: RefCell::new(None),
                };
                let mut budget = policy();
                budget.max_steps = 10000000;
                budget.max_expression_depth = u64::MAX;
                assert_eq!(
                    checked.evaluate(&native, None, &budget),
                    Err(Error::Limit("callback depth"))
                );
                assert_eq!(native.calls.get(), 126);
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "callback_depth_on_controlled_stacks",
                "--nocapture",
            ])
            .env(CHILD, size.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stack {size}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn linked_apply(
    _: &[EvaluationArgument],
    context: &htlk_executable::NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    context.invoke_callback(0, &[V::Present(Value::Integer(1))], meter)?;
    Ok(V::Present(Value::Bool(true)))
}
fn linked_echo(
    arguments: &[EvaluationArgument],
    _: &htlk_executable::NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    let [EvaluationArgument::Value(V::Present(value))] = arguments else {
        return Err(Error::OperandType);
    };
    Ok(V::Present(meter.copy_value(value)?))
}
fn linked_bad(
    _: &[EvaluationArgument],
    _: &htlk_executable::NativeCallContext<'_>,
    _: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    Ok(V::Present(Value::Bool(false)))
}

#[test]
fn registry_pins_full_manifests_and_routes_callbacks_without_frame_dispatch() {
    use htlk_executable::{NativeFunction, NativeRegistry, NativeRegistryError};
    let codec = Limits::default();
    let (env, expression) = callback_fixture();
    let manifest = env.libraries[&id()].clone();
    let mut registry = NativeRegistry::new(&codec).unwrap();
    registry
        .register(
            manifest.clone(),
            [
                (
                    "apply".parse().unwrap(),
                    NativeFunction::new(linked_apply, 1).unwrap(),
                ),
                (
                    "callback".parse().unwrap(),
                    NativeFunction::new(linked_echo, 1).unwrap(),
                ),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();
    assert_eq!(registry.library(&id()), Some(&manifest));
    let frame = Native::new(V::Present(Value::Bool(false)));
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    assert_eq!(
        registry
            .evaluate(&checked, &frame, None, &policy())
            .unwrap()
            .value,
        V::Present(Value::Bool(true))
    );
    assert_eq!(frame.calls.get(), 0);
    let changed = Library::new(
        "test".into(),
        "different".into(),
        id(),
        manifest.functions().to_vec(),
        &codec,
    )
    .unwrap();
    assert_eq!(
        registry.verify_library(&changed),
        Err(NativeRegistryError::ManifestMismatch)
    );
    let mut changed_env = env.clone();
    changed_env.libraries.insert(id(), changed);
    let checked = CheckedExpression::new(&expression, C::Eval, &changed_env, None, &codec).unwrap();
    assert_eq!(
        registry.evaluate(&checked, &frame, None, &policy()),
        Err(Error::Registry(Box::new(
            NativeRegistryError::ManifestMismatch
        )))
    );
    let subset = Library::new(
        "test".into(),
        "1".into(),
        id(),
        vec![manifest.functions()[0].clone()],
        &codec,
    )
    .unwrap();
    assert_eq!(
        registry.verify_library(&subset),
        Err(NativeRegistryError::ManifestMismatch)
    );
}

#[test]
fn registry_rejects_incomplete_registration_and_checks_native_callback_results() {
    use htlk_executable::{NativeFunction, NativeRegistry, NativeRegistryError};
    let codec = Limits::default();
    let (env, expression) = callback_fixture();
    let manifest = env.libraries[&id()].clone();
    let mut registry = NativeRegistry::new(&codec).unwrap();
    assert_eq!(
        registry.register(manifest.clone(), Default::default()),
        Err(NativeRegistryError::FunctionCoverage)
    );
    assert!(registry.library(&id()).is_none());
    let functions = [
        (
            "apply".parse().unwrap(),
            NativeFunction::new(linked_apply, 1).unwrap(),
        ),
        (
            "callback".parse().unwrap(),
            NativeFunction::new(linked_bad, 1).unwrap(),
        ),
    ]
    .into_iter()
    .collect::<std::collections::BTreeMap<_, _>>();
    registry
        .register(manifest.clone(), functions.clone())
        .unwrap();
    assert_eq!(
        registry.register(manifest, functions),
        Err(NativeRegistryError::DuplicateLibrary)
    );
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    assert_eq!(
        registry.evaluate(&checked, &EvaluationFrame::default(), None, &policy()),
        Err(Error::OperandType)
    );
    assert!(matches!(
        NativeFunction::new(linked_echo, 0),
        Err(NativeRegistryError::InvalidWork)
    ));
}

#[test]
fn registry_charges_dispatch_before_invoking_native_code() {
    use htlk_executable::{NativeFunction, NativeRegistry};
    fn forbidden(
        _: &[EvaluationArgument],
        _: &htlk_executable::NativeCallContext<'_>,
        _: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        panic!("must fail its prepaid dispatch charge");
    }
    let codec = Limits::default();
    let env = environment(vec![(
        "constant",
        signature(false, vec![], port(P::Boolean, true)),
    )]);
    let mut registry = NativeRegistry::new(&codec).unwrap();
    registry
        .register(
            env.libraries[&id()].clone(),
            [(
                "constant".parse().unwrap(),
                NativeFunction::new(forbidden, 100001).unwrap(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
    let expression = call("constant", vec![]);
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    assert!(matches!(
        registry.evaluate(&checked, &EvaluationFrame::default(), None, &policy()),
        Err(Error::Limit(_))
    ));
}

fn higher_fixture() -> (ExpressionTypeEnvironment, Expression) {
    let limits = Limits::default();
    let leaf = T::new(
        K::Function {
            parameters: vec![port(P::Boolean, true)],
            returns: Box::new(port(P::Boolean, true)),
        },
        TypeContext::Signature,
        &limits,
    )
    .unwrap();
    let higher = T::new(
        K::Function {
            parameters: vec![Port::new(leaf.clone(), true)],
            returns: Box::new(port(P::Boolean, true)),
        },
        TypeContext::Signature,
        &limits,
    )
    .unwrap();
    let env = environment(vec![
        (
            "outer",
            signature(
                false,
                vec![Port::new(higher, true), Port::new(leaf.clone(), true)],
                port(P::Boolean, true),
            ),
        ),
        (
            "apply",
            signature(false, vec![Port::new(leaf, true)], port(P::Boolean, true)),
        ),
        (
            "leaf",
            signature(
                true,
                vec![Port::new(variable(), true)],
                Port::new(variable(), true),
            ),
        ),
    ]);
    let refs = ["apply", "leaf"]
        .into_iter()
        .map(|name| {
            Expression::new(
                E::FunctionRef {
                    library: id(),
                    name: name.parse().unwrap(),
                },
                C::Eval,
                &limits,
            )
            .unwrap()
        })
        .collect();
    (env, call("outer", refs))
}
fn forward_higher(
    _: &[EvaluationArgument],
    context: &htlk_executable::NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    context.invoke_callback_with(0, &[htlk_executable::CallbackArgument::Function(1)], meter)
}
fn apply_higher(
    _: &[EvaluationArgument],
    context: &htlk_executable::NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    assert_eq!(context.expression_path(), &[0]);
    context.invoke_callback(0, &[V::Present(Value::Bool(true))], meter)
}
fn higher_leaf(
    arguments: &[EvaluationArgument],
    context: &htlk_executable::NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    assert_eq!(context.expression_path(), &[1]);
    assert_eq!(context.parameters(), &[port(P::Boolean, true)]);
    assert_eq!(context.returns(), &port(P::Boolean, true));
    linked_echo(arguments, context, meter)
}
#[test]
fn registry_forwards_higher_order_capabilities_with_ground_signatures_and_original_locations() {
    use htlk_executable::{NativeFunction, NativeRegistry};
    let limits = Limits::default();
    let (env, expression) = higher_fixture();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    registry
        .register(
            env.libraries[&id()].clone(),
            [
                (
                    "outer".parse().unwrap(),
                    NativeFunction::new(forward_higher, 1).unwrap(),
                ),
                (
                    "apply".parse().unwrap(),
                    NativeFunction::new(apply_higher, 1).unwrap(),
                ),
                (
                    "leaf".parse().unwrap(),
                    NativeFunction::new(higher_leaf, 1).unwrap(),
                ),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &limits).unwrap();
    assert_eq!(
        registry
            .evaluate(&checked, &EvaluationFrame::default(), None, &policy())
            .unwrap()
            .value,
        V::Present(Value::Bool(true))
    );
}
#[test]
fn incompatible_higher_order_forwarding_fails_before_callee_dispatch() {
    use htlk_executable::{NativeFunction, NativeRegistry};
    fn wrong(
        _: &[EvaluationArgument],
        context: &htlk_executable::NativeCallContext<'_>,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        context.invoke_callback_with(0, &[htlk_executable::CallbackArgument::Function(0)], meter)
    }
    fn forbidden(
        _: &[EvaluationArgument],
        _: &htlk_executable::NativeCallContext<'_>,
        _: &mut EvaluationMeter<'_>,
    ) -> Result<V, Error> {
        panic!("incompatible callable must not be invoked");
    }
    let limits = Limits::default();
    let (env, expression) = higher_fixture();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    registry
        .register(
            env.libraries[&id()].clone(),
            [
                (
                    "outer".parse().unwrap(),
                    NativeFunction::new(wrong, 1).unwrap(),
                ),
                (
                    "apply".parse().unwrap(),
                    NativeFunction::new(forbidden, 1).unwrap(),
                ),
                (
                    "leaf".parse().unwrap(),
                    NativeFunction::new(higher_leaf, 1).unwrap(),
                ),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &limits).unwrap();
    assert_eq!(
        registry.evaluate(&checked, &EvaluationFrame::default(), None, &policy()),
        Err(Error::OperandType)
    );
}
