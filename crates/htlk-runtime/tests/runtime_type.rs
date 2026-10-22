//! Actual-value admission and declared, variant-aware projection semantics.
use htlk_analyzer::ExpressionTypeEnvironment;
use htlk_cbor::{Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    BuiltinType as P, EvaluatorLimits, PathStep, Port, TypeContext, ValueType as T,
    ValueTypeKind as K,
};
use htlk_runtime::{CheckedExpression, EvaluationFrame};
use htlk_runtime::{
    EvaluationError as Error, EvaluationValue, project_typed_value, validate_typed_value,
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
fn ty(kind: K) -> T {
    T::new(kind, TypeContext::Value, &Limits::default()).unwrap()
}
fn record(fields: Vec<(&str, T, bool)>) -> T {
    ty(K::Record(
        fields
            .into_iter()
            .map(|(name, t, required)| (name.into(), Port::new(t, required)))
            .collect(),
    ))
}
fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(Map::try_from_entries(fields.into_iter().map(|(k, v)| (k.into(), v))).unwrap())
}
fn project(value: &Value, t: &T, fields: &[&str]) -> Result<EvaluationValue, Error> {
    Ok(project_typed_value(
        value,
        t,
        &fields
            .iter()
            .map(|s| PathStep::Field((*s).into()))
            .collect::<Vec<_>>(),
        None,
        &Limits::default(),
        &policy(),
    )?
    .value)
}

#[test]
fn native_type_value_cartesian_product_has_no_coercion_or_container_shape_shortcuts() {
    let l = Limits::default();
    let values = [
        Value::Null,
        Value::Bool(false),
        Value::Integer(-1),
        Value::Integer(9007199254740993),
        Value::Float(htlk_cbor::FiniteFloat::new(1.5).unwrap()),
        Value::Text("yes".into()),
        Value::Text("no".into()),
        Value::Bytes(vec![0, 255]),
        Value::Array(vec![]),
        Value::Array(vec![Value::Integer(1)]),
        Value::Array(vec![Value::Bool(false)]),
        map(vec![]),
        map(vec![("x", Value::Integer(1))]),
        map(vec![("x", Value::Bool(false))]),
    ];
    for value in &values {
        for primitive in [
            P::Null,
            P::Boolean,
            P::Integer,
            P::Float,
            P::String,
            P::Bytes,
            P::Json,
        ] {
            let expected = match primitive {
                P::Null => matches!(value, Value::Null),
                P::Boolean => matches!(value, Value::Bool(_)),
                P::Integer => matches!(value, Value::Integer(_)),
                P::Float => matches!(value, Value::Float(_)),
                P::String => matches!(value, Value::Text(_)),
                P::Bytes => matches!(value, Value::Bytes(_)),
                P::Json => !matches!(value, Value::Bytes(_) | Value::Integer(9007199254740993)),
                _ => unreachable!(),
            };
            assert_eq!(
                validate_typed_value(value, &T::builtin(primitive), None, &l, &policy()).is_ok(),
                expected,
                "{primitive:?} {value:?}"
            );
        }
        for (shape, expected) in [
            (
                ty(K::List(Box::new(T::builtin(P::Integer)))),
                matches!(value,Value::Array(items)if items.iter().all(|v|matches!(v,Value::Integer(_)))),
            ),
            (
                ty(K::Map(Box::new(T::builtin(P::Integer)))),
                matches!(value,Value::Map(items)if items.iter().all(|(_,v)|matches!(v,Value::Integer(_)))),
            ),
            (
                record(vec![("x", T::builtin(P::Integer), true)]),
                matches!(value,Value::Map(items)if matches!(items.get("x"),Some(Value::Integer(_)))),
            ),
            (
                ty(K::Enum(vec!["yes".into()])),
                value == &Value::Text("yes".into()),
            ),
            (
                ty(K::Union(vec![T::builtin(P::Boolean), T::builtin(P::Null)])),
                matches!(value, Value::Bool(_) | Value::Null),
            ),
        ] {
            assert_eq!(
                validate_typed_value(value, &shape, None, &l, &policy()).is_ok(),
                expected,
                "{shape:?} {value:?}"
            );
        }
    }
}

#[test]
fn regex_protocol_shapes_and_projection_steps_preserve_operational_limit_errors() {
    let l = Limits::default();
    for value in [
        Value::Null,
        map(vec![]),
        map(vec![
            ("pattern", Value::Integer(1)),
            ("flags", Value::Text("".into())),
        ]),
        map(vec![
            ("pattern", Value::Text("a".into())),
            ("flags", Value::Text("".into())),
            ("extra", Value::Null),
        ]),
        map(vec![
            ("pattern", Value::Text("[".into())),
            ("flags", Value::Text("".into())),
        ]),
    ] {
        assert_eq!(
            validate_typed_value(&value, &T::builtin(P::Regex), None, &l, &policy()),
            Err(Error::OperandType)
        );
    }
    let regex = map(vec![
        ("pattern", Value::Text("[a-z]{100}".into())),
        ("flags", Value::Text("".into())),
    ]);
    let mut small = policy();
    small.max_regex_compiled_bytes = 1;
    assert!(matches!(
        validate_typed_value(&regex, &T::builtin(P::Regex), None, &l, &small),
        Err(Error::Limit(_))
    ));
    for primitive in [P::McpResourceResult, P::McpPromptResult] {
        for value in [Value::Null, Value::Bytes(vec![]), map(vec![])] {
            assert!(
                validate_typed_value(&value, &T::builtin(primitive), None, &l, &policy()).is_err()
            );
        }
    }
    let array = Value::Array(vec![Value::Integer(7)]);
    let list = ty(K::List(Box::new(T::builtin(P::Integer))));
    for value_type in [&list, &T::builtin(P::Json)] {
        for path in [
            vec![PathStep::Index(0)],
            vec![PathStep::Index(1)],
            vec![PathStep::Index(u64::MAX)],
            vec![PathStep::Field("x".into())],
        ] {
            let actual = project_typed_value(&array, value_type, &path, None, &l, &policy());
            assert_eq!(actual.is_ok(), path == [PathStep::Index(0)]);
        }
    }
    let object = map(vec![("x", Value::Integer(7))]);
    let dictionary = ty(K::Map(Box::new(T::builtin(P::Integer))));
    for value_type in [&dictionary, &T::builtin(P::Json)] {
        for path in [
            vec![PathStep::Field("x".into())],
            vec![PathStep::Field("missing".into())],
            vec![PathStep::Index(0)],
        ] {
            let actual = project_typed_value(&object, value_type, &path, None, &l, &policy());
            assert_eq!(actual.is_ok(), path == [PathStep::Field("x".into())]);
        }
    }
    assert!(
        project_typed_value(
            &Value::Integer(7),
            &T::builtin(P::Integer),
            &[PathStep::Field("x".into())],
            None,
            &l,
            &policy()
        )
        .is_err()
    );
    let schema = ty(K::Schema(htlk_executable::digest::Digest::from_bytes(
        [0; 32],
    )));
    assert_eq!(
        project_typed_value(&Value::Null, &schema, &[], None, &l, &policy()).unwrap_err(),
        Error::UnresolvedType
    );
    let signature = T::new(K::Var("t".parse().unwrap()), TypeContext::Signature, &l).unwrap();
    assert!(validate_typed_value(&Value::Null, &signature, None, &l, &policy()).is_err());
}
#[test]
fn optional_fields_extra_members_and_null_are_distinct() {
    let t = record(vec![("optional", T::builtin(P::Integer), false)]);
    let v = map(vec![("extra", Value::Integer(1))]);
    validate_typed_value(&v, &t, None, &Limits::default(), &policy()).unwrap();
    assert_eq!(
        project(&v, &t, &["optional"]).unwrap(),
        EvaluationValue::Absent
    );
    assert_eq!(project(&v, &t, &["extra"]), Err(Error::InvalidProjection));
    assert_eq!(
        project(&v, &t, &["optional", "nested"]),
        Err(Error::AbsentOperand)
    );
    assert_eq!(
        validate_typed_value(
            &map(vec![("optional", Value::Null)]),
            &t,
            None,
            &Limits::default(),
            &policy()
        ),
        Err(Error::OperandType)
    );
    let required = record(vec![("required", T::builtin(P::Integer), true)]);
    assert_eq!(
        validate_typed_value(&v, &required, None, &Limits::default(), &policy()),
        Err(Error::OperandType)
    );
}
#[test]
fn union_projection_uses_declared_variants_at_each_path_step() {
    let a = record(vec![("a", T::builtin(P::Integer), true)]);
    let b = record(vec![("b", T::builtin(P::Integer), true)]);
    let union = ty(K::Union(vec![a.clone(), b.clone()]));
    let v = map(vec![
        ("a", Value::Integer(1)),
        ("b", Value::Text("undeclared extra".into())),
    ]);
    assert_eq!(
        project(&v, &union, &["b"]).unwrap(),
        EvaluationValue::Absent
    );
    assert_eq!(
        project(&v, &union, &["unknown"]),
        Err(Error::InvalidProjection)
    );
    let nested = ty(K::Union(vec![
        record(vec![("child", a, true)]),
        record(vec![("child", b, true)]),
    ]));
    assert_eq!(
        project(&map(vec![("child", v)]), &nested, &["child", "b"]).unwrap(),
        EvaluationValue::Absent
    );
}

#[test]
fn nested_projection_cannot_reactivate_a_rejected_outer_variant() {
    let a = record(vec![
        ("kind", ty(K::Enum(vec!["a".into()])), true),
        (
            "child",
            record(vec![("a", T::builtin(P::Integer), true)]),
            true,
        ),
    ]);
    let b = record(vec![
        ("kind", ty(K::Enum(vec!["b".into()])), true),
        (
            "child",
            record(vec![("b", T::builtin(P::Integer), true)]),
            true,
        ),
    ]);
    let union = ty(K::Union(vec![a, b]));
    let value = map(vec![
        ("kind", Value::Text("a".into())),
        (
            "child",
            map(vec![("a", Value::Integer(1)), ("b", Value::Integer(2))]),
        ),
    ]);
    assert_eq!(
        project(&value, &union, &["child", "b"]).unwrap(),
        EvaluationValue::Absent
    );
    assert_eq!(
        project(&value, &union, &["child", "a"]).unwrap(),
        EvaluationValue::Present(Value::Integer(1))
    );
    use htlk_executable::{
        CoreFunction, Expression, ExpressionContext, ExpressionKind, FunctionId, ValueReference,
    };
    let l = Limits::default();
    let source = ValueReference::Input("item".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references
        .insert(source.clone(), Port::new(union, true));
    let reference = Expression::new(
        ExpressionKind::Ref {
            source: source.clone(),
            path: vec![PathStep::Field("child".into()), PathStep::Field("b".into())],
        },
        &l,
    )
    .unwrap();
    let expression = Expression::new(
        ExpressionKind::Call {
            function: FunctionId::Core(CoreFunction::Present),
            arguments: vec![reference],
        },
        &l,
    )
    .unwrap();
    let checked =
        CheckedExpression::new(&expression, ExpressionContext::Eval, &env, None, &l).unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .bind(source, vec![], Ok(EvaluationValue::Present(value)), &l)
        .unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &policy()).unwrap().value,
        EvaluationValue::Present(Value::Bool(false))
    );
}
#[test]
fn maps_indices_json_and_limits_are_strict() {
    let t = ty(K::Map(Box::new(T::builtin(P::Integer))));
    assert_eq!(
        project(&map(vec![]), &t, &["missing"]),
        Err(Error::InvalidProjection)
    );
    let list = ty(K::List(Box::new(T::builtin(P::Integer))));
    assert_eq!(
        project_typed_value(
            &Value::Array(vec![]),
            &list,
            &[PathStep::Index(0)],
            None,
            &Limits::default(),
            &policy()
        ),
        Err(Error::InvalidProjection)
    );
    assert_eq!(
        validate_typed_value(
            &Value::Bytes(vec![1]),
            &T::builtin(P::Json),
            None,
            &Limits::default(),
            &policy()
        ),
        Err(Error::OperandType)
    );
    let mut tiny = policy();
    tiny.max_steps = 1;
    assert!(matches!(
        validate_typed_value(
            &Value::Array(vec![Value::Integer(1)]),
            &list,
            None,
            &Limits::default(),
            &tiny
        ),
        Err(Error::Limit(_))
    ));
}

#[test]
fn checked_execution_enforces_dynamic_operands_and_keeps_laziness() {
    use htlk_executable::{
        BinaryOperator, Expression, ExpressionContext as C, ExpressionKind as E, ScalarLiteral,
        ValueReference,
    };
    let codec = Limits::default();
    let source = ValueReference::Input("value".parse().unwrap());
    let reference = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![],
        },
        &codec,
    )
    .unwrap();
    let mut env = ExpressionTypeEnvironment::default();
    env.references
        .insert(source.clone(), Port::new(T::builtin(P::Json), true));
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            source.clone(),
            vec![],
            Ok(EvaluationValue::Present(Value::Integer(1))),
            &codec,
        )
        .unwrap();
    let expression = Expression::new(E::Not(Box::new(reference.clone())), &codec).unwrap();
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &policy()),
        Err(Error::OperandType)
    );
    let lazy = Expression::new(
        E::Binary {
            operator: BinaryOperator::And,
            left: Box::new(Expression::literal(ScalarLiteral::Boolean(false))),
            right: Box::new(expression),
        },
        &codec,
    )
    .unwrap();
    frame
        .bind(
            source.clone(),
            vec![],
            Err(Error::UnavailableSource),
            &codec,
        )
        .unwrap();
    assert_eq!(
        CheckedExpression::new(&lazy, C::Eval, &env, None, &codec)
            .unwrap()
            .evaluate(&frame, None, &policy())
            .unwrap()
            .value,
        EvaluationValue::Present(Value::Bool(false))
    );
    frame
        .bind(source, vec![], Ok(EvaluationValue::Pending), &codec)
        .unwrap();
    assert_eq!(
        CheckedExpression::new(&reference, C::Eval, &env, None, &codec)
            .unwrap()
            .evaluate(&frame, None, &policy())
            .unwrap()
            .value,
        EvaluationValue::Pending
    );
}

#[test]
fn schemas_validate_at_the_prescribed_root() {
    use htlk_analyzer::{NativeSchemaOptions, NativeSchemas, SchemaCatalog, embedded_schema_base};
    use htlk_executable::JsonDocument;
    let codec = Limits::default();
    let document = JsonDocument::new(br#"{"type":"integer","minimum":2}"#, &codec).unwrap();
    let root = embedded_schema_base(&document, &codec).unwrap();
    let t = ty(K::Schema(document.digest()));
    let catalog = SchemaCatalog::new([(root, document)].into_iter().collect(), &codec).unwrap();
    let schemas = NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &codec).unwrap();
    validate_typed_value(&Value::Integer(2), &t, Some(&schemas), &codec, &policy()).unwrap();
    assert_eq!(
        validate_typed_value(&Value::Integer(1), &t, Some(&schemas), &codec, &policy()),
        Err(Error::OperandType)
    );
    assert_eq!(
        validate_typed_value(&Value::Integer(2), &t, None, &codec, &policy()),
        Err(Error::UnresolvedType)
    );
}

#[test]
fn checked_reference_projection_can_read_large_roots_for_small_outputs() {
    use htlk_executable::{
        Expression, ExpressionContext as C, ExpressionKind as E, ValueReference,
    };
    let codec = Limits::default();
    let source = ValueReference::Input("record".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        source.clone(),
        Port::new(record(vec![("value", T::builtin(P::Integer), false)]), true),
    );
    let expression = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![PathStep::Field("value".into())],
        },
        &codec,
    )
    .unwrap();
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            source.clone(),
            vec![],
            Ok(EvaluationValue::Present(map(vec![
                ("value", Value::Integer(1)),
                ("padding", Value::Text("x".repeat(1000))),
            ]))),
            &codec,
        )
        .unwrap();
    let mut budget = policy();
    budget.max_output_bytes = 1;
    assert_eq!(
        checked.evaluate(&frame, None, &budget).unwrap().value,
        EvaluationValue::Present(Value::Integer(1))
    );
    frame
        .bind(
            source,
            vec![],
            Ok(EvaluationValue::Present(map(vec![]))),
            &codec,
        )
        .unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &budget).unwrap().value,
        EvaluationValue::Absent
    );
}

#[test]
fn runtime_type_depth_on_controlled_stacks() {
    const CHILD: &str = "HTLK_RUNTIME_TYPE_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(|| {
                let codec = Limits {
                    max_depth: 128,
                    ..Limits::default()
                };
                let mut t = T::builtin(P::Integer);
                let mut v = Value::Integer(1);
                for _ in 0..120 {
                    t = T::new(K::List(Box::new(t)), TypeContext::Value, &codec).unwrap();
                    v = Value::Array(vec![v]);
                }
                let mut budget = policy();
                budget.max_steps = 1000000;
                validate_typed_value(&v, &t, None, &codec, &budget).unwrap();
                use htlk_executable::{Expression, ExpressionContext};
                let mut bytes = b"\x82\x63not".repeat(126);
                bytes.extend_from_slice(b"\x82\x67literal\xf5");
                let expression = Expression::decode(&bytes, &codec).unwrap();
                let env = ExpressionTypeEnvironment::default();
                let checked = CheckedExpression::new(
                    &expression,
                    ExpressionContext::Eval,
                    &env,
                    None,
                    &codec,
                )
                .unwrap();
                assert_eq!(
                    checked
                        .evaluate(&EvaluationFrame::default(), None, &budget)
                        .unwrap()
                        .value,
                    EvaluationValue::Present(Value::Bool(true))
                );
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_type_depth_on_controlled_stacks",
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

#[test]
fn optional_parent_presence_does_not_require_the_final_optional_member() {
    use htlk_executable::{
        Expression, ExpressionContext as C, ExpressionKind as E, ValueReference,
    };
    let codec = Limits::default();
    let source = ValueReference::Input("value".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        source.clone(),
        Port::new(
            record(vec![(
                "parent",
                record(vec![("child", T::builtin(P::Boolean), false)]),
                false,
            )]),
            true,
        ),
    );
    let expression = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![
                PathStep::Field("parent".into()),
                PathStep::Field("child".into()),
            ],
        },
        &codec,
    )
    .unwrap();
    let checked = CheckedExpression::new(&expression, C::Eval, &env, None, &codec).unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            source,
            vec![],
            Ok(EvaluationValue::Present(map(vec![("parent", map(vec![]))]))),
            &codec,
        )
        .unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &policy()).unwrap().value,
        EvaluationValue::Absent
    );
}

#[test]
fn declared_json_result_rejects_non_json_extra_fields_on_structural_error_values() {
    use htlk_executable::{
        Expression, ExpressionContext as C, ExpressionKind as E, ValueReference,
    };
    let limits = Limits::default();
    let source = ValueReference::Input("error".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references
        .insert(source.clone(), Port::new(T::builtin(P::Error), true));
    let expression = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![],
        },
        &limits,
    )
    .unwrap();
    let expected = Port::new(T::builtin(P::Json), true);
    let checked =
        CheckedExpression::new(&expression, C::Eval, &env, Some(&expected), &limits).unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            source,
            vec![],
            Ok(EvaluationValue::Present(map(vec![
                ("code", Value::Text("test".into())),
                ("message", Value::Text("test".into())),
                ("extra", Value::Bytes(vec![1])),
            ]))),
            &limits,
        )
        .unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &policy()),
        Err(Error::OperandType)
    );
}

#[test]
fn json_preparation_counts_collection_work_and_keeps_native_value_size_distinct() {
    let value = Value::Text("\0".repeat(20));
    let limits = Limits::default();
    let mut budget = policy();
    budget.max_value_bytes = htlk_cbor::encode(&value, &limits).unwrap().len() as u64;
    validate_typed_value(&value, &T::builtin(P::Json), None, &limits, &budget).unwrap();
    budget = policy();
    budget.max_collection_visits = 1;
    assert!(matches!(
        validate_typed_value(
            &Value::Array(vec![Value::Null, Value::Null]),
            &T::builtin(P::Json),
            None,
            &limits,
            &budget
        ),
        Err(Error::Limit(_))
    ));
}
