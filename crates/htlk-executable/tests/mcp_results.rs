//! Cross-field integrity, provenance, absence, and canonical result-policy admission.

use htlk_executable::{
    AdoptedInference, BuiltinType, CandidateSelection, DeclaredContractPolicy, ExtractionPolicy,
    InferredSchemaPolicy, JsonDocument, McpResultError, McpToolResult, ToolResultPolicy,
    TypeContext, ValueType, builtin_record_type,
    cbor::{Limits, Map, Value},
    digest::Digest,
};

fn json(value: &str) -> Value {
    JsonDocument::new(value.as_bytes(), &Limits::default())
        .unwrap()
        .value()
        .clone()
}
fn change(value: &Value, key: &str, replacement: Option<Value>) -> Value {
    let Value::Map(record) = value else {
        panic!("record")
    };
    let mut entries: Vec<_> = record
        .iter()
        .filter(|(name, _)| *name != key)
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect();
    if let Some(replacement) = replacement {
        entries.push((key.into(), replacement));
    }
    Value::Map(Map::try_from_entries(entries).unwrap())
}
fn member<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(value) = value else {
        panic!("record")
    };
    value.get(key).unwrap()
}
fn selected_null() -> Value {
    json(
        r#"{
      "protocol_result":{"content":[{"type":"text","text":"é\n```json\nnull\n```"}],"_meta":{"upstream":"retained"}},
      "extraction":{"status":"selected","selected_candidate_id":"candidate_0","candidates":[
        {"id":"candidate_0","value":null,"validation":"parsed","sources":[
          {"origin":"text","method":"fence","content_block_index":0,"byte_start":11,"byte_end":15}
        ]}
      ]},
      "data":null,
      "validation":{
        "declared_schema":{"status":"not_configured"},
        "inferred_schema":{"status":"not_configured"},
        "application_schema":{"status":"not_configured"}
      }
    }"#,
    )
}

#[test]
fn canonical_envelopes_reject_unjustified_completion_and_source_shapes() {
    let limits = Limits::default();
    let value = selected_null();
    let extraction = member(&value, "extraction");
    let Value::Array(candidates) = member(extraction, "candidates") else {
        panic!()
    };
    let candidate = &candidates[0];
    let Value::Array(sources) = member(candidate, "sources") else {
        panic!()
    };
    for invalid in [
        Value::Null,
        json("{}"),
        change(&value, "protocol_result", Some(Value::Null)),
        change(&value, "protocol_result", Some(json(r#"{"content":null}"#))),
    ] {
        assert!(McpToolResult::from_value(&invalid, &limits).is_err());
    }
    for invalid in [
        Value::Null,
        json(r#"{"status":"ambiguous","candidates":[]}"#),
        json(r#"{"status":"exhausted","candidates":[]}"#),
        json(r#"{"status":"exhausted","candidates":[],"exhausted_limit":"invented"}"#),
        json(r#"{"status":"none","candidates":[],"exhausted_limit":"work"}"#),
    ] {
        assert!(
            McpToolResult::from_value(
                &change(&change(&value, "data", None), "extraction", Some(invalid)),
                &limits
            )
            .is_err()
        );
    }
    let replace_candidate = |candidate| {
        change(
            &value,
            "extraction",
            Some(change(
                extraction,
                "candidates",
                Some(Value::Array(vec![candidate])),
            )),
        )
    };
    for (key, replacement) in [
        ("sources", Value::Array(vec![])),
        ("sources", Value::Null),
        ("id", Value::Null),
    ] {
        assert!(
            McpToolResult::from_value(
                &replace_candidate(change(candidate, key, Some(replacement))),
                &limits
            )
            .is_err()
        );
    }
    for (key, replacement) in [
        ("origin", Value::Text("invented".into())),
        ("method", Value::Text("interpretation".into())),
        ("content_block_index", Value::Integer(-1)),
        ("content_block_index", Value::Null),
        ("byte_start", Value::Integer(2)),
        ("byte_start", Value::Null),
    ] {
        let source = change(&sources[0], key, Some(replacement));
        assert!(
            McpToolResult::from_value(
                &replace_candidate(change(
                    candidate,
                    "sources",
                    Some(Value::Array(vec![source]))
                )),
                &limits
            )
            .is_err()
        );
    }
    let schema = Digest::from_bytes([5; 32]);
    for (kind, mode) in [
        ("declared_schema", "observe"),
        ("inferred_schema", "report"),
        ("application_schema", "observe"),
    ] {
        let assessment = json(&format!(
            r#"{{"status":"valid","schema":"{schema}","mode":"{mode}"}}"#
        ));
        let invalid = change(
            &value,
            "validation",
            Some(change(member(&value, "validation"), kind, Some(assessment))),
        );
        assert!(McpToolResult::from_value(&invalid, &limits).is_err());
    }
    let envelope = McpToolResult::from_value(&value, &limits).unwrap();
    assert_eq!(envelope.to_value(&limits).unwrap(), value);
    assert!(
        envelope
            .to_value(&Limits {
                max_document_bytes: 1,
                ..limits
            })
            .is_err()
    );
}

#[test]
fn policies_do_not_coerce_unknown_selectors_modes_or_numeric_ceilings() {
    let limits = Limits::default();
    let policy = ExtractionPolicy::default().to_value(&limits).unwrap();
    for selection in [
        Value::Null,
        json("[]"),
        json(r#"["first"]"#),
        json(r#"["block",-1]"#),
        json(r#"["schema","invented"]"#),
    ] {
        assert!(
            ExtractionPolicy::from_value(&change(&policy, "selection", Some(selection)), &limits)
                .is_err()
        );
    }
    for number in [Value::Null, Value::Integer(-1), Value::Integer(129)] {
        let ceilings = change(member(&policy, "limits"), "depth", Some(number));
        assert!(
            ExtractionPolicy::from_value(&change(&policy, "limits", Some(ceilings)), &limits)
                .is_err()
        );
    }
    let canonical = ToolResultPolicy::default().to_value(&limits).unwrap();
    let schema = Digest::from_bytes([1; 32]);
    for mode in ["enforce", "unknown"] {
        let inferred = json(&format!(
            r#"{{"schema":"{schema}","evidence":"{schema}","mode":"{mode}"}}"#
        ));
        let value = change(&canonical, "inferred_schema", Some(inferred));
        assert_eq!(
            ToolResultPolicy::from_value(&value, &limits).is_ok(),
            mode == "enforce"
        );
    }
    for value in [Value::Null, json("[]")] {
        assert!(ToolResultPolicy::from_value(&value, &limits).is_err());
    }
}

#[test]
fn result_error_chains_are_redacted_and_source_validation_work_is_bounded() {
    use std::error::Error;
    let limits = Limits::default();
    let json_error =
        JsonDocument::new(br#"{"private-marker":1,"private-marker":2}"#, &limits).unwrap_err();
    let codec = htlk_executable::cbor::decode(&[0xff], &limits).unwrap_err();
    for (error, source) in [
        (McpResultError::from(codec), true),
        (McpResultError::from(json_error), true),
        (McpResultError::Shape("candidate provenance"), false),
        (McpResultError::Limit("source validation work"), false),
    ] {
        assert_eq!(error.source().is_some(), source);
        assert!(!error.to_string().contains("private-marker"));
        assert!(!error.to_string().is_empty());
    }
    // Distinct whitespace spans may all prove the same large literal value. The
    // envelope fits its ingress budget but reparsing every span must remain bounded.
    let payload = "x".repeat(2000);
    let literal = format!("\"{payload}\"");
    let original = format!("{}{literal}", " ".repeat(100));
    let sources: Vec<_> = (0..100).map(|start| json(&format!(r#"{{"origin":"text","method":"embedded","content_block_index":0,"byte_start":{start},"byte_end":{}}}"#, original.len()))).collect();
    let base = selected_null();
    let candidate = json(r#"{"id":"candidate_0","validation":"parsed","value":null,"sources":[]}"#);
    let candidate = change(
        &change(&candidate, "value", Some(Value::Text(payload.clone()))),
        "sources",
        Some(Value::Array(sources)),
    );
    let extraction = change(
        member(&base, "extraction"),
        "candidates",
        Some(Value::Array(vec![candidate])),
    );
    let protocol = JsonDocument::from_value(&Value::Text(original), &limits).unwrap();
    let protocol = json(&format!(
        r#"{{"content":[{{"type":"text","text":{}}}]}}"#,
        std::str::from_utf8(protocol.as_bytes()).unwrap()
    ));
    let value = change(
        &change(
            &change(&base, "data", Some(Value::Text(payload))),
            "protocol_result",
            Some(protocol),
        ),
        "extraction",
        Some(extraction),
    );
    let bytes = JsonDocument::from_value(&value, &limits)
        .unwrap()
        .as_bytes()
        .len();
    let tight = Limits {
        max_document_bytes: bytes,
        ..limits
    };
    assert_eq!(
        McpToolResult::from_value(&value, &tight),
        Err(McpResultError::Limit("source validation work"))
    );
}

#[test]
fn selected_null_absence_and_utf8_provenance_are_distinct() {
    let limits = Limits::default();
    let value = selected_null();
    let envelope = McpToolResult::from_value(&value, &limits).unwrap();
    assert_eq!(
        McpToolResult::decode(&envelope.encode(&limits).unwrap(), &limits).unwrap(),
        envelope
    );
    assert_eq!(member(envelope.value(), "data"), &Value::Null);
    assert!(McpToolResult::from_value(&change(&value, "data", None), &limits).is_err());
    assert!(
        McpToolResult::from_value(&change(&value, "data", Some(Value::Integer(0))), &limits)
            .is_err()
    );

    let none = change(
        &change(&value, "data", None),
        "extraction",
        Some(json(r#"{"status":"none","candidates":[]}"#)),
    );
    assert!(McpToolResult::from_value(&none, &limits).is_ok());
    assert!(McpToolResult::from_value(&change(&none, "data", Some(Value::Null)), &limits).is_err());
    let exhausted = change(
        &none,
        "extraction",
        Some(json(
            r#"{"status":"exhausted","candidates":[],"exhausted_limit":"work"}"#,
        )),
    );
    assert!(McpToolResult::from_value(&exhausted, &limits).is_ok());
}

#[test]
fn forged_candidates_sources_and_schema_assessments_are_rejected() {
    let limits = Limits::default();
    let value = selected_null();
    for (from, to) in [
        ("\"byte_start\":11", "\"byte_start\":1"),
        ("\"byte_end\":15", "\"byte_end\":700"),
        ("\"content_block_index\":0", "\"content_block_index\":1"),
        ("\"origin\":\"text\"", "\"origin\":\"structured_content\""),
        ("\"value\":null", "\"value\":true"),
        ("\"id\":\"candidate_0\"", "\"id\":\"candidate_1\""),
        (
            "\"selected_candidate_id\":\"candidate_0\"",
            "\"selected_candidate_id\":\"candidate_1\"",
        ),
        ("\"status\":\"selected\"", "\"status\":\"ambiguous\""),
        ("\"validation\":\"parsed\"", "\"validation\":\"valid\""),
    ] {
        let text = String::from_utf8(
            JsonDocument::from_value(&value, &limits)
                .unwrap()
                .as_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains(from));
        let malformed = json(&text.replace(from, to));
        assert!(
            McpToolResult::from_value(&malformed, &limits).is_err(),
            "{from}"
        );
    }
    let invalid_assessment = change(
        member(&value, "validation"),
        "declared_schema",
        Some(json(
            r#"{"status":"valid","schema":"invented","mode":"observe"}"#,
        )),
    );
    assert!(
        McpToolResult::from_value(
            &change(&value, "validation", Some(invalid_assessment)),
            &limits
        )
        .is_err()
    );
    let candidate = &match member(member(&value, "extraction"), "candidates") {
        Value::Array(values) => values,
        _ => panic!(),
    }[0];
    let duplicate = change(
        member(&value, "extraction"),
        "candidates",
        Some(Value::Array(vec![
            candidate.clone(),
            change(candidate, "id", Some(Value::Text("candidate_1".into()))),
        ])),
    );
    assert!(
        McpToolResult::from_value(&change(&value, "extraction", Some(duplicate)), &limits).is_err()
    );
}

#[test]
fn structured_sources_are_checked_against_original_values() {
    let limits = Limits::default();
    let value = selected_null();
    let source = json(r#"{"origin":"structured_content","method":"structured_content"}"#);
    let Value::Array(candidates) = member(member(&value, "extraction"), "candidates") else {
        panic!()
    };
    let candidate = change(
        &candidates[0],
        "sources",
        Some(Value::Array(vec![source.clone()])),
    );
    let extraction = change(
        member(&value, "extraction"),
        "candidates",
        Some(Value::Array(vec![candidate.clone()])),
    );
    let envelope = change(&value, "extraction", Some(extraction));
    assert!(McpToolResult::from_value(&envelope, &limits).is_err());
    let protocol = change(
        member(&value, "protocol_result"),
        "structuredContent",
        Some(Value::Null),
    );
    let envelope = change(&envelope, "protocol_result", Some(protocol));
    assert!(McpToolResult::from_value(&envelope, &limits).is_ok());
    let duplicate_source = change(
        &candidate,
        "sources",
        Some(Value::Array(vec![source.clone(), source])),
    );
    let extraction = change(
        member(&envelope, "extraction"),
        "candidates",
        Some(Value::Array(vec![duplicate_source])),
    );
    assert!(
        McpToolResult::from_value(&change(&envelope, "extraction", Some(extraction)), &limits)
            .is_err()
    );
}

#[test]
fn protocol_source_kinds_and_assessment_claims_are_independently_checked() {
    let limits = Limits::default();
    let value = selected_null();
    let invalid = change(
        &value,
        "protocol_result",
        Some(json(
            r#"{"content":[{"type":"image","data":"AA==","mimeType":"image/png"}]}"#,
        )),
    );
    assert!(McpToolResult::from_value(&invalid, &limits).is_err());
    let validation = change(
        member(&value, "validation"),
        "inferred_schema",
        Some(json(r#"{"status":"guaranteed"}"#)),
    );
    assert!(
        McpToolResult::from_value(&change(&value, "validation", Some(validation)), &limits)
            .is_err()
    );
    let error = htlk_executable::MetadataError::from(McpResultError::Shape("policy mode"));
    assert!(std::error::Error::source(&error).is_some());
    let extraction = member(&value, "extraction");
    let Value::Array(candidates) = member(extraction, "candidates") else {
        panic!()
    };
    let candidate = &candidates[0];
    for source in [
        json(r#"{"origin":"text","method":"fence","content_block_index":0,"byte_start":11}"#),
        json(r#"{"origin":"structured_content","method":"structured_content","byte_start":0}"#),
    ] {
        let candidate = change(candidate, "sources", Some(Value::Array(vec![source])));
        let extraction = change(
            extraction,
            "candidates",
            Some(Value::Array(vec![candidate])),
        );
        assert!(
            McpToolResult::from_value(&change(&value, "extraction", Some(extraction)), &limits)
                .is_err()
        );
    }
    for status in ["valid", "not_configured"] {
        let incomplete = json(&format!(
            r#"{{"status":"{status}","schema":"{}"}}"#,
            Digest::from_bytes([1; 32])
        ));
        let validation = change(
            member(&value, "validation"),
            "inferred_schema",
            Some(incomplete),
        );
        assert!(
            McpToolResult::from_value(&change(&value, "validation", Some(validation)), &limits)
                .is_err()
        );
    }
}

#[test]
fn bounded_diagnostic_sinks_propagate_formatting_failure_without_panicking() {
    use std::fmt::{self, Write};
    struct Sink(usize);
    impl Write for Sink {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            self.0 = self.0.checked_sub(value.len()).ok_or(fmt::Error)?;
            Ok(())
        }
    }
    let limits = Limits::default();
    let depth = Limits {
        max_depth: 0,
        ..limits.clone()
    };
    let bytes = Limits {
        max_document_bytes: 0,
        ..limits.clone()
    };
    let unsupported = Limits {
        max_depth: 129,
        ..limits.clone()
    };
    let errors = vec![
        htlk_executable::cbor::decode(&[0x18, 0], &limits).unwrap_err(),
        htlk_executable::cbor::decode(&[0], &bytes).unwrap_err(),
        htlk_executable::cbor::decode(&[0x81, 0], &depth).unwrap_err(),
        htlk_executable::cbor::decode(&[0], &unsupported).unwrap_err(),
    ];
    for error in errors {
        let rendered = error.to_string();
        for maximum in 0..rendered.len() {
            assert!(write!(&mut Sink(maximum), "{error}").is_err());
        }
        assert!(write!(&mut Sink(rendered.len()), "{error}").is_ok());
    }
    let nested =
        htlk_executable::GraphRecordError::Options(htlk_executable::ExecutionOptionsError::Codec(
            htlk_executable::cbor::decode(&[], &limits).unwrap_err(),
        ));
    assert!(!nested.to_string().is_empty());
}

#[test]
fn policy_identity_binds_all_interpretation_controls_and_origins() {
    let limits = Limits::default();
    let id = Digest::from_bytes([1; 32]);
    let evidence = Digest::from_bytes([2; 32]);
    let mut policy = ToolResultPolicy::default();
    assert_eq!(policy.declared_contract, DeclaredContractPolicy::Enforce);
    assert_eq!(
        InferredSchemaPolicy::default(),
        InferredSchemaPolicy::Observe
    );
    for selection in [
        CandidateSelection::Unique,
        CandidateSelection::Candidate("candidate_2".into()),
        CandidateSelection::Block(3),
        CandidateSelection::Schema(id),
    ] {
        policy.extraction.selection = selection;
        policy.declared_contract = DeclaredContractPolicy::Report;
        policy.inferred_schema = Some(AdoptedInference {
            schema: id,
            evidence,
            mode: InferredSchemaPolicy::Observe,
        });
        policy.application_schema = Some(evidence);
        let bytes = policy.encode(&limits).unwrap();
        assert_eq!(ToolResultPolicy::decode(&bytes, &limits).unwrap(), policy);
        let mut trailing = bytes;
        trailing.push(0);
        assert!(ToolResultPolicy::decode(&trailing, &limits).is_err());
    }
    let original = policy.extraction.digest(&limits).unwrap();
    policy.extraction.limits.work += 1;
    assert_ne!(original, policy.extraction.digest(&limits).unwrap());
    let canonical = policy.to_value(&limits).unwrap();
    for key in [
        "extraction",
        "declared_contract",
        "inferred_schema",
        "application_schema",
    ] {
        assert!(ToolResultPolicy::from_value(&change(&canonical, key, None), &limits).is_err());
    }
    assert!(
        ToolResultPolicy::from_value(&change(&canonical, "extra", Some(Value::Null)), &limits)
            .is_err()
    );
    let bad_mode = change(
        &canonical,
        "declared_contract",
        Some(Value::Text("observe".into())),
    );
    assert!(ToolResultPolicy::from_value(&bad_mode, &limits).is_err());
}

#[test]
fn unknown_profiles_invalid_limits_and_noncanonical_ids_do_not_default() {
    let limits = Limits::default();
    let policy = ExtractionPolicy::default().to_value(&limits).unwrap();
    assert!(
        ExtractionPolicy::from_value(
            &change(&policy, "profile", Some(Value::Text("future".into()))),
            &limits
        )
        .is_err()
    );
    let Value::Map(ceilings) = member(&policy, "limits") else {
        panic!()
    };
    for (key, _) in ceilings.iter() {
        let invalid = change(member(&policy, "limits"), key, Some(Value::Integer(0)));
        assert!(
            ExtractionPolicy::from_value(&change(&policy, "limits", Some(invalid)), &limits)
                .is_err()
        );
    }
    for id in [
        "candidate_",
        "candidate_01",
        "candidate_+1",
        "candidate_-1",
        "candidate_4294967296",
    ] {
        let invalid = change(
            &policy,
            "selection",
            Some(Value::Array(vec![
                Value::Text("candidate".into()),
                Value::Text(id.into()),
            ])),
        );
        assert!(ExtractionPolicy::from_value(&invalid, &limits).is_err());
    }
    let mut tight = limits.clone();
    tight.max_document_bytes = 1;
    assert!(matches!(
        ToolResultPolicy::default().encode(&tight),
        Err(McpResultError::Codec(_))
    ));
    assert!(McpToolResult::from_value(&selected_null(), &tight).is_err());
    assert!(builtin_record_type(BuiltinType::McpToolResult, &tight).is_err());
    let ty = ValueType::builtin(BuiltinType::McpToolResult);
    assert_eq!(
        ValueType::decode(
            &ty.encode(TypeContext::Value, &limits).unwrap(),
            TypeContext::Value,
            &limits
        )
        .unwrap(),
        ty
    );
}

#[test]
fn unified_binding_requires_explicit_policy_and_distinguishes_schema_roots_from_evidence() {
    use htlk_executable::{
        McpBinding, McpBindingKind, McpTransport, MetadataError, ServerIdentity,
    };
    use std::collections::BTreeSet;
    let limits = Limits::default();
    let id = |byte| Digest::from_bytes([byte; 32]);
    let mut policy = ToolResultPolicy {
        application_schema: Some(id(5)),
        inferred_schema: Some(AdoptedInference {
            schema: id(3),
            evidence: id(4),
            mode: InferredSchemaPolicy::Observe,
        }),
        ..ToolResultPolicy::default()
    };
    policy.extraction.selection = CandidateSelection::Schema(id(2));
    let binding = McpBinding::new(
        ServerIdentity::new(
            "test".into(),
            McpTransport::Stdio,
            "upstream".into(),
            "1".into(),
            &limits,
        )
        .unwrap(),
        id(0),
        McpBindingKind::Tool {
            name: "tool".into(),
            input_schema: id(1),
            output_schema: None,
            result_policy: Box::new(policy),
        },
        &limits,
    )
    .unwrap();
    assert_eq!(
        binding.schema_roots().collect::<BTreeSet<_>>(),
        BTreeSet::from([id(1), id(2), id(3), id(5)])
    );
    assert_eq!(binding.inference_evidence(), Some(id(4)));
    let value = binding.to_value(&limits).unwrap();
    assert_eq!(member(&value, "output_schema"), &Value::Null);
    assert_eq!(
        McpBinding::decode(&binding.encode(&limits).unwrap(), &limits).unwrap(),
        binding
    );
    for field in ["output_schema", "result_policy"] {
        assert!(
            matches!(McpBinding::from_value(&change(&value, field, None), &limits), Err(MetadataError::MissingField(name)) if name == field)
        );
    }
    let mut kind = binding.kind().clone();
    let McpBindingKind::Tool { result_policy, .. } = &mut kind else {
        panic!()
    };
    result_policy.extraction.limits.work += 1;
    let changed = McpBinding::new(
        binding.server().clone(),
        binding.descriptor(),
        kind,
        &limits,
    )
    .unwrap();
    assert_ne!(
        binding.digest(&limits).unwrap(),
        changed.digest(&limits).unwrap()
    );
}
