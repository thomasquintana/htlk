//! Compiler-to-runtime start admission, host idempotency and immutable root evidence.
mod support;
use htlk::{
    compiler::{CompilationSuccess, Compiler},
    runtime::{
        Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
        clock::CoordinatorClock,
        failure::{FailureCode, RuntimeFailure},
        identity::{ArtifactId, InvocationId, ScopeInstanceId},
        records::{InputManifest, InputValue, ManifestEntry, RunStatus, SecurityLabels},
        store::{
            JournalEvent, RecordFamily, RecordKey, ReplayState, SqliteStore, StoreConfig,
            StoreError,
        },
    },
};
use htlk_executable::{
    ExecutionLimits, Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
};
use support::{catalogs, native_compiler};

struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Policy {
    deny: AtomicBool,
    extra: AtomicBool,
    label_calls: AtomicUsize,
}
impl Policy {
    fn new() -> Self {
        Self {
            deny: AtomicBool::new(false),
            extra: AtomicBool::new(false),
            label_calls: AtomicUsize::new(0),
        }
    }
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if self.deny.load(Ordering::SeqCst) || !matches!(context.principal(), "alice" | "bob") {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        if matches!(action, Authorization::ReadArtifact { .. }) && context.principal() != "alice" {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        value: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        self.label_calls.fetch_add(1, Ordering::SeqCst);
        let label = if value.is_none() {
            "absence"
        } else if self.extra.load(Ordering::SeqCst) {
            "boundary-only"
        } else {
            "protected"
        };
        Ok(SecurityLabels::new(vec![label.into()], &Limits::default()).unwrap())
    }
}
struct Fixture {
    compiler: Compiler,
    runtime: Arc<Runtime>,
    store: SqliteStore,
    clock: Arc<Clock>,
    policy: Arc<Policy>,
}
impl Fixture {
    async fn new(retention: u64) -> Self {
        let (compiler, registry) = native_compiler();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let clock = Arc::new(Clock(AtomicI64::new(1000)));
        let policy = Arc::new(Policy::new());
        let config = RuntimeConfig {
            idempotency_retention_ms: retention,
            ..RuntimeConfig::default()
        };
        let runtime = Arc::new(
            Runtime::new(store.clone(), Arc::new(registry), policy.clone(), config)
                .unwrap()
                .with_clock(clock.clone()),
        );
        Self {
            compiler,
            runtime,
            store,
            clock,
            policy,
        }
    }
    async fn compile(&self, source: &str) -> CompilationSuccess {
        let output = self.compiler.compile_source(source, &catalogs()).unwrap();
        self.runtime
            .register_graph(&context("alice"), &output.executable_bytes, &[])
            .await
            .unwrap();
        output
    }
}
fn context(name: &str) -> HostContext {
    HostContext::new(name.into(), &Limits::default()).unwrap()
}
fn inputs(entries: &[(&str, InputValue)]) -> BTreeMap<Identifier, InputValue> {
    entries
        .iter()
        .map(|(name, value)| (name.parse().unwrap(), value.clone()))
        .collect()
}
fn inline(value: Value) -> InputValue {
    InputValue::Inline(value)
}
async fn checkpoint(
    f: &Fixture,
    output: &CompilationSuccess,
    run: &htlk::runtime::identity::RunId,
) -> InputManifest {
    let header = f.store.run_header(run).await.unwrap();
    let scope = f
        .store
        .record(
            run,
            &RecordKey {
                family: RecordFamily::Scope,
                id: header.root_scope.to_string(),
            },
        )
        .await
        .unwrap();
    let Value::Map(fields) = scope.payload else {
        panic!()
    };
    let Some(Value::Bytes(bytes)) = fields.get("input_checkpoint") else {
        panic!()
    };
    InputManifest::decode(
        bytes,
        &output.canonical_document.fields().scopes[&output.canonical_document.fields().root_scope]
            .fields()
            .inputs,
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn start_freezes_explicit_absence_present_null_and_original_inline_artifacts() {
    actix::System::new().block_on(async{
        let f=Fixture::new(10000).await;
        let output=f.compile(r#"ir_version="0.1" graph test.inputs{
          inputs={text=string,note=optional(string),nullable=optional(union(string,null))} outputs={text=string}
          preconditions=length(inputs.text)>0 nodes{} edges{edge text{from=inputs.text to=outputs.text}}
        }"#).await;
        let supplied=inputs(&[("text",inline(Value::Text("hello".into()))),("nullable",inline(Value::Null))]);
        let started=f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"request",None).await.unwrap();
        let manifest=checkpoint(&f,&output,&started.run_id).await;
        assert_eq!(manifest.entries().len(),3);
        assert_eq!(manifest.influence_labels().as_slice(),&["absence","protected"]);
        let entries=manifest.entries().map(|(name,entry)|(name.to_string(),entry.clone())).collect::<BTreeMap<_,_>>();
        assert_eq!(entries["note"],ManifestEntry::Absent);
        let ManifestEntry::Value{artifact_id,..}=entries["nullable"] else{panic!()};
        assert_eq!(f.runtime.read_artifact(&context("alice"),artifact_id).await.unwrap().value,Value::Null);
        let root=f.store.run_header(&started.run_id).await.unwrap();
        assert_eq!(root.root_scope,ScopeInstanceId::root(&started.run_id,&Limits::default()).unwrap());
        let events=f.store.journal(&started.run_id,0,100).await.unwrap();
        assert_eq!(events[1].event,JournalEvent::RunInitialized);
        let mut replay=ReplayState::begin(&events[0],&Limits::default()).unwrap();
        for event in &events[1..]{replay.apply(event,&Limits::default()).unwrap();}
        assert_eq!(replay.header(),&root);
        assert_eq!(replay.start_receipt().unwrap().idempotency_key,"request");
        assert_eq!(replay.start_receipt().unwrap().principal,"alice");
    });
}

#[test]
fn false_or_erroneous_root_preconditions_create_failed_runs_without_child_work() {
    actix::System::new().block_on(async{
        let f=Fixture::new(10000).await;
        for (condition,value,expected) in [("false",Value::Text("ok".into()),FailureCode::Preconditions),("length(inputs.data)>0",Value::Integer(17),FailureCode::Expression)]{
            let source=format!(r#"ir_version="0.1" graph test.contract{{inputs={{data=json}} outputs={{value=integer}} preconditions={condition}
              nodes{{work=eval(integer,7)}} edges{{edge value{{from=work.outputs.value to=outputs.value}}}}}}"#);
            let output=f.compile(&source).await;
            let started=f.runtime.start_run(&context("alice"),output.fingerprint,&inputs(&[("data",inline(value))]),condition,None).await.unwrap();
            let header=f.store.run_header(&started.run_id).await.unwrap();assert_eq!(header.status,RunStatus::Failed);
            let root=f.store.record(&started.run_id,&RecordKey{family:RecordFamily::Scope,id:header.root_scope.to_string()}).await.unwrap();assert!(root.final_record);
            let Value::Map(root)=root.payload else{panic!()};
            let failure=RuntimeFailure::from_value(root.get("failure").unwrap(),&Limits::default()).unwrap();assert_eq!(failure.code(),expected);
            let invocation=InvocationId::new(header.root_scope,&"work".parse().unwrap(),&Limits::default()).unwrap();
            assert_eq!(f.store.record(&started.run_id,&RecordKey{family:RecordFamily::Invocation,id:invocation.to_string()}).await,Err(StoreError::Missing));
        }
    });
}

#[test]
fn invalid_inputs_and_unsupported_budgets_leave_no_run_and_changed_keys_conflict() {
    actix::System::new().block_on(async{
        let f=Fixture::new(10000).await;
        let output=f.compile(r#"ir_version="0.1" graph test.invalid{inputs={text=string} outputs={} nodes{} edges{}}"#).await;
        for supplied in [BTreeMap::new(),inputs(&[("text",inline(Value::Integer(7)))]),inputs(&[("extra",inline(Value::Text("x".into())))])]{
            assert_eq!(f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"invalid",None).await.err().unwrap().code(),FailureCode::Input);
            assert!(f.store.pending_outbox(100).await.unwrap().is_empty());
        }
        let supplied=inputs(&[("text",inline(Value::Text("valid".into())))]);
        let hard=ExecutionLimits::new().with_max_tokens(0).unwrap();
        assert_eq!(f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"budget",Some(&hard)).await.err().unwrap().code(),FailureCode::BudgetUnsupported);
        assert!(f.store.pending_outbox(100).await.unwrap().is_empty());
        let first=f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"stable",None).await.unwrap();
        let count=f.policy.label_calls.load(Ordering::SeqCst);
        assert_eq!(f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"stable",Some(&ExecutionLimits::new())).await.unwrap(),first);
        assert_eq!(f.policy.label_calls.load(Ordering::SeqCst),count);
        assert_eq!(f.runtime.start_run(&context("alice"),output.fingerprint,&inputs(&[("text",inline(Value::Integer(7)))]),"stable",None).await.err().unwrap().code(),FailureCode::IdempotencyConflict);
        let bob=f.runtime.start_run(&context("bob"),output.fingerprint,&supplied,"stable",None).await.unwrap();assert_ne!(first.run_id,bob.run_id);
        f.policy.deny.store(true,Ordering::SeqCst);
        assert_eq!(f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"stable",None).await.err().unwrap().code(),FailureCode::Policy);
    });
}

#[test]
fn concurrent_identical_starts_commit_one_run_and_one_root_inventory() {
    actix::System::new().block_on(async {
        let f = Fixture::new(10000).await;
        let output = f
            .compile(
                r#"ir_version="0.1" graph test.concurrent{inputs={} outputs={} nodes{} edges{}}"#,
            )
            .await;
        let a = f.runtime.clone();
        let b = f.runtime.clone();
        let fingerprint = output.fingerprint;
        let a = actix::spawn(async move {
            a.start_run(
                &context("alice"),
                fingerprint,
                &BTreeMap::new(),
                "same",
                None,
            )
            .await
        });
        let b = actix::spawn(async move {
            b.start_run(
                &context("alice"),
                fingerprint,
                &BTreeMap::new(),
                "same",
                None,
            )
            .await
        });
        let a = a.await.unwrap().unwrap();
        let b = b.await.unwrap().unwrap();
        assert_eq!(a, b);
        let notices = f.store.pending_outbox(100).await.unwrap();
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().all(|notice| notice.run_id == a.run_id));
    });
}

#[test]
fn receipt_expiry_uses_persisted_clock_watermark_and_run_ids_are_never_reused() {
    actix::System::new().block_on(async {
        let f = Fixture::new(10).await;
        let output = f
            .compile(
                r#"ir_version="0.1" graph test.retention{inputs={} outputs={} nodes{} edges{}}"#,
            )
            .await;
        let first = f
            .runtime
            .start_run(
                &context("alice"),
                output.fingerprint,
                &BTreeMap::new(),
                "key",
                None,
            )
            .await
            .unwrap();
        f.clock.0.store(1011, Ordering::SeqCst);
        f.runtime
            .start_run(
                &context("alice"),
                output.fingerprint,
                &BTreeMap::new(),
                "advance",
                None,
            )
            .await
            .unwrap();
        f.clock.0.store(1005, Ordering::SeqCst);
        let next = f
            .runtime
            .start_run(
                &context("alice"),
                output.fingerprint,
                &BTreeMap::new(),
                "key",
                None,
            )
            .await
            .unwrap();
        assert_ne!(first.run_id, next.run_id);
        f.clock.0.store(1000, Ordering::SeqCst);
        assert_eq!(
            f.runtime
                .start_run(
                    &context("alice"),
                    output.fingerprint,
                    &BTreeMap::new(),
                    "key",
                    None
                )
                .await
                .unwrap(),
            next
        );
        assert_eq!(
            f.store.run_header(&next.run_id).await.unwrap().last_time_ms,
            1011
        );
    });
}

#[test]
fn historical_receipts_do_not_require_old_native_code_but_fresh_work_does() {
    actix::System::new().block_on(async {
        let f = Fixture::new(10000).await;
        let output = f
            .compile(r#"ir_version="0.1" graph test.history{inputs={} outputs={} nodes{} edges{}}"#)
            .await;
        let first = f
            .runtime
            .start_run(
                &context("alice"),
                output.fingerprint,
                &BTreeMap::new(),
                "original",
                None,
            )
            .await
            .unwrap();
        let registry = Arc::new(NativeRegistry::new(&Limits::default()).unwrap());
        let fresh = Runtime::new(
            f.store.clone(),
            registry,
            f.policy.clone(),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(f.clock.clone());
        assert_eq!(
            fresh
                .start_run(
                    &context("alice"),
                    output.fingerprint,
                    &BTreeMap::new(),
                    "original",
                    None
                )
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            fresh
                .start_run(
                    &context("alice"),
                    output.fingerprint,
                    &BTreeMap::new(),
                    "new",
                    None
                )
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Profile
        );
    });
}

#[test]
fn artifact_aliases_keep_original_ids_and_new_boundary_influence_cannot_strip_labels() {
    actix::System::new().block_on(async{
        let f=Fixture::new(10000).await;
        let output=f.compile(r#"ir_version="0.1" graph test.alias{inputs={text=string} outputs={text=string} nodes{} edges{edge text{from=inputs.text to=outputs.text}}}"#).await;
        let first=f.runtime.start_run(&context("alice"),output.fingerprint,&inputs(&[("text",inline(Value::Text("secret".into())))]),"first",None).await.unwrap();
        let manifest=checkpoint(&f,&output,&first.run_id).await;
        let ManifestEntry::Value{artifact_id,..}=manifest.entries().next().unwrap().1 else{panic!()};
        let artifact_id:ArtifactId=*artifact_id;
        f.policy.extra.store(true,Ordering::SeqCst);
        let supplied=inputs(&[("text",InputValue::Artifact(artifact_id))]);
        let second=f.runtime.start_run(&context("alice"),output.fingerprint,&supplied,"second",None).await.unwrap();
        let manifest=checkpoint(&f,&output,&second.run_id).await;
        assert!(matches!(manifest.entries().next().unwrap().1,ManifestEntry::Value{artifact_id:id,..} if *id==artifact_id));
        assert_eq!(manifest.influence_labels().as_slice(),&["boundary-only","protected"]);
        assert_eq!(f.runtime.read_artifact(&context("alice"),artifact_id).await.unwrap().metadata.security_labels().as_slice(),&["protected"]);
        assert_eq!(f.runtime.start_run(&context("bob"),output.fingerprint,&supplied,"denied",None).await.err().unwrap().code(),FailureCode::Policy);
    });
}

#[test]
fn definition_local_limits_do_not_replace_original_run_wide_ceilings() {
    actix::System::new().block_on(async{
        let f=Fixture::new(10000).await;
        let output=f.compile(r#"ir_version="0.1" graph test.limits{inputs={} outputs={} nodes{} edges{} limits{timeout_ms=400 max_mcp_calls=2}}"#).await;
        let requested=ExecutionLimits::new().with_timeout_ms(900).unwrap().with_max_mcp_calls(8).unwrap();
        let started=f.runtime.start_run(&context("alice"),output.fingerprint,&BTreeMap::new(),"limits",Some(&requested)).await.unwrap();
        let header=f.store.run_header(&started.run_id).await.unwrap();
        let scope=f.store.record(&started.run_id,&RecordKey{family:RecordFamily::Scope,id:header.root_scope.to_string()}).await.unwrap();
        let usage=f.store.record(&started.run_id,&RecordKey{family:RecordFamily::Usage,id:"run".into()}).await.unwrap();
        let Value::Map(scope)=scope.payload else{panic!()};let Value::Map(usage)=usage.payload else{panic!()};
        let root_limits=ExecutionLimits::from_value(scope.get("effective_limits").unwrap(),&Limits::default()).unwrap();
        let run_limits=ExecutionLimits::from_value(usage.get("limits").unwrap(),&Limits::default()).unwrap();
        assert_eq!(root_limits.max_mcp_calls(),Some(2));assert_eq!(run_limits.max_mcp_calls(),Some(8));
        assert_eq!(scope.get("deadline_ms"),Some(&Value::Integer(1400)));assert_eq!(usage.get("deadline_ms"),Some(&Value::Integer(1900)));
    });
}
