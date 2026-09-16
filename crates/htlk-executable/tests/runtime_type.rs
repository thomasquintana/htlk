//! Actual-value admission and declared, variant-aware projection semantics.
use htlk_cbor::{Limits, Map, Value};
use htlk_executable::{
    EvaluationError as Error, EvaluationValue, EvaluatorLimits, PathStep, Port, PrimitiveType as P,
    TypeContext, ValueType as T, ValueTypeKind as K, project_typed_value, validate_typed_value,
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
fn optional_fields_extra_members_and_null_are_distinct() {
    let t = record(vec![("optional", T::primitive(P::Integer), false)]);
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
    let required = record(vec![("required", T::primitive(P::Integer), true)]);
    assert_eq!(
        validate_typed_value(&v, &required, None, &Limits::default(), &policy()),
        Err(Error::OperandType)
    );
}
#[test]
fn union_projection_uses_declared_variants_at_each_path_step() {
    let a = record(vec![("a", T::primitive(P::Integer), true)]);
    let b = record(vec![("b", T::primitive(P::Integer), true)]);
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
fn maps_indices_json_and_limits_are_strict() {
    let t = ty(K::Map(Box::new(T::primitive(P::Integer))));
    assert_eq!(
        project(&map(vec![]), &t, &["missing"]),
        Err(Error::InvalidProjection)
    );
    let list = ty(K::List(Box::new(T::primitive(P::Integer))));
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
            &T::primitive(P::Json),
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
        BinaryOperator, CheckedExpression, EvaluationFrame, Expression, ExpressionContext as C,
        ExpressionKind as E, ExpressionTypeEnvironment, ScalarLiteral, ValueReference,
    };
    let codec = Limits::default();
    let source = ValueReference::Input("value".parse().unwrap());
    let reference = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![],
        },
        C::Eval,
        &codec,
    )
    .unwrap();
    let mut env = ExpressionTypeEnvironment::default();
    env.references
        .insert(source.clone(), Port::new(T::primitive(P::Json), true));
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            source.clone(),
            vec![],
            Ok(EvaluationValue::Present(Value::Integer(1))),
            &codec,
        )
        .unwrap();
    let expression = Expression::new(E::Not(Box::new(reference.clone())), C::Eval, &codec).unwrap();
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
        C::Eval,
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
    use htlk_executable::{
        JsonDocument, NativeSchemaOptions, NativeSchemas, SchemaCatalog, embedded_schema_base,
    };
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
        CheckedExpression, EvaluationFrame, Expression, ExpressionContext as C,
        ExpressionKind as E, ExpressionTypeEnvironment, ValueReference,
    };
    let codec = Limits::default();
    let source = ValueReference::Input("record".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        source.clone(),
        Port::new(
            record(vec![("value", T::primitive(P::Integer), false)]),
            true,
        ),
    );
    let expression = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![PathStep::Field("value".into())],
        },
        C::Eval,
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
                let mut t = T::primitive(P::Integer);
                let mut v = Value::Integer(1);
                for _ in 0..120 {
                    t = T::new(K::List(Box::new(t)), TypeContext::Value, &codec).unwrap();
                    v = Value::Array(vec![v]);
                }
                let mut budget = policy();
                budget.max_steps = 1000000;
                validate_typed_value(&v, &t, None, &codec, &budget).unwrap();
                use htlk_executable::{
                    CheckedExpression, EvaluationFrame, Expression, ExpressionContext,
                    ExpressionTypeEnvironment,
                };
                let mut bytes = b"\x82\x63not".repeat(126);
                bytes.extend_from_slice(b"\x82\x67literal\xf5");
                let expression =
                    Expression::decode(&bytes, ExpressionContext::Eval, &codec).unwrap();
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
