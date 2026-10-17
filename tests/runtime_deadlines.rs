//! Descendant stops and late local acceptance cannot stall a live fallback.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::RuntimeFailure,
    mcp::{McpAdapter, McpFuture, McpPolicy, McpRequest},
    records::{RunStatus, SecurityLabels},
    store::{RecordFamily, SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct Policy {
    calls: AtomicUsize,
    validating: AtomicBool,
    release: AtomicBool,
}
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
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
    fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 3 {
            self.validating.store(true, Ordering::SeqCst);
            let until = Instant::now() + Duration::from_secs(5);
            while !self.release.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(McpPolicy {
            policy_version: "test".into(),
            ..Default::default()
        })
    }
}
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Adapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    calls: AtomicUsize,
}
impl McpAdapter for Adapter {
    fn descriptor<'a>(&'a self, b: &'a McpBinding, _: &'a Limits) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&b.descriptor()].clone()) })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, _: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(
                br#"{"content":[],"structuredContent":{"evidence_text":"late validation"}}"#
                    .to_vec(),
            )
        })
    }
}
#[test]
fn timed_out_task_stops_descendants_and_a_late_writer_does_not_abandon_the_live_owner() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler();let graph=compiler.compile_source(r#"
            ir_version="0.1"
            task child{inputs={}outputs={value=json}nodes{args=eval(json,{query="question"}) lookup=call(mcp.tool("research","lookup"))}edges{edge args{from=args.outputs.value to=lookup.inputs.arguments}edge value{from=lookup.outputs.value to=outputs.value}}}
            graph test.deadline{inputs={}outputs={value=json}nodes{primary=use(child){limits{timeout_ms=50}} fallback=eval(json,{fallback=true}){when=status(@primary)=="failed"}}
                edges{edge primary{from=primary.outputs.value to=outputs.value when=status(@primary)=="succeeded"}edge fallback{from=fallback.outputs.value to=outputs.value when=status(@fallback)=="succeeded"}}}
        "#,&support::catalogs()).unwrap();
        let adapter=Arc::new(Adapter{descriptors:graph.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),graph.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),calls:AtomicUsize::new(0)});let policy=Arc::new(Policy{calls:AtomicUsize::new(0),validating:AtomicBool::new(false),release:AtomicBool::new(false)});let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let runtime=Arc::new(Runtime::new(store.clone(),Arc::new(registry),policy.clone(),RuntimeConfig::default()).unwrap().with_clock(clock.clone()).with_mcp(adapter.clone()));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();
        for _ in 0..1000{if policy.validating.load(Ordering::SeqCst){break}actix::clock::sleep(Duration::from_millis(2)).await;}let validating=policy.validating.load(Ordering::SeqCst);if !validating{policy.release.store(true,Ordering::SeqCst)}assert!(validating);assert_eq!(adapter.calls.load(Ordering::SeqCst),1);let fence=store.run_header(&run.run_id).await.unwrap().fence;
        clock.0.store(1050,Ordering::SeqCst);let mut stopped=false;
        for _ in 0..1000{let nodes=store.records(&run.run_id,RecordFamily::Invocation,None,100).await.unwrap();let primary=nodes.iter().any(|n|matches!(&n.payload,Value::Map(m) if m.get("local_id")==Some(&Value::Text("primary".into()))&&m.get("status")==Some(&Value::Text("failed".into()))));let child=nodes.iter().any(|n|matches!(&n.payload,Value::Map(m) if m.get("local_id")==Some(&Value::Text("lookup".into()))&&m.get("status")==Some(&Value::Text("cancelled".into()))));if primary&&child{stopped=true;break}actix::clock::sleep(Duration::from_millis(2)).await;}
        policy.release.store(true,Ordering::SeqCst);assert!(stopped,"deadline must stop descendants while the worker is calculating");
        let mut completed=None;for _ in 0..1000{if let Ok(view)=runtime.inspect_run(&caller,&run.run_id).await&&view.header.status.is_terminal(){completed=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}let view=completed.expect("late terminal record incorrectly abandoned a still-live owner");assert_eq!(view.header.status,RunStatus::Succeeded);assert_eq!(view.header.fence,fence);assert_eq!(adapter.calls.load(Ordering::SeqCst),1);
        for node in store.records(&run.run_id,RecordFamily::Invocation,None,100).await.unwrap(){if let Value::Map(m)=node.payload&&m.get("local_id")==Some(&Value::Text("lookup".into())){assert_eq!(m.get("outputs"),Some(&Value::Null));}}
        engine.shutdown().await.unwrap();
    })
}
