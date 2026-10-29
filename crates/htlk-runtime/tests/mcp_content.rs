//! Complete explicit content-library linking, checked narrowing and bounded pure access.

use htlk_analyzer::ExpressionTypeEnvironment;
use htlk_executable::{
    BuiltinType as P, EvaluatorLimits, Expression, ExpressionContext, ExpressionKind, FunctionId,
    JsonDocument, JsonError, McpToolResult, Port, TypeContext, ValueReference, ValueType as T,
    ValueTypeKind as K,
    cbor::{Limits, Map, Value},
};
use htlk_runtime::{
    CheckedExpression, EvaluationError as E, EvaluationFrame, EvaluationResult,
    EvaluationValue as V, NativeRegistry, NativeRegistryError, extract_mcp_json,
    link_mcp_content_library, mcp_content_library,
};

fn budget() -> EvaluatorLimits {
    EvaluatorLimits {
        max_expression_depth: 64,
        max_value_bytes: 65536,
        max_collection_visits: 10000,
        max_regex_bytes: 1024,
        max_regex_compiled_bytes: 4096,
        max_output_bytes: 65536,
        max_steps: 1_000_000,
    }
}
fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(
        Map::try_from_entries(fields.into_iter().map(|(key, value)| (key.into(), value))).unwrap(),
    )
}
fn json(text: &str) -> Value {
    JsonDocument::new(text.as_bytes(), &Limits::default())
        .unwrap()
        .value()
        .clone()
}
fn envelope(texts: &[&str], structured: Option<Value>) -> Value {
    let mut blocks: Vec<_> = texts
        .iter()
        .map(|text| {
            object(vec![
                ("type", Value::Text("text".into())),
                ("text", Value::Text((*text).into())),
            ])
        })
        .collect();
    blocks.insert(
        0,
        object(vec![
            ("type", Value::Text("image".into())),
            ("mimeType", Value::Text("image/png".into())),
            ("data", Value::Text("AA==".into())),
        ]),
    );
    let mut protocol = vec![("content", Value::Array(blocks))];
    if let Some(structured) = structured {
        protocol.push(("structuredContent", structured));
    }
    let protocol = JsonDocument::from_value(&object(protocol), &Limits::default()).unwrap();
    let extraction =
        extract_mcp_json(&protocol, &Default::default(), None, &Limits::default()).unwrap();
    let absent = || object(vec![("status", Value::Text("not_configured".into()))]);
    let mut fields = vec![
        ("protocol_result", protocol.value().clone()),
        (
            "extraction",
            extraction.to_value(&Limits::default()).unwrap(),
        ),
        (
            "validation",
            object(vec![
                ("declared_schema", absent()),
                ("inferred_schema", absent()),
                ("application_schema", absent()),
            ]),
        ),
    ];
    if let Some(data) = extraction.data() {
        fields.push(("data", data.clone()));
    }
    McpToolResult::from_value(&object(fields), &Limits::default())
        .unwrap()
        .to_value(&Limits::default())
        .unwrap()
}
fn evaluate(
    name: &str,
    arguments: Vec<Value>,
    expected: Option<T>,
    policy: &EvaluatorLimits,
) -> Result<EvaluationResult, E> {
    let limits = Limits::default();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    let library = link_mcp_content_library(&mut registry, &limits).unwrap();
    let signature = &library
        .functions()
        .iter()
        .find(|(id, _)| id.as_str() == name)
        .unwrap()
        .1;
    let mut env = ExpressionTypeEnvironment::default();
    let mut frame = EvaluationFrame::default();
    let mut operands = Vec::new();
    for (index, (argument, parameter)) in arguments
        .into_iter()
        .zip(signature.parameters())
        .enumerate()
    {
        let source = ValueReference::Input(format!("arg_{index}").parse().unwrap());
        env.references.insert(source.clone(), parameter.clone());
        frame
            .bind(source.clone(), vec![], Ok(V::Present(argument)), &limits)
            .unwrap();
        operands.push(
            Expression::new(
                ExpressionKind::Ref {
                    source,
                    path: vec![],
                },
                &limits,
            )
            .unwrap(),
        );
    }
    let expression = Expression::new(
        ExpressionKind::Call {
            function: FunctionId::Library {
                library: library.implementation_digest(),
                name: name.parse().unwrap(),
            },
            arguments: operands,
        },
        &limits,
    )
    .unwrap();
    env.libraries
        .insert(library.implementation_digest(), library);
    let expected = expected.map(|ty| Port::new(ty, true));
    let checked = CheckedExpression::new(
        &expression,
        ExpressionContext::Eval,
        &env,
        expected.as_ref(),
        &limits,
    )
    .unwrap();
    registry.evaluate(&checked, &frame, None, policy)
}
fn call(name: &str, arguments: Vec<Value>) -> V {
    evaluate(name, arguments, None, &budget()).unwrap().value
}

#[test]
fn complete_linking_is_explicit_and_exact() {
    let limits = Limits::default();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    let manifest = mcp_content_library(&limits).unwrap();
    assert_eq!(
        registry.verify_library(&manifest),
        Err(NativeRegistryError::UnknownLibrary)
    );
    let linked = link_mcp_content_library(&mut registry, &limits).unwrap();
    assert_eq!(linked, manifest);
    assert_eq!(linked.functions().len(), 13);
    assert_eq!(
        link_mcp_content_library(&mut registry, &limits).unwrap_err(),
        NativeRegistryError::DuplicateLibrary
    );
    let tight = Limits {
        max_document_bytes: 1,
        ..limits
    };
    assert!(mcp_content_library(&tight).is_err());
}

#[test]
fn original_text_kinds_structure_candidates_and_validation_share_one_envelope_api() {
    let result = envelope(&["one", "two"], Some(json(r#"{"x":1}"#)));
    assert_eq!(
        call("text_blocks", vec![result.clone()]),
        V::Present(json(r#"["one","two"]"#))
    );
    assert_eq!(
        call("content_kinds", vec![result.clone()]),
        V::Present(json(r#"["image","text","text"]"#))
    );
    assert_eq!(
        call("structured_content", vec![result.clone()]),
        V::Present(json(r#"{"x":1}"#))
    );
    assert_eq!(
        call("data", vec![result.clone()]),
        V::Present(json(r#"{"x":1}"#))
    );
    let V::Present(Value::Map(validation)) = call("validation", vec![result.clone()]) else {
        panic!()
    };
    assert_eq!(validation.len(), 3);
    let V::Present(Value::Array(candidates)) = call("candidates", vec![result]) else {
        panic!()
    };
    assert_eq!(candidates.len(), 1);
    let empty = envelope(&["plain text"], None);
    assert_eq!(call("structured_content", vec![empty.clone()]), V::Absent);
    assert_eq!(call("data", vec![empty]), V::Absent);
}

#[test]
fn explicit_selection_preserves_ambiguity_missingness_and_present_null() {
    let result = envelope(&["null", r#"{"x":1}"#], None);
    assert_eq!(call("data", vec![result.clone()]), V::Absent);
    assert_eq!(
        call(
            "select_candidate",
            vec![result.clone(), Value::Text("candidate_0".into())]
        ),
        V::Present(Value::Null)
    );
    assert_eq!(
        call(
            "select_candidate",
            vec![result.clone(), Value::Text("candidate_1".into())]
        ),
        V::Present(json(r#"{"x":1}"#))
    );
    assert_eq!(
        call(
            "select_candidate",
            vec![result, Value::Text("candidate_99".into())]
        ),
        V::Absent
    );
}

#[test]
fn json_parsing_pointer_access_and_narrowing_never_repair_or_coerce() {
    for text in ["null", "true", "42", "1.5", r#""string""#, "[]", "{}"] {
        assert_eq!(
            call("parse_json", vec![Value::Text(text.into())]),
            V::Present(json(text))
        );
    }
    for text in [
        r#"{"x":1,"x":2}"#,
        "9007199254740993",
        "1+2",
        "{unquoted:1}",
    ] {
        assert!(matches!(
            evaluate(
                "parse_json",
                vec![Value::Text(text.into())],
                None,
                &budget()
            ),
            Err(E::Json(_))
        ));
    }
    let input = json(r#"{"a/b~":[1,null]}"#);
    assert_eq!(
        call(
            "json_at",
            vec![input.clone(), Value::Text("/a~1b~0/1".into())]
        ),
        V::Present(Value::Null)
    );
    assert_eq!(
        call(
            "json_at",
            vec![input.clone(), Value::Text("/missing".into())]
        ),
        V::Absent
    );
    assert!(matches!(
        evaluate(
            "json_at",
            vec![input, Value::Text("/~wrong".into())],
            None,
            &budget()
        ),
        Err(E::JsonPointer(_))
    ));
    assert_eq!(
        evaluate(
            "checked",
            vec![Value::Integer(42)],
            Some(T::builtin(P::Integer)),
            &budget()
        )
        .unwrap()
        .value,
        V::Present(Value::Integer(42))
    );
    assert!(matches!(
        evaluate(
            "checked",
            vec![Value::Text("42".into())],
            Some(T::builtin(P::Integer)),
            &budget()
        ),
        Err(E::OperandType)
    ));
    let record = T::new(
        K::Record(vec![(
            "title".into(),
            Port::new(T::builtin(P::String), true),
        )]),
        TypeContext::Value,
        &Limits::default(),
    )
    .unwrap();
    assert!(matches!(
        evaluate("checked", vec![json("{}")], Some(record), &budget()),
        Err(E::OperandType)
    ));
}

#[test]
fn joining_is_explicit_and_literal_matching_and_unique_capture_are_checked() {
    assert_eq!(
        call(
            "join_text",
            vec![json(r#"["{","}"]"#), Value::Text("".into())]
        ),
        V::Present(Value::Text("{}".into()))
    );
    assert_eq!(
        call(
            "join_text",
            vec![json(r#"["a","b"]"#), Value::Text("é".into())]
        ),
        V::Present(Value::Text("aéb".into()))
    );
    assert_eq!(
        call(
            "text_contains",
            vec![
                Value::Text("page marker-é".into()),
                Value::Text("marker-é".into())
            ]
        ),
        V::Present(Value::Bool(true))
    );
    let expression = json(r#"{"pattern":"^([0-9]+): marker$","flags":"m"}"#);
    assert_eq!(
        call(
            "regex_capture",
            vec![
                Value::Text("12: marker".into()),
                expression.clone(),
                Value::Integer(1)
            ]
        ),
        V::Present(Value::Text("12".into()))
    );
    assert_eq!(
        call(
            "regex_capture",
            vec![
                Value::Text("unrelated".into()),
                expression.clone(),
                Value::Integer(1)
            ]
        ),
        V::Absent
    );
    assert!(matches!(
        evaluate(
            "regex_capture",
            vec![
                Value::Text("12: marker\n13: marker".into()),
                expression.clone(),
                Value::Integer(1)
            ],
            None,
            &budget()
        ),
        Err(E::OperandType)
    ));
    assert!(matches!(
        evaluate(
            "regex_capture",
            vec![
                Value::Text("12: marker".into()),
                expression,
                Value::Integer(2)
            ],
            None,
            &budget()
        ),
        Err(E::InvalidProjection)
    ));
}

#[test]
fn variable_work_exhaustion_is_not_a_cached_false_or_success() {
    let result = envelope(&[r#"{"x":1}"#], None);
    for (name, arguments) in [
        ("text_blocks", vec![result]),
        ("parse_json", vec![Value::Text("[1,2,3]".into())]),
        (
            "text_contains",
            vec![Value::Text("marker".into()), Value::Text("marker".into())],
        ),
    ] {
        let successful = evaluate(name, arguments.clone(), None, &budget()).unwrap();
        let mut constrained = budget();
        constrained.max_steps = successful.usage.steps - 1;
        assert!(matches!(
            evaluate(name, arguments, None, &constrained),
            Err(E::Limit(_))
        ));
    }
    let failure = E::Json(JsonError::Syntax { offset: 3 });
    assert_eq!(failure.code(), "E_EXPRESSION");
    assert!(std::error::Error::source(&failure).is_some());
}

#[test]
fn explicit_join_amplification_and_dynamic_pointer_indices_fail_before_success() {
    let mut policy = budget();
    policy.max_value_bytes = 128;
    let result = evaluate(
        "join_text",
        vec![json(r#"["a","b","c"]"#), Value::Text("x".repeat(100))],
        None,
        &policy,
    );
    assert!(matches!(result, Err(E::Limit("joined text"))));
    assert!(matches!(
        evaluate(
            "json_at",
            vec![json("[1]"), Value::Text("/01".into())],
            None,
            &budget()
        ),
        Err(E::JsonPointer(_))
    ));
    assert!(matches!(
        evaluate(
            "regex_capture",
            vec![
                Value::Text("x".into()),
                json(r#"{"pattern":"(x)","flags":""}"#),
                Value::Integer(-1)
            ],
            None,
            &budget()
        ),
        Err(E::InvalidProjection)
    ));
}
