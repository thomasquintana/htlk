//! Static expression names, presence, inference and callback constraints.
use htlk_cbor::Limits;
use htlk_executable::digest::Digest;
use htlk_executable::{
    BinaryOperator as B, CoreFunction, Expression as E, ExpressionContext as C,
    ExpressionKind as K, ExpressionTypeEnvironment as Env, ExpressionTypeError as Error,
    FunctionId, FunctionSignature as Sig, Library, PathStep, Port, PrimitiveType as P,
    RuntimeTypeCheckKind as Check, ScalarLiteral as L, TypeContext as TC, ValueReference as R,
    ValueType as T, ValueTypeKind as TK, check_condition, check_expression,
};
fn d() -> Digest {
    Digest::from_bytes([1; 32])
}
fn t(p: P) -> T {
    T::primitive(p)
}
fn port(p: P) -> Port {
    Port::new(t(p), true)
}
fn ty(k: TK) -> T {
    T::new(k, TC::Signature, &Limits::default()).unwrap()
}
fn var(name: &str) -> T {
    ty(TK::Var(name.parse().unwrap()))
}
fn expr(k: K) -> E {
    E::new(k, C::LoopUntil, &Limits::default()).unwrap()
}
fn input(name: &str) -> E {
    expr(K::Ref {
        source: R::Input(name.parse().unwrap()),
        path: vec![],
    })
}
fn call(name: &str, args: Vec<E>) -> E {
    expr(K::Call {
        function: FunctionId::Library {
            library: d(),
            name: name.parse().unwrap(),
        },
        arguments: args,
    })
}
fn callback(name: &str) -> E {
    expr(K::FunctionRef {
        library: d(),
        name: name.parse().unwrap(),
    })
}
fn library(env: &mut Env, signatures: Vec<(&str, Sig)>) {
    env.libraries.insert(
        d(),
        Library::new(
            "test".into(),
            "1".into(),
            d(),
            signatures
                .into_iter()
                .map(|(n, s)| (n.parse().unwrap(), s))
                .collect(),
            &Limits::default(),
        )
        .unwrap(),
    );
}
fn signature(vars: &[&str], params: Vec<Port>, returns: Port) -> Sig {
    Sig::new(
        vars.iter().map(|v| v.parse().unwrap()).collect(),
        params,
        returns,
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn all_branches_are_resolved_and_conditions_are_boolean() {
    let l = Limits::default();
    let mut env = Env::default();
    let e = expr(K::Binary {
        operator: B::And,
        left: Box::new(E::literal(L::Boolean(false))),
        right: Box::new(input("unknown")),
    });
    assert_eq!(
        check_condition(&e, C::Preconditions, &env, &l).unwrap_err(),
        Error::UnknownReference
    );
    env.references
        .insert(R::Input("unknown".parse().unwrap()), port(P::Boolean));
    assert_eq!(
        check_condition(&e, C::Preconditions, &env, &l)
            .unwrap()
            .result(),
        &port(P::Boolean)
    );
    assert_eq!(
        check_condition(&E::literal(L::Integer(1)), C::Preconditions, &env, &l).unwrap_err(),
        Error::TypeMismatch
    );
    assert_eq!(
        check_expression(
            &expr(K::Status("missing".parse().unwrap())),
            C::LoopUntil,
            &env,
            None,
            &l
        )
        .unwrap_err(),
        Error::UnknownOutcome
    );
    let output = R::Output {
        node: "worker".parse().unwrap(),
        port: "value".parse().unwrap(),
    };
    env.references.insert(output.clone(), port(P::String));
    let e = expr(K::Ref {
        source: output,
        path: vec![],
    });
    assert!(matches!(
        check_expression(&e, C::Preconditions, &env, None, &l),
        Err(Error::Expression(_))
    ));
}

#[test]
fn presence_and_unknown_json_generate_explicit_checks() {
    let l = Limits::default();
    let mut env = Env::default();
    env.references.insert(
        R::Input("maybe".parse().unwrap()),
        Port::new(t(P::Boolean), false),
    );
    let result = check_condition(&input("maybe"), C::Preconditions, &env, &l).unwrap();
    assert!(!result.result().required());
    assert!(
        result
            .runtime_checks()
            .iter()
            .any(|c| c.kind == Check::Present)
    );
    env.references
        .insert(R::Input("data".parse().unwrap()), port(P::Json));
    let result = check_condition(&input("data"), C::Preconditions, &env, &l).unwrap();
    assert!(
        result
            .runtime_checks()
            .iter()
            .any(|c| matches!(c.kind, Check::Value(_)))
    );
    let e = expr(K::List(vec![input("maybe")]));
    let result = check_expression(&e, C::Eval, &env, None, &l).unwrap();
    assert!(
        result
            .runtime_checks()
            .iter()
            .any(|c| c.expression_path == [0] && c.kind == Check::Present)
    );
    let e = expr(K::Record(vec![("optional".into(), input("maybe"))]));
    let result = check_expression(&e, C::Eval, &env, None, &l).unwrap();
    let TK::Record(fields) = result.result().value_type().kind() else {
        panic!()
    };
    assert!(!fields[0].1.required());
}

#[test]
fn comparisons_use_scalar_representations_not_implicit_conversion() {
    let l = Limits::default();
    let mut env = Env::default();
    let binary = |a, b| {
        expr(K::Binary {
            operator: B::Eq,
            left: Box::new(a),
            right: Box::new(b),
        })
    };
    assert_eq!(
        check_expression(
            &binary(E::literal(L::Integer(1)), E::literal(L::String("1".into()))),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap_err(),
        Error::OperandType
    );
    env.references.insert(
        R::Input("x".parse().unwrap()),
        Port::new(ty(TK::Union(vec![t(P::String), t(P::Integer)])), true),
    );
    let e = binary(input("x"), E::literal(L::Integer(1)));
    assert!(
        check_expression(&e, C::Eval, &env, None, &l)
            .unwrap()
            .runtime_checks()
            .iter()
            .any(|c| c.kind == Check::Comparison(B::Eq))
    );
    let e = expr(K::Call {
        function: FunctionId::Core(CoreFunction::Length),
        arguments: vec![E::literal(L::Boolean(true))],
    });
    assert_eq!(
        check_expression(&e, C::Eval, &env, None, &l).unwrap_err(),
        Error::OperandType
    );
}

#[test]
fn projections_keep_union_presence_and_schema_obligations() {
    let l = Limits::default();
    let mut env = Env::default();
    let record = |name: &str, p| ty(TK::Record(vec![(name.into(), port(p))]));
    env.references.insert(
        R::Input("x".parse().unwrap()),
        Port::new(
            ty(TK::Union(vec![
                record("a", P::Integer),
                record("b", P::String),
                t(P::Null),
            ])),
            true,
        ),
    );
    let project = |name: &str| {
        expr(K::Ref {
            source: R::Input("x".parse().unwrap()),
            path: vec![PathStep::Field(name.into())],
        })
    };
    let result = check_expression(&project("a"), C::Eval, &env, None, &l).unwrap();
    assert_eq!(result.result(), &Port::new(t(P::Integer), false));
    assert_eq!(
        check_expression(&project("missing"), C::Eval, &env, None, &l).unwrap_err(),
        Error::InvalidProjection
    );
    env.outcomes.insert("worker".parse().unwrap());
    let e = expr(K::Get {
        value: Box::new(expr(K::Error("worker".parse().unwrap()))),
        path: vec![PathStep::Field("code".into())],
    });
    assert_eq!(
        check_expression(&e, C::LoopUntil, &env, None, &l)
            .unwrap()
            .result(),
        &port(P::String)
    );
    env.references.insert(
        R::Input("x".parse().unwrap()),
        Port::new(ty(TK::Schema(d())), true),
    );
    assert!(
        check_expression(&project("a"), C::Eval, &env, None, &l)
            .unwrap()
            .runtime_checks()
            .iter()
            .any(|c| matches!(c.kind, Check::SchemaProjection { .. }))
    );
    env.references.insert(
        R::Input("snapshot".parse().unwrap()),
        port(P::ResourceSnapshot),
    );
    let projected = expr(K::Ref {
        source: R::Input("snapshot".parse().unwrap()),
        path: vec![
            PathStep::Field("contents".into()),
            PathStep::Index(0),
            PathStep::Field("data".into()),
        ],
    });
    assert_eq!(
        check_expression(&projected, C::Eval, &env, None, &l)
            .unwrap()
            .result(),
        &Port::new(t(P::Bytes), false)
    );
}

#[test]
fn generic_calls_resolve_from_arguments_and_expected_results() {
    let l = Limits::default();
    let mut env = Env::default();
    library(
        &mut env,
        vec![
            (
                "identity",
                signature(
                    &["t"],
                    vec![Port::new(var("t"), true)],
                    Port::new(var("t"), true),
                ),
            ),
            (
                "same",
                signature(
                    &["t"],
                    vec![Port::new(var("t"), true), Port::new(var("t"), true)],
                    Port::new(var("t"), true),
                ),
            ),
            ("make", signature(&["t"], vec![], Port::new(var("t"), true))),
        ],
    );
    assert_eq!(
        check_expression(
            &call("identity", vec![E::literal(L::Integer(2))]),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap()
        .result(),
        &port(P::Integer)
    );
    assert_eq!(
        check_expression(
            &call(
                "same",
                vec![E::literal(L::Integer(2)), E::literal(L::String("x".into()))]
            ),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap_err(),
        Error::TypeMismatch
    );
    assert_eq!(
        check_expression(&call("make", vec![]), C::Eval, &env, None, &l).unwrap_err(),
        Error::UnresolvedGeneric
    );
    assert_eq!(
        check_condition(&call("make", vec![]), C::Eval, &env, &l)
            .unwrap()
            .result(),
        &port(P::Boolean)
    );
    let e = call("identity", vec![expr(K::List(vec![]))]);
    let expected = Port::new(ty(TK::List(Box::new(t(P::String)))), true);
    assert_eq!(
        check_expression(&e, C::Eval, &env, Some(&expected), &l)
            .unwrap()
            .result(),
        &expected
    );
    assert_eq!(
        check_expression(&e, C::Eval, &env, None, &l).unwrap_err(),
        Error::UnresolvedGeneric
    );
}

#[test]
fn callbacks_are_freshly_instantiated_and_presence_is_variant_checked() {
    let l = Limits::default();
    let mut env = Env::default();
    let fn_type = ty(TK::Function {
        parameters: vec![Port::new(var("t"), true)],
        returns: Box::new(Port::new(var("t"), true)),
    });
    library(
        &mut env,
        vec![
            (
                "apply",
                signature(
                    &["t"],
                    vec![Port::new(var("t"), true), Port::new(fn_type, true)],
                    Port::new(var("t"), true),
                ),
            ),
            (
                "identity",
                signature(
                    &["t"],
                    vec![Port::new(var("t"), true)],
                    Port::new(var("t"), true),
                ),
            ),
            (
                "wrong",
                signature(&[], vec![port(P::String)], port(P::String)),
            ),
            (
                "absent",
                signature(&[], vec![port(P::Integer)], Port::new(t(P::Integer), false)),
            ),
        ],
    );
    let e = call(
        "apply",
        vec![E::literal(L::Integer(2)), callback("identity")],
    );
    let result = check_expression(&e, C::Eval, &env, None, &l).unwrap();
    assert_eq!(result.result(), &port(P::Integer));
    assert!(
        result
            .nodes()
            .iter()
            .any(|n| n.callable && n.expression_path == [1])
    );
    for name in ["wrong", "absent"] {
        assert_eq!(
            check_expression(
                &call("apply", vec![E::literal(L::Integer(2)), callback(name)]),
                C::Eval,
                &env,
                None,
                &l
            )
            .unwrap_err(),
            Error::CallbackMismatch
        );
    }
    assert_eq!(
        check_expression(&callback("identity"), C::Eval, &env, None, &l).unwrap_err(),
        Error::CallableAsValue
    );
    assert_eq!(
        check_expression(
            &call("identity", vec![callback("identity")]),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap_err(),
        Error::CallableAsValue
    );
}

#[test]
fn recursive_generic_equations_are_rejected() {
    let l = Limits::default();
    let mut env = Env::default();
    let recursive = ty(TK::Function {
        parameters: vec![Port::new(ty(TK::List(Box::new(var("t")))), true)],
        returns: Box::new(Port::new(var("t"), true)),
    });
    library(
        &mut env,
        vec![
            (
                "outer",
                signature(&["t"], vec![Port::new(recursive, true)], port(P::Boolean)),
            ),
            (
                "inner",
                signature(
                    &["u"],
                    vec![Port::new(var("u"), true)],
                    Port::new(ty(TK::List(Box::new(var("u")))), true),
                ),
            ),
        ],
    );
    assert_eq!(
        check_expression(
            &call("outer", vec![callback("inner")]),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap_err(),
        Error::RecursiveGeneric
    );
}

#[test]
fn structural_records_allow_extra_fields_but_require_declared_coverage() {
    let l = Limits::default();
    let env = Env::default();
    let expected = Port::new(ty(TK::Record(vec![("a".into(), port(P::String))])), true);
    let e = expr(K::Record(vec![
        ("a".into(), E::literal(L::String("x".into()))),
        ("b".into(), E::literal(L::Integer(1))),
    ]));
    assert!(check_expression(&e, C::Eval, &env, Some(&expected), &l).is_ok());
    assert_eq!(
        check_expression(&expr(K::Record(vec![])), C::Eval, &env, Some(&expected), &l).unwrap_err(),
        Error::TypeMismatch
    );
    let expected = Port::new(ty(TK::Map(Box::new(t(P::Integer)))), true);
    assert_eq!(
        check_expression(&e, C::Eval, &env, Some(&expected), &l).unwrap_err(),
        Error::TypeMismatch
    );
}

#[test]
fn manifests_templates_and_named_argument_types_are_checked() {
    use htlk_executable::{PromptTemplate, TemplatePart};
    let l = Limits::default();
    let mut env = Env::default();
    let template = PromptTemplate::new(
        vec![("name".parse().unwrap(), port(P::String))],
        vec![TemplatePart::Slot("name".parse().unwrap())],
        &l,
    )
    .unwrap();
    let id = template.digest(&l).unwrap();
    env.templates.insert(id, template.clone());
    let render = |value| {
        expr(K::Render {
            template: id,
            arguments: vec![("name".parse().unwrap(), value)],
        })
    };
    assert_eq!(
        check_expression(
            &render(E::literal(L::String("Ada".into()))),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap()
        .result(),
        &port(P::String)
    );
    assert_eq!(
        check_expression(&render(E::literal(L::Integer(1))), C::Eval, &env, None, &l).unwrap_err(),
        Error::TypeMismatch
    );
    env.templates.clear();
    env.templates.insert(d(), template);
    assert_eq!(
        check_expression(&E::literal(L::Null), C::Eval, &env, None, &l).unwrap_err(),
        Error::IdentityMismatch
    );
}

#[test]
fn derived_type_work_is_bounded_and_analysis_is_deterministic() {
    let l = Limits::default();
    let env = Env::default();
    let e = expr(K::Record(
        (0..50)
            .map(|i| (format!("f{i}"), E::literal(L::Boolean(true))))
            .collect(),
    ));
    let a = check_expression(&e, C::Eval, &env, None, &l).unwrap();
    assert_eq!(check_expression(&e, C::Eval, &env, None, &l).unwrap(), a);
    let tight = Limits {
        max_total_payload_bytes: 2000,
        ..l
    };
    assert!(e.to_value(C::Eval, &tight).is_ok());
    assert!(matches!(
        check_expression(&e, C::Eval, &env, None, &tight),
        Err(Error::InferenceLimit)
    ));
}

#[test]
fn nullable_generics_collect_all_non_null_variants() {
    let l = Limits::default();
    let mut env = Env::default();
    let nullable = ty(TK::Union(vec![var("t"), t(P::Null)]));
    library(
        &mut env,
        vec![(
            "unwrap",
            signature(
                &["t"],
                vec![Port::new(nullable, true)],
                Port::new(var("t"), true),
            ),
        )],
    );
    env.references.insert(
        R::Input("x".parse().unwrap()),
        Port::new(
            ty(TK::Union(vec![t(P::Null), t(P::String), t(P::Integer)])),
            true,
        ),
    );
    let result =
        check_expression(&call("unwrap", vec![input("x")]), C::Eval, &env, None, &l).unwrap();
    assert_eq!(
        result.result().value_type(),
        &ty(TK::Union(vec![t(P::String), t(P::Integer)]))
    );
}

#[test]
fn later_arguments_resolve_earlier_generic_union_constraints() {
    let l = Limits::default();
    let mut env = Env::default();
    let either = ty(TK::Union(vec![var("t"), var("u")]));
    library(
        &mut env,
        vec![
            (
                "pick",
                signature(
                    &["t", "u"],
                    vec![
                        Port::new(either.clone(), true),
                        Port::new(var("t"), true),
                        Port::new(var("u"), true),
                    ],
                    Port::new(var("t"), true),
                ),
            ),
            (
                "ambiguous",
                signature(&["t", "u"], vec![Port::new(either, true)], port(P::Boolean)),
            ),
        ],
    );
    let args = vec![
        E::literal(L::Integer(1)),
        E::literal(L::Integer(2)),
        E::literal(L::String("x".into())),
    ];
    assert_eq!(
        check_expression(&call("pick", args), C::Eval, &env, None, &l)
            .unwrap()
            .result(),
        &port(P::Integer)
    );
    assert_eq!(
        check_expression(
            &call("ambiguous", vec![E::literal(L::Integer(1))]),
            C::Eval,
            &env,
            None,
            &l
        )
        .unwrap_err(),
        Error::AmbiguousGeneric
    );
}

#[test]
fn callback_arguments_are_contravariant_and_returns_covariant() {
    let l = Limits::default();
    let mut env = Env::default();
    let expected = ty(TK::Function {
        parameters: vec![port(P::String)],
        returns: Box::new(Port::new(t(P::Json), false)),
    });
    library(
        &mut env,
        vec![
            (
                "use_fn",
                signature(&[], vec![Port::new(expected, true)], port(P::Boolean)),
            ),
            (
                "wide",
                signature(&[], vec![Port::new(t(P::Json), false)], port(P::String)),
            ),
        ],
    );
    assert!(
        check_expression(
            &call("use_fn", vec![callback("wide")]),
            C::Eval,
            &env,
            None,
            &l
        )
        .is_ok()
    );
}

fn depth_exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let mut bytes = b"\x82\x63not".repeat(126);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    let expr = E::decode(&bytes, C::Eval, &l).unwrap();
    let checked = check_condition(&expr, C::Eval, &Env::default(), &l).unwrap();
    assert_eq!(checked.result(), &port(P::Boolean));
    assert_eq!(checked.nodes().len(), 127);
    assert_eq!(checked.clone(), checked);
    let mut bytes = b"\x83\x63and\x82\x67literal\xf5".repeat(126);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    let expr = E::decode(&bytes, C::Eval, &l).unwrap();
    assert_eq!(
        check_condition(&expr, C::Eval, &Env::default(), &l)
            .unwrap()
            .nodes()
            .len(),
        253
    );
}
#[test]
fn type_analysis_on_controlled_stacks() {
    const CHILD: &str = "HTLK_TYPE_CHECK_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(depth_exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "type_analysis_on_controlled_stacks",
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
