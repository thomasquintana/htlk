//! Source-to-actor execution over actual transactional records and native profiles.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    identity::RunId,
    records::{InputValue, RunStatus, SecurityLabels},
    store::{RecordFamily, RecordKey, RunHeader, SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use support::{catalogs, native_compiler};
struct Policy;
#[test]
fn late_conditional_writer_conflicts_before_consumer_admission() {
    actix::System::new().block_on(async{
    let (_,store,header)=execute(r#"ir_version="0.1" graph test.conflict{inputs={} outputs={value=integer}nodes{
      first=eval(integer,1) late=eval(integer,2) consumer=eval(integer,inputs.value){inputs={value=integer}}
    }edges{edge a{from=first.outputs.value to=consumer.inputs.value} edge b{from=late.outputs.value to=consumer.inputs.value when=status(@late)=="succeeded"} edge result{from=consumer.outputs.value to=outputs.value}}}"#,BTreeMap::new()).await;
    assert_eq!(header.status,RunStatus::Failed);
    let nodes=store.records(&header.run_id,RecordFamily::Invocation,None,100).await.unwrap();
    let consumer=nodes.iter().find(|node|if let Value::Map(fields)=&node.payload{fields.get("local_id")==Some(&Value::Text("consumer".into()))}else{false}).unwrap();
    let Value::Map(fields)=&consumer.payload else{panic!()};assert_eq!(fields.get("input_hash"),Some(&Value::Null));assert_eq!(RuntimeFailure::from_value(fields.get("failure").unwrap(),&Limits::default()).unwrap().code(),FailureCode::BindingConflict);
})
}
#[test]
fn terminal_failure_fallback_can_succeed_without_propagating_every_failed_child() {
    actix::System::new().block_on(async{
    let (runtime,store,header)=execute(r#"ir_version="0.1" graph test.fallback{inputs={} outputs={value=integer}nodes{
      primary=eval(integer,1){postconditions=false} recovery=eval(integer,2){when=status(@primary)=="failed"}
    }edges{edge primary{from=primary.outputs.value to=outputs.value when=status(@primary)=="succeeded"} edge recovery{from=recovery.outputs.value to=outputs.value when=status(@recovery)=="succeeded"}}postconditions=status(@recovery)=="succeeded"}"#,BTreeMap::new()).await;
    assert_eq!(header.status,RunStatus::Succeeded);assert_eq!(result(&runtime,&store,&header,"value").await,Value::Integer(2));
})
}
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
        Ok(SecurityLabels::new(vec!["protected".into()], &Limits::default()).unwrap())
    }
}
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
async fn execute(
    source: &str,
    inputs: BTreeMap<Identifier, InputValue>,
) -> (Arc<Runtime>, SqliteStore, RunHeader) {
    let (compiler, registry) = native_compiler();
    let result = compiler.compile_source(source, &catalogs()).unwrap();
    let store = SqliteStore::in_memory(StoreConfig::default())
        .await
        .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store.clone(),
            Arc::new(registry),
            Arc::new(Policy),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(Arc::new(Clock)),
    );
    let context = HostContext::new("alice".into(), &Limits::default()).unwrap();
    runtime
        .register_graph(&context, &result.executable_bytes, &[])
        .await
        .unwrap();
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            poll_interval: Duration::from_millis(2),
            ..Default::default()
        },
    )
    .unwrap();
    let run = runtime
        .start_run(&context, result.fingerprint, &inputs, "start", None)
        .await
        .unwrap();
    let header = wait(&store, &run.run_id).await;
    engine.shutdown().await.unwrap();
    (runtime, store, header)
}
async fn wait(store: &SqliteStore, run: &RunId) -> RunHeader {
    for _ in 0..2000 {
        let header = store.run_header(run).await.unwrap();
        if header.status.is_terminal() {
            return header;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    let journal = store.journal(run, 0, 100).await.unwrap();
    panic!("execution stalled: {journal:?}")
}
async fn result(runtime: &Runtime, store: &SqliteStore, header: &RunHeader, port: &str) -> Value {
    let record = store
        .record(
            &header.run_id,
            &RecordKey {
                family: RecordFamily::Scope,
                id: header.root_scope.to_string(),
            },
        )
        .await
        .unwrap();
    let Value::Map(scope) = record.payload else {
        panic!()
    };
    let Some(Value::Map(outputs)) = scope.get("outputs") else {
        panic!()
    };
    let Some(Value::Map(entry)) = outputs.get(port) else {
        panic!()
    };
    let Some(Value::Text(id)) = entry.get("artifact_id") else {
        panic!()
    };
    runtime
        .read_artifact(
            &HostContext::new("alice".into(), &Limits::default()).unwrap(),
            id.parse().unwrap(),
        )
        .await
        .unwrap()
        .value
}
#[test]
fn actors_execute_pure_dependency_chains_and_commit_root_passthrough() {
    actix::System::new().block_on(async{
        let (runtime,store,header)=execute(r#"ir_version="0.1" prompt message(name=string)="Hello {name}"
      graph test.chain{inputs={name=string} outputs={value=string} nodes{
        first=eval(string,render(&message,{name=inputs.name})){inputs={name=string}}
        second=eval(string,inputs.value){inputs={value=string}}
      }edges{edge input{from=inputs.name to=first.inputs.name} edge chain{from=first.outputs.value to=second.inputs.value} edge output{from=second.outputs.value to=outputs.value}}}"#,
      BTreeMap::from([("name".parse().unwrap(),InputValue::Inline(Value::Text("world".into())))])).await;
    assert_eq!(header.status,RunStatus::Succeeded);assert_eq!(result(&runtime,&store,&header,"value").await,Value::Text("Hello world".into()));
})
}
#[test]
fn task_instances_and_loop_body_boundaries_execute_without_retrying_scopes() {
    actix::System::new().block_on(async{
    let (runtime,store,header)=execute(r#"ir_version="0.1"
      task leaf{inputs={seed=integer} outputs={value=integer} nodes{} edges{edge pass{from=inputs.seed to=outputs.value}}}
      graph test.nested{inputs={seed=integer} outputs={value=integer} nodes{
        task=use(leaf)
        repeat=loop{inputs={seed=integer} outputs={value=integer} carried={counter=inputs.seed}
          body{nodes{copy=eval(integer,inputs.counter){inputs={counter=integer}}}edges{edge carry{from=carried.counter to=copy.inputs.counter} edge next{from=copy.outputs.value to=next.counter} edge output{from=copy.outputs.value to=outputs.value}}}
          until=next.counter>=0 max_iterations=3 postconditions=outputs.value>=0
        }
      }edges{edge input{from=inputs.seed to=task.inputs.seed} edge loop{from=task.outputs.value to=repeat.inputs.seed} edge output{from=repeat.outputs.value to=outputs.value}}}"#,
      BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Integer(7)))])).await;
    assert_eq!(header.status,RunStatus::Succeeded);assert_eq!(result(&runtime,&store,&header,"value").await,Value::Integer(7));
})
}
#[test]
fn pure_loop_exact_bound_fails_and_final_iteration_still_requires_next() {
    actix::System::new().block_on(async{
    for (until,next_guard,expected)in [("false","true",FailureCode::LoopLimit),("true","false",FailureCode::OutputMissing)]{
        let source=format!(r#"ir_version="0.1" graph test.bound{{inputs={{seed=integer}} outputs={{value=integer}}
          nodes{{repeat=loop{{inputs={{seed=integer}} outputs={{value=integer}} carried={{counter=inputs.seed}}
            body{{nodes{{copy=eval(integer,0)}}edges{{edge next{{from=copy.outputs.value to=next.counter when={next_guard}}} edge output{{from=copy.outputs.value to=outputs.value}}}}}}
            until={until} max_iterations=3
          }}}}edges{{edge input{{from=inputs.seed to=repeat.inputs.seed}} edge output{{from=repeat.outputs.value to=outputs.value}}}}}}"#);
        let (_,store,header)=execute(&source,BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Integer(0)))])).await;assert_eq!(header.status,RunStatus::Failed);
        let nodes=store.records(&header.run_id,RecordFamily::Invocation,None,100).await.unwrap();
        assert!(nodes.iter().any(|node|if let Value::Map(fields)=&node.payload{fields.get("local_id")==Some(&Value::Text("repeat".into()))&&fields.get("failure").is_some_and(|value|RuntimeFailure::from_value(value,&Limits::default()).is_ok_and(|failure|failure.code()==expected))}else{false}));
    }
})
}
