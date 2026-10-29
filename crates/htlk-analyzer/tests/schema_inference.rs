//! Structural generalization, immutable evidence, explicit revisions and inference bounds.

use htlk_analyzer::{
    InferenceContext, InferenceError as E, InferencePolicy, InferenceRevision, InferenceSample,
    InferenceSampleKind, NativeSchemaOptions, NativeSchemas, SampleObservation, SchemaCatalog,
    embedded_schema_base, infer_schema,
};
use htlk_executable::{
    InferredSchemaPolicy, JsonDocument, JsonPointer,
    cbor::{Limits, Map, Value},
    digest::Digest,
};

fn context() -> InferenceContext {
    InferenceContext {
        server: Digest::from_bytes([1; 32]),
        descriptor: Digest::from_bytes([2; 32]),
        extraction_policy: Digest::from_bytes([3; 32]),
    }
}
fn json(text: &str) -> JsonDocument {
    JsonDocument::new(text.as_bytes(), &Limits::default()).unwrap()
}
fn sample(reference: &str, value: &str) -> InferenceSample {
    InferenceSample {
        reference: reference.into(),
        value: json(value),
        context: context(),
        observation: SampleObservation {
            successful: true,
            complete: true,
            truncated: false,
            uncertain: false,
        },
        sanitized: false,
        excluded_fields: Vec::new(),
    }
}
fn infer(samples: &[InferenceSample]) -> InferenceRevision {
    infer_schema(
        samples,
        context(),
        &InferencePolicy::default(),
        &Limits::default(),
    )
    .unwrap()
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(value) = value else {
        panic!("record")
    };
    value.get(key).unwrap()
}
fn change(value: &Value, key: &str, replacement: Value) -> JsonDocument {
    let Value::Map(value) = value else {
        panic!("record")
    };
    let mut fields: Vec<_> = value
        .iter()
        .filter(|(name, _)| *name != key)
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect();
    fields.push((key.into(), replacement));
    JsonDocument::from_value(
        &Value::Map(Map::try_from_entries(fields).unwrap()),
        &Limits::default(),
    )
    .unwrap()
}
fn validates(revision: &InferenceRevision, value: &str) -> bool {
    let limits = Limits::default();
    let catalog = SchemaCatalog::new(
        vec![(
            embedded_schema_base(revision.schema(), &limits).unwrap(),
            revision.schema().clone(),
        )],
        &limits,
    )
    .unwrap();
    NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &limits)
        .unwrap()
        .validate_schema_document(&revision.schema().digest(), &json(value), &limits)
        .unwrap()
}

fn replace_path(value: &Value, path: &[&str], replacement: Value) -> Value {
    let Some((head, tail)) = path.split_first() else {
        return replacement;
    };
    match value {
        Value::Array(values) => {
            let mut values = values.clone();
            let index: usize = head.parse().unwrap();
            values[index] = replace_path(&values[index], tail, replacement);
            Value::Array(values)
        }
        Value::Map(_) => change(
            value,
            head,
            replace_path(field(value, head), tail, replacement),
        )
        .value()
        .clone(),
        _ => panic!("invalid fixture path"),
    }
}

#[test]
fn independent_revision_ingress_rejects_forged_sample_eligibility_and_metadata() {
    let limits = Limits::default();
    let revision = infer(&[
        sample("a", r#"{"private":"secret-value"}"#),
        sample("b", "null"),
    ]);
    let evidence = revision.evidence().value();
    for (path, replacement) in [
        (vec!["samples"], Value::Null),
        (vec!["samples", "0"], Value::Null),
        (
            vec!["samples", "0", "reference"],
            Value::Text(String::new()),
        ),
        (vec!["samples", "1", "reference"], Value::Text("a".into())),
        (
            vec!["samples", "0", "value_digest"],
            Value::Text("forged".into()),
        ),
        (vec!["samples", "0", "value_bytes"], Value::Integer(0)),
        (vec!["samples", "0", "value_bytes"], Value::Integer(-1)),
        (vec!["samples", "0", "value_bytes"], Value::Null),
        (
            vec!["samples", "0", "observation", "successful"],
            Value::Bool(false),
        ),
        (vec!["samples", "0", "observation", "complete"], Value::Null),
        (
            vec!["samples", "0", "excluded_reason"],
            Value::Text("incomplete".into()),
        ),
        (
            vec!["samples", "0", "excluded_fields"],
            json(r#"["/private"]"#).value().clone(),
        ),
        (vec!["source_flags", "uncertain"], Value::Bool(true)),
        (vec!["observations"], Value::Null),
        (vec!["observations"], json("{}").value().clone()),
        (vec!["observations", "", "types"], Value::Null),
        (
            vec!["observations", "", "types"],
            json(r#"{"invented":2}"#).value().clone(),
        ),
        (
            vec!["observations", "", "types"],
            json(r#"{"object":0}"#).value().clone(),
        ),
        (
            vec!["observations", "", "types"],
            json(r#"{"object":1}"#).value().clone(),
        ),
        (vec!["server"], Value::Null),
    ] {
        let malformed =
            JsonDocument::from_value(&replace_path(evidence, &path, replacement), &limits).unwrap();
        let error =
            InferenceRevision::from_documents(revision.schema().clone(), malformed, &limits)
                .unwrap_err();
        assert!(!error.to_string().contains("secret-value"), "{path:?}");
    }
    let mut sanitized = sample("a", "null");
    sanitized.sanitized = true;
    sanitized
        .excluded_fields
        .push(JsonPointer::new("/private", &limits).unwrap());
    let sanitized = infer(&[sanitized]);
    let malformed = replace_path(
        sanitized.evidence().value(),
        &["samples", "0", "excluded_fields"],
        json(r#"["invalid-pointer"]"#).value().clone(),
    );
    assert!(
        InferenceRevision::from_documents(
            sanitized.schema().clone(),
            JsonDocument::from_value(&malformed, &limits).unwrap(),
            &limits
        )
        .is_err()
    );
    for tight in [
        Limits {
            max_document_bytes: 1,
            ..limits.clone()
        },
        Limits {
            max_depth: 1,
            ..limits.clone()
        },
    ] {
        assert!(matches!(
            InferenceRevision::from_documents(
                revision.schema().clone(),
                revision.evidence().clone(),
                &tight
            ),
            Err(E::Limit { .. })
        ));
    }
}

#[test]
fn policy_ingress_and_difference_fail_closed_under_host_limits() {
    let limits = Limits::default();
    let original = InferencePolicy::default().document(&limits).unwrap();
    for (path, replacement) in [
        (vec!["profile"], Value::Text("future".into())),
        (vec!["sample_kind"], Value::Text("mixed".into())),
        (vec!["limits", "samples"], Value::Integer(0)),
        (vec!["limits", "depth"], Value::Integer(129)),
    ] {
        let document =
            JsonDocument::from_value(&replace_path(original.value(), &path, replacement), &limits)
                .unwrap();
        assert!(InferencePolicy::from_document(&document, &limits).is_err());
    }
    let errors = InferencePolicy {
        sample_kind: InferenceSampleKind::ToolError,
        ..Default::default()
    };
    assert_eq!(
        InferencePolicy::from_document(&errors.document(&limits).unwrap(), &limits).unwrap(),
        errors
    );
    let old = infer(&[sample("a", r#"{"x":1,"removed":true}"#)]);
    let new = infer(&[sample("b", r#"{"x":"changed"}"#)]);
    let difference = old.difference(&new, &Default::default(), &limits).unwrap();
    assert_eq!(
        field(difference.value(), "changed_paths"),
        json(r#"["/properties/removed","/properties/x/type","/required"]"#).value()
    );
    let mut other = sample("other", "null");
    other.context.server = Digest::from_bytes([9; 32]);
    let other = infer_schema(
        &[other.clone()],
        other.context,
        &Default::default(),
        &limits,
    )
    .unwrap();
    assert_eq!(
        old.difference(&other, &Default::default(), &limits),
        Err(E::Context)
    );
    for resource in ["depth", "nodes", "output_bytes"] {
        let mut policy = InferencePolicy::default();
        match resource {
            "depth" => policy.limits.depth = 1,
            "nodes" => policy.limits.nodes = 1,
            _ => policy.limits.output_bytes = 1,
        }
        assert!(
            matches!(old.difference(&new, &policy, &limits), Err(E::Limit { resource: actual, .. }) if actual == resource)
        );
    }
    // An independently encoded policy can reduce limits while keeping its own
    // digest valid. Revision admission must still recheck all selected evidence.
    for resource in [
        "samples",
        "sample_bytes",
        "nodes",
        "allocation_bytes",
        "output_bytes",
    ] {
        let mut policy = InferencePolicy::default();
        match resource {
            "samples" => policy.limits.samples = 1,
            "sample_bytes" => policy.limits.sample_bytes = 1,
            "nodes" => policy.limits.nodes = 1,
            "allocation_bytes" => policy.limits.allocation_bytes = 1,
            _ => policy.limits.output_bytes = 1,
        }
        let revision = infer(&[sample("a", "1"), sample("b", "2")]);
        let document = policy.document(&limits).unwrap();
        let evidence = change(
            revision.evidence().value(),
            "policy",
            document.value().clone(),
        );
        let evidence = change(
            evidence.value(),
            "policy_digest",
            Value::Text(document.digest().to_string()),
        );
        assert!(
            matches!(InferenceRevision::from_documents(revision.schema().clone(),evidence,&limits),Err(E::Limit {resource:actual,..}) if actual==resource),
            "{resource}"
        );
    }
}

#[test]
fn inference_errors_keep_typed_sources_and_redacted_operational_messages() {
    use std::error::Error;
    let json = JsonDocument::new(
        br#"{"private-secret":1,"private-secret":2}"#,
        &Limits::default(),
    )
    .unwrap_err();
    let codec = htlk_executable::cbor::decode(&[0xff], &Limits::default()).unwrap_err();
    for (error, wrapped) in [
        (E::Shape("sample metadata"), false),
        (E::Context, false),
        (E::ConflictingSample, false),
        (E::NoSamples, false),
        (
            E::Limit {
                resource: "samples",
                selected_samples: 2,
            },
            false,
        ),
        (E::from(json), true),
        (E::from(codec), true),
        (E::AllocationFailed, false),
    ] {
        assert!(!error.to_string().contains("private-secret"));
        assert_eq!(error.source().is_some(), wrapped);
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn primitives_numeric_generalization_and_empty_collections_are_not_incidental_enums() {
    for (value, schema) in [
        ("null", r#"{"type":"null"}"#),
        ("true", r#"{"type":"boolean"}"#),
        ("42", r#"{"type":"integer"}"#),
        ("1.0", r#"{"type":"integer"}"#),
        ("1.5", r#"{"type":"number"}"#),
        (r#""sample""#, r#"{"type":"string"}"#),
        ("[]", r#"{"type":"array","items":true}"#),
        (
            "{}",
            r#"{"type":"object","properties":{},"required":[],"additionalProperties":true}"#,
        ),
    ] {
        let revision = infer(&[sample("sample:one", value)]);
        assert_eq!(revision.schema(), &json(schema));
        assert_eq!(
            InferenceRevision::from_documents(
                revision.schema().clone(),
                revision.evidence().clone(),
                &Limits::default()
            )
            .unwrap(),
            revision
        );
    }
    let numbers = infer(&[sample("a", "1"), sample("b", "1.5")]);
    assert_eq!(numbers.schema(), &json(r#"{"type":"number"}"#));
    let strings = infer(&[sample("a", r#""red""#), sample("b", r#""blue""#)]);
    assert!(validates(&strings, r#""any unseen string""#));
    let empty = infer(&[sample("a", "[]"), sample("b", "[]")]);
    assert!(validates(&empty, r#"["unseen",{"anything":true}]"#));
}

#[test]
fn missing_null_and_nested_presence_are_independent_and_objects_remain_open() {
    let revision = infer(&[
        sample("a", r#"{"price":1,"discount":null,"nested":{"a":1}}"#),
        sample("b", r#"{"price":1.5,"currency":"USD","nested":{"b":true}}"#),
    ]);
    let schema = revision.schema().value();
    assert_eq!(
        field(schema, "required"),
        json(r#"["nested","price"]"#).value()
    );
    assert_eq!(
        field(field(schema, "properties"), "price"),
        json(r#"{"type":"number"}"#).value()
    );
    assert_eq!(
        field(field(schema, "properties"), "discount"),
        json(r#"{"type":"null"}"#).value()
    );
    assert!(validates(
        &revision,
        r#"{"price":20,"nested":{},"new_property":false}"#
    ));
    assert!(!validates(&revision, r#"{"price":"unknown","nested":{}}"#));
    let observations = field(revision.evidence().value(), "observations");
    let discount = field(observations, "/properties/discount");
    assert_eq!(field(discount, "present"), &Value::Integer(1));
    assert_eq!(field(discount, "total"), &Value::Integer(2));
    assert_eq!(field(field(discount, "types"), "null"), &Value::Integer(1));
    let nullable = infer(&[sample("a", r#"{"x":null}"#), sample("b", r#"{"x":"text"}"#)]);
    assert_eq!(
        field(nullable.schema().value(), "required"),
        json(r#"["x"]"#).value()
    );
    assert!(validates(&nullable, r#"{"x":null}"#));
    assert!(validates(&nullable, r#"{"x":"other"}"#));
    assert!(!validates(&nullable, "{}"));
}

#[test]
fn heterogeneous_array_unions_and_reordered_samples_have_stable_identity() {
    let a = sample("artifact:a", r#"[1,"x",null,{"a":1}]"#);
    let b = sample("artifact:b", r#"[1.5,{"b":true}]"#);
    let first = infer(&[a.clone(), b.clone()]);
    let second = infer(&[b.clone(), a.clone(), a.clone()]);
    assert_eq!(
        first, second,
        "reordering and duplicate source references do not revise evidence"
    );
    assert!(validates(&first, r#"[null,2.5,"new",{}]"#));
    assert!(!validates(&first, "[false]"));
    assert_eq!(
        field(first.evidence().value(), "sample_count"),
        &Value::Integer(2)
    );
    let repeated = infer(&[sample("a", "1"), sample("b", "1")]);
    assert_eq!(
        field(repeated.evidence().value(), "sample_count"),
        &Value::Integer(2)
    );
    let mut conflict = a.clone();
    conflict.value = json("[]");
    assert_eq!(
        infer_schema(
            &[a, conflict],
            context(),
            &InferencePolicy::default(),
            &Limits::default()
        ),
        Err(E::ConflictingSample)
    );
}

#[test]
fn excluded_samples_sanitization_and_error_inference_keep_explicit_origins() {
    let mut error = sample("error", r#"{"error":{"code":"UNKNOWN"}}"#);
    error.observation.successful = false;
    let mut truncated = sample("truncated", r#"{"wrong":1}"#);
    truncated.observation.truncated = true;
    let mut uncertain = sample("uncertain", r#"{"wrong":2}"#);
    uncertain.observation.uncertain = true;
    let mut incomplete = sample("incomplete", r#"{"wrong":3}"#);
    incomplete.observation.complete = false;
    let success = sample("success", r#"{"token":"private-marker-never-copy"}"#);
    let revision = infer(&[
        success.clone(),
        error.clone(),
        truncated,
        uncertain,
        incomplete,
    ]);
    assert_eq!(
        field(revision.evidence().value(), "sample_count"),
        &Value::Integer(1)
    );
    let Value::Array(excluded) = field(revision.evidence().value(), "excluded_samples") else {
        panic!()
    };
    assert_eq!(excluded.len(), 4);
    InferenceRevision::from_documents(
        revision.schema().clone(),
        revision.evidence().clone(),
        &Limits::default(),
    )
    .unwrap();
    assert!(
        !std::str::from_utf8(revision.evidence().as_bytes())
            .unwrap()
            .contains("private-marker-never-copy")
    );
    assert!(
        !std::str::from_utf8(revision.schema().as_bytes())
            .unwrap()
            .contains("private-marker-never-copy")
    );
    let policy = InferencePolicy {
        sample_kind: InferenceSampleKind::ToolError,
        ..InferencePolicy::default()
    };
    let errors = infer_schema(&[success, error], context(), &policy, &Limits::default()).unwrap();
    assert!(validates(&errors, r#"{"error":{"code":"ANOTHER"}}"#));
    let mut sanitized = sample("operator:sanitized", r#"{"keep":1}"#);
    sanitized
        .excluded_fields
        .push(JsonPointer::new("/secret", &Limits::default()).unwrap());
    assert!(matches!(
        infer_schema(
            &[sanitized.clone()],
            context(),
            &InferencePolicy::default(),
            &Limits::default()
        ),
        Err(E::Shape(_))
    ));
    sanitized.sanitized = true;
    let revision = infer(&[sanitized]);
    InferenceRevision::from_documents(
        revision.schema().clone(),
        revision.evidence().clone(),
        &Limits::default(),
    )
    .unwrap();
}

#[test]
fn revisions_are_explicit_and_differences_do_not_mutate_old_contracts() {
    let old = infer(&[sample("a", r#"{"price":1}"#)]);
    let original = old.clone();
    let new = infer(&[
        sample("a", r#"{"price":1}"#),
        sample("b", r#"{"price":2,"currency":"USD"}"#),
    ]);
    assert_eq!(old, original);
    assert_ne!(old.schema().digest(), new.schema().digest());
    assert!(
        validates(&old, r#"{"price":3,"currency":42}"#),
        "the old schema already permits additional fields"
    );
    assert!(
        !validates(&new, r#"{"price":3,"currency":42}"#),
        "even optional new property constraints change acceptance"
    );
    let difference = old
        .difference(&new, &InferencePolicy::default(), &Limits::default())
        .unwrap();
    assert_eq!(
        field(difference.value(), "changed_paths"),
        json(r#"["/properties/currency"]"#).value()
    );
    let observe = new.adoption(InferredSchemaPolicy::Observe);
    let enforce = new.adoption(InferredSchemaPolicy::Enforce);
    assert_eq!(observe.schema, enforce.schema);
    assert_eq!(observe.evidence, enforce.evidence);
    assert_ne!(observe.mode, enforce.mode);
    let old = infer(&[
        sample("a", r#"{"a/b~":1}"#),
        sample("b", r#"{"a/b~":"text"}"#),
    ]);
    let new = infer(&[
        sample("c", r#"{"a/b~":1}"#),
        sample("d", r#"{"a/b~":true}"#),
    ]);
    let difference = old
        .difference(&new, &Default::default(), &Limits::default())
        .unwrap();
    assert_eq!(
        field(difference.value(), "changed_paths"),
        json(r#"["/properties/a~1b~0/anyOf"]"#).value()
    );
}

#[test]
fn every_inference_limit_fails_without_a_partially_adoptable_revision() {
    let limits = Limits::default();
    for resource in [
        "samples",
        "depth",
        "sample_bytes",
        "nodes",
        "allocation_bytes",
        "output_bytes",
    ] {
        let mut policy = InferencePolicy::default();
        match resource {
            "samples" => policy.limits.samples = 1,
            "depth" => policy.limits.depth = 1,
            "sample_bytes" => policy.limits.sample_bytes = 1,
            "nodes" => policy.limits.nodes = 1,
            "allocation_bytes" => policy.limits.allocation_bytes = 1,
            "output_bytes" => policy.limits.output_bytes = 1,
            _ => unreachable!(),
        }
        let result = infer_schema(
            &[sample("a", r#"{"nested":{"x":1}}"#), sample("b", "null")],
            context(),
            &policy,
            &limits,
        );
        assert!(
            matches!(result, Err(E::Limit { resource: actual, .. }) if actual == resource),
            "{resource}: {result:?}"
        );
    }
    assert_eq!(
        infer_schema(&[], context(), &InferencePolicy::default(), &limits),
        Err(E::NoSamples)
    );
    let mut wrong = sample("wrong", "1");
    wrong.context.descriptor = Digest::from_bytes([4; 32]);
    assert_eq!(
        infer_schema(&[wrong], context(), &InferencePolicy::default(), &limits),
        Err(E::Context)
    );
    let mut metadata = sample("operator:metadata", "1");
    metadata.sanitized = true;
    metadata.excluded_fields = vec![JsonPointer::new("", &limits).unwrap(); 1000];
    let mut policy = InferencePolicy::default();
    policy.limits.allocation_bytes = 8192;
    assert!(matches!(
        infer_schema(&[metadata], context(), &policy, &limits),
        Err(E::Limit {
            resource: "allocation_bytes",
            ..
        })
    ));
}

#[test]
fn canonical_evidence_rejects_forged_schema_policy_counts_and_presence() {
    let limits = Limits::default();
    let revision = infer(&[sample("a", r#"{"a/b~":1}"#)]);
    let evidence = revision.evidence();
    for (key, value) in [
        (
            "schema",
            Value::Text(Digest::from_bytes([9; 32]).to_string()),
        ),
        (
            "policy_digest",
            Value::Text(Digest::from_bytes([9; 32]).to_string()),
        ),
        ("sample_count", Value::Integer(2)),
        ("complete", Value::Bool(false)),
        ("limit_exhausted", Value::Text("nodes".into())),
        ("algorithm", Value::Text("future".into())),
        ("extra", Value::Null),
    ] {
        let malformed = change(evidence.value(), key, value);
        assert!(
            InferenceRevision::from_documents(revision.schema().clone(), malformed, &limits)
                .is_err(),
            "{key}"
        );
    }
    let observations = field(evidence.value(), "observations");
    assert_eq!(
        field(field(observations, "/properties/a~1b~0"), "present"),
        &Value::Integer(1)
    );
    let inconsistent = change(field(observations, ""), "present", Value::Integer(2));
    let observations = change(observations, "", inconsistent.value().clone());
    let malformed = change(
        evidence.value(),
        "observations",
        observations.value().clone(),
    );
    assert!(
        InferenceRevision::from_documents(revision.schema().clone(), malformed, &limits).is_err()
    );
    let policy = InferencePolicy::default();
    assert_eq!(
        InferencePolicy::from_document(&policy.document(&limits).unwrap(), &limits).unwrap(),
        policy
    );
    let mut invalid = policy.clone();
    invalid.limits.depth = 0;
    assert!(invalid.document(&limits).is_err());
}

#[test]
fn default_sample_ceiling_accepts_realistic_references_and_depth_stops_on_small_stacks() {
    let samples: Vec<_> = (0..256)
        .map(|index| sample(&format!("artifact:sha256:{index:064x}"), "1"))
        .collect();
    let revision = infer(&samples);
    assert_eq!(
        field(revision.evidence().value(), "sample_count"),
        &Value::Integer(256)
    );
    InferenceRevision::from_documents(
        revision.schema().clone(),
        revision.evidence().clone(),
        &Limits::default(),
    )
    .unwrap();
    let deep = format!("{}0{}", "[".repeat(64), "]".repeat(64));
    let value = sample("deep", &deep);
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut policy = InferencePolicy::default();
            policy.limits.depth = 4;
            assert!(matches!(
                infer_schema(&[value], context(), &policy, &Limits::default()),
                Err(E::Limit {
                    resource: "depth",
                    ..
                })
            ));
        })
        .unwrap()
        .join()
        .unwrap();
    let old = infer(&[sample("old", "{}")]);
    let new = infer(&[sample("new", r#"{"a/b~":1}"#)]);
    let difference = old
        .difference(&new, &InferencePolicy::default(), &Limits::default())
        .unwrap();
    assert_eq!(
        field(difference.value(), "changed_paths"),
        json(r#"["/properties/a~1b~0","/required"]"#).value()
    );
}
