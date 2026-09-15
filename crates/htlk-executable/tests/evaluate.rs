//! Native evaluator behavior, state precedence, and exact resource boundaries.
use htlk_cbor::{FiniteFloat, Limits, Map, Value};
use htlk_executable::digest::Digest;
use htlk_executable::{
    BinaryOperator as B, CoreFunction as C, EvaluationArgument as A, EvaluationContext,
    EvaluationError as Err, EvaluationFrame as Frame, EvaluationMeter,
    EvaluationOutcome as Outcome, EvaluationValue as V, EvaluatorLimits, Expression as E,
    ExpressionContext as Context, ExpressionKind as K, FunctionId as F, Identifier, PathStep, Port,
    PrimitiveType as P, PromptTemplate, ScalarLiteral as S, TemplatePart, ValueReference as R,
    ValueType, evaluate,
};

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
fn expression(kind: K) -> E {
    E::new(kind, Context::LoopUntil, &Limits::default()).unwrap()
}
fn input(name: &str) -> E {
    expression(K::Ref {
        source: R::Input(name.parse().unwrap()),
        path: vec![],
    })
}
fn core(f: C, x: E) -> E {
    expression(K::Call {
        function: F::Core(f),
        arguments: vec![x],
    })
}
fn binary(op: B, a: E, b: E) -> E {
    expression(K::Binary {
        operator: op,
        left: Box::new(a),
        right: Box::new(b),
    })
}
fn run(e: &E, frame: &impl EvaluationContext) -> Result<V, Err> {
    Ok(evaluate(e, Context::LoopUntil, frame, &Limits::default(), &policy())?.value)
}
fn bind(frame: &mut Frame, name: &str, state: Result<V, Err>) {
    frame
        .bind(
            R::Input(name.parse().unwrap()),
            vec![],
            state,
            &Limits::default(),
        )
        .unwrap();
}

#[test]
fn lazy_booleans_preserve_pending_error_and_absence() {
    let mut f = Frame::default();
    bind(&mut f, "pending", Ok(V::Pending));
    bind(&mut f, "unavailable", Err(Err::UnavailableSource));
    bind(&mut f, "absent", Ok(V::Absent));
    assert_eq!(
        run(
            &binary(B::And, E::literal(S::Boolean(false)), input("unavailable")),
            &f
        )
        .unwrap(),
        V::Present(Value::Bool(false))
    );
    assert_eq!(
        run(
            &binary(B::Or, E::literal(S::Boolean(true)), input("unavailable")),
            &f
        )
        .unwrap(),
        V::Present(Value::Bool(true))
    );
    assert_eq!(
        run(&binary(B::And, input("pending"), input("unavailable")), &f).unwrap(),
        V::Pending
    );
    assert_eq!(
        run(&core(C::Present, input("unavailable")), &f),
        Err(Err::UnavailableSource)
    );
    assert_eq!(
        run(&core(C::Present, input("absent")), &f).unwrap(),
        V::Present(Value::Bool(false))
    );
    assert_eq!(
        run(&core(C::Present, E::literal(S::Null)), &f).unwrap(),
        V::Present(Value::Bool(true))
    );
    assert_eq!(
        run(&binary(B::Eq, input("absent"), E::literal(S::Null)), &f),
        Err(Err::AbsentOperand)
    );
    assert_eq!(
        run(&expression(K::Not(Box::new(input("pending")))), &f).unwrap(),
        V::Pending
    );
}

#[test]
fn conditions_require_booleans_and_preserve_pending() {
    use htlk_executable::{ConditionValue, evaluate_condition};
    let mut frame = Frame::default();
    bind(&mut frame, "waiting", Ok(V::Pending));
    assert_eq!(
        evaluate_condition(
            &input("waiting"),
            Context::Preconditions,
            &frame,
            &Limits::default(),
            &policy()
        )
        .unwrap()
        .value,
        ConditionValue::Pending
    );
    assert_eq!(
        evaluate_condition(
            &E::literal(S::Integer(1)),
            Context::Preconditions,
            &frame,
            &Limits::default(),
            &policy()
        )
        .unwrap_err(),
        Err::OperandType
    );
    assert_eq!(
        evaluate_condition(
            &E::literal(S::Boolean(false)),
            Context::Preconditions,
            &frame,
            &Limits::default(),
            &policy()
        )
        .unwrap()
        .value,
        ConditionValue::Ready(false)
    );
}

#[test]
fn scalar_comparisons_do_not_coerce_and_length_counts_unicode_scalars() {
    let f = Frame::default();
    assert_eq!(
        run(
            &binary(
                B::Eq,
                E::literal(S::Integer(1)),
                E::literal(S::Float(FiniteFloat::new(1.0).unwrap()))
            ),
            &f
        ),
        Err(Err::OperandType)
    );
    assert_eq!(
        run(
            &binary(
                B::Lt,
                E::literal(S::Boolean(false)),
                E::literal(S::Boolean(true))
            ),
            &f
        ),
        Err(Err::OperandType)
    );
    assert_eq!(
        run(
            &binary(
                B::Eq,
                E::literal(S::Bytes(vec![1])),
                E::literal(S::Bytes(vec![1]))
            ),
            &f
        ),
        Err(Err::OperandType)
    );
    assert_eq!(
        run(
            &binary(B::Ne, E::literal(S::Null), expression(K::List(vec![]))),
            &f
        )
        .unwrap(),
        V::Present(Value::Bool(true))
    );
    assert_eq!(
        run(&core(C::Length, E::literal(S::String("é😀".into()))), &f).unwrap(),
        V::Present(Value::Integer(2))
    );
    assert_eq!(
        run(&core(C::Length, E::literal(S::Bytes(vec![1, 2, 3]))), &f).unwrap(),
        V::Present(Value::Integer(3))
    );
}

#[test]
fn collections_preserve_null_omit_absence_and_reject_absent_list_elements() {
    let mut f = Frame::default();
    bind(&mut f, "missing", Ok(V::Absent));
    let record = expression(K::Record(vec![
        ("optional".into(), input("missing")),
        ("null".into(), E::literal(S::Null)),
    ]));
    assert_eq!(
        run(&record, &f).unwrap(),
        V::Present(Value::Map(
            Map::try_from_entries([("null".into(), Value::Null)]).unwrap()
        ))
    );
    let get = expression(K::Get {
        value: Box::new(record),
        path: vec![PathStep::Field("optional".into())],
    });
    assert_eq!(run(&get, &f).unwrap(), V::Absent);
    assert_eq!(
        run(&expression(K::List(vec![input("missing")])), &f),
        Err(Err::AbsentOperand)
    );
    let get = expression(K::Get {
        value: Box::new(expression(K::List(vec![E::literal(S::Integer(7))]))),
        path: vec![PathStep::Index(0)],
    });
    assert_eq!(run(&get, &f).unwrap(), V::Present(Value::Integer(7)));
}

#[test]
fn reference_paths_support_explicit_optional_bindings_and_strict_fallback() {
    let mut f = Frame::default();
    bind(
        &mut f,
        "record",
        Ok(V::Present(Value::Map(
            Map::try_from_entries([("known".into(), Value::Integer(4))]).unwrap(),
        ))),
    );
    let projected = |name: &str| {
        expression(K::Ref {
            source: R::Input("record".parse().unwrap()),
            path: vec![PathStep::Field(name.into())],
        })
    };
    assert_eq!(
        run(&projected("known"), &f).unwrap(),
        V::Present(Value::Integer(4))
    );
    assert_eq!(run(&projected("optional"), &f), Err(Err::InvalidProjection));
    f.bind(
        R::Input("record".parse().unwrap()),
        vec![PathStep::Field("optional".into())],
        Ok(V::Absent),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(run(&projected("optional"), &f).unwrap(), V::Absent);
}

#[test]
fn outcomes_and_prompt_rendering_are_native_and_ordered() {
    let mut f = Frame::default();
    let node: Identifier = "worker".parse().unwrap();
    f.set_outcome(node.clone(), Outcome::Pending, &Limits::default())
        .unwrap();
    assert_eq!(
        run(&expression(K::Status(node.clone())), &f).unwrap(),
        V::Pending
    );
    f.set_outcome(
        node.clone(),
        Outcome::Failed {
            code: "E_TEST".into(),
            message: "redacted".into(),
        },
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        run(&expression(K::Status(node.clone())), &f).unwrap(),
        V::Present(Value::Text("failed".into()))
    );
    let error_code = expression(K::Get {
        value: Box::new(expression(K::Error(node))),
        path: vec![PathStep::Field("code".into())],
    });
    assert_eq!(
        run(&error_code, &f).unwrap(),
        V::Present(Value::Text("E_TEST".into()))
    );
    let template = PromptTemplate::new(
        vec![(
            "n".parse().unwrap(),
            Port::new(ValueType::primitive(P::Integer), true),
        )],
        vec![
            TemplatePart::Text("n=".into()),
            TemplatePart::Slot("n".parse().unwrap()),
            TemplatePart::Text("!".into()),
        ],
        &Limits::default(),
    )
    .unwrap();
    let template = f.insert_template(template, &Limits::default()).unwrap();
    let render = expression(K::Render {
        template,
        arguments: vec![("n".parse().unwrap(), E::literal(S::Integer(42)))],
    });
    assert_eq!(
        run(&render, &f).unwrap(),
        V::Present(Value::Text("n=42!".into()))
    );
}

struct Functions(Frame);
impl EvaluationContext for Functions {
    fn resolve(&self, r: &R, p: &[PathStep], m: &mut EvaluationMeter<'_>) -> Result<V, Err> {
        self.0.resolve(r, p, m)
    }
    fn outcome(&self, n: &Identifier) -> Option<&Outcome> {
        self.0.outcome(n)
    }
    fn template(&self, d: &Digest) -> Option<&PromptTemplate> {
        self.0.template(d)
    }
    fn call(
        &self,
        _: Digest,
        name: &Identifier,
        args: &[A],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<V, Err> {
        meter.charge(10)?;
        match (name.as_str(), args) {
            ("optional", [A::Value(V::Absent)]) => Ok(V::Present(Value::Bool(true))),
            ("callback", [A::Function { name, .. }]) => {
                Ok(V::Present(Value::Text(name.as_str().to_owned())))
            }
            _ => Err(Err::UnknownFunction),
        }
    }
}
#[test]
fn native_function_arguments_keep_optional_and_callable_metadata_separate() {
    let d = Digest::from_bytes([1; 32]);
    let mut f = Frame::default();
    bind(&mut f, "optional", Ok(V::Absent));
    let f = Functions(f);
    let call = |name: &str, args| {
        expression(K::Call {
            function: F::Library {
                library: d,
                name: name.parse().unwrap(),
            },
            arguments: args,
        })
    };
    assert_eq!(
        run(&call("optional", vec![input("optional")]), &f).unwrap(),
        V::Present(Value::Bool(true))
    );
    let callback = expression(K::FunctionRef {
        library: d,
        name: "selected".parse().unwrap(),
    });
    assert_eq!(run(&callback, &f), Err(Err::CallableAsValue));
    assert_eq!(
        run(&call("callback", vec![callback]), &f).unwrap(),
        V::Present(Value::Text("selected".into()))
    );
}

#[test]
fn policy_limits_are_fresh_and_small_outputs_can_read_larger_inputs() {
    let f = Frame::default();
    let e = E::literal(S::Boolean(true));
    let mut limits = policy();
    limits.max_steps = 2;
    limits.max_output_bytes = 1;
    let a = evaluate(&e, Context::Eval, &f, &Limits::default(), &limits).unwrap();
    assert_eq!(a.usage.steps, 2);
    assert_eq!(
        evaluate(&e, Context::Eval, &f, &Limits::default(), &limits).unwrap(),
        a
    );
    limits.max_steps = 1;
    assert_eq!(
        evaluate(&e, Context::Eval, &f, &Limits::default(), &limits)
            .unwrap_err()
            .code(),
        "E_EXPRESSION_LIMIT"
    );
    let e = binary(
        B::Gt,
        core(C::Length, E::literal(S::String("long input".into()))),
        E::literal(S::Integer(0)),
    );
    let mut limits = policy();
    limits.max_output_bytes = 1;
    assert_eq!(
        evaluate(&e, Context::Eval, &f, &Limits::default(), &limits)
            .unwrap()
            .value,
        V::Present(Value::Bool(true))
    );
    limits.max_value_bytes = 1;
    assert_eq!(
        evaluate(&e, Context::Eval, &f, &Limits::default(), &limits)
            .unwrap_err()
            .code(),
        "E_EXPRESSION_LIMIT"
    );
    let e = expression(K::Regex {
        pattern: "[a-z]+".into(),
        flags: "i".into(),
    });
    let result = run(&e, &f).unwrap();
    assert!(matches!(result, V::Present(Value::Map(_))));
    let mut limits = policy();
    limits.max_regex_bytes = 1;
    assert_eq!(
        evaluate(&e, Context::Eval, &f, &Limits::default(), &limits)
            .unwrap_err()
            .code(),
        "E_EXPRESSION_LIMIT"
    );
}

fn exercise_depth() {
    let codec = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let mut bytes = b"\x82\x63not".repeat(126);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    let expr = E::decode(&bytes, Context::Eval, &codec).unwrap();
    assert_eq!(
        evaluate(&expr, Context::Eval, &Frame::default(), &codec, &policy())
            .unwrap()
            .value,
        V::Present(Value::Bool(true))
    );
    let mut limits = policy();
    limits.max_expression_depth = 126;
    assert_eq!(
        evaluate(&expr, Context::Eval, &Frame::default(), &codec, &limits)
            .unwrap_err()
            .code(),
        "E_EXPRESSION_LIMIT"
    );
}
#[test]
fn evaluator_depth_on_controlled_stacks() {
    const CHILD: &str = "HTLK_EVALUATOR_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(exercise_depth)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "evaluator_depth_on_controlled_stacks",
                "--nocapture",
            ])
            .env(CHILD, size.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stack {size}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
