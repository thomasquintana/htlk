//! Compiled declarations, prepared values, transactional visibility and protected reads.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy, VerifiedExecutable,
    artifact::{ArtifactInput, ArtifactMetadata, ArtifactProducer, PreparedArtifact},
    failure::{FailureCode, RuntimeFailure},
    identity::{RunId, ScopeInstanceId},
    records::SecurityLabels,
    store::{
        RecordFamily, RecordKey, RecordWrite, RunLease, RunSeed, SqliteStore, StoreConfig,
        Transition, TransitionContext,
    },
};
use htlk_executable::{
    Identifier, ValueType,
    cbor::{self, Limits, Map, Value},
    digest::{Digest, hash_bytes},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use support::{catalogs, native_compiler};

struct Policy {
    read: AtomicBool,
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if context.principal() != "reader" {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        if matches!(action, Authorization::ReadArtifact { .. }) && !self.read.load(Ordering::SeqCst)
        {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
}
struct Fixture {
    runtime: Runtime,
    store: SqliteStore,
    policy: Arc<Policy>,
    graph: Arc<VerifiedExecutable<'static>>,
    run: RunId,
    lease: RunLease,
}
impl Fixture {
    async fn new() -> Self {
        let (compiler, registry) = native_compiler();
        let output=compiler.compile_source(r#"ir_version="0.1" graph test.artifacts{
          inputs={native=optional(string),json=optional(json),blob=optional(bytes)} outputs={native=optional(string),json=optional(json),blob=optional(bytes)} nodes{}
          edges{edge native{from=inputs.native to=outputs.native} edge json{from=inputs.json to=outputs.json} edge blob{from=inputs.blob to=outputs.blob}}
        }"#,&catalogs()).unwrap();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let policy = Arc::new(Policy {
            read: AtomicBool::new(true),
        });
        let runtime = Runtime::new(
            store.clone(),
            Arc::new(registry),
            policy.clone(),
            RuntimeConfig::default(),
        )
        .unwrap();
        runtime
            .register_graph(&context("reader"), &output.executable_bytes, &[])
            .await
            .unwrap();
        let graph = runtime
            .executable(&context("reader"), output.fingerprint)
            .await
            .unwrap();
        let run = RunId::new("artifact-run".into(), &Limits::default()).unwrap();
        let root = ScopeInstanceId::root(&run, &Limits::default()).unwrap();
        store
            .create_run(RunSeed {
                run_id: run.clone(),
                fingerprint: output.fingerprint,
                root_scope: root,
                actor: "reader".into(),
                causation: "test-kernel-creation".into(),
                timestamp_ms: 0,
            })
            .await
            .unwrap();
        let lease = store
            .acquire(&run, "coordinator".into(), 0, 100)
            .await
            .unwrap();
        Self {
            runtime,
            store,
            policy,
            graph,
            run,
            lease,
        }
    }
    fn prepare(&self, port: &str, value: &Value) -> PreparedArtifact {
        let fields = self.graph.document().fields();
        let ty = fields.scopes[&fields.root_scope]
            .fields()
            .inputs
            .get(port)
            .unwrap()
            .value_type();
        self.prepare_as(port, ty, value)
    }
    fn prepare_as(&self, port: &str, ty: &ValueType, value: &Value) -> PreparedArtifact {
        PreparedArtifact::new(
            &self.graph,
            ArtifactInput {
                producer: ArtifactProducer::RunInput {
                    run_id: self.run.clone(),
                    extension_number: 0,
                    port: port.parse().unwrap(),
                },
                value_type: ty,
                value,
                input_artifact_ids: vec![],
                security_labels: SecurityLabels::new(vec!["protected".into()], &Limits::default())
                    .unwrap(),
                accepted_at: 1,
            },
            &Limits::default(),
        )
        .unwrap()
    }
    async fn accept(&self, artifact: &PreparedArtifact, command: &str) {
        let metadata = artifact.metadata();
        self.store
            .commit(
                &self.lease,
                Transition {
                    context: TransitionContext {
                        command_id: command.into(),
                        actor: "reader".into(),
                        causation: "test-acceptance".into(),
                        timestamp_ms: 1,
                    },
                    writes: vec![RecordWrite {
                        key: RecordKey {
                            family: RecordFamily::Artifact,
                            id: metadata.artifact_id().to_string(),
                        },
                        expected_revision: None,
                        final_record: true,
                        payload: metadata.to_value(&Limits::default()).unwrap(),
                    }],
                    next_status: None,
                    root: None,
                    expected_fingerprint: None,
                },
            )
            .await
            .unwrap();
    }
}
fn context(name: &str) -> HostContext {
    HostContext::new(name.into(), &Limits::default()).unwrap()
}

#[test]
fn staged_bytes_are_invisible_until_final_metadata_and_reads_use_current_policy() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let value = Value::Text("protected original bytes".into());
        let proposal = f.prepare("native", &value);
        let id = proposal.metadata().artifact_id();
        f.store.stage_artifact(&proposal).await.unwrap();
        assert!(
            f.runtime
                .read_artifact(&context("reader"), id)
                .await
                .is_err()
        );
        f.accept(&proposal, "accept").await;
        let accepted = f
            .runtime
            .read_artifact(&context("reader"), id)
            .await
            .unwrap();
        assert_eq!(accepted.value, value);
        assert_eq!(accepted.metadata, *proposal.metadata());
        assert_eq!(
            accepted.metadata.security_labels().as_slice(),
            &["protected"]
        );
        assert!(!format!("{accepted:?}").contains("protected original bytes"));
        assert_eq!(
            f.runtime
                .read_artifact(&context("other"), id)
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
        f.policy.read.store(false, Ordering::SeqCst);
        assert_eq!(
            f.runtime
                .read_artifact(&context("reader"), id)
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
    });
}

#[test]
fn identical_native_bytes_share_storage_but_keep_source_types_and_provenance() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let value = Value::Text("same".into());
        let native = f.prepare("native", &value);
        let json = f.prepare("json", &value);
        assert_ne!(
            native.metadata().content_digest(),
            json.metadata().content_digest()
        );
        assert_ne!(
            native.metadata().artifact_id(),
            json.metadata().artifact_id()
        );
        assert_eq!(
            native.metadata().storage_reference(),
            json.metadata().storage_reference()
        );
        assert_eq!(
            f.store.stage_artifact(&native).await.unwrap(),
            f.store.stage_artifact(&json).await.unwrap()
        );
        f.accept(&native, "native").await;
        f.accept(&json, "json").await;
        let a = f
            .runtime
            .read_artifact(&context("reader"), native.metadata().artifact_id())
            .await
            .unwrap();
        let b = f
            .runtime
            .read_artifact(&context("reader"), json.metadata().artifact_id())
            .await
            .unwrap();
        assert_eq!(a.value, b.value);
        assert_ne!(a.metadata.value_type(), b.metadata.value_type());
    });
}

#[test]
fn metadata_integrity_checks_source_content_producer_and_canonical_bytes() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let proposal = f.prepare("native", &Value::Text("original".into()));
        let l = Limits::default();
        let Value::Map(metadata) = proposal.metadata().to_value(&l).unwrap() else {
            panic!()
        };
        let other_bytes = cbor::encode(&Value::Text("changed".into()), &l).unwrap();
        let changed = Value::Map(
            Map::try_from_entries(metadata.iter().map(|(key, value)| {
                (
                    key.to_string(),
                    if key == "storage_reference" {
                        Value::Text(hash_bytes(&other_bytes).to_string())
                    } else {
                        value.clone()
                    },
                )
            }))
            .unwrap(),
        );
        let changed = ArtifactMetadata::from_value(&changed, &l).unwrap();
        assert!(changed.verify_value(&other_bytes, &l).is_err());
        let wrong_id = Value::Map(
            Map::try_from_entries(metadata.iter().map(|(key, value)| {
                (
                    key.to_string(),
                    if key == "artifact_id" {
                        Value::Text(format!("sha256:{}", "0".repeat(64)))
                    } else {
                        value.clone()
                    },
                )
            }))
            .unwrap(),
        );
        assert!(ArtifactMetadata::from_value(&wrong_id, &l).is_err());
        let mut entries = metadata
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect::<Vec<_>>();
        entries.push(("unknown".into(), Value::Null));
        assert!(
            ArtifactMetadata::from_value(&Value::Map(Map::try_from_entries(entries).unwrap()), &l)
                .is_err()
        );
        let bad = PreparedArtifact::new(
            &f.graph,
            ArtifactInput {
                producer: ArtifactProducer::RunInput {
                    run_id: f.run.clone(),
                    extension_number: 0,
                    port: "native".parse().unwrap(),
                },
                value_type: &ValueType::builtin(htlk_executable::BuiltinType::String),
                value: &Value::Integer(7),
                input_artifact_ids: vec![],
                security_labels: SecurityLabels::default(),
                accepted_at: 1,
            },
            &l,
        );
        assert!(bad.is_err());
    });
}

#[test]
fn null_and_bytes_remain_present_native_values_and_noncanonical_float_bytes_fail() {
    actix::System::new().block_on(async{
        for (port,value) in [("json",Value::Null),("blob",Value::Bytes(vec![0,255]))] {
            let f=Fixture::new().await;
            let proposal=f.prepare(port,&value);
            f.store.stage_artifact(&proposal).await.unwrap();f.accept(&proposal,"accept").await;
            assert_eq!(f.runtime.read_artifact(&context("reader"),proposal.metadata().artifact_id()).await.unwrap().value,value);
        }
        let f=Fixture::new().await;
        let proposal=f.prepare("json",&Value::Float(cbor::FiniteFloat::new(1.0).unwrap()));
        let value=proposal.metadata().to_value(&Limits::default()).unwrap();
        let bytes=cbor::encode(&value,&Limits::default()).unwrap();
        let exact=Limits{max_document_bytes:bytes.len(),..Limits::default()};
        assert_eq!(cbor::encode(&proposal.metadata().to_value(&exact).unwrap(),&exact).unwrap(),bytes);
        assert!(proposal.metadata().to_value(&Limits{max_document_bytes:bytes.len()-1,..exact}).is_err());
        let mut wide=vec![0xfb];wide.extend_from_slice(&1.0f64.to_bits().to_be_bytes());
        let Value::Map(fields)=value else{panic!()};
        let modified=Value::Map(Map::try_from_entries(fields.iter().map(|(key,value)|(key.to_string(),if key=="storage_reference"{Value::Text(hash_bytes(&wide).to_string())}else{value.clone()}))).unwrap());
        let metadata=ArtifactMetadata::from_value(&modified,&Limits::default()).unwrap();
        assert!(matches!(metadata.verify_value(&wide,&Limits::default()),Err(htlk::runtime::artifact::ArtifactError::Codec(error)) if matches!(error.kind(),cbor::ErrorKind::NonCanonicalEncoding)));
    });
}
