//! Audit windows, cross-run roots, immutable tombstones and shared physical bytes.
#[allow(dead_code)]
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    identity::{ArtifactId, RunId},
    records::{InputValue, RunStatus, SecurityLabels},
    retention::RetentionDisposition,
    store::{ReplayState, SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(&self, c: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if c.principal() == "alice" {
            Ok(())
        } else {
            Err(RuntimeFailure::new(FailureCode::Policy))
        }
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
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
async fn result(runtime: &Runtime, caller: &HostContext, run: &RunId) -> ArtifactId {
    for _ in 0..1000 {
        if let Ok(view) = runtime.inspect_run(caller, run).await
            && view.header.status == RunStatus::Succeeded
        {
            let Some(Value::Map(outputs)) = view.outputs else {
                panic!()
            };
            let Some(Value::Map(entry)) = outputs.get("value") else {
                panic!()
            };
            let Some(Value::Text(id)) = entry.get("artifact_id") else {
                panic!()
            };
            return id.parse().unwrap();
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("run did not complete")
}
#[test]
fn retention_protects_active_and_audited_cross_run_references_and_never_reuses_expired_ids() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler_with_timeouts(10000,1000);
        let graph=compiler.compile_source(r#"ir_version="0.1" graph test.retained{inputs={value=json}outputs={value=json}nodes{}edges{edge value{from=inputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let waiting=compiler.compile_source(r#"ir_version="0.1" graph test.pin{inputs={value=json}outputs={value=json}nodes{question=eval(json,{}) window=wait(boolean){topic="retain" timeout_ms=5000}}edges{edge question{from=question.outputs.value to=window.inputs.request}edge result{from=inputs.value to=outputs.value}}postconditions=status(@window)=="succeeded"}"#,&support::catalogs()).unwrap();
        let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
        let registry=Arc::new(registry);
        let runtime=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig{idempotency_retention_ms:10,audit_retention_ms:100,..Default::default()}).unwrap().with_clock(clock.clone()));
        let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();let unauthorized=HostContext::new("bob".into(),&Limits::default()).unwrap();
        for executable in [&graph,&waiting]{runtime.register_graph(&caller,&executable.executable_bytes,&[]).await.unwrap();}
        let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();
        let inputs=BTreeMap::from([("value".parse().unwrap(),InputValue::Inline(Value::Text("shared".into())))]);
        let first=runtime.start_run(&caller,graph.fingerprint,&inputs,"first",None).await.unwrap();let a=result(&runtime,&caller,&first.run_id).await;
        clock.0.store(1050,Ordering::SeqCst);
        let shorter=Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig{idempotency_retention_ms:1,audit_retention_ms:1,..Default::default()}).unwrap().with_clock(clock.clone());
        assert_eq!(shorter.expire_artifact(&caller,a).await.unwrap(),RetentionDisposition::Protected,"restart cannot shorten the saved audit promise");
        let second=runtime.start_run(&caller,graph.fingerprint,&inputs,"second",None).await.unwrap();let b=result(&runtime,&caller,&second.run_id).await;assert_ne!(a,b);
        assert_eq!(runtime.expire_artifact(&caller,a).await.unwrap(),RetentionDisposition::Protected);
        assert_eq!(runtime.expire_artifact(&unauthorized,a).await.unwrap_err().code(),FailureCode::Policy);
        let pin=runtime.start_run(&caller,waiting.fingerprint,&BTreeMap::from([("value".parse().unwrap(),InputValue::Artifact(a))]),"pin",None).await.unwrap();
        let mut wait=None;for _ in 0..1000{if let Ok(view)=runtime.inspect_run(&caller,&pin.run_id).await&&let Some(w)=view.waits.first(){wait=Some(w.wait_id);break}actix::clock::sleep(Duration::from_millis(2)).await;}
        let wait=wait.expect("pin wait did not open");clock.0.store(1200,Ordering::SeqCst);
        assert_eq!(runtime.expire_artifact(&caller,a).await.unwrap(),RetentionDisposition::Protected);
        assert_eq!(runtime.expire_artifact(&caller,b).await.unwrap(),RetentionDisposition::Expired{at_ms:1200});
        let gc=runtime.collect_garbage(&caller,None,100).await.unwrap();assert_eq!(gc.removed,0);assert_eq!(runtime.read_artifact(&caller,a).await.unwrap().value,Value::Text("shared".into()));
        runtime.submit_reply(&caller,wait,Value::Bool(true),"close").await.unwrap();assert_eq!(result(&runtime,&caller,&pin.run_id).await,a);
        assert_eq!(runtime.expire_artifact(&caller,a).await.unwrap(),RetentionDisposition::Protected,"retained output in a newly completed run");
        clock.0.store(1300,Ordering::SeqCst);
        let expired=runtime.expire_artifact(&caller,a).await.unwrap();assert_eq!(expired,RetentionDisposition::Expired{at_ms:1300});
        let sequence=store.run_header(&first.run_id).await.unwrap().sequence;
        assert_eq!(runtime.expire_artifact(&caller,a).await.unwrap(),expired);assert_eq!(store.run_header(&first.run_id).await.unwrap().sequence,sequence);
        assert_eq!(runtime.read_artifact(&unauthorized,a).await.unwrap_err().code(),FailureCode::Policy);
        assert_eq!(runtime.read_artifact(&caller,a).await.unwrap_err().code(),FailureCode::RetentionExpired);
        assert_eq!(runtime.start_run(&caller,graph.fingerprint,&BTreeMap::from([("value".parse().unwrap(),InputValue::Artifact(a))]),"expired-reference",None).await.unwrap_err().code(),FailureCode::RetentionExpired);
        assert_eq!(runtime.collect_garbage(&caller,None,100).await.unwrap().removed,1);
        let entries=store.journal(&first.run_id,0,100).await.unwrap();let mut replay=ReplayState::begin(&entries[0],&Limits::default()).unwrap();for entry in &entries[1..]{replay.apply(entry,&Limits::default()).unwrap();}assert_eq!(replay.header(),&store.run_header(&first.run_id).await.unwrap());
        let fresh=runtime.start_run(&caller,graph.fingerprint,&inputs,"fresh",None).await.unwrap();let c=result(&runtime,&caller,&fresh.run_id).await;assert_ne!(a,c);assert_ne!(b,c);
        assert_eq!(runtime.read_artifact(&caller,c).await.unwrap().value,Value::Text("shared".into()));assert_eq!(runtime.read_artifact(&caller,a).await.unwrap_err().code(),FailureCode::RetentionExpired);
        engine.shutdown().await.unwrap();
    })
}
