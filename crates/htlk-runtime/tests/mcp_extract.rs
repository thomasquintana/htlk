//! Literal JSON extraction, conservative selection, original spans and adversarial bounds.

use htlk_analyzer::{NativeSchemaOptions, NativeSchemas, SchemaCatalog};
use htlk_executable::{
    CandidateSelection, ExtractionPolicy, JsonDocument, McpToolResult,
    cbor::{Limits, Map, Value},
};
use htlk_runtime::{
    ExtractionError, ExtractionSource, ExtractionStatus as S, TextExtractionMethod as M,
    extract_mcp_json,
};

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
fn protocol(texts: &[&str], structured: Option<Value>) -> JsonDocument {
    let content = texts
        .iter()
        .map(|text| {
            object(vec![
                ("type", Value::Text("text".into())),
                ("text", Value::Text((*text).into())),
            ])
        })
        .collect();
    let mut fields = vec![("content", Value::Array(content))];
    if let Some(value) = structured {
        fields.push(("structuredContent", value));
    }
    JsonDocument::from_value(&object(fields), &Limits::default()).unwrap()
}
fn envelope(document: &JsonDocument, report: &htlk_runtime::ExtractionReport) -> McpToolResult {
    let absent = || object(vec![("status", Value::Text("not_configured".into()))]);
    let mut fields = vec![
        ("protocol_result", document.value().clone()),
        ("extraction", report.to_value(&Limits::default()).unwrap()),
        (
            "validation",
            object(vec![
                ("declared_schema", absent()),
                ("inferred_schema", absent()),
                ("application_schema", absent()),
            ]),
        ),
    ];
    if let Some(value) = report.data() {
        fields.push(("data", value.clone()));
    }
    McpToolResult::from_value(&object(fields), &Limits::default()).unwrap()
}

#[test]
fn whole_blocks_and_fences_accept_every_json_root_without_prose_interpretation() {
    let limits = Limits::default();
    for value in [
        r#"{"a":1}"#,
        "[1,true,null]",
        r#""text""#,
        "42",
        "1.5",
        "true",
        "false",
        "null",
    ] {
        for (text, expected_method) in [
            (format!(" \n{value}\t"), M::WholeBlock),
            (
                format!("Here is the value:\n```json\n{value}\n```\nDone."),
                M::Fence,
            ),
            (format!("  ~~~~JSON\n{value}\n~~~~~\n"), M::Fence),
        ] {
            let document = protocol(&[&text], None);
            let report =
                extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
            assert_eq!(report.status(), S::Selected, "{text}");
            assert_eq!(report.data(), Some(&json(value)));
            assert!(
                matches!(report.candidates()[0].sources()[0], ExtractionSource::Text { method, .. } if method == expected_method)
            );
            envelope(&document, &report);
        }
    }
    for text in [
        "",
        "Navigation completed. Page title: Store.",
        "There are 42 items; true and null are words here.",
        "```javascript\n42\n```",
        "```json\n42",
    ] {
        let document = protocol(&[text], None);
        let report =
            extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
        assert_eq!(report.status(), S::None, "{text}");
        assert!(report.data().is_none());
        envelope(&document, &report);
    }
}

#[test]
fn quoted_braces_escapes_and_utf8_spans_preserve_exact_source_evidence() {
    let value = r#"{"nested":{"text":"brace } and quote \" and backslash \\"},"items":[{"x":1}]}"#;
    let prefix = "Résumé: ";
    let text = format!("{prefix}{value} finished");
    let document = protocol(&[&text], None);
    let report = extract_mcp_json(
        &document,
        &ExtractionPolicy::default(),
        None,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(report.status(), S::Selected);
    assert_eq!(
        report.candidates().len(),
        1,
        "nested documents are not independent answers"
    );
    assert_eq!(report.data(), Some(&json(value)));
    assert_eq!(
        report.candidates()[0].sources(),
        &[ExtractionSource::Text {
            block: 0,
            start: prefix.len(),
            end: prefix.len() + value.len(),
            method: M::Embedded
        }]
    );
    envelope(&document, &report);
}

#[test]
fn ambiguity_deduplication_and_structured_preference_are_deterministic() {
    let limits = Limits::default();
    let document = protocol(&[r#"first {"id":1} second {"id":2}"#], None);
    let report = extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
    assert_eq!(report.status(), S::Ambiguous);
    assert!(report.data().is_none());
    assert_eq!(report.candidates().len(), 2);
    assert_eq!(
        report,
        extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap()
    );
    envelope(&document, &report);

    let document = protocol(
        &[r#"{"id":1}"#, "same value: {\"id\":1}"],
        Some(json(r#"{"id":1}"#)),
    );
    let report = extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
    assert_eq!(report.status(), S::Selected);
    assert_eq!(report.candidates().len(), 1);
    assert_eq!(report.candidates()[0].sources().len(), 3);
    envelope(&document, &report);

    let document = protocol(&[r#"{"id":2}"#], Some(json(r#"{"id":1}"#)));
    let report = extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
    assert_eq!(report.selected_index(), Some(0));
    assert_eq!(report.data(), Some(&json(r#"{"id":1}"#)));
    assert_eq!(report.candidates().len(), 2);
    envelope(&document, &report);
}

#[test]
fn malformed_documents_do_not_repair_or_prevent_later_independent_json() {
    let limits = Limits::default();
    for text in [
        r#"broken {"bad":} then {"ok":true}"#,
        r#"prefix {"bad":"\q"} then {"ok":true}"#,
    ] {
        let document = protocol(&[text], None);
        let report =
            extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
        assert_eq!(report.data(), Some(&json(r#"{"ok":true}"#)));
        assert!(!report.diagnostics().is_empty());
        envelope(&document, &report);
    }
    for text in [
        r#"{"duplicate":1,"duplicate":2}"#,
        "9007199254740993",
        "{missing:quotes}",
    ] {
        let report = extract_mcp_json(
            &protocol(&[text], None),
            &ExtractionPolicy::default(),
            None,
            &limits,
        )
        .unwrap();
        assert_eq!(report.status(), S::None);
        assert!(!report.diagnostics().is_empty());
    }
    let document = protocol(&[r#"{"part":"#, "42}"], None);
    let report = extract_mcp_json(&document, &ExtractionPolicy::default(), None, &limits).unwrap();
    assert_eq!(
        report.status(),
        S::None,
        "unrelated blocks must never be joined"
    );
}

#[test]
fn explicit_candidate_block_and_schema_selection_are_checked_not_guessed() {
    let limits = Limits::default();
    let document = protocol(&[r#"{"first":1}"#, r#"{"wanted":2}"#], None);
    for selection in [
        CandidateSelection::Candidate("candidate_1".into()),
        CandidateSelection::Block(1),
    ] {
        let policy = ExtractionPolicy {
            selection,
            ..ExtractionPolicy::default()
        };
        let report = extract_mcp_json(&document, &policy, None, &limits).unwrap();
        assert_eq!(report.data(), Some(&json(r#"{"wanted":2}"#)));
    }
    let schema = JsonDocument::new(br#"{"type":"object","required":["wanted"]}"#, &limits).unwrap();
    let catalog = SchemaCatalog::new(
        vec![(
            htlk_analyzer::embedded_schema_base(&schema, &limits).unwrap(),
            schema.clone(),
        )],
        &limits,
    )
    .unwrap();
    let native = NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &limits).unwrap();
    let policy = ExtractionPolicy {
        selection: CandidateSelection::Schema(schema.digest()),
        ..ExtractionPolicy::default()
    };
    assert_eq!(
        extract_mcp_json(&document, &policy, None, &limits),
        Err(ExtractionError::SchemaUnavailable)
    );
    let report = extract_mcp_json(&document, &policy, Some(&native), &limits).unwrap();
    assert_eq!(report.data(), Some(&json(r#"{"wanted":2}"#)));
    let unlinked = ExtractionPolicy {
        selection: CandidateSelection::Schema(htlk_executable::digest::Digest::from_bytes([9; 32])),
        ..Default::default()
    };
    assert!(matches!(
        extract_mcp_json(&document, &unlinked, Some(&native), &limits),
        Err(ExtractionError::Schema(
            htlk_analyzer::NativeSchemaError::UnknownRoot
        ))
    ));
    let document = protocol(&[r#"{"wanted":1} {"wanted":2}"#], None);
    assert_eq!(
        extract_mcp_json(&document, &policy, Some(&native), &limits)
            .unwrap()
            .status(),
        S::Ambiguous
    );
}

#[test]
fn every_cumulative_limit_withholds_partial_selection_and_stops_before_overrun() {
    let limits = Limits::default();
    for resource in [
        "inspected_bytes",
        "candidates",
        "depth",
        "work",
        "parsed_bytes",
        "allocation_bytes",
        "output_bytes",
        "diagnostic_bytes",
    ] {
        let mut policy = ExtractionPolicy::default();
        let mut document = protocol(&[r#"{"a":1}"#, r#"{"b":2}"#], None);
        match resource {
            "inspected_bytes" => policy.limits.inspected_bytes = 7,
            "candidates" => policy.limits.candidates = 1,
            "depth" => {
                policy.limits.depth = 1;
                document = protocol(&["[[[0]]]"], None);
            }
            "work" => policy.limits.work = 1,
            "parsed_bytes" => policy.limits.parsed_bytes = 7,
            "allocation_bytes" => policy.limits.allocation_bytes = 1,
            "output_bytes" => policy.limits.output_bytes = 512,
            "diagnostic_bytes" => {
                policy.limits.diagnostic_bytes = 1;
                document = protocol(&["plain prose"], None);
            }
            _ => unreachable!(),
        }
        let report = extract_mcp_json(&document, &policy, None, &limits).unwrap();
        assert_eq!(report.status(), S::Exhausted, "{resource}");
        assert_eq!(report.exhausted_limit(), Some(resource));
        assert!(report.data().is_none());
        assert!(report.selected_index().is_none());
        let usage = report.usage();
        assert!(usage.inspected_bytes <= policy.limits.inspected_bytes);
        assert!(usage.work <= policy.limits.work);
        assert!(usage.parsed_bytes <= policy.limits.parsed_bytes);
        assert!(usage.allocation_bytes <= policy.limits.allocation_bytes);
        assert!(usage.output_bytes <= policy.limits.output_bytes);
        assert!(usage.diagnostic_bytes <= policy.limits.diagnostic_bytes);
        envelope(&document, &report);
    }
    let braces = format!("{}!{}", "{".repeat(30), "}".repeat(30));
    let mut policy = ExtractionPolicy::default();
    policy.limits.work = 2000;
    let report = extract_mcp_json(&protocol(&[&braces], None), &policy, None, &limits).unwrap();
    assert_eq!(report.exhausted_limit(), Some("work"));
    let braces = "{".repeat(10000);
    assert_eq!(
        extract_mcp_json(
            &protocol(&[&braces], None),
            &ExtractionPolicy::default(),
            None,
            &limits
        )
        .unwrap()
        .exhausted_limit(),
        Some("depth")
    );
}

#[test]
fn protocol_failures_are_never_salvaged_and_tool_errors_remain_evidence() {
    let limits = Limits::default();
    let malformed = JsonDocument::new(br#"{"content":"{\"looks\":\"valid\"}"}"#, &limits).unwrap();
    assert!(matches!(
        extract_mcp_json(&malformed, &ExtractionPolicy::default(), None, &limits),
        Err(ExtractionError::Protocol(_))
    ));
    let error = JsonDocument::new(br#"{"isError":true,"content":[{"type":"text","text":"Error details: {\"status\":\"unknown\"}"}]}"#, &limits).unwrap();
    let original = error.clone();
    let report = extract_mcp_json(&error, &ExtractionPolicy::default(), None, &limits).unwrap();
    assert_eq!(
        report.status(),
        S::Selected,
        "selection is evidence, not tool success"
    );
    assert_eq!(error, original, "isError remains untouched");
    envelope(&error, &report);
}

#[test]
fn late_budget_exhaustion_cannot_publish_a_preferred_or_partially_scanned_candidate() {
    let limits = Limits::default();
    let document = protocol(
        &[
            "display\n```json\n{\"value\":1}\n```\ntrailing",
            "mismatch {] later {\"nested\":{\"value\":2}}",
            "duplicate {\"value\":1}",
        ],
        Some(json(r#"{"preferred":[1,{"x":"retained"}]}"#)),
    );
    let complete = extract_mcp_json(&document, &Default::default(), None, &limits).unwrap();
    assert_eq!(complete.status(), S::Selected);
    let usage = complete.usage();
    for (resource, maximum) in [
        ("work", usage.work),
        ("allocation_bytes", usage.allocation_bytes),
        ("output_bytes", usage.output_bytes),
    ] {
        let mut boundaries: Vec<_> = (1..maximum).step_by((maximum / 32).max(1)).collect();
        boundaries.extend([maximum - 1, maximum]);
        for bound in boundaries {
            let mut policy = ExtractionPolicy::default();
            match resource {
                "work" => policy.limits.work = bound,
                "allocation_bytes" => policy.limits.allocation_bytes = bound,
                _ => policy.limits.output_bytes = bound,
            }
            let report = extract_mcp_json(&document, &policy, None, &limits).unwrap();
            if bound == maximum {
                assert_eq!(report.data(), complete.data(), "{resource}/{bound}");
            } else {
                assert_eq!(
                    report.exhausted_limit(),
                    Some(resource),
                    "{resource}/{bound}"
                );
                assert_eq!(report.status(), S::Exhausted);
                assert!(report.data().is_none() && report.selected_index().is_none());
            }
        }
    }
    for selection in [
        CandidateSelection::Candidate("candidate_99".into()),
        CandidateSelection::Block(99),
    ] {
        let policy = ExtractionPolicy {
            selection,
            ..Default::default()
        };
        let report = extract_mcp_json(&document, &policy, None, &limits).unwrap();
        assert_eq!(report.status(), S::None);
        assert!(!report.candidates().is_empty());
        assert!(report.data().is_none());
    }
}
