//! Manifest completeness, influence preservation, frozen hashes and stable outcomes.
use htlk_executable::{
    BuiltinType, Port, PortTable, ValueType,
    cbor::{self, Limits, Map, Value},
};
use htlk_runtime::{
    EvaluationError,
    failure::{FailureBoundary, FailureCode, RuntimeFailure},
    identity::{ArtifactId, RunId, ValueDigest, host_request_digest},
    records::*,
};

fn ports(required: bool) -> PortTable {
    PortTable::new(
        vec![(
            "seed".parse().unwrap(),
            Port::new(ValueType::builtin(BuiltinType::String), required),
        )],
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn all_failure_codes_round_trip_and_malformed_closed_records_fail_without_leaking_data() {
    use htlk_runtime::failure::FailureRecordError;
    use std::error::Error as _;
    let l = Limits::default();
    let codes = [
        FailureCode::Executable,
        FailureCode::Digest,
        FailureCode::RetentionExpired,
        FailureCode::Profile,
        FailureCode::Input,
        FailureCode::BindingConflict,
        FailureCode::Dependency,
        FailureCode::OutputMissing,
        FailureCode::Expression,
        FailureCode::ExpressionAbsent,
        FailureCode::ExpressionLimit,
        FailureCode::Preconditions,
        FailureCode::Postconditions,
        FailureCode::McpIdentity,
        FailureCode::McpDescriptor,
        FailureCode::McpTransport,
        FailureCode::McpTimeout,
        FailureCode::McpProtocol,
        FailureCode::McpToolError,
        FailureCode::OutputSchema,
        FailureCode::EffectUncertain,
        FailureCode::WaitExpired,
        FailureCode::ApprovalDenied,
        FailureCode::Policy,
        FailureCode::Budget,
        FailureCode::BudgetUnsupported,
        FailureCode::Timeout,
        FailureCode::LoopLimit,
        FailureCode::ExtensionConflict,
        FailureCode::Extension,
        FailureCode::IdempotencyConflict,
        FailureCode::Cancelled,
        FailureCode::Fenced,
        FailureCode::Invariant,
    ];
    for code in codes {
        let failure = RuntimeFailure::new(code);
        let value = failure.to_value(&l).unwrap();
        assert_eq!(RuntimeFailure::from_value(&value, &l).unwrap(), failure);
        assert_eq!(code.as_str().parse::<FailureCode>().unwrap(), code);
        assert_eq!(failure.message(), code.message());
        assert!(failure.to_string().contains(code.as_str()));
        assert!(failure.source().is_none());
        let bytes = cbor::encode(&value, &l).unwrap();
        let exact = Limits {
            max_document_bytes: bytes.len(),
            ..l.clone()
        };
        assert_eq!(failure.to_value(&exact).unwrap(), value);
        let tiny = Limits {
            max_document_bytes: bytes.len() - 1,
            ..l.clone()
        };
        let error = failure.to_value(&tiny).unwrap_err();
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
    }
    for value in [
        Value::Null,
        Value::Map(Map::new()),
        Value::Map(
            Map::try_from_entries([
                ("code".into(), Value::Bool(false)),
                ("message".into(), Value::Text("SECRET".into())),
            ])
            .unwrap(),
        ),
        Value::Map(
            Map::try_from_entries([
                ("code".into(), Value::Text("E_POLICY".into())),
                ("message".into(), Value::Text("SECRET".into())),
                ("extra".into(), Value::Null),
            ])
            .unwrap(),
        ),
    ] {
        let error = RuntimeFailure::from_value(&value, &l).unwrap_err();
        assert_eq!(error, FailureRecordError::Shape);
        assert!(error.source().is_none());
        assert!(!error.to_string().contains("SECRET"));
    }
    assert_eq!(
        "UNKNOWN_SECRET".parse::<FailureCode>(),
        Err(FailureRecordError::Shape)
    );
    let error = FailureRecordError::from(cbor::decode(&[], &l).unwrap_err());
    assert!(error.source().is_some());
    assert!(!error.to_string().is_empty());
}
fn entry(producer: &str, labels: &[&str]) -> ManifestEntry {
    let l = Limits::default();
    let content_digest = ValueDigest::new(
        &ValueType::builtin(BuiltinType::String),
        &Value::Text("same bytes".into()),
        &l,
    )
    .unwrap();
    let artifact_id = ArtifactId::run_input(
        &RunId::new(producer.into(), &l).unwrap(),
        0,
        &"seed".parse().unwrap(),
        content_digest,
        &l,
    )
    .unwrap();
    ManifestEntry::Value {
        artifact_id,
        content_digest,
        security_labels: SecurityLabels::new(labels.iter().map(|s| (*s).into()).collect(), &l)
            .unwrap(),
    }
}
fn freeze(port: &PortTable, entry: ManifestEntry, influence: SecurityLabels) -> InputManifest {
    InputManifest::freeze(
        port,
        vec![("seed".parse().unwrap(), entry)],
        influence,
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn manifest_hash_excludes_provenance_but_includes_declarations_and_influences() {
    let l = Limits::default();
    let a = freeze(
        &ports(true),
        entry("producer-a", &["protected"]),
        SecurityLabels::default(),
    );
    let b = freeze(
        &ports(true),
        entry("producer-b", &["protected"]),
        SecurityLabels::default(),
    );
    assert_ne!(a.to_value(&l).unwrap(), b.to_value(&l).unwrap());
    assert_eq!(a.input_hash(), b.input_hash());
    assert_eq!(a.influence_labels().as_slice(), &["protected"]);
    let optional = freeze(
        &ports(false),
        entry("producer-a", &["protected"]),
        SecurityLabels::default(),
    );
    assert_ne!(a.input_hash(), optional.input_hash());
    let unlabeled = freeze(
        &ports(true),
        entry("producer-a", &[]),
        SecurityLabels::default(),
    );
    assert_ne!(a.input_hash(), unlabeled.input_hash());
    let guard = SecurityLabels::new(vec!["secret-guard".into()], &l).unwrap();
    let influenced = freeze(&ports(true), entry("producer-a", &["protected"]), guard);
    assert_ne!(a.input_hash(), influenced.input_hash());
    let json_ports = PortTable::new(
        vec![(
            "seed".parse().unwrap(),
            Port::new(ValueType::builtin(BuiltinType::Json), true),
        )],
        &l,
    )
    .unwrap();
    assert_ne!(
        a.input_hash(),
        freeze(
            &json_ports,
            entry("producer-a", &["protected"]),
            SecurityLabels::default()
        )
        .input_hash()
    );
}

#[test]
fn explicit_absence_is_complete_and_preserves_control_labels() {
    let l = Limits::default();
    let p = ports(false);
    let absent = freeze(
        &p,
        ManifestEntry::Absent,
        SecurityLabels::new(vec!["guard".into()], &l).unwrap(),
    );
    let public = freeze(&p, ManifestEntry::Absent, SecurityLabels::default());
    assert_ne!(absent.input_hash(), public.input_hash());
    let Value::Map(manifest) = absent.to_value(&l).unwrap() else {
        panic!()
    };
    assert_eq!(
        manifest.get("seed"),
        Some(&Value::Map(
            Map::try_from_entries([("state".into(), Value::Text("absent".into()))]).unwrap()
        ))
    );
    assert!(InputManifest::freeze(&p, vec![], SecurityLabels::default(), &l).is_err());
    assert!(
        InputManifest::freeze(
            &ports(true),
            vec![("seed".parse().unwrap(), ManifestEntry::Absent)],
            SecurityLabels::default(),
            &l
        )
        .is_err()
    );
    assert!(
        InputManifest::freeze(
            &p,
            vec![("unknown".parse().unwrap(), ManifestEntry::Absent)],
            SecurityLabels::default(),
            &l
        )
        .is_err()
    );
    assert!(
        InputManifest::freeze(
            &p,
            vec![
                ("seed".parse().unwrap(), ManifestEntry::Absent),
                ("seed".parse().unwrap(), ManifestEntry::Absent)
            ],
            SecurityLabels::default(),
            &l
        )
        .is_err()
    );
    let empty = InputManifest::freeze(&PortTable::default(), vec![], SecurityLabels::default(), &l)
        .unwrap();
    assert_eq!(empty.entries().len(), 0);
    assert_ne!(empty.input_hash(), public.input_hash());
}

#[test]
fn checkpoints_recompute_hash_and_reject_wrong_versions_and_declarations() {
    let l = Limits::default();
    let manifest = freeze(
        &ports(true),
        entry("producer", &["secret"]),
        SecurityLabels::default(),
    );
    let bytes = manifest.encode(&l).unwrap();
    assert_eq!(
        InputManifest::decode(&bytes, &ports(true), &l).unwrap(),
        manifest
    );
    assert_eq!(
        InputManifest::decode(&bytes, &ports(false), &l),
        Err(RuntimeRecordError::Integrity)
    );
    let Value::Map(record) = cbor::decode(&bytes, &l).unwrap() else {
        panic!()
    };
    for (field, replacement, integrity) in [
        (
            "input_hash",
            Value::Text(format!("sha256:{}", "0".repeat(64))),
            true,
        ),
        ("record_version", Value::Text("0.2".into()), false),
        (
            "influence_labels",
            Value::Array(vec![Value::Text("other".into())]),
            true,
        ),
        ("influence_labels", Value::Array(vec![]), true),
    ] {
        let modified = Value::Map(
            Map::try_from_entries(record.iter().map(|(k, v)| {
                (
                    k.to_string(),
                    if k == field {
                        replacement.clone()
                    } else {
                        v.clone()
                    },
                )
            }))
            .unwrap(),
        );
        let result = InputManifest::decode(&cbor::encode(&modified, &l).unwrap(), &ports(true), &l);
        assert!(result.is_err());
        if integrity {
            assert_eq!(result, Err(RuntimeRecordError::Integrity));
        }
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(InputManifest::decode(&trailing, &ports(true), &l).is_err());
}

#[test]
fn labels_and_tagged_ingress_are_bounded_without_null_or_object_guessing() {
    let l = Limits::default();
    let labels = SecurityLabels::new(vec!["z".into(), "a".into(), "z".into()], &l).unwrap();
    assert_eq!(labels.as_slice(), &["a", "z"]);
    let other = SecurityLabels::new(vec!["z".into(), "é".into()], &l).unwrap();
    assert_eq!(
        labels.union(&other, &l).unwrap().as_slice(),
        &["a", "z", "é"]
    );
    assert!(
        SecurityLabels::from_value(
            &Value::Array(vec![Value::Text("z".into()), Value::Text("a".into())]),
            &l
        )
        .is_err()
    );
    let secret = InputValue::Inline(Value::Map(
        Map::try_from_entries([("artifact_id".into(), Value::Text("not-a-reference".into()))])
            .unwrap(),
    ));
    assert_eq!(
        InputValue::from_value(&secret.to_value(&l).unwrap(), &l).unwrap(),
        secret
    );
    assert!(!format!("{secret:?}").contains("not-a-reference"));
    let null = InputValue::Inline(Value::Null);
    assert_eq!(
        InputValue::from_value(&null.to_value(&l).unwrap(), &l).unwrap(),
        null
    );
    let bad = Value::Map(
        Map::try_from_entries([
            ("kind".into(), Value::Text("artifact".into())),
            ("value".into(), Value::Null),
        ])
        .unwrap(),
    );
    assert!(InputValue::from_value(&bad, &l).is_err());
    assert!(
        secret
            .to_value(&Limits {
                max_document_bytes: 4,
                ..l.clone()
            })
            .is_err()
    );
    assert!(labels.to_value(&Limits { max_depth: 0, ..l }).is_err());
}

#[test]
fn checkpoint_encoding_charges_full_record_headers_and_nested_labels() {
    let l = Limits::default();
    let manifest = freeze(
        &ports(true),
        entry("producer", &["a", "b"]),
        SecurityLabels::default(),
    );
    let bytes = manifest.encode(&l).unwrap();
    let mut exact = Limits {
        max_document_bytes: bytes.len(),
        ..l.clone()
    };
    assert_eq!(manifest.encode(&exact).unwrap(), bytes);
    exact.max_document_bytes -= 1;
    assert!(manifest.encode(&exact).is_err());
    let shallow = Limits { max_depth: 3, ..l };
    assert!(manifest.encode(&shallow).is_err());
    assert!(manifest.to_value(&shallow).is_ok());
}

#[test]
fn all_terminal_lifecycles_are_immutable_and_retries_do_not_revive_failure() {
    use InvocationStatus::*;
    let states = [Pending, Active, Succeeded, Failed, Skipped, Cancelled];
    let expected = [
        (Pending, Active),
        (Pending, Failed),
        (Pending, Skipped),
        (Pending, Cancelled),
        (Active, Succeeded),
        (Active, Failed),
        (Active, Cancelled),
    ];
    for before in states {
        for after in states {
            assert_eq!(
                before.can_transition_to(after),
                expected.contains(&(before, after)),
                "{before:?} -> {after:?}"
            );
        }
        if before.is_terminal() {
            assert!(!before.can_transition_to(Active));
        }
    }
    for terminal in [
        RunStatus::Succeeded,
        RunStatus::Failed,
        RunStatus::Cancelled,
    ] {
        assert!(RunStatus::Running.can_transition_to(terminal));
        assert!(!terminal.can_transition_to(RunStatus::Running));
        assert!(!terminal.can_transition_to(terminal));
    }
}

#[test]
fn failure_mapping_preserves_operational_meaning_and_persisted_messages() {
    assert_eq!(
        FailureCode::for_evaluation(&EvaluationError::OperandType, FailureBoundary::Input),
        FailureCode::Input
    );
    assert_eq!(
        FailureCode::for_evaluation(&EvaluationError::OperandType, FailureBoundary::Output),
        FailureCode::OutputSchema
    );
    assert_eq!(
        FailureCode::for_evaluation(&EvaluationError::Limit("steps"), FailureBoundary::Output),
        FailureCode::ExpressionLimit
    );
    assert_eq!(
        FailureCode::for_evaluation(&EvaluationError::UnavailableSource, FailureBoundary::Input),
        FailureCode::Dependency
    );
    let l = Limits::default();
    let failure = RuntimeFailure::new(FailureCode::Postconditions);
    let value = failure.to_value(&l).unwrap();
    assert_eq!(RuntimeFailure::from_value(&value, &l).unwrap(), failure);
    assert!("E_UNKNOWN".parse::<FailureCode>().is_err());
    let old = Value::Map(
        Map::try_from_entries([
            ("code".into(), Value::Text("E_TIMEOUT".into())),
            (
                "message".into(),
                Value::Text("Previously committed redacted wording.".into()),
            ),
        ])
        .unwrap(),
    );
    assert_eq!(
        RuntimeFailure::from_value(&old, &l).unwrap().message(),
        "Previously committed redacted wording."
    );
}

#[test]
fn request_digest_scopes_all_semantic_arguments_but_not_receipt_keys() {
    let l = Limits::default();
    let original = host_request_digest("alice", "start_run", "graph", &Value::Null, &l).unwrap();
    for (principal, method, target, args) in [
        ("bob", "start_run", "graph", Value::Null),
        ("alice", "extend_run", "graph", Value::Null),
        ("alice", "start_run", "different", Value::Null),
        ("alice", "start_run", "graph", Value::Bool(false)),
    ] {
        assert_ne!(
            original,
            host_request_digest(principal, method, target, &args, &l).unwrap()
        );
    }
}

#[test]
fn multi_set_label_union_charges_empty_set_traversal_and_keeps_canonical_spelling() {
    let l = Limits::default();
    let a = SecurityLabels::new(vec!["z".into(), "a".into()], &l).unwrap();
    let b = SecurityLabels::new(vec!["a".into(), "é".into()], &l).unwrap();
    assert_eq!(
        SecurityLabels::combine([&a, &b, &a], &l)
            .unwrap()
            .as_slice(),
        &["a", "z", "é"]
    );
    let empty = SecurityLabels::default();
    let small = Limits {
        max_document_bytes: 8,
        ..l
    };
    let mut visits = 0;
    let sets = std::iter::repeat_n(&empty, 1000).inspect(|_| visits += 1);
    assert!(SecurityLabels::combine(sets, &small).is_err());
    assert!(visits <= 9);
}
