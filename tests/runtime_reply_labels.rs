//! Trusted reply classification protects outcomes, boundaries and published values.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    identity::{RunId, WaitId},
    records::{InputValue, RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Policy {
    secret: AtomicBool,
    classifications: AtomicUsize,
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if let Authorization::SubmitReply {
            authority,
            policy_version,
            requesting_principal,
            topic,
            ..
        } = action
        {
            assert_eq!(
                (authority, policy_version, requesting_principal, topic),
                ("reviewers", "policy-v1", "owner", "reply")
            );
            if context.principal() != "responder" {
                return Err(RuntimeFailure::new(FailureCode::Policy));
            }
        }
        let labels = match action {
            Authorization::ReadBoundary { labels, .. }
            | Authorization::InspectRun { labels, .. }
            | Authorization::SubmitReply { labels, .. } => Some(labels),
            Authorization::ReadArtifact { metadata } => Some(metadata.security_labels()),
            _ => None,
        };
        if context.principal() == "viewer"
            && labels.is_some_and(|l| l.as_slice().contains(&"reply-secret".into()))
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
    fn wait_policy(
        &self,
        context: &HostContext,
        _: Digest,
        _: Digest,
        topic: &str,
        request: &Value,
    ) -> Result<htlk::runtime::execution::WaitPolicy, RuntimeFailure> {
        assert_eq!((context.principal(), topic), ("owner", "reply"));
        assert_eq!(request, &Value::Text("public request".into()));
        Ok(htlk::runtime::execution::WaitPolicy {
            authority: "reviewers".into(),
            version: "policy-v1".into(),
        })
    }
    fn reply_labels(
        &self,
        context: &HostContext,
        _: &RunId,
        _: WaitId,
        value: &Value,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        assert_eq!(context.principal(), "responder");
        assert_eq!(value, &Value::Text("classified answer".into()));
        self.classifications.fetch_add(1, Ordering::SeqCst);
        Ok(if self.secret.load(Ordering::SeqCst) {
            SecurityLabels::new(vec!["reply-secret".into()], &Limits::default()).unwrap()
        } else {
            SecurityLabels::default()
        })
    }
}
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
#[test]
fn accepted_reply_labels_are_atomic_immutable_and_survive_copy_and_history() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler();let graph=compiler.compile_source(r#"
            ir_version="0.1" graph test.reply_labels{
                inputs={request=json}outputs={value=string}
                nodes{review=wait(string){topic="reply" timeout_ms=500} copy=eval(string,inputs.value){inputs={value=string}}}
                edges{edge request{from=inputs.request to=review.inputs.request}edge copy{from=review.outputs.value to=copy.inputs.value}edge value{from=copy.outputs.value to=outputs.value}}
            }
        "#,&support::catalogs()).unwrap();
        let policy=Arc::new(Policy{secret:AtomicBool::new(true),classifications:AtomicUsize::new(0)});let runtime=Arc::new(Runtime::new(SqliteStore::in_memory(StoreConfig::default()).await.unwrap(),Arc::new(registry),policy.clone(),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)));
        let owner=HostContext::new("owner".into(),&Limits::default()).unwrap();let viewer=HostContext::new("viewer".into(),&Limits::default()).unwrap();let responder=HostContext::new("responder".into(),&Limits::default()).unwrap();runtime.register_graph(&owner,&graph.executable_bytes,&[]).await.unwrap();let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&owner,graph.fingerprint,&BTreeMap::from([("request".parse().unwrap(),InputValue::Inline(Value::Text("public request".into())))]),"start",None).await.unwrap();
        let mut wait=None;for _ in 0..1000{if let Ok(view)=runtime.inspect_run(&viewer,&run.run_id).await&&let Some(w)=view.waits.first(){wait=Some(w.wait_id);break}actix::clock::sleep(Duration::from_millis(2)).await;}let wait=wait.expect("wait did not open");
        assert_eq!(runtime.submit_reply(&viewer,wait,Value::Text("classified answer".into()),"unauthorized").await.unwrap_err().code(),FailureCode::Policy);assert_eq!(policy.classifications.load(Ordering::SeqCst),0);
        runtime.submit_reply(&responder,wait,Value::Text("classified answer".into()),"reply").await.unwrap();
        assert_eq!(runtime.inspect_run(&viewer,&run.run_id).await.unwrap_err().code(),FailureCode::Policy);
        let mut completed=None;for _ in 0..1000{if let Ok(view)=runtime.inspect_run(&owner,&run.run_id).await&&view.header.status.is_terminal(){completed=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}
        let view=completed.expect("reply graph stalled");assert_eq!(view.header.status,RunStatus::Succeeded);let Some(Value::Map(outputs))=view.outputs else{panic!()};let Some(Value::Map(output))=outputs.get("value")else{panic!()};let Some(Value::Text(id))=output.get("artifact_id")else{panic!()};let id=id.parse().unwrap();
        let value=runtime.read_artifact(&owner,id).await.unwrap();assert_eq!(value.value,Value::Text("classified answer".into()));assert_eq!(value.metadata.security_labels().as_slice(),&["reply-secret".to_string()]);assert_eq!(runtime.read_artifact(&viewer,id).await.unwrap_err().code(),FailureCode::Policy);
        policy.secret.store(false,Ordering::SeqCst);engine.shutdown().await.unwrap();runtime.submit_reply(&responder,wait,Value::Text("classified answer".into()),"reply").await.unwrap();assert_eq!(policy.classifications.load(Ordering::SeqCst),1);assert_eq!(runtime.read_artifact(&viewer,id).await.unwrap_err().code(),FailureCode::Policy);
    })
}
