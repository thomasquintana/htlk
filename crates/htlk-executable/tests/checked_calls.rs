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
