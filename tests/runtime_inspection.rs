//! Stable logical inspection exposes protected decisions and causes, not raw replies.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    identity::InvocationId,
    records::{RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(&self, context: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if context.principal() == "alice" {
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
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
#[test]
fn inspection_retains_dependency_causes_selected_edges_boundaries_and_bounded_history() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler();let graph=compiler.compile_source(r#"
            ir_version="0.1" graph test.inspection{
                inputs={}outputs={value=integer}
                nodes{primary=eval(integer,1){postconditions=false}consumer=eval(integer,inputs.value){inputs={value=integer}}fallback=eval(integer,2){when=status(@consumer)=="failed"}}
                edges{edge dependency{from=primary.outputs.value to=consumer.inputs.value}edge primary{from=consumer.outputs.value to=outputs.value when=status(@consumer)=="succeeded"}edge recovery{from=fallback.outputs.value to=outputs.value when=status(@fallback)=="succeeded"}}
            }
        "#,&support::catalogs()).unwrap();let runtime=Arc::new(Runtime::new(SqliteStore::in_memory(StoreConfig::default()).await.unwrap(),Arc::new(registry),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();let denied=HostContext::new("bob".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();let mut done=None;for _ in 0..1000{if let Ok(view)=runtime.inspect_run(&caller,&run.run_id).await&&view.header.status.is_terminal(){done=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}let view=done.expect("inspection graph stalled");assert_eq!(view.header.status,RunStatus::Succeeded);assert!(view.failure.is_none());assert_eq!(view.scopes.len(),1);assert!(view.bindings.iter().any(|b|b.id.contains(":$outputs:value")));
        let invocation=|name:&str|InvocationId::new(view.header.root_scope,&name.parse().unwrap(),&Limits::default()).unwrap();let primary=invocation("primary");let consumer=runtime.inspect_node(&caller,&run.run_id,invocation("consumer")).await.unwrap();assert_eq!(consumer.failure.unwrap().code(),FailureCode::Dependency);assert!(consumer.input_manifest.is_none());assert_eq!(consumer.bindings.len(),1);assert!(consumer.causes.iter().any(|c|c.invocation==primary&&c.code==FailureCode::Postconditions&&c.location.is_some()));assert_eq!(consumer.origin_executable,graph.fingerprint);assert!(consumer.canonical_path.to_string().contains("/nodes/"));assert_eq!(runtime.inspect_node(&denied,&run.run_id,invocation("consumer")).await.unwrap_err().code(),FailureCode::Policy);
        let fallback=runtime.inspect_node(&caller,&run.run_id,invocation("fallback")).await.unwrap();assert!(fallback.input_manifest.is_some());assert!(fallback.deadline_ms.is_some());assert!(fallback.limits.is_some());assert!(fallback.causes.iter().any(|c|c.invocation==invocation("consumer")));assert!(fallback.attempts.is_empty());
        let mut after=0;let mut sequences=Vec::new();loop{let page=runtime.inspect_history(&caller,&run.run_id,after,2).await.unwrap();for event in page.entries{assert!(event.sequence>after);sequences.push(event.sequence);}match page.next{Some(next)=>after=next,None=>break}}
        assert_eq!(sequences.len()as i64,view.header.sequence);assert_eq!(sequences,(1..=view.header.sequence).collect::<Vec<_>>());assert_eq!(runtime.inspect_history(&denied,&run.run_id,0,2).await.unwrap_err().code(),FailureCode::Policy);engine.shutdown().await.unwrap();
    })
}
