//! End-to-end regressions for the Draft 0.1 alignment review.
#[allow(dead_code)]
mod support;
use htlk::{
    compiler::Compiler,
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        clock::CoordinatorClock,
        execution::{ExecutionConfig, ExecutionEngine, RunView},
        failure::{FailureCode, RuntimeFailure},
        identity::{InvocationId, RunId, WaitId},
        mcp::{McpAdapter, McpFuture, McpPolicy, McpRequest},
        records::{InputValue, RunStatus, SecurityLabels},
        store::{RecordFamily, RecordKey, SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding, PolicyDocument,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Policy {
    clock: Arc<Clock>,
    mode: AtomicUsize,
    deny_mode: AtomicUsize,
    reply_at: AtomicI64,
    approval_at: AtomicI64,
    release: Arc<std::sync::atomic::AtomicBool>,
    release_on_extend: std::sync::atomic::AtomicBool,
    read_at: AtomicI64,
}
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, action: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if matches!(action, Authorization::ReadArtifact { .. }) {
            let at = self.read_at.load(Ordering::SeqCst);
            if at != 0 {
                self.clock.0.store(at, Ordering::SeqCst);
            }
        }
        if matches!(action, Authorization::ExtendRun { .. })
            && self.release_on_extend.load(Ordering::SeqCst)
        {
            self.release.store(true, Ordering::SeqCst);
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
    fn reply_labels(
        &self,
        _: &HostContext,
        _: &RunId,
        _: WaitId,
        _: &Value,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        let at = self.reply_at.load(Ordering::SeqCst);
        if at != 0 {
            self.clock.0.store(at, Ordering::SeqCst);
        }
        Ok(SecurityLabels::default())
    }
    fn approval_labels(
        &self,
        _: &HostContext,
        _: &RunId,
        _: WaitId,
        _: &Value,
        allow: bool,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        if !allow && self.deny_mode.load(Ordering::SeqCst) != usize::MAX {
            self.mode
                .store(self.deny_mode.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        let at = self.approval_at.load(Ordering::SeqCst);
        if at != 0 {
            self.clock.0.store(at, Ordering::SeqCst);
        }
        Ok(SecurityLabels::default())
    }
    fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
        let mode = self.mode.load(Ordering::SeqCst);
        Ok(McpPolicy {
            replay_safe: true,
            approval_authority: (mode != 0).then(|| "review".into()),
            policy_version: mode.to_string(),
            ..Default::default()
        })
    }
}
struct Adapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    raw: Vec<u8>,
    calls: AtomicUsize,
    hold: std::sync::atomic::AtomicBool,
    release: Arc<std::sync::atomic::AtomicBool>,
}
impl McpAdapter for Adapter {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&binding.descriptor()].clone()) })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            while self.hold.load(Ordering::SeqCst) && !self.release.load(Ordering::SeqCst) {
                actix::clock::sleep(Duration::from_millis(1)).await;
            }
            Ok(self.raw.clone())
        })
    }
}
struct Harness {
    compiler: Compiler,
    runtime: Arc<Runtime>,
    policy: Arc<Policy>,
    adapter: Arc<Adapter>,
    store: SqliteStore,
    caller: HostContext,
    fingerprint: Digest,
}
impl Harness {
    async fn new(source: &str, steps: u64, raw: Vec<u8>) -> Self {
        let (base, mut registry) = support::native_compiler_with_timeouts(10000, 1000);
        let mut config = base.config().clone();
        let mut fields = config.policy.fields().clone();
        fields.evaluator_limits.max_steps = steps;
        config.policy = PolicyDocument::new(fields, &Limits::default()).unwrap();
        config.profile = registry.link_policy(&config.policy).unwrap();
        let compiler = Compiler::new(config).unwrap();
        let graph = compiler
            .compile_source(source, &support::catalogs())
            .unwrap();
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let adapter = Arc::new(Adapter {
            descriptors: graph
                .canonical_document
                .fields()
                .bindings
                .values()
                .map(|b| {
                    (
                        b.descriptor(),
                        graph.canonical_document.fields().documents[&b.descriptor()].clone(),
                    )
                })
                .collect(),
            raw,
            calls: AtomicUsize::new(0),
            hold: std::sync::atomic::AtomicBool::new(false),
            release: release.clone(),
        });
        let clock = Arc::new(Clock(AtomicI64::new(1000)));
        let policy = Arc::new(Policy {
            clock: clock.clone(),
            mode: AtomicUsize::new(0),
            deny_mode: AtomicUsize::new(usize::MAX),
            reply_at: AtomicI64::new(0),
            approval_at: AtomicI64::new(0),
            release,
            release_on_extend: std::sync::atomic::AtomicBool::new(false),
            read_at: AtomicI64::new(0),
        });
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let runtime = Arc::new(
            Runtime::new(
                store.clone(),
                Arc::new(registry),
                policy.clone(),
                RuntimeConfig::default(),
            )
            .unwrap()
            .with_clock(clock)
            .with_mcp(adapter.clone()),
        );
        let caller = HostContext::new("host".into(), &Limits::default()).unwrap();
        runtime
            .register_graph(&caller, &graph.executable_bytes, &[])
            .await
            .unwrap();
        Self {
            compiler,
            runtime,
            policy,
            adapter,
            store,
            caller,
            fingerprint: graph.fingerprint,
        }
    }
    fn launch(&self) -> ExecutionEngine {
        ExecutionEngine::launch(
            self.runtime.clone(),
            ExecutionConfig {
                poll_interval: Duration::from_millis(1),
                ..Default::default()
            },
        )
        .unwrap()
    }
    async fn start(&self, inputs: BTreeMap<Identifier, InputValue>) -> RunId {
        self.runtime
            .start_run(&self.caller, self.fingerprint, &inputs, "start", None)
            .await
            .unwrap()
            .run_id
    }
    async fn observe(&self, run: &RunId, predicate: impl Fn(&RunView) -> bool) -> RunView {
        for _ in 0..2000 {
            if let Ok(view) = self.runtime.inspect_run(&self.caller, run).await
                && predicate(&view)
            {
                return view;
            }
            actix::clock::sleep(Duration::from_millis(2)).await;
        }
        panic!("run did not reach expected observation")
    }
}
const TOOL: &str = r#"ir_version="0.1" graph test.tool { inputs={} outputs={value=json} nodes{args=eval(json,{query="q"}) work=call(mcp.tool("research","lookup")){retry{max_attempts=2 on=["E_OUTPUT_SCHEMA"] backoff_ms=[0]}}} edges{edge args{from=args.outputs.value to=work.inputs.arguments}edge value{from=work.outputs.value to=outputs.value}}}"#;

#[test]
fn denial_is_terminal_in_the_disposition_transaction_despite_policy_replacement() {
    actix::System::new().block_on(async {
        for replacement in [0, 2] {
            let h = Harness::new(TOOL, 100000, vec![]).await;
            h.policy.mode.store(1, Ordering::SeqCst);
            h.policy.deny_mode.store(replacement, Ordering::SeqCst);
            let engine = h.launch();
            let run = h.start(BTreeMap::new()).await;
            let view = h.observe(&run, |v| !v.waits.is_empty()).await;
            let wait = &view.waits[0];
            h.runtime
                .submit_approval(
                    &h.caller,
                    wait.wait_id,
                    wait.request_digest.unwrap(),
                    false,
                    "deny",
                )
                .await
                .unwrap();
            let record = h
                .store
                .record(
                    &run,
                    &RecordKey {
                        family: RecordFamily::Invocation,
                        id: wait.invocation_id.to_string(),
                    },
                )
                .await
                .unwrap();
            assert!(
                record.final_record,
                "denial must finalize without another reducer pass"
            );
            let node = h
                .runtime
                .inspect_node(&h.caller, &run, wait.invocation_id)
                .await
                .unwrap();
            assert_eq!(node.failure.unwrap().code(), FailureCode::ApprovalDenied);
            assert_eq!(
                h.observe(&run, |v| v.header.status.is_terminal())
                    .await
                    .header
                    .status,
                RunStatus::Failed
            );
            assert_eq!(h.adapter.calls.load(Ordering::SeqCst), 0);
            engine.shutdown().await.unwrap();
        }
    });
}

#[test]
fn classification_crossing_expiry_cannot_accept_a_reply_or_approval() {
    actix::System::new().block_on(async {
        for approval in [false, true] {
            let source = if approval { TOOL } else { r#"ir_version="0.1" graph test.wait{inputs={request=json}outputs={value=string}nodes{work=wait(string){topic="reply" timeout_ms=50}}edges{edge request{from=inputs.request to=work.inputs.request}edge value{from=work.outputs.value to=outputs.value}}}"# };
            let h = Harness::new(source, 100000, vec![]).await;
            h.policy.mode.store(usize::from(approval), Ordering::SeqCst);
            let engine = h.launch();
            let inputs = if approval { BTreeMap::new() } else { BTreeMap::from([("request".parse().unwrap(), InputValue::Inline(Value::Null))]) };
            let run = h.start(inputs).await;
            let view = h.observe(&run, |v| !v.waits.is_empty()).await; let wait = &view.waits[0];
            let error = if approval {
                h.policy.approval_at.store(wait.expires_at, Ordering::SeqCst);
                h.runtime.submit_approval(&h.caller, wait.wait_id, wait.request_digest.unwrap(), true, "late").await.unwrap_err()
            } else {
                h.policy.reply_at.store(wait.expires_at, Ordering::SeqCst);
                h.runtime.submit_reply(&h.caller, wait.wait_id, Value::Text("answer".into()), "late").await.unwrap_err()
            };
            assert_eq!(error.code(), if approval { FailureCode::ApprovalDenied } else { FailureCode::WaitExpired });
            let record = h.store.record(&run, &RecordKey{family:RecordFamily::Wait,id:wait.wait_id.to_string()}).await.unwrap();
            assert!(!matches!(record.payload, Value::Map(m) if m.get("state")==Some(&Value::Text("accepted".into()))));
            assert_eq!(h.adapter.calls.load(Ordering::SeqCst), 0);
            engine.shutdown().await.unwrap();
        }
    });
}

#[test]
fn mcp_output_fuel_exhaustion_is_not_a_schema_retry() {
    actix::System::new().block_on(async {
        let raw = format!(
            r#"{{"content":[],"structuredContent":{{"evidence_text":"{}"}}}}"#,
            "x".repeat(16000)
        )
        .into_bytes();
        let h = Harness::new(TOOL, 2000, raw).await;
        let engine = h.launch();
        let run = h.start(BTreeMap::new()).await;
        let view = h.observe(&run, |v| v.header.status.is_terminal()).await;
        let id = InvocationId::new(
            view.header.root_scope,
            &"work".parse().unwrap(),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            h.runtime
                .inspect_node(&h.caller, &run, id)
                .await
                .unwrap()
                .failure
                .unwrap()
                .code(),
            FailureCode::ExpressionLimit
        );
        assert_eq!(h.adapter.calls.load(Ordering::SeqCst), 1);
        engine.shutdown().await.unwrap();
    });
}

#[test]
fn inspection_retains_contract_sites_and_every_boundary_validation_failure() {
    actix::System::new().block_on(async {
        let h=Harness::new(r#"ir_version="0.1" graph test.contract{inputs={}outputs={value=json}nodes{work=eval(json,1){postconditions=length(outputs.value)>0}}edges{edge value{from=work.outputs.value to=outputs.value}}}"#,100000,vec![]).await;
        let engine=h.launch();let run=h.start(BTreeMap::new()).await;let view=h.observe(&run,|v|v.header.status.is_terminal()).await;
        let id=InvocationId::new(view.header.root_scope,&"work".parse().unwrap(),&Limits::default()).unwrap();
        let node=h.runtime.inspect_node(&h.caller,&run,id).await.unwrap();
        assert_eq!(node.failure.unwrap().code(),FailureCode::Expression);
        assert!(node.diagnostics.iter().any(|d|matches!(&d.decision,Value::Map(m)if matches!(m.get("canonical_path"),Some(Value::Text(p))if p.ends_with("/postconditions")))));
        engine.shutdown().await.unwrap();
        let h=Harness::new(r#"ir_version="0.1" graph test.boundary{inputs={a=json,b=json}outputs={a=string,b=string}nodes{}edges{edge a{from=inputs.a to=outputs.a}edge b{from=inputs.b to=outputs.b}}}"#,100000,vec![]).await;
        let engine=h.launch();let run=h.start(BTreeMap::from([("a".parse().unwrap(),InputValue::Inline(Value::Integer(1))),("b".parse().unwrap(),InputValue::Inline(Value::Integer(2)))])).await;
        let view=h.observe(&run,|v|v.header.status.is_terminal()).await;
        assert_eq!(view.diagnostics.len(),2);
        for port in ["a","b"]{assert!(view.diagnostics.iter().any(|d|matches!(&d.decision,Value::Map(m)if matches!(m.get("canonical_path"),Some(Value::Text(p))if p.ends_with(&format!("/outputs/{port}"))))));}
        engine.shutdown().await.unwrap();
    });
}

#[test]
fn blocked_requires_a_quiescent_scheduler_and_first_loop_iteration_is_visible() {
    actix::System::new().block_on(async {
        let h=Harness::new(r#"ir_version="0.1" graph test.loop{inputs={seed=json}outputs={value=boolean}nodes{repeat=loop{inputs={seed=json}outputs={value=boolean}carried={data=inputs.seed}body{nodes{question=wait(boolean){topic="loop" timeout_ms=500}}edges{edge request{from=carried.data to=question.inputs.request}edge next{from=carried.data to=next.data}edge value{from=question.outputs.value to=outputs.value}}}until=true max_iterations=1}}edges{edge seed{from=inputs.seed to=repeat.inputs.seed}edge value{from=repeat.outputs.value to=outputs.value}}}"#,100000,vec![]).await;
        let run=h.start(BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Null))])).await;
        assert!(!h.runtime.inspect_run(&h.caller,&run).await.unwrap().blocked,"uninstantiated internal work is eligible");
        let engine=h.launch();let view=h.observe(&run,|v|v.blocked).await;
        let id=InvocationId::new(view.header.root_scope,&"repeat".parse().unwrap(),&Limits::default()).unwrap();
        assert_eq!(h.runtime.inspect_node(&h.caller,&run,id).await.unwrap().iteration,Some(0));
        h.policy.clock.0.store(view.waits[0].expires_at,Ordering::SeqCst);
        assert!(!h.runtime.inspect_run(&h.caller,&run).await.unwrap().blocked,"due timers make internal work eligible");
        engine.shutdown().await.unwrap();
    });
}

#[test]
fn mounted_waits_are_blocked_and_expired_atomic_closures_do_not_install() {
    use htlk::compiler::{JoinEdge, JoinEndpoint, JoinInput, JoinOperand, JoinOutput, JoinSpec};
    use htlk::runtime::execution::{CloseWait, ExtensionRequest};
    actix::System::new().block_on(async {
        let source=r#"ir_version="0.1" graph test.planning{inputs={context=string}outputs={context=string}nodes{request=eval(json,{})window=wait(boolean){topic="plan" timeout_ms=500}}edges{edge request{from=request.outputs.value to=window.inputs.request}edge output{from=inputs.context to=outputs.context}}postconditions=status(@window)=="succeeded"}"#;
        for expire in [false,true] {
            let h=Harness::new(source,100000,vec![]).await;
            let old=h.compiler.compile_source(source,&support::catalogs()).unwrap();
            let added=h.compiler.compile_source(r#"ir_version="0.1" graph test.added{inputs={context=string}outputs={context=string}nodes{}edges{edge output{from=inputs.context to=outputs.context}}}"#,&support::catalogs()).unwrap();
            let endpoint=|graph,port:&str|JoinEndpoint{graph,port:port.into()};
            let join=JoinSpec{id:"test.join".into(),left_alias:"old".into(),right_alias:"added".into(),
                edges:vec![JoinEdge{id:"context".into(),from:endpoint(JoinOperand::Left,"context"),to:endpoint(JoinOperand::Right,"context")}],
                inputs:vec![JoinInput{name:"context".into(),to:vec![endpoint(JoinOperand::Left,"context")]}],
                outputs:vec![JoinOutput{name:"context".into(),from:endpoint(JoinOperand::Right,"context")}]};
            let composed=h.compiler.join_graphs(&old.canonical_document,&added.canonical_document,&join,&support::catalogs()).unwrap();
            h.runtime.register_graph(&h.caller,&composed.executable_bytes,&[]).await.unwrap();
            let engine=h.launch();let run=h.start(BTreeMap::from([("context".parse().unwrap(),InputValue::Inline(Value::Text("preserved".into())))])).await;
            let view=h.observe(&run,|v|v.blocked).await;let wait=&view.waits[0];
            let request=ExtensionRequest{expected_fingerprint:h.fingerprint,composed_fingerprint:composed.fingerprint,inputs:BTreeMap::new(),close_wait:expire.then(||CloseWait{wait_id:wait.wait_id,value:Value::Bool(true),idempotency_key:"close".into()})};
            if expire {
                h.policy.reply_at.store(wait.expires_at,Ordering::SeqCst);
                assert_eq!(h.runtime.extend_run(&h.caller,&run,&request,"extend").await.unwrap_err().code(),FailureCode::WaitExpired);
                let header=h.store.run_header(&run).await.unwrap();assert_eq!(header.fingerprint,h.fingerprint);assert_eq!(header.root_scope,view.header.root_scope);
            } else {
                h.runtime.extend_run(&h.caller,&run,&request,"extend").await.unwrap();
                let mounted=h.observe(&run,|v|v.blocked&&v.header.fingerprint==composed.fingerprint).await;
                assert_eq!(mounted.waits[0].wait_id,wait.wait_id);
            }
            engine.shutdown().await.unwrap();
        }
    });
}

#[test]
fn response_arriving_during_extension_is_durable_and_never_redispatched() {
    use htlk::compiler::{JoinEdge, JoinEndpoint, JoinOperand, JoinOutput, JoinSpec};
    use htlk::runtime::execution::{CloseWait, ExtensionRequest};
    actix::System::new().block_on(async {
        let source=r#"ir_version="0.1" graph test.race{inputs={}outputs={value=json}nodes{args=eval(json,{query="q"})work=call(mcp.tool("research","lookup"))window=wait(boolean){topic="plan" timeout_ms=5000}}edges{edge args{from=args.outputs.value to=work.inputs.arguments}edge request{from=args.outputs.value to=window.inputs.request}edge value{from=work.outputs.value to=outputs.value}}postconditions=status(@window)=="succeeded"}"#;
        let h=Harness::new(source,100000,br#"{"content":[],"structuredContent":{"evidence_text":"durable"}}"#.to_vec()).await;
        h.adapter.hold.store(true,Ordering::SeqCst);
        let old=h.compiler.compile_source(source,&support::catalogs()).unwrap();
        let added=h.compiler.compile_source(r#"ir_version="0.1" graph test.added{inputs={value=json}outputs={value=json}nodes{}edges{edge output{from=inputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let endpoint=|graph|JoinEndpoint{graph,port:"value".into()};
        let composed=h.compiler.join_graphs(&old.canonical_document,&added.canonical_document,&JoinSpec{id:"test.race_join".into(),left_alias:"old".into(),right_alias:"added".into(),edges:vec![JoinEdge{id:"value".into(),from:endpoint(JoinOperand::Left),to:endpoint(JoinOperand::Right)}],inputs:vec![],outputs:vec![JoinOutput{name:"value".into(),from:endpoint(JoinOperand::Right)}]},&support::catalogs()).unwrap();
        h.runtime.register_graph(&h.caller,&composed.executable_bytes,&[]).await.unwrap();
        let engine=h.launch();let run=h.runtime.start_run(&h.caller,h.fingerprint,&BTreeMap::new(),"start",Some(&htlk_executable::ExecutionLimits::new().with_max_mcp_calls(1).unwrap())).await.unwrap().run_id;
        // The coordinator serializes graph decisions while the worker holds the
        // MCP future, so create the planning wait before entering that future.
        let mut waits=Vec::new();
        for _ in 0..2000 { waits=h.store.records(&run,RecordFamily::Wait,None,100).await.unwrap();if !waits.is_empty()&&h.adapter.calls.load(Ordering::SeqCst)==1{break;}actix::clock::sleep(Duration::from_millis(2)).await; }
        let wait=waits.first().expect("planning wait must precede dispatch");
        h.policy.release_on_extend.store(true,Ordering::SeqCst);
        h.runtime.extend_run(&h.caller,&run,&ExtensionRequest{expected_fingerprint:h.fingerprint,composed_fingerprint:composed.fingerprint,inputs:BTreeMap::new(),close_wait:Some(CloseWait{wait_id:wait.key.id.parse().unwrap(),value:Value::Bool(true),idempotency_key:"close".into()})},"extend").await.unwrap();
        assert_eq!(h.observe(&run,|v|v.header.status.is_terminal()).await.header.status,RunStatus::Succeeded);
        assert_eq!(h.adapter.calls.load(Ordering::SeqCst),1);
        assert_eq!(h.store.records(&run,RecordFamily::Response,None,100).await.unwrap().len(),1);
        for usage in h.store.records(&run,RecordFamily::Usage,None,100).await.unwrap(){assert!(matches!(usage.payload,Value::Map(m)if m.get("concurrency_reserved")==Some(&Value::Integer(0))));}
        engine.shutdown().await.unwrap();
    });
}

#[test]
fn scope_boundary_validation_cannot_publish_after_crossing_its_deadline() {
    actix::System::new().block_on(async {
        let h=Harness::new(r#"ir_version="0.1" graph test.deadline{inputs={value=string}outputs={value=string}nodes{}edges{edge output{from=inputs.value to=outputs.value}}}"#,100000,vec![]).await;
        let run=h.start(BTreeMap::from([("value".parse().unwrap(),InputValue::Inline(Value::Text("value".into())))])).await;
        h.policy.read_at.store(11000,Ordering::SeqCst);
        let engine=h.launch();let view=h.observe(&run,|v|v.header.status.is_terminal()).await;
        assert_eq!(view.header.status,RunStatus::Failed);assert_eq!(view.failure.unwrap().code(),FailureCode::Timeout);assert!(view.outputs.is_none());
        engine.shutdown().await.unwrap();
    });
}
