//! Host configuration, persisted metadata and redacted public error boundaries.
#[allow(dead_code)]
#[path = "support/e2e.rs"]
mod e2e;
#[allow(dead_code)]
mod support;
use htlk::runtime::{
    artifact::{ArtifactError, ArtifactMetadata},
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureBoundary, FailureCode, RuntimeFailure},
    records::{InputValue, RuntimeRecordError},
    store::{SqliteStore, StoreConfig, StoreError},
    *,
};
use htlk_executable::{
    cbor::{self, Limits, Map, Value},
    digest::Digest,
    *,
};
use std::{collections::BTreeMap, error::Error as _, sync::Arc, time::Duration};

#[test]
fn service_and_worker_configuration_rejects_each_invalid_boundary() {
    actix::System::new().block_on(async {
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let (_, registry) = support::native_compiler();
        let registry = Arc::new(registry);
        for case in 0..8 {
            let mut config = RuntimeConfig::default();
            match case {
                0 => config.max_cached_executables = 1025,
                1 => config.idempotency_retention_ms = 0,
                2 => config.idempotency_retention_ms = u64::MAX,
                3 => config.audit_retention_ms = 1,
                4 => config.audit_retention_ms = u64::MAX,
                5 => config.cancellation_grace_ms = 0,
                6 => config.cancellation_grace_ms = u64::MAX,
                _ => config.limits.max_depth = 129,
            };
            let error = Runtime::new(
                store.clone(),
                registry.clone(),
                Arc::new(e2e::Policy),
                config,
            )
            .err()
            .unwrap();
            assert!(matches!(
                error.code(),
                FailureCode::Profile | FailureCode::Input
            ));
            assert!(!error.to_string().is_empty());
            assert!(error.source().is_some());
        }
        let runtime = Arc::new(
            Runtime::new(
                store.clone(),
                registry,
                Arc::new(e2e::Policy),
                RuntimeConfig::default(),
            )
            .unwrap(),
        );
        for case in 0..7 {
            let mut config = ExecutionConfig::default();
            match case {
                0 => config.workers = 0,
                1 => config.workers = 65,
                2 => config.max_runs = 0,
                3 => config.max_runs = 4097,
                4 => config.lease_ms = 99,
                5 => config.poll_interval = Duration::ZERO,
                _ => config.poll_interval = Duration::from_secs(11),
            }
            assert_eq!(
                ExecutionEngine::launch(runtime.clone(), config)
                    .err()
                    .unwrap()
                    .code(),
                FailureCode::Profile
            );
        }
        assert_eq!(runtime.config().max_cached_executables, 16);
        store.close().await.unwrap();
    });
}

#[test]
fn cache_eviction_disabled_cache_and_missing_records_preserve_independent_admission() {
    actix::System::new().block_on(async {
        for capacity in [0, 1] {
            let (compiler, registry) = support::native_compiler();
            let store = SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap();
            let runtime = Runtime::new(
                store.clone(),
                Arc::new(registry),
                Arc::new(e2e::Policy),
                RuntimeConfig {
                    max_cached_executables: capacity,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut graphs = vec![];
            for id in ["one", "two", "three"] {
                let source = format!(
                    "ir_version=\"0.1\"graph test.{id}{{inputs={{}}outputs={{}}nodes{{}}edges{{}}}}"
                );
                let graph = compiler
                    .compile_source(&source, &support::catalogs())
                    .unwrap();
                runtime
                    .register_graph(&e2e::caller(), &graph.executable_bytes, &[])
                    .await
                    .unwrap();
                graphs.push(graph);
            }
            for graph in graphs {
                assert_eq!(
                    runtime
                        .executable(&e2e::caller(), graph.fingerprint)
                        .await
                        .unwrap()
                        .fingerprint(),
                    graph.fingerprint
                );
                assert_eq!(
                    runtime
                        .inspect_graph(&e2e::caller(), graph.fingerprint)
                        .await
                        .unwrap()
                        .fingerprint,
                    graph.fingerprint
                );
            }
            assert_eq!(
                runtime
                    .executable(&e2e::caller(), Digest::from_bytes([0; 32]))
                    .await
                    .err()
                    .unwrap()
                    .code(),
                FailureCode::Executable
            );
            assert_eq!(
                runtime
                    .read_artifact(
                        &e2e::caller(),
                        Digest::from_bytes([0; 32]).to_string().parse().unwrap()
                    )
                    .await
                    .err()
                    .unwrap()
                    .code(),
                FailureCode::Input
            );
            store.close().await.unwrap();
        }
    });
}

fn replace(value: &Value, key: &str, replacement: Option<Value>) -> Value {
    let Value::Map(fields) = value else { panic!() };
    let mut fields = fields
        .iter()
        .filter(|(k, _)| *k != key)
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect::<Vec<_>>();
    if let Some(value) = replacement {
        fields.push((key.into(), value));
    }
    Value::Map(Map::try_from_entries(fields).unwrap())
}

#[test]
fn accepted_artifact_records_reject_every_missing_or_mistyped_field_and_producer_member() {
    actix::System::new().block_on(async {
        let h=e2e::Harness::new(None,2).await;
        let graph=h.compiler.compile_source(r#"ir_version="0.1"graph test.metadata{inputs={seed=integer}outputs={value=integer}nodes{n=eval(integer,inputs.seed){inputs={seed=integer}}}edges{edge input{from=inputs.seed to=n.inputs.seed}edge output{from=n.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let view=h.execute(&graph,BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Integer(42)))]),"metadata").await;
        for manifest in [&view.inputs,view.outputs.as_ref().unwrap()] {
            let Value::Map(fields)=manifest else{panic!()};let (_,Value::Map(entry))=fields.iter().next().unwrap()else{panic!()};let Some(Value::Text(id))=entry.get("artifact_id")else{panic!()};
            let artifact=h.runtime.read_artifact(&e2e::caller(),id.parse().unwrap()).await.unwrap();let metadata=&artifact.metadata;let value=metadata.to_value(&Limits::default()).unwrap();
            assert!(metadata.accepted_at()<=view.header.last_time_ms);assert_eq!(metadata.validation_profile(),graph.fingerprint);
            assert_eq!(ArtifactMetadata::from_value(&value,&Limits::default()).unwrap(),*metadata);
            let Value::Map(fields)=&value else{panic!()};
            for (key,_) in fields.iter(){for replacement in [None,Some(Value::Null)]{assert!(ArtifactMetadata::from_value(&replace(&value,key,replacement),&Limits::default()).is_err(),"{key}");}}
            assert!(ArtifactMetadata::from_value(&replace(&value,"extra",Some(Value::Null)),&Limits::default()).is_err());
            let producer=fields.get("producer").unwrap();let Value::Map(members)=producer else{panic!()};
            for (key,_) in members.iter(){for replacement in [None,Some(Value::Null),Some(Value::Text("bad-spelling".into()))]{assert!(ArtifactMetadata::from_value(&replace(&value,"producer",Some(replace(producer,key,replacement))),&Limits::default()).is_err(),"producer/{key}");}}
            for bad in [replace(&value,"accepted_at",Some(Value::Integer(-1))),replace(&value,"input_artifact_ids",Some(Value::Array(vec![Value::Text("invalid".into())]))),replace(&value,"input_artifact_ids",Some(Value::Array(vec![Value::Text(id.clone()),Value::Text(id.clone())]))),replace(&value,"security_labels",Some(Value::Array(vec![Value::Text("z".into()),Value::Text("a".into())])))]{assert!(ArtifactMetadata::from_value(&bad,&Limits::default()).is_err());}
            let viewer=HostContext::new("viewer".into(),&Limits::default()).unwrap();assert_eq!(h.runtime.read_artifact(&viewer,metadata.artifact_id()).await.err().unwrap().code(),FailureCode::Policy);
        }
        h.close().await;
    });
}

#[test]
fn runtime_and_artifact_stage_errors_keep_safe_classifications_and_typed_causes() {
    let l = Limits::default();
    for store in [
        StoreError::Deadline(FailureCode::Timeout),
        StoreError::Expired,
        StoreError::Database,
        StoreError::Closed,
        StoreError::Schema,
        StoreError::Missing,
        StoreError::Conflict,
        StoreError::IdempotencyConflict,
        StoreError::Fenced,
        StoreError::Terminal,
        StoreError::Invalid,
        StoreError::Corrupt,
    ] {
        let code = store.code();
        let runtime = RuntimeError::from(store);
        assert_eq!(runtime.code(), code);
        assert!(runtime.source().is_some());
        assert!(!store.to_string().is_empty());
        assert!(!runtime.to_string().is_empty());
    }
    for boundary in [
        FailureBoundary::Expression,
        FailureBoundary::Input,
        FailureBoundary::Output,
    ] {
        assert_eq!(ArtifactError::Integrity.code(boundary), FailureCode::Digest);
        assert_eq!(ArtifactError::Shape.code(boundary), FailureCode::Digest);
        assert_eq!(
            ArtifactError::Value(EvaluationError::UnavailableSource).code(boundary),
            FailureCode::Dependency
        );
        assert_eq!(
            ArtifactError::Value(EvaluationError::Limit("steps")).code(boundary),
            FailureCode::ExpressionLimit
        );
        assert!(!ArtifactError::Limit.to_string().is_empty());
        let _ = ArtifactError::Limit.code(boundary);
    }
    let errors = [
        ArtifactError::from(cbor::decode(&[], &l).unwrap_err()),
        ArtifactError::from(EvaluationError::OperandType),
        ArtifactError::from(RuntimeRecordError::Integrity),
        ArtifactError::from(TypeError::UnknownPortField),
        ArtifactError::from(
            identity::RunId::new(
                "x".repeat(100),
                &Limits {
                    max_document_bytes: 1,
                    ..l.clone()
                },
            )
            .unwrap_err(),
        ),
    ];
    for error in errors {
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
        let runtime = RuntimeError::from(error);
        assert!(runtime.source().is_some());
        assert!(!runtime.to_string().is_empty());
    }
    for error in [
        RuntimeError::from(RuntimeRecordError::Integrity),
        RuntimeError::from(RuntimeFailure::new(FailureCode::Policy)),
        RuntimeError::from(cbor::decode(&[], &l).unwrap_err()),
    ] {
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
    }
    let secret = Attestation {
        format: "detached".into(),
        evidence: b"PRIVATE_EVIDENCE".to_vec(),
    };
    assert!(!format!("{secret:?}").contains("PRIVATE_EVIDENCE"));
    assert!(
        HostContext::new(
            "secret".into(),
            &Limits {
                max_document_bytes: 1,
                ..l
            }
        )
        .is_err()
    );
}

#[test]
fn detached_source_reports_validate_every_origin_span_optional_field_and_closed_record() {
    use htlk::runtime::source::{SourceEntry, SourceOrigin, SourceSidecar};
    let l = Limits::default();
    for origin in [
        SourceOrigin::Inline,
        SourceOrigin::Module {
            package_digest: Digest::from_bytes([1; 32]),
            module_path: "module".into(),
            source_path: "src/module.htlk".into(),
        },
        SourceOrigin::Synthetic {
            reason: "generated wrapper".into(),
        },
    ] {
        let span = if matches!(origin, SourceOrigin::Synthetic { .. }) {
            None
        } else {
            Some((0, 12))
        };
        let source = SourceSidecar {
            provenance: Digest::from_bytes([2; 32]),
            entries: vec![SourceEntry {
                canonical_path: JsonPointer::new("/scopes", &l).unwrap(),
                origin: origin.clone(),
                span,
                declaration_name: Some("declaration".into()),
                description: Some("description".into()),
            }],
        };
        let value = source.to_value(&l).unwrap();
        assert_eq!(SourceSidecar::from_value(&value, &l).unwrap(), source);
        for span in [
            Some((12, 0)),
            Some((0, u64::MAX)),
            if source.entries[0].span.is_some() {
                None
            } else {
                Some((0, 0))
            },
        ] {
            let mut bad = source.clone();
            bad.entries[0].span = span;
            assert!(bad.to_value(&l).is_err());
        }
        for key in ["provenance", "entries"] {
            assert!(SourceSidecar::from_value(&replace(&value, key, None), &l).is_err());
            assert!(
                SourceSidecar::from_value(&replace(&value, key, Some(Value::Null)), &l).is_err()
            );
        }
        let Value::Map(fields) = &value else { panic!() };
        let Some(Value::Array(entries)) = fields.get("entries") else {
            panic!()
        };
        let entry = &entries[0];
        for key in [
            "canonical_path",
            "origin",
            "span",
            "declaration_name",
            "description",
        ] {
            for replacement in [None, Some(Value::Bool(false))] {
                let entry = replace(entry, key, replacement);
                let changed = replace(&value, "entries", Some(Value::Array(vec![entry])));
                assert!(SourceSidecar::from_value(&changed, &l).is_err(), "{key}");
            }
        }
        for bad_origin in [
            Value::Array(vec![]),
            Value::Array(vec![Value::Text("unknown".into())]),
            Value::Array(vec![
                Value::Text("module".into()),
                Value::Text("bad".into()),
                Value::Text("m".into()),
                Value::Text("m.htlk".into()),
            ]),
            Value::Array(vec![Value::Text("synthetic".into()), Value::Null]),
        ] {
            let changed = replace(
                &value,
                "entries",
                Some(Value::Array(vec![replace(
                    entry,
                    "origin",
                    Some(bad_origin),
                )])),
            );
            assert!(SourceSidecar::from_value(&changed, &l).is_err());
        }
        assert!(
            source
                .to_value(&Limits {
                    max_document_bytes: 1,
                    ..l.clone()
                })
                .is_err()
        );
        assert!(SourceSidecar::from_value(&Value::Null, &l).is_err());
    }
}

#[test]
fn source_attachment_pages_are_independent_of_executable_identity_and_check_targets() {
    use htlk::runtime::source::{SourceEntry, SourceOrigin, SourceSidecar};
    actix::System::new().block_on(async {
        let h = e2e::Harness::new(None, 1).await;
        let graph = h
            .compiler
            .compile_source(
                r#"ir_version="0.1"graph test.sources{inputs={}outputs={}nodes{}edges{}}"#,
                &support::catalogs(),
            )
            .unwrap();
        h.runtime
            .register_graph(&e2e::caller(), &graph.executable_bytes, &[])
            .await
            .unwrap();
        let mut source = SourceSidecar {
            provenance: Digest::from_bytes([0; 32]),
            entries: vec![SourceEntry {
                canonical_path: JsonPointer::new("/does_not_exist", &Limits::default()).unwrap(),
                origin: SourceOrigin::Inline,
                span: Some((0, 0)),
                declaration_name: None,
                description: None,
            }],
        };
        assert_eq!(
            h.runtime
                .attach_source(&e2e::caller(), graph.fingerprint, &source)
                .await
                .unwrap_err()
                .code(),
            FailureCode::Input
        );
        source.entries[0].canonical_path = JsonPointer::new("", &Limits::default()).unwrap();
        let mut expected = BTreeMap::new();
        for index in 0..4 {
            source.provenance = Digest::from_bytes([index; 32]);
            let id = h
                .runtime
                .attach_source(&e2e::caller(), graph.fingerprint, &source)
                .await
                .unwrap();
            assert_eq!(
                h.runtime
                    .attach_source(&e2e::caller(), graph.fingerprint, &source)
                    .await
                    .unwrap(),
                id
            );
            expected.insert(id, source.clone());
        }
        let mut cursor = None;
        let mut found = BTreeMap::new();
        loop {
            let page = h
                .runtime
                .inspect_sources(&e2e::caller(), graph.fingerprint, cursor, 1)
                .await
                .unwrap();
            for attachment in page.sources {
                assert_eq!(attachment.attached_by, "alice");
                found.insert(attachment.id, attachment.sidecar);
            }
            if page.next.is_none() {
                break;
            }
            cursor = page.next;
        }
        assert_eq!(found, expected);
        assert!(
            h.runtime
                .inspect_sources(&e2e::caller(), graph.fingerprint, None, 0)
                .await
                .is_err()
        );
        assert_eq!(
            h.runtime
                .executable(&e2e::caller(), graph.fingerprint)
                .await
                .unwrap()
                .fingerprint(),
            graph.fingerprint
        );
        h.close().await;
    });
}
