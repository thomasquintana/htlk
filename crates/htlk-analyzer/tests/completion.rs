//! Regression cases for the remaining Draft 0.1 analysis obligations.
use htlk_analyzer::{ExpressionTypeEnvironment, check_expression_diagnostic};
use htlk_executable::{
    BinaryOperator, CoreFunction, Expression, ExpressionContext, ExpressionKind, FunctionId,
    ScalarLiteral, cbor::Limits,
};

#[test]
fn regex_syntax_is_checked_in_a_lazy_branch_at_its_original_path() {
    let limits = Limits::default();
    let literal = Expression::new(
        ExpressionKind::Regex {
            pattern: "(?=PRIVATE_PATTERN)".into(),
            flags: String::new(),
        },
        &limits,
    )
    .unwrap();
    let length = Expression::new(
        ExpressionKind::Call {
            function: FunctionId::Core(CoreFunction::Length),
            arguments: vec![literal],
        },
        &limits,
    )
    .unwrap();
    let comparison = Expression::new(
        ExpressionKind::Binary {
            operator: BinaryOperator::Eq,
            left: Box::new(length),
            right: Box::new(Expression::literal(ScalarLiteral::Integer(0))),
        },
        &limits,
    )
    .unwrap();
    let expression = Expression::new(
        ExpressionKind::Binary {
            operator: BinaryOperator::And,
            left: Box::new(Expression::literal(ScalarLiteral::Boolean(false))),
            right: Box::new(comparison),
        },
        &limits,
    )
    .unwrap();
    let failure = check_expression_diagnostic(
        &expression,
        ExpressionContext::Eval,
        &ExpressionTypeEnvironment::default(),
        None,
        None,
        &limits,
    )
    .unwrap_err();
    assert_eq!(failure.expression_path, [1, 0, 0]);
    assert!(!format!("{failure:?}").contains("PRIVATE_PATTERN"));
}

#[test]
fn explicit_regex_controls_enforce_pattern_and_compiler_boundaries() {
    use htlk_analyzer::{
        ExpressionAnalysisOptions, ExpressionTypeError, RegexError, RegexOptions,
        check_expression_with_options, validate_regex,
    };
    let options = RegexOptions {
        max_pattern_bytes: 3,
        max_compiled_bytes: 4096,
    };
    for flags in ["", "i", "m", "s", "im", "is", "ms", "ims"] {
        validate_regex("abc", flags, options).unwrap();
    }
    assert_eq!(
        validate_regex("abcd", "", options),
        Err(RegexError::PatternLimit)
    );
    assert_eq!(validate_regex("a", "mi", options), Err(RegexError::Syntax));
    assert_eq!(
        validate_regex(
            "[a-z]{100}",
            "",
            RegexOptions {
                max_pattern_bytes: 100,
                max_compiled_bytes: 1,
            }
        ),
        Err(RegexError::CompiledLimit)
    );
    let limits = Limits::default();
    let literal = Expression::new(
        ExpressionKind::Regex {
            pattern: "abcd".into(),
            flags: String::new(),
        },
        &limits,
    )
    .unwrap();
    let error = check_expression_with_options(
        &literal,
        ExpressionContext::Eval,
        &ExpressionTypeEnvironment::default(),
        None,
        ExpressionAnalysisOptions {
            regex: options,
            schemas: None,
        },
        &limits,
    )
    .unwrap_err();
    assert_eq!(
        error.error,
        ExpressionTypeError::Regex(RegexError::PatternLimit)
    );
}

fn schema(
    json: &str,
) -> (
    htlk_analyzer::NativeSchemas,
    htlk_executable::digest::Digest,
) {
    use htlk_analyzer::{NativeSchemaOptions, NativeSchemas, SchemaCatalog, embedded_schema_base};
    let l = Limits::default();
    let doc = htlk_executable::JsonDocument::new(json.as_bytes(), &l).unwrap();
    let id = doc.digest();
    let uri = embedded_schema_base(&doc, &l).unwrap();
    (
        NativeSchemas::compile(
            &SchemaCatalog::new(vec![(uri, doc)], &l).unwrap(),
            NativeSchemaOptions::default(),
            &l,
        )
        .unwrap(),
        id,
    )
}

#[test]
fn whole_schema_roots_reject_disjoint_eval_and_edge_boundaries() {
    use htlk_analyzer::{
        ExpressionSite, ExpressionTypeError, ScopeVerificationErrorKind,
        check_expression_with_schemas, verify_scope_graph,
    };
    use htlk_executable::*;
    let l = Limits::default();
    let (schemas, id) = schema(r#"{"type":"object","properties":{"flag":{"type":"boolean"}}}"#);
    let ty = ValueType::new(ValueTypeKind::Schema(id), TypeContext::Value, &l).unwrap();
    let source = ValueReference::Input("source".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references
        .insert(source.clone(), Port::new(ty.clone(), true));
    let expr = Expression::new(
        ExpressionKind::Ref {
            source,
            path: vec![],
        },
        &l,
    )
    .unwrap();
    let integer = Port::new(ValueType::builtin(BuiltinType::Integer), true);
    assert_eq!(
        check_expression_with_schemas(
            &expr,
            ExpressionContext::Eval,
            &env,
            Some(&integer),
            &schemas,
            &l
        )
        .unwrap_err(),
        ExpressionTypeError::TypeMismatch
    );
    let result = check_expression_with_schemas(
        &Expression::literal(ScalarLiteral::Integer(1)),
        ExpressionContext::Eval,
        &env,
        Some(&Port::new(ty.clone(), true)),
        &schemas,
        &l,
    );
    assert_eq!(result.unwrap_err(), ExpressionTypeError::TypeMismatch);
    let scope = Scope::new(
        ScopeFields {
            inputs: PortTable::new(vec![("source".parse().unwrap(), Port::new(ty, true))], &l)
                .unwrap(),
            outputs: PortTable::new(vec![("result".parse().unwrap(), integer)], &l).unwrap(),
            edges: vec![Edge::new(
                "transfer".parse().unwrap(),
                EdgeSource::Input("source".parse().unwrap()),
                EdgeDestination::Output("result".parse().unwrap()),
            )],
            ..Default::default()
        },
        &l,
    )
    .unwrap();
    let error = verify_scope_graph(
        &scope,
        ScopeContext::Ordinary,
        None,
        &Default::default(),
        &Default::default(),
        &schemas,
        &l,
    )
    .unwrap_err();
    assert_eq!(
        error.site,
        Some(ExpressionSite::EdgeBoundary("transfer".parse().unwrap()))
    );
    assert_eq!(
        error.kind,
        ScopeVerificationErrorKind::Expression(Box::new(ExpressionTypeError::TypeMismatch))
    );
}

#[test]
fn schema_shapes_keep_named_fields_items_presence_and_exact_obligations() {
    use htlk_analyzer::{RuntimeTypeCheckKind, check_expression_with_schemas};
    use htlk_executable::*;
    let l = Limits::default();
    let (schemas, id) = schema(
        r##"{"type":"object","required":["items"],"properties":{
        "items":{"type":"array","items":{"$ref":"#/$defs/item"}},
        "maybe":{"type":["string","null"]}},"$defs":{"item":{"type":"object",
        "required":["flag"],"properties":{"flag":{"type":"boolean"}}}}}"##,
    );
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        ValueReference::Input("source".parse().unwrap()),
        Port::new(
            ValueType::new(ValueTypeKind::Schema(id), TypeContext::Value, &l).unwrap(),
            true,
        ),
    );
    let analyze = |path| {
        let expr = Expression::new(
            ExpressionKind::Ref {
                source: ValueReference::Input("source".parse().unwrap()),
                path,
            },
            &l,
        )
        .unwrap();
        check_expression_with_schemas(&expr, ExpressionContext::Eval, &env, None, &schemas, &l)
            .unwrap()
    };
    let items = analyze(vec![PathStep::Field("items".into())]);
    assert!(items.result().required());
    let ValueTypeKind::List(item) = items.result().value_type().kind() else {
        panic!()
    };
    let ValueTypeKind::Record(fields) = item.kind() else {
        panic!()
    };
    assert!(fields[0].1.required());
    assert_eq!(
        fields[0].1.value_type(),
        &ValueType::builtin(BuiltinType::Boolean)
    );
    assert!(items.runtime_checks().iter().any(
        |c| matches!(c.kind, RuntimeTypeCheckKind::SchemaProjection { schema, .. } if schema == id)
    ));
    let maybe = analyze(vec![PathStep::Field("maybe".into())]);
    assert!(!maybe.result().required());
    assert!(matches!(
        maybe.result().value_type().kind(),
        ValueTypeKind::Union(_)
    ));
}

#[test]
fn focused_analysis_checks_the_exact_retained_plan_budget() {
    use htlk_analyzer::{ExpressionTypeError, check_expression};
    use htlk_executable::TypeContext;
    let l = Limits::default();
    let expression = Expression::new(
        ExpressionKind::List(
            (0..50)
                .map(|_| Expression::literal(ScalarLiteral::Boolean(true)))
                .collect(),
        ),
        &l,
    )
    .unwrap();
    let env = ExpressionTypeEnvironment::default();
    let analysis = check_expression(&expression, ExpressionContext::Eval, &env, None, &l).unwrap();
    assert!(analysis.runtime_checks().is_empty());
    let retained = size_of_val(&analysis)
        + analysis
            .result()
            .value_type()
            .encode(TypeContext::Signature, &l)
            .unwrap()
            .len()
        + analysis
            .nodes()
            .iter()
            .map(|n| {
                size_of_val(n)
                    + size_of_val(n.expression_path.as_slice())
                    + n.port
                        .value_type()
                        .encode(TypeContext::Signature, &l)
                        .unwrap()
                        .len()
            })
            .sum::<usize>();
    let exact = Limits {
        max_document_bytes: retained,
        ..l
    };
    assert!(check_expression(&expression, ExpressionContext::Eval, &env, None, &exact).is_ok());
    let tight = Limits {
        max_document_bytes: retained - 1,
        ..exact
    };
    assert!(expression.to_value(&tight).is_ok());
    assert_eq!(
        check_expression(&expression, ExpressionContext::Eval, &env, None, &tight).unwrap_err(),
        ExpressionTypeError::InferenceLimit
    );
}

#[test]
fn repeated_schema_projections_share_the_enclosing_inference_allowance() {
    use htlk_analyzer::{ExpressionTypeError, NativeSchemaError, check_expression_with_schemas};
    use htlk_executable::*;
    let branches = vec![r#"{"type":"boolean"}"#; 30].join(",");
    let (schemas, id) = schema(&format!(
        r#"{{"type":"object","properties":{{"flag":{{"allOf":[{branches}]}}}}}}"#
    ));
    let l = Limits::default();
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        ValueReference::Input("source".parse().unwrap()),
        Port::new(
            ValueType::new(ValueTypeKind::Schema(id), TypeContext::Value, &l).unwrap(),
            true,
        ),
    );
    let projection = Expression::new(
        ExpressionKind::Ref {
            source: ValueReference::Input("source".parse().unwrap()),
            path: vec![PathStep::Field("flag".into())],
        },
        &l,
    )
    .unwrap();
    let expression = Expression::new(ExpressionKind::List(vec![projection; 10]), &l).unwrap();
    check_expression_with_schemas(
        &expression,
        ExpressionContext::Eval,
        &env,
        None,
        &schemas,
        &l,
    )
    .unwrap();
    let tight = Limits {
        max_document_bytes: 8000,
        ..l
    };
    assert!(expression.to_value(&tight).is_ok());
    assert!(
        matches!(check_expression_with_schemas(&expression, ExpressionContext::Eval, &env, None, &schemas, &tight),
        Err(ExpressionTypeError::Schema(error)) if *error == NativeSchemaError::AdmissionLimit)
    );
}

#[test]
fn schema_derived_members_do_not_open_authored_record_constructors() {
    use htlk_analyzer::{ExpressionTypeError, check_expression_with_schemas};
    use htlk_executable::*;
    let l = Limits::default();
    let (schemas, id) = schema(r#"{"type":"object","properties":{"flag":{"type":"boolean"}}}"#);
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        ValueReference::Input("source".parse().unwrap()),
        Port::new(
            ValueType::new(ValueTypeKind::Schema(id), TypeContext::Value, &l).unwrap(),
            true,
        ),
    );
    let member = Expression::new(
        ExpressionKind::Ref {
            source: ValueReference::Input("source".parse().unwrap()),
            path: vec![PathStep::Field("flag".into())],
        },
        &l,
    )
    .unwrap();
    let record =
        Expression::new(ExpressionKind::Record(vec![("known".into(), member)]), &l).unwrap();
    let projection = Expression::new(
        ExpressionKind::Get {
            value: Box::new(record),
            path: vec![PathStep::Field("undeclared".into())],
        },
        &l,
    )
    .unwrap();
    assert_eq!(
        check_expression_with_schemas(
            &projection,
            ExpressionContext::Eval,
            &env,
            None,
            &schemas,
            &l
        )
        .unwrap_err(),
        ExpressionTypeError::InvalidProjection
    );
}
