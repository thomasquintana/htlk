//! Reopened durable delivery checkpoints resume without an external capability.
mod support;
use htlk::runtime::{
    Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::RuntimeFailure,
    identity::RunId,
    mcp::{McpAdapter, McpFuture, McpRequest},
    records::{InputValue, RunStatus, SecurityLabels},
    store::{
        RecordFamily, RecordKey, RecordWrite, ReplayState, SqliteStore, StoreConfig, StoredRecord,
        Transition, TransitionContext,
    },
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Policy;
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
}
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Held {
    descriptors: BTreeMap<Digest, JsonDocument>,
    sent: AtomicUsize,
    release: AtomicBool,
}
const RESPONSE: &[u8] =
    br#"{"content":[],"structuredContent":{"evidence_text":"recorded before crash"}}"#;
impl McpAdapter for Held {
    fn descriptor<'a>(
        &'a self,
        b: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&b.descriptor()].clone()) })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.sent.fetch_add(1, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                actix::clock::sleep(Duration::from_millis(1)).await;
            }
            Ok(RESPONSE.to_vec())
        })
    }
}
fn object(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Map(Map::try_from_entries(entries.into_iter().map(|(k, v)| (k.to_string(), v))).unwrap())
}
fn change(value: &Value, changes: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let Value::Map(value) = value else { panic!() };
    let mut fields: BTreeMap<_, _> = value
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    fields.extend(changes.into_iter().map(|(k, v)| (k.to_string(), v)));
    Value::Map(Map::try_from_entries(fields).unwrap())
}
fn update(record: &StoredRecord, payload: Value, final_record: bool) -> RecordWrite {
    RecordWrite {
        key: record.key.clone(),
        expected_revision: Some(record.revision),
        final_record,
        payload,
    }
}
fn transaction(writes: Vec<RecordWrite>, time: i64) -> Transition {
    Transition {
        context: TransitionContext {
            command_id: "crash-checkpoint".into(),
            actor: "fixture".into(),
            causation: "crash-checkpoint".into(),
            timestamp_ms: time,
        },
        writes,
        next_status: None,
        root: None,
        expected_fingerprint: None,
    }
}
async fn terminal(
    runtime: &Runtime,
    caller: &HostContext,
    run: &RunId,
) -> htlk::runtime::execution::RunView {
    for _ in 0..2000 {
        if let Ok(view) = runtime.inspect_run(caller, run).await
            && view.header.status.is_terminal()
        {
            return view;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("recovery stalled")
}
async fn open_wait(
    runtime: &Runtime,
    caller: &HostContext,
    run: &RunId,
    exclude: Option<htlk::runtime::identity::WaitId>,
) -> htlk::runtime::identity::WaitId {
    for _ in 0..2000 {
        match runtime.inspect_run(caller, run).await {
            Ok(view) => {
                assert!(
                    !view.header.status.is_terminal(),
                    "run terminated before its wait settled: {view:#?}"
                );
                if view.blocked
                    && let Some(wait) = view.waits.iter().find(|w| Some(w.wait_id) != exclude)
                {
                    return wait.wait_id;
                }
            }
            Err(error) if error.code() == htlk::runtime::failure::FailureCode::Fenced => {}
            Err(error) => panic!("wait inspection failed: {error:?}"),
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("scheduler did not settle at an open wait")
}
#[test]
fn reopened_loop_resumes_committed_feedback_and_the_same_open_wait() {
    actix::System::new().block_on(async {
        static NEXT:AtomicU64=AtomicU64::new(0);
        let path=std::env::temp_dir().join(format!("htlk-loop-recovery-{}-{}.sqlite",std::process::id(),NEXT.fetch_add(1,Ordering::SeqCst)));
        let (compiler,registry)=support::native_compiler_with_timeouts(10000,1000);let registry=Arc::new(registry);
        let graph=compiler.compile_source(r#"
            ir_version="0.1" graph test.loop_recovery {
                inputs={seed=boolean} outputs={value=boolean}
                nodes{repeat=loop{
                    inputs={seed=boolean} outputs={value=boolean} carried={done=inputs.seed}
                    body{nodes{question=eval(json,{}) reply=wait(boolean){topic="iteration" timeout_ms=5000}}
                        edges{edge question{from=question.outputs.value to=reply.inputs.request}edge next{from=reply.outputs.value to=next.done}edge value{from=reply.outputs.value to=outputs.value}}}
                    until=next.done max_iterations=3
                }}
                edges{edge seed{from=inputs.seed to=repeat.inputs.seed}edge value{from=repeat.outputs.value to=outputs.value}}
            }
        "#,&support::catalogs()).unwrap();
        let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();let runtime=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock.clone()));runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();
        let config=ExecutionConfig{lease_ms:100,poll_interval:Duration::from_millis(1),..Default::default()};let engine=ExecutionEngine::launch(runtime.clone(),config.clone()).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::from([("seed".parse().unwrap(),InputValue::Inline(Value::Bool(false)))]),"start",None).await.unwrap();
        // Freeze this checkpoint only after feedback and the next wait have both
        // settled; merely observing the wait row can precede scheduler quiescence.
        let first=open_wait(&runtime,&caller,&run.run_id,None).await;runtime.submit_reply(&caller,first,Value::Bool(false),"first").await.unwrap();let second=open_wait(&runtime,&caller,&run.run_id,Some(first)).await;assert_ne!(first,second);
        let before=store.records(&run.run_id,RecordFamily::Invocation,None,100).await.unwrap();engine.shutdown().await.unwrap();store.close().await.unwrap();drop(runtime);
        clock.0.store(1200,Ordering::SeqCst);let store=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();let runtime=Arc::new(Runtime::new(store.clone(),registry,Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock));let engine=ExecutionEngine::launch(runtime.clone(),config).unwrap();assert_eq!(open_wait(&runtime,&caller,&run.run_id,Some(first)).await,second);
        let mut submitted=false;
        for _ in 0..2000{
            match runtime.submit_reply(&caller,second,Value::Bool(true),"second").await{
                Ok(_)=>{submitted=true;break},Err(e)if e.code()==htlk::runtime::failure::FailureCode::Fenced=>{},Err(e)=>panic!("{e:?}"),
            }
            actix::clock::sleep(Duration::from_millis(2)).await;
        }
        assert!(submitted);let view=terminal(&runtime,&caller,&run.run_id).await;assert_eq!(view.header.status,RunStatus::Succeeded,"reopened loop failed: {view:#?}");
        assert_eq!(store.records(&run.run_id,RecordFamily::Wait,None,100).await.unwrap().len(),2);
        for record in before{let after=store.record(&run.run_id,&record.key).await.unwrap();let (Value::Map(before),Value::Map(after))=(&record.payload,&after.payload)else{panic!()};assert_eq!(before.get("input_hash"),after.get("input_hash"));}
        runtime.submit_reply(&caller,first,Value::Bool(false),"first").await.unwrap();engine.shutdown().await.unwrap();store.close().await.unwrap();std::fs::remove_file(&path).unwrap();
    })
}
#[test]
fn reopened_response_recorded_checkpoint_finishes_with_no_mcp_adapter_and_matches_replay() {
    actix::System::new().block_on(async {
        static NEXT:AtomicU64=AtomicU64::new(0);
        let path=std::env::temp_dir().join(format!("htlk-runtime-recovery-{}-{}.sqlite",std::process::id(),NEXT.fetch_add(1,Ordering::SeqCst)));
        let (compiler,registry)=support::native_compiler_with_timeouts(10000,5000);let registry=Arc::new(registry);
        let graph=compiler.compile_source(r#"ir_version="0.1" graph test.crash{inputs={}outputs={value=json}nodes{args=eval(json,{query="question"}) lookup=call(mcp.tool("research","lookup"))}edges{edge args{from=args.outputs.value to=lookup.inputs.arguments}edge value{from=lookup.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let adapter=Arc::new(Held{descriptors:graph.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),graph.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),sent:AtomicUsize::new(0),release:AtomicBool::new(false)});
        let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();
        let runtime=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock.clone()).with_mcp(adapter.clone()));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();
        let config=ExecutionConfig{lease_ms:100,poll_interval:Duration::from_millis(1),..Default::default()};let engine=ExecutionEngine::launch(runtime.clone(),config.clone()).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();
        for _ in 0..2000{if adapter.sent.load(Ordering::SeqCst)==1{break}actix::clock::sleep(Duration::from_millis(1)).await;}assert_eq!(adapter.sent.load(Ordering::SeqCst),1);engine.shutdown().await.unwrap();
        clock.0.store(1200,Ordering::SeqCst);let lease=store.acquire(&run.run_id,"checkpoint-owner".into(),1200,1300).await.unwrap();
        let attempt=store.records(&run.run_id,RecordFamily::Attempt,None,100).await.unwrap().pop().unwrap();
        let mut writes=vec![update(&attempt,change(&attempt.payload,[("dispatch_state",Value::Text("response_recorded".into()))]),true),RecordWrite{key:RecordKey{family:RecordFamily::Response,id:attempt.key.id.clone()},expected_revision:None,final_record:true,payload:object([("raw",Value::Bytes(RESPONSE.to_vec())),("raw_digest",Value::Text(htlk_executable::digest::hash_bytes(RESPONSE).to_string())),("delivery",Value::Text("response_received".into())),("influence_labels",SecurityLabels::default().to_value(&Limits::default()).unwrap())])}];
        for reservation in store.records(&run.run_id,RecordFamily::Reservation,None,100).await.unwrap(){
            writes.push(update(&reservation,change(&reservation.payload,[("state",Value::Text("released".into()))]),true));
        }
        for usage in store.records(&run.run_id,RecordFamily::Usage,None,100).await.unwrap(){writes.push(update(&usage,change(&usage.payload,[("concurrency_reserved",Value::Integer(0))]),false));}
        store.commit(&lease,transaction(writes,1200)).await.unwrap();adapter.release.store(true,Ordering::SeqCst);actix::clock::sleep(Duration::from_millis(20)).await;store.close().await.unwrap();drop(runtime);
        clock.0.store(1400,Ordering::SeqCst);let reopened=SqliteStore::open(&path,StoreConfig::default()).await.unwrap();
        let recovered=Arc::new(Runtime::new(reopened.clone(),registry,Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock));
        let engine=ExecutionEngine::launch(recovered.clone(),config).unwrap();let view=terminal(&recovered,&caller,&run.run_id).await;assert_eq!(view.header.status,RunStatus::Succeeded);assert_eq!(adapter.sent.load(Ordering::SeqCst),1);assert!(view.header.fence>lease.fence());
        let entries=reopened.journal(&run.run_id,0,100).await.unwrap();let mut replay=ReplayState::begin(&entries[0],&Limits::default()).unwrap();for entry in &entries[1..]{replay.apply(entry,&Limits::default()).unwrap();}assert_eq!(replay.header(),&view.header);
        for family in [RecordFamily::Scope,RecordFamily::Invocation,RecordFamily::Artifact,RecordFamily::Usage,RecordFamily::Reservation,RecordFamily::Attempt,RecordFamily::Response,RecordFamily::Guard,RecordFamily::Binding]{for record in reopened.records(&run.run_id,family,None,100).await.unwrap(){assert_eq!(replay.record(&record.key),Some(&record));}}
        engine.shutdown().await.unwrap();reopened.close().await.unwrap();std::fs::remove_file(&path).unwrap();
    })
}
#[test]
fn unavailable_profile_corrupt_checkpoint_and_impossible_progress_fail_clearly() {
    actix::System::new().block_on(async {
        for cause in ["profile","checkpoint","progress"]{
            let (compiler,registry)=support::native_compiler();let registry=Arc::new(registry);
            let graph=compiler.compile_source(r#"ir_version="0.1" graph test.bad_recovery{inputs={value=string}outputs={value=integer}nodes{n=eval(integer,1)}edges{edge value{from=n.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
            let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let runtime=Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock.clone());let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();
            let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::from([("value".parse().unwrap(),InputValue::Inline(Value::Text("input".into())))]),"start",None).await.unwrap();
            if cause!="profile"{
                let lease=store.acquire(&run.run_id,"corruption-fixture".into(),1000,1100).await.unwrap();let header=store.run_header(&run.run_id).await.unwrap();let root=store.record(&run.run_id,&RecordKey{family:RecordFamily::Scope,id:header.root_scope.to_string()}).await.unwrap();
                let payload=if cause=="checkpoint"{change(&root.payload,[("input_checkpoint",Value::Bytes(vec![0]))])}else{change(&root.payload,[("children_instantiated",Value::Bool(true))])};store.commit(&lease,transaction(vec![update(&root,payload,false)],1000)).await.unwrap();
            }
            clock.0.store(1200,Ordering::SeqCst);
            let registry=if cause=="profile"{Arc::new(NativeRegistry::new(&Limits::default()).unwrap())}else{registry};
            let recovered=Arc::new(Runtime::new(store.clone(),registry,Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock));let engine=ExecutionEngine::launch(recovered.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();
            let view=terminal(&recovered,&caller,&run.run_id).await;assert_eq!(view.header.status,RunStatus::Failed);assert!(view.outputs.is_none());let root=store.record(&run.run_id,&RecordKey{family:RecordFamily::Scope,id:view.header.root_scope.to_string()}).await.unwrap();let Value::Map(root)=root.payload else{panic!()};let Some(Value::Map(failure))=root.get("failure")else{panic!()};assert_eq!(failure.get("code"),Some(&Value::Text(match cause{"profile"=>"E_PROFILE","checkpoint"=>"E_DIGEST",_=>"E_INVARIANT"}.into())));engine.shutdown().await.unwrap();
        }
    })
}

#[test]
fn recovery_rejects_corrupt_scope_invocation_wait_and_accounting_facts_without_publishing() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler_with_timeouts(30000,10000);let registry=Arc::new(registry);
        let graph=compiler.compile_source(r#"ir_version="0.1"task question{inputs={}outputs={value=boolean}nodes{args=eval(json,{})reply=wait(boolean){topic="recover"timeout_ms=20000}}edges{edge args{from=args.outputs.value to=reply.inputs.request}edge value{from=reply.outputs.value to=outputs.value}}}graph test.corruption{inputs={}outputs={value=boolean}nodes{question=use(question)}edges{edge value{from=question.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let bad_digest=Value::Text(Digest::from_bytes([0;32]).to_string());
        let cases=[
            (RecordFamily::Scope,"definition_digest",bad_digest.clone()),
            (RecordFamily::Scope,"input_checkpoint",Value::Bytes(vec![0])),
            (RecordFamily::Scope,"input_hash",bad_digest.clone()),
            (RecordFamily::Scope,"inputs",Value::Null),
            (RecordFamily::Scope,"scope_use",Value::Null),
            (RecordFamily::Scope,"status",Value::Text("impossible".into())),
            (RecordFamily::Invocation,"definition_digest",bad_digest.clone()),
            (RecordFamily::Invocation,"local_id",Value::Text("missing".into())),
            (RecordFamily::Invocation,"input_checkpoint",Value::Bytes(vec![0])),
            (RecordFamily::Invocation,"input_hash",bad_digest.clone()),
            (RecordFamily::Invocation,"inputs",Value::Null),
            (RecordFamily::Invocation,"child_scope",bad_digest.clone()),
            (RecordFamily::Invocation,"status",Value::Text("impossible".into())),
            (RecordFamily::Wait,"invocation_id",bad_digest.clone()),
            (RecordFamily::Wait,"generation",Value::Integer(1)),
            (RecordFamily::Wait,"purpose",Value::Text("approval".into())),
            (RecordFamily::Wait,"response_type",Value::Null),
            (RecordFamily::Wait,"topic",Value::Text("changed".into())),
            (RecordFamily::Wait,"authorization_policy",object([("authority",Value::Text("".into())),("version",Value::Text("1".into())),("requester",Value::Text("alice".into()))])),
            (RecordFamily::Wait,"expires_at",Value::Integer(i64::MAX)),
            (RecordFamily::Wait,"state",Value::Text("impossible".into())),
            (RecordFamily::Usage,"limits",Value::Null),
            (RecordFamily::Usage,"concurrency_reserved",Value::Integer(1)),
            (RecordFamily::Usage,"calls_spent",Value::Integer(-1)),
        ];
        for(family,field,value)in cases{
            let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
            let runtime=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock.clone()));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();
            let config=ExecutionConfig{lease_ms:100,poll_interval:Duration::from_millis(1),..Default::default()};let engine=ExecutionEngine::launch(runtime.clone(),config.clone()).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"corrupt",None).await.unwrap();open_wait(&runtime,&caller,&run.run_id,None).await;
            engine.shutdown().await.unwrap();clock.0.store(1200,Ordering::SeqCst);let lease=store.acquire(&run.run_id,"corruption".into(),1200,1300).await.unwrap();
            let records=store.records(&run.run_id,family,None,100).await.unwrap();
            let record=if family==RecordFamily::Scope{let header=store.run_header(&run.run_id).await.unwrap();records.iter().find(|r|r.key.id==header.root_scope.to_string()).unwrap()}else{records.iter().find(|r|!r.final_record).unwrap()};
            let artifact_count=store.records(&run.run_id,RecordFamily::Artifact,None,100).await.unwrap().len();
            store.commit(&lease,transaction(vec![update(record,change(&record.payload,[(field,value)]),false)],1200)).await.unwrap();
            clock.0.store(1400,Ordering::SeqCst);let recovered=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock));let engine=ExecutionEngine::launch(recovered.clone(),config).unwrap();
            let deadline=std::time::Instant::now()+Duration::from_secs(5);
            loop{let header=store.run_header(&run.run_id).await.unwrap();if header.status.is_terminal(){assert_eq!(header.status,RunStatus::Failed,"{family:?}/{field}");break;}assert!(std::time::Instant::now()<deadline,"recovery stalled for {family:?}/{field}");actix::clock::sleep(Duration::from_millis(2)).await;}
            if let Ok(view)=recovered.inspect_run(&caller,&run.run_id).await{assert!(view.outputs.is_none(),"{family:?}/{field}");}
            assert_eq!(store.records(&run.run_id,RecordFamily::Artifact,None,100).await.unwrap().len(),artifact_count,"corrupt recovery published an artifact: {family:?}/{field}");
            engine.shutdown().await.unwrap();store.close().await.unwrap();
        }
    });
}

#[test]
fn recorded_mcp_checkpoint_corruption_cannot_change_the_frozen_request_or_publish_a_response() {
    actix::System::new().block_on(async {
        let(compiler,registry)=support::native_compiler_with_timeouts(30000,10000);let registry=Arc::new(registry);
        let graph=compiler.compile_source(r#"ir_version="0.1"graph test.checkpoint{inputs={}outputs={value=json}nodes{args=eval(json,{query="question"})lookup=call(mcp.tool("research","lookup"))}edges{edge args{from=args.outputs.value to=lookup.inputs.arguments}edge value{from=lookup.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let bad=Value::Text(Digest::from_bytes([0;32]).to_string());
        let cases=[
            (false,"invocation_id",bad.clone()),(false,"method",Value::Text("prompts/get".into())),
            (false,"params",object([])),(false,"requested_uri",Value::Text("context://forged".into())),
            (false,"timeout_ms",Value::Integer(0)),(false,"timeout_ms",Value::Integer(i64::MAX)),
            (false,"owner_fence",Value::Integer(0)),(false,"owner_fence",Value::Integer(i64::MAX)),
            (false,"number",Value::Integer(2)),(false,"deduplication",Value::Text("forged".into())),
            (false,"protocol_key",Value::Text("forged".into())),(false,"encoded_request",Value::Null),
            (false,"encoded_request",Value::Bytes(b"{}".to_vec())),(false,"dispatch_state",Value::Text("prepared".into())),
            (true,"delivery",Value::Text("not_sent".into())),(true,"delivery",Value::Text("unknown".into())),
            (true,"raw_digest",bad),(true,"raw",Value::Null),
            (true,"failure_code",Value::Text("E_POLICY".into())),
        ];
        for(response_field,field,value)in cases{
            let adapter=Arc::new(Held{descriptors:graph.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),graph.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),sent:AtomicUsize::new(0),release:AtomicBool::new(false)});
            let clock=Arc::new(Clock(AtomicI64::new(1000)));let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();let runtime=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock.clone()).with_mcp(adapter.clone()));runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();
            let config=ExecutionConfig{lease_ms:100,poll_interval:Duration::from_millis(1),..Default::default()};let engine=ExecutionEngine::launch(runtime.clone(),config.clone()).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"checkpoint",None).await.unwrap();
            let deadline=std::time::Instant::now()+Duration::from_secs(10);while adapter.sent.load(Ordering::SeqCst)==0{assert!(std::time::Instant::now()<deadline);actix::clock::sleep(Duration::from_millis(1)).await;}
            engine.shutdown().await.unwrap();clock.0.store(1200,Ordering::SeqCst);let lease=store.acquire(&run.run_id,"checkpoint".into(),1200,1300).await.unwrap();let attempt=store.records(&run.run_id,RecordFamily::Attempt,None,100).await.unwrap().pop().unwrap();
            let mut attempt_payload=change(&attempt.payload,[("dispatch_state",Value::Text("response_recorded".into()))]);
            let mut response=object([("raw",Value::Bytes(RESPONSE.to_vec())),("raw_digest",Value::Text(htlk_executable::digest::hash_bytes(RESPONSE).to_string())),("delivery",Value::Text("response_received".into())),("influence_labels",SecurityLabels::default().to_value(&Limits::default()).unwrap())]);
            if response_field{response=change(&response,[(field,value)]);}else{attempt_payload=change(&attempt_payload,[(field,value)]);}
            let mut writes=vec![update(&attempt,attempt_payload,true),RecordWrite{key:RecordKey{family:RecordFamily::Response,id:attempt.key.id.clone()},expected_revision:None,final_record:true,payload:response}];
            for reservation in store.records(&run.run_id,RecordFamily::Reservation,None,100).await.unwrap(){writes.push(update(&reservation,change(&reservation.payload,[("state",Value::Text("released".into()))]),true));}
            for usage in store.records(&run.run_id,RecordFamily::Usage,None,100).await.unwrap(){writes.push(update(&usage,change(&usage.payload,[("concurrency_reserved",Value::Integer(0))]),false));}
            store.commit(&lease,transaction(writes,1200)).await.unwrap();adapter.release.store(true,Ordering::SeqCst);clock.0.store(1400,Ordering::SeqCst);
            let recovered=Arc::new(Runtime::new(store.clone(),registry.clone(),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(clock));let engine=ExecutionEngine::launch(recovered.clone(),config).unwrap();
            let deadline=std::time::Instant::now()+Duration::from_secs(5);loop{let header=store.run_header(&run.run_id).await.unwrap();if header.status.is_terminal(){assert_eq!(header.status,RunStatus::Failed,"response={response_field}/{field}");break;}assert!(std::time::Instant::now()<deadline,"stalled response={response_field}/{field}");actix::clock::sleep(Duration::from_millis(2)).await;}
            assert_eq!(adapter.sent.load(Ordering::SeqCst),1);assert_eq!(store.records(&run.run_id,RecordFamily::Artifact,None,100).await.unwrap().len(),1,"corrupt checkpoint published an output");engine.shutdown().await.unwrap();store.close().await.unwrap();
        }
    });
}
