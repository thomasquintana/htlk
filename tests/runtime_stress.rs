//! Bounded all-PR campaigns for competing SQLite coordinators and host commands.
#[allow(dead_code)]
#[path = "support/e2e.rs"]
mod e2e;
#[allow(dead_code)]
mod support;
use htlk::runtime::{
    Runtime, RuntimeConfig,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::FailureCode,
    records::{InputValue, RunStatus},
    store::{RecordFamily, SqliteStore, StoreConfig},
};
use htlk_executable::{Identifier, cbor::Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

#[test]
fn competing_coordinators_and_duplicate_host_commands_preserve_one_authoritative_result() {
    actix::System::new().block_on(async {
        for workers in [1,2,4]{
            let directory=e2e::TempDir::new();let path=directory.path().join("shared.sqlite");
            let(compiler,registry)=support::native_compiler_with_timeouts(60000,10000);let registry=Arc::new(registry);
            let graph=compiler.compile_source(r#"ir_version="0.1"task leaf{inputs={seed=integer}outputs={value=integer}nodes{}edges{edge value{from=inputs.seed to=outputs.value}}}graph test.competing{inputs={seed=integer}outputs={value=integer}nodes{leaf=use(leaf)copy=eval(integer,inputs.value){inputs={value=integer}}}edges{edge seed{from=inputs.seed to=leaf.inputs.seed}edge copy{from=leaf.outputs.value to=copy.inputs.value}edge value{from=copy.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
            let store=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();let second=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();
            let left=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(e2e::Policy),RuntimeConfig::default()).unwrap());let right=Arc::new(Runtime::new(second.clone(),registry,Arc::new(e2e::Policy),RuntimeConfig::default()).unwrap());
            left.register_graph(&e2e::caller(),&graph.executable_bytes,&[]).await.unwrap();
            let config=ExecutionConfig{workers,max_runs:3,lease_ms:1000,poll_interval:Duration::from_millis(1)};
            let engines=[ExecutionEngine::launch(left.clone(),config.clone()).unwrap(),ExecutionEngine::launch(right.clone(),config).unwrap()];
            let mut commands=vec![];
            let seeds=[i64::MIN,-1,0,1,i64::MAX];
            for index in 0..24{
                for runtime in [&left,&right]{
                    let runtime=runtime.clone();let fingerprint=graph.fingerprint;let seed=seeds[index%seeds.len()];
                    commands.push((index,seed,actix::spawn(async move{let inputs:BTreeMap<Identifier,InputValue>=BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Integer(seed)))]);runtime.start_run(&e2e::caller(),fingerprint,&inputs,&format!("request-{index}"),None).await.unwrap()})));
                }
            }
            let mut runs=BTreeMap::new();
            for(index,seed,command)in commands{let run=command.await.unwrap();if let Some((existing,_))=runs.get(&index){assert_eq!(existing,&run.run_id);}else{runs.insert(index,(run.run_id,seed));}}
            assert_eq!(runs.values().map(|(run,_)|run).collect::<BTreeSet<_>>().len(),24);
            for(index,(run,seed))in runs{
                let view=e2e::terminal(&left,&run).await;assert_eq!(view.header.status,RunStatus::Succeeded);assert_eq!(e2e::output(&left,&view,"value").await,Value::Integer(seed));
                assert_eq!(store.records(&run,RecordFamily::Invocation,None,100).await.unwrap().len(),2);assert_eq!(view.scopes.len(),2);
                assert_eq!(left.cancel_run(&e2e::caller(),&run,"late cancellation","late").await.unwrap().status,RunStatus::Succeeded);
                let wrong=BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Integer(if seed==0{1}else{0})))]);
                assert_eq!(right.start_run(&e2e::caller(),graph.fingerprint,&wrong,&format!("request-{index}"),None).await.err().unwrap().code(),FailureCode::IdempotencyConflict);
                e2e::assert_replay(&second,&run).await;
            }
            for engine in engines{engine.shutdown().await.unwrap();}store.close().await.unwrap();second.close().await.unwrap();
        }
    });
}
