//! Abrupt worker-process termination at real durable checkpoints, followed by recovery.
#[allow(dead_code)]
#[path = "support/e2e.rs"]
mod e2e;
#[allow(dead_code)]
mod support;
use htlk::{
    compiler::*,
    runtime::{
        clock::CoordinatorClock,
        execution::{ExecutionConfig, ExecutionEngine},
        failure::{FailureCode, RuntimeFailure},
        mcp::{McpEndpoint, NativeMcpAdapter},
        records::RunStatus,
        store::{RecordFamily, SqliteStore, StoreConfig},
        *,
    },
};
use htlk_executable::{
    cbor::{Limits, Value},
    digest::Digest,
    *,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Clock(i64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0)
    }
}

fn configuration(_marker: PathBuf, block: bool) -> (Compiler, NativeRegistry) {
    let (base, mut registry) = support::native_compiler_with_timeouts(60000, 30000);
    let l = Limits::default();
    let digest = Digest::from_bytes([91; 32]);
    let signature = FunctionSignature::new(
        vec![],
        vec![Port::new(ValueType::builtin(BuiltinType::Json), true)],
        Port::new(ValueType::builtin(BuiltinType::Boolean), true),
        &l,
    )
    .unwrap();
    let library = Library::new(
        "test.crash_gate".into(),
        "1".into(),
        digest,
        vec![("accept".parse().unwrap(), signature)],
        &l,
    )
    .unwrap();
    let gate: NativeFunctionImpl = if block {
        |_, _, _| {
            let marker = std::env::var_os("HTLK_CRASH_MARKER").unwrap();
            std::fs::write(marker, b"response persisted; postcondition running").unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    } else {
        |_, _, _| Ok(EvaluationValue::Present(Value::Bool(true)))
    };
    registry
        .register(
            library.clone(),
            BTreeMap::from([(
                "accept".parse().unwrap(),
                NativeFunction::new(gate, 1).unwrap(),
            )]),
        )
        .unwrap();
    let mut config = base.config().clone();
    config.libraries.insert(digest, library);
    (Compiler::new(config).unwrap(), registry)
}

fn source(mode: &str) -> String {
    match mode{
        "response"=>format!(r#"ir_version="0.1"predicate_library gate=predicates.library("test.crash_gate"){{version="1"digest="{}"}}graph test.crash{{inputs={{}}outputs={{value=json}}nodes{{args=eval(json,{{value=null}})echo=call(mcp.tool("fixture","echo")){{postconditions=gate.accept(outputs.value)}}}}edges{{edge args{{from=args.outputs.value to=echo.inputs.arguments}}edge value{{from=echo.outputs.value to=outputs.value}}}}}}"#,Digest::from_bytes([91;32])),
        "intent"=>r#"ir_version="0.1"graph test.crash{inputs={}outputs={value=json}nodes{args=eval(json,{value=null})echo=call(mcp.tool("fixture","echo"))}edges{edge args{from=args.outputs.value to=echo.inputs.arguments}edge value{from=echo.outputs.value to=outputs.value}}}"#.into(),
        "wait"=>r#"ir_version="0.1"graph test.crash{inputs={}outputs={value=boolean}nodes{args=eval(json,{})reply=wait(boolean){topic="recover"timeout_ms=50000}}edges{edge args{from=args.outputs.value to=reply.inputs.request}edge value{from=reply.outputs.value to=outputs.value}}}"#.into(),
        "loop"=>r#"ir_version="0.1"graph test.crash{inputs={}outputs={value=boolean}nodes{seed=eval(boolean,false)repeat=loop{inputs={seed=boolean}outputs={value=boolean}carried={done=inputs.seed}body{nodes{args=eval(json,{})reply=wait(boolean){topic="recover"timeout_ms=50000}}edges{edge args{from=args.outputs.value to=reply.inputs.request}edge next{from=reply.outputs.value to=next.done}edge value{from=reply.outputs.value to=outputs.value}}}until=next.done max_iterations=2}}edges{edge seed{from=seed.outputs.value to=repeat.inputs.seed}edge value{from=repeat.outputs.value to=outputs.value}}}"#.into(),
        _=>panic!("unknown checkpoint"),
    }
}

#[test]
fn subprocess_worker() {
    let Some(path) = std::env::var_os("HTLK_CRASH_DATABASE") else {
        return;
    };
    let mode = std::env::var("HTLK_CRASH_MODE").unwrap();
    let marker = PathBuf::from(std::env::var_os("HTLK_CRASH_MARKER").unwrap());
    let ready = marker.with_extension("started");
    actix::System::new().block_on(async {
        let (compiler, registry) = configuration(marker, mode == "response");
        let graph = compiler
            .compile_source(&source(&mode), &e2e::catalogs("streamable_http"))
            .unwrap();
        let store = SqliteStore::open(path, StoreConfig::default())
            .await
            .unwrap();
        let adapter = NativeMcpAdapter::new(
            BTreeMap::from([(
                "test".into(),
                McpEndpoint::Http {
                    url: std::env::var("HTLK_CRASH_URL").unwrap(),
                    headers: BTreeMap::new(),
                },
            )]),
            5000,
        )
        .unwrap();
        let runtime = Arc::new(
            Runtime::new(
                store,
                Arc::new(registry),
                Arc::new(e2e::Policy),
                RuntimeConfig::default(),
            )
            .unwrap()
            .with_mcp(Arc::new(adapter)),
        );
        runtime
            .register_graph(&e2e::caller(), &graph.executable_bytes, &[])
            .await
            .unwrap();
        let _engine = ExecutionEngine::launch(
            runtime.clone(),
            ExecutionConfig {
                lease_ms: 100,
                poll_interval: Duration::from_millis(2),
                ..Default::default()
            },
        )
        .unwrap();
        let run = runtime
            .start_run(
                &e2e::caller(),
                graph.fingerprint,
                &BTreeMap::new(),
                "crash",
                None,
            )
            .await
            .unwrap();
        std::fs::write(ready, run.run_id.as_str()).unwrap();
        if mode == "loop" {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let view = e2e::inspect(&runtime, &run.run_id).await;
                if let Some(wait) = view.waits.first() {
                    runtime
                        .submit_reply(
                            &e2e::caller(),
                            wait.wait_id,
                            Value::Bool(false),
                            "first-iteration",
                        )
                        .await
                        .unwrap();
                    break;
                }
                assert!(Instant::now() < deadline);
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
        }
        loop {
            actix::clock::sleep(Duration::from_millis(10)).await;
        }
    });
}

fn launch(path: &Path, marker: &Path, mode: &str, peer: &e2e::Peer) -> Worker {
    let McpEndpoint::Http { url, .. } = &peer.endpoint else {
        unreachable!()
    };
    Worker(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "subprocess_worker",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("HTLK_CRASH_DATABASE", path)
            .env("HTLK_CRASH_MARKER", marker)
            .env("HTLK_CRASH_MODE", mode)
            .env("HTLK_CRASH_URL", url)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn killed_workers_resume_real_response_intent_wait_and_loop_checkpoints_without_redispatch() {
    actix::System::new().block_on(async {
        for mode in ["response","intent","wait","loop"]{
            let directory=e2e::TempDir::new();let path=directory.path().join("runtime.sqlite");let marker=directory.path().join("postcondition-ready");let peer=e2e::Peer::new("http",if mode=="intent"{"hold"}else{"success"});
            let mut worker=launch(&path,&marker,mode,&peer);
            let deadline=Instant::now()+Duration::from_secs(30);
            // Open only after the worker has initialized the database, so the
            // parent never wins a concurrent schema migration by accident.
            while !marker.with_extension("started").exists(){assert!(worker.0.try_wait().unwrap().is_none(),"worker exited during startup");assert!(Instant::now()<deadline);actix::clock::sleep(Duration::from_millis(2)).await;}
            let store=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();
            let (run,before,wait)=loop{
                assert!(worker.0.try_wait().unwrap().is_none(),"{mode} worker exited before checkpoint");assert!(Instant::now()<deadline,"{mode} checkpoint not reached; requests={:?}",peer.requests());
                let runs=store.running_runs(None,10).await.unwrap();
                if let Some(run)=runs.first(){
                    let header=store.run_header(run).await.unwrap();let waits=store.records(run,RecordFamily::Wait,None,10).await.unwrap();let responses=store.records(run,RecordFamily::Response,None,10).await.unwrap();
                    let ready=match mode{"response"=>marker.exists()&&responses.len()==1,"intent"=>peer.calls().len()==1,"wait"=>waits.len()==1,"loop"=>waits.len()==2,_=>unreachable!()};
                    if ready{let wait=waits.iter().find_map(|record|matches!(&record.payload,Value::Map(fields)if fields.get("state")==Some(&Value::Text("open".into()))).then(||record.key.id.parse::<identity::WaitId>().unwrap()));break(run.clone(),header,wait);}
                }
                actix::clock::sleep(Duration::from_millis(2)).await;
            };
            let invocations=store.records(&run,RecordFamily::Invocation,None,100).await.unwrap();
            worker.0.kill().unwrap();let status=worker.0.wait().unwrap();assert!(!status.success());
            let (compiler,registry)=configuration(marker.clone(),false);let graph=compiler.compile_source(&source(mode),&e2e::catalogs("streamable_http")).unwrap();assert_eq!(graph.fingerprint,before.fingerprint);
            // No adapter is installed in the recovering service: a saved response
            // must be accepted locally and an unsafe intent must remain uncertain.
            let runtime=Arc::new(Runtime::new(store.clone(),Arc::new(registry),Arc::new(e2e::Policy),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock(before.expires_at_ms.unwrap()+1))));
            let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{lease_ms:100,poll_interval:Duration::from_millis(2),..Default::default()}).unwrap();
            if let Some(wait)=wait{
                loop{match runtime.submit_reply(&e2e::caller(),wait,Value::Bool(true),"recovered-reply").await{Ok(_)=>break,Err(error)if error.code()==FailureCode::Fenced=>{},Err(error)=>panic!("{mode}: {error:?}")}assert!(Instant::now()<deadline);actix::clock::sleep(Duration::from_millis(2)).await;}
            }
            let view=e2e::terminal(&runtime,&run).await;assert!(view.header.fence>before.fence);
            if mode=="intent"{assert_eq!(view.header.status,RunStatus::Failed);let node=runtime.inspect_node(&e2e::caller(),&run,identity::InvocationId::new(before.root_scope,&"echo".parse().unwrap(),&Limits::default()).unwrap()).await.unwrap();assert_eq!(node.failure.unwrap().code(),FailureCode::EffectUncertain);}
            else{assert_eq!(view.header.status,RunStatus::Succeeded,"{mode}: {:?}",view.failure);if mode=="wait"||mode=="loop"{assert_eq!(e2e::output(&runtime,&view,"value").await,Value::Bool(true));}else{assert!(matches!(e2e::output(&runtime,&view,"value").await,Value::Map(m)if m.get("value")==Some(&Value::Null)));}}
            for original in invocations{let restored=store.record(&run,&original.key).await.unwrap();let(Value::Map(a),Value::Map(b))=(&original.payload,&restored.payload)else{panic!()};for key in ["definition_digest","origin_executable","input_hash"]{if matches!(a.get(key),Some(Value::Text(_))){assert_eq!(a.get(key),b.get(key),"{mode}/{key}");}}}
            assert_eq!(peer.calls().len(),usize::from(matches!(mode,"intent"|"response")));e2e::assert_replay(&store,&run).await;
            engine.shutdown().await.unwrap();store.close().await.unwrap();
        }
    });
}
