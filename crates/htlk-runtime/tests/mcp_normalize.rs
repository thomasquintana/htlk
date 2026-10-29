//! Independent declared/recovered/inferred/application checks and authoritative tool errors.

use htlk_analyzer::{NativeSchemaOptions, NativeSchemas, SchemaCatalog, embedded_schema_base};
use htlk_executable::{
    AdoptedInference, DeclaredContractPolicy, EvaluatorLimits, InferredSchemaPolicy, JsonDocument,
    McpBinding, McpBindingKind, McpTransport, ServerIdentity, ToolResultPolicy,
    cbor::{Limits, Value},
    digest::Digest,
};
use htlk_runtime::{ToolResultFailure as F, normalize_mcp_tool_result};

fn json(text: &str) -> JsonDocument {
    JsonDocument::new(text.as_bytes(), &Limits::default()).unwrap()
}
fn evaluator() -> EvaluatorLimits {
    EvaluatorLimits {
        max_expression_depth: 64,
        max_value_bytes: 65536,
        max_collection_visits: 10000,
        max_regex_bytes: 1024,
        max_regex_compiled_bytes: 4096,
        max_output_bytes: 65536,
        max_steps: 100000,
    }
}
fn fixture(
    declared: Option<&JsonDocument>,
    policy: ToolResultPolicy,
    additional: &[JsonDocument],
) -> (McpBinding, NativeSchemas) {
    let limits = Limits::default();
    let input = json(r#"{"type":"object"}"#);
    let mut descriptor = format!(
        r#"{{"name":"tool","inputSchema":{}"#,
        std::str::from_utf8(input.as_bytes()).unwrap()
    );
    if let Some(declared) = declared {
        descriptor += &format!(
            r#", "outputSchema":{}"#,
            std::str::from_utf8(declared.as_bytes()).unwrap()
        );
    }
    descriptor.push('}');
    let binding = McpBinding::new(
        ServerIdentity::new(
            "test".into(),
            McpTransport::Stdio,
            "fixture".into(),
            "1".into(),
            &limits,
        )
        .unwrap(),
        json(&descriptor).digest(),
        McpBindingKind::Tool {
            name: "tool".into(),
            input_schema: input.digest(),
            output_schema: declared.map(JsonDocument::digest),
            result_policy: Box::new(policy),
        },
        &limits,
    )
    .unwrap();
    let mut documents = vec![input];
    if let Some(declared) = declared {
        documents.push(declared.clone());
    }
    documents.extend_from_slice(additional);
    let documents = documents
        .into_iter()
        .map(|document| (embedded_schema_base(&document, &limits).unwrap(), document))
        .collect();
    let catalog = SchemaCatalog::new(documents, &limits).unwrap();
    (
        binding,
        NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &limits).unwrap(),
    )
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(value) = value else {
        panic!("record")
    };
    value.get(key).unwrap()
}

#[test]
fn text_recovery_does_not_establish_declared_compliance_and_report_is_explicit() {
    let output =
        json(r#"{"type":"object","properties":{"price":{"type":"number"}},"required":["price"]}"#);
    let response = json(r#"{"content":[{"type":"text","text":"Price: {\"price\":25}"}]}"#);
    let (binding, native) = fixture(Some(&output), ToolResultPolicy::default(), &[]);
    let result = normalize_mcp_tool_result(
        &response,
        &binding,
        &native,
        &Limits::default(),
        &evaluator(),
    )
    .unwrap();
    assert_eq!(result.failure(), Some(F::DeclaredContract));
    assert_eq!(
        result.extraction().data(),
        Some(json(r#"{"price":25}"#).value())
    );
    let validation = field(result.envelope().value(), "validation");
    assert_eq!(
        field(field(validation, "declared_schema"), "status"),
        &Value::Text("missing".into())
    );
    let policy = ToolResultPolicy {
        declared_contract: DeclaredContractPolicy::Report,
        ..ToolResultPolicy::default()
    };
    let (binding, native) = fixture(Some(&output), policy, &[]);
    let result = normalize_mcp_tool_result(
        &response,
        &binding,
        &native,
        &Limits::default(),
        &evaluator(),
    )
    .unwrap();
    assert_eq!(result.failure(), None);
    assert_eq!(
        field(result.envelope().value(), "data"),
        json(r#"{"price":25}"#).value()
    );
    assert_eq!(
        response,
        json(r#"{"content":[{"type":"text","text":"Price: {\"price\":25}"}]}"#)
    );
}

#[test]
fn observe_grants_no_refined_type_and_enforce_or_application_checks_reject_mismatch() {
    let schema =
        json(r#"{"type":"object","properties":{"price":{"type":"number"}},"required":["price"]}"#);
    let response = json(r#"{"content":[],"structuredContent":{"price":"unknown"}}"#);
    let mut policy = ToolResultPolicy {
        inferred_schema: Some(AdoptedInference {
            schema: schema.digest(),
            evidence: Digest::from_bytes([9; 32]),
            mode: InferredSchemaPolicy::Observe,
        }),
        ..ToolResultPolicy::default()
    };
    let (binding, native) = fixture(None, policy.clone(), std::slice::from_ref(&schema));
    let result = normalize_mcp_tool_result(
        &response,
        &binding,
        &native,
        &Limits::default(),
        &evaluator(),
    )
    .unwrap();
    assert_eq!(result.failure(), None);
    assert_eq!(
        field(
            field(
                field(result.envelope().value(), "validation"),
                "inferred_schema"
            ),
            "status"
        ),
        &Value::Text("invalid".into())
    );
    policy.inferred_schema.as_mut().unwrap().mode = InferredSchemaPolicy::Enforce;
    let (binding, native) = fixture(None, policy.clone(), std::slice::from_ref(&schema));
    assert_eq!(
        normalize_mcp_tool_result(
            &response,
            &binding,
            &native,
            &Limits::default(),
            &evaluator()
        )
        .unwrap()
        .failure(),
        Some(F::InferredSchema)
    );
    policy.inferred_schema.as_mut().unwrap().mode = InferredSchemaPolicy::Observe;
    policy.application_schema = Some(schema.digest());
    let (binding, native) = fixture(None, policy, &[schema]);
    assert_eq!(
        normalize_mcp_tool_result(
            &response,
            &binding,
            &native,
            &Limits::default(),
            &evaluator()
        )
        .unwrap()
        .failure(),
        Some(F::ApplicationSchema)
    );
}

#[test]
fn schema_less_text_is_success_but_required_data_and_tool_errors_remain_distinct() {
    let response = json(r#"{"content":[{"type":"text","text":"Navigation completed."}]}"#);
    let (binding, native) = fixture(None, ToolResultPolicy::default(), &[]);
    let result = normalize_mcp_tool_result(
        &response,
        &binding,
        &native,
        &Limits::default(),
        &evaluator(),
    )
    .unwrap();
    assert_eq!(result.failure(), None);
    assert!(result.extraction().data().is_none());
    let schema = json(r#"{"type":"object","required":["title"]}"#);
    let policy = ToolResultPolicy {
        application_schema: Some(schema.digest()),
        ..ToolResultPolicy::default()
    };
    let (binding, native) = fixture(None, policy, &[schema]);
    assert_eq!(
        normalize_mcp_tool_result(
            &response,
            &binding,
            &native,
            &Limits::default(),
            &evaluator()
        )
        .unwrap()
        .failure(),
        Some(F::ApplicationSchema)
    );
    let error = json(
        r#"{"isError":true,"content":[{"type":"text","text":"Details: {\"order_id\":\"A123\"}"}]}"#,
    );
    let result =
        normalize_mcp_tool_result(&error, &binding, &native, &Limits::default(), &evaluator())
            .unwrap();
    assert_eq!(result.failure(), Some(F::ToolError));
    assert_eq!(
        result.extraction().data(),
        Some(json(r#"{"order_id":"A123"}"#).value())
    );
    assert_eq!(
        field(
            field(result.envelope().value(), "protocol_result"),
            "isError"
        ),
        &Value::Bool(true)
    );
}

#[test]
fn required_constraints_accept_any_json_root_and_preserve_null_as_present() {
    for (value, schema) in [
        ("null", r#"{"type":"null"}"#),
        ("42", r#"{"type":"integer"}"#),
        ("[1,2]", r#"{"type":"array","items":{"type":"integer"}}"#),
    ] {
        let schema = json(schema);
        let policy = ToolResultPolicy {
            application_schema: Some(schema.digest()),
            ..ToolResultPolicy::default()
        };
        let (binding, native) = fixture(None, policy, &[schema]);
        let response = JsonDocument::from_value(
            &Value::Map(
                htlk_executable::cbor::Map::try_from_entries([(
                    "content".into(),
                    Value::Array(vec![Value::Map(
                        htlk_executable::cbor::Map::try_from_entries([
                            ("type".into(), Value::Text("text".into())),
                            ("text".into(), Value::Text(value.into())),
                        ])
                        .unwrap(),
                    )]),
                )])
                .unwrap(),
            ),
            &Limits::default(),
        )
        .unwrap();
        let result = normalize_mcp_tool_result(
            &response,
            &binding,
            &native,
            &Limits::default(),
            &evaluator(),
        )
        .unwrap();
        assert_eq!(result.failure(), None);
        assert_eq!(result.extraction().data(), Some(json(value).value()));
    }
}

#[test]
fn required_extraction_exhaustion_is_a_budget_failure_not_a_retryable_schema_mismatch() {
    let schema = json(r#"{"type":"null"}"#);
    let mut policy = ToolResultPolicy {
        application_schema: Some(schema.digest()),
        ..ToolResultPolicy::default()
    };
    policy.extraction.limits.work = 1;
    let (binding, native) = fixture(None, policy, &[schema]);
    let response = json(r#"{"content":[{"type":"text","text":"null"}]}"#);
    let result = normalize_mcp_tool_result(
        &response,
        &binding,
        &native,
        &Limits::default(),
        &evaluator(),
    )
    .unwrap();
    assert_eq!(result.failure(), Some(F::RequiredExtraction));
    assert_eq!(
        result.failure().unwrap().code(),
        htlk_runtime::failure::FailureCode::Budget
    );
    assert_eq!(result.extraction().exhausted_limit(), Some("work"));
    assert!(result.extraction().data().is_none());
}
