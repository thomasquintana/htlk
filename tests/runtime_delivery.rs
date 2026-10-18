//! Delivery evidence, rather than error spelling or replay safety, owns reservations.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    mcp::{
        McpAdapter, McpAttemptFuture, McpConnection, McpDelivery, McpFuture, McpOutcome, McpPolicy,
        McpRequest,
    },
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
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const SOURCE: &str = r#"ir_version="0.1" graph test.delivery{inputs={}outputs={value=json}nodes{args=eval(json,{query="question"}) lookup=call(mcp.tool("research","lookup")){retry{max_attempts=2 on=["MCP_TRANSPORT"] backoff_ms=[0]}}}edges{edge args{from=args.outputs.value to=lookup.inputs.arguments}edge value{from=lookup.outputs.value to=outputs.value}}}"#;
struct Policy {
    safe: bool,
}
struct Deduplicating {
    descriptors: BTreeMap<Digest, JsonDocument>,
    keys: Mutex<Vec<Option<String>>>,
    effects: AtomicUsize,
    mismatch: bool,
}
impl McpAdapter for Deduplicating {
    fn deduplication(
        &self,
        _: &McpBinding,
        _: htlk::runtime::identity::InvocationId,
        _: &Value,
        _: &Limits,
    ) -> Result<Option<htlk::runtime::mcp::McpDeduplication>, RuntimeFailure> {
        Ok(Some(if self.mismatch {
            htlk::runtime::mcp::McpDeduplication::Argument {
                name: "query".into(),
                value: Value::Text("not-authored".into()),
            }
        } else {
            htlk::runtime::mcp::McpDeduplication::Protocol
        }))
    }
    fn descriptor<'a>(&'a self, b: &'a McpBinding, _: &'a Limits) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&b.descriptor()].clone()) })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, request: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let mut keys = self.keys.lock().unwrap();
            let first = keys.is_empty();
            if request.idempotency_key.is_none() || !keys.contains(&request.idempotency_key) {
                self.effects.fetch_add(1, Ordering::SeqCst);
            }
            keys.push(request.idempotency_key);
            drop(keys);
            if first {
                Err(RuntimeFailure::new(FailureCode::McpTransport))
            } else {
                Ok(br#"{"content":[],"structuredContent":{"evidence_text":"complete"}}"#.to_vec())
            }
        })
    }
}
#[test]
fn verified_protocol_keys_are_invocation_stable_and_authored_arguments_cannot_be_inserted() {
    actix::System::new().block_on(async {
        for mismatch in [false,true]{
            let (compiler,registry)=support::native_compiler_with_defaults(htlk_executable::ExecutionLimits::new().with_timeout_ms(1000).unwrap().with_attempt_timeout_ms(100).unwrap().with_max_concurrency(2).unwrap());let graph=compiler.compile_source(SOURCE,&support::catalogs()).unwrap();let adapter=Arc::new(Deduplicating{descriptors:graph.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),graph.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),keys:Mutex::new(vec![]),effects:AtomicUsize::new(0),mismatch});let runtime=Arc::new(Runtime::new(SqliteStore::in_memory(StoreConfig::default()).await.unwrap(),Arc::new(registry),Arc::new(Policy{safe:false}),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)).with_mcp(adapter.clone()));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"start",Some(&htlk_executable::ExecutionLimits::new().with_max_mcp_calls(2).unwrap())).await.unwrap();let mut done=None;for _ in 0..2000{if let Ok(view)=runtime.inspect_run(&caller,&run.run_id).await&&view.header.status.is_terminal(){done=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}let view=done.expect("deduplication workflow stalled");let keys=adapter.keys.lock().unwrap().clone();
            if mismatch{assert_eq!(view.header.status,RunStatus::Failed);assert!(keys.is_empty());assert_eq!(adapter.effects.load(Ordering::SeqCst),0);}else{
                assert_eq!(view.header.status,RunStatus::Succeeded);let invocation=htlk::runtime::identity::InvocationId::new(view.header.root_scope,&"lookup".parse().unwrap(),&Limits::default()).unwrap();let expected=Some(invocation.effect_key(&Limits::default()).unwrap().to_string());assert_eq!(keys,vec![expected.clone(),expected]);assert_eq!(adapter.effects.load(Ordering::SeqCst),1);assert!(matches!(view.usage,Value::Map(m) if m.get("calls_spent")==Some(&Value::Integer(2))&&m.get("concurrency_reserved")==Some(&Value::Integer(1))));
            }
            engine.shutdown().await.unwrap();
        }
    })
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
        Ok(McpPolicy {
            replay_safe: self.safe,
            policy_version: "test-policy".into(),
            ..Default::default()
        })
    }
}
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
struct Adapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    connections: AtomicUsize,
    sent: AtomicUsize,
    connect_fault: bool,
    protocol_fault: bool,
}
impl McpAdapter for Adapter {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            let count = self.connections.fetch_add(1, Ordering::SeqCst);
            if self.connect_fault && count == 0 {
                return Err(RuntimeFailure::new(FailureCode::McpTransport));
            }
            Ok(self.descriptors[&binding.descriptor()].clone())
        })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, _: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.sent.fetch_add(1, Ordering::SeqCst);
            if self.protocol_fault {
                Err(RuntimeFailure::new(FailureCode::McpProtocol))
            } else {
                Ok(br#"{"content":[],"structuredContent":{"evidence_text":"complete"}}"#.to_vec())
            }
        })
    }
}
struct Correlated(Arc<Adapter>);
#[test]
fn descriptor_drift_is_known_unsent_and_never_repaired_by_an_authored_retry() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler();let source=SOURCE.replace("on=[\"MCP_TRANSPORT\"]","on=[\"MCP_DESCRIPTOR\"]");let graph=compiler.compile_source(&source,&support::catalogs()).unwrap();let descriptors=graph.canonical_document.fields().bindings.values().map(|binding|{let original=&graph.canonical_document.fields().documents[&binding.descriptor()];let Value::Map(fields)=original.value()else{panic!()};let fields=fields.iter().filter(|(k,_)|*k!="description").map(|(k,v)|(k.to_string(),v.clone())).chain(std::iter::once(("description".into(),Value::Text("drifted".into()))));let changed=JsonDocument::from_value(&Value::Map(htlk_executable::cbor::Map::try_from_entries(fields).unwrap()),&Limits::default()).unwrap();(binding.descriptor(),changed)}).collect();let adapter=Arc::new(Adapter{descriptors,connections:AtomicUsize::new(0),sent:AtomicUsize::new(0),connect_fault:false,protocol_fault:false});let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let runtime=Arc::new(Runtime::new(store.clone(),Arc::new(registry),Arc::new(Policy{safe:true}),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)).with_mcp(adapter.clone()));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();let mut done=None;for _ in 0..1000{if let Ok(view)=runtime.inspect_run(&caller,&run.run_id).await&&view.header.status.is_terminal(){done=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}assert_eq!(done.expect("drift stalled").header.status,RunStatus::Failed);assert_eq!(adapter.connections.load(Ordering::SeqCst),1);assert_eq!(adapter.sent.load(Ordering::SeqCst),0);assert_eq!(store.records(&run.run_id,RecordFamily::Attempt,None,100).await.unwrap().len(),1);let nodes=store.records(&run.run_id,RecordFamily::Invocation,None,100).await.unwrap();assert!(nodes.iter().any(|n|matches!(&n.payload,Value::Map(m) if matches!(m.get("failure"),Some(Value::Map(f)) if f.get("code")==Some(&Value::Text("MCP_DESCRIPTOR".into()))))));engine.shutdown().await.unwrap();
    })
}
struct KnownConnection<'a> {
    adapter: &'a Adapter,
    binding: &'a McpBinding,
    descriptor: JsonDocument,
}
impl McpConnection for KnownConnection<'_> {
    fn descriptor(&self) -> &JsonDocument {
        &self.descriptor
    }
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a> {
        Box::pin(async move {
            McpOutcome {
                delivery: McpDelivery::ResponseReceived,
                result: self.adapter.invoke(self.binding, request).await,
            }
        })
    }
}
impl McpAdapter for Correlated {
    fn descriptor<'a>(&'a self, b: &'a McpBinding, l: &'a Limits) -> McpFuture<'a, JsonDocument> {
        self.0.descriptor(b, l)
    }
    fn invoke<'a>(&'a self, b: &'a McpBinding, r: McpRequest) -> McpFuture<'a, Vec<u8>> {
        self.0.invoke(b, r)
    }
    fn connect<'a>(
        &'a self,
        b: &'a McpBinding,
        context: &'a HostContext,
        l: &'a Limits,
    ) -> McpFuture<'a, Box<dyn McpConnection + 'a>> {
        Box::pin(async move {
            assert_eq!(context.principal(), "alice");
            let descriptor = self.0.descriptor(b, l).await?;
            Ok(Box::new(KnownConnection {
                adapter: &self.0,
                binding: b,
                descriptor,
            }) as Box<dyn McpConnection + 'a>)
        })
    }
}
#[test]
fn known_unsent_can_retry_unsafe_operations_but_ambiguous_protocol_failures_retain_capacity() {
    actix::System::new().block_on(async {
        for (connect_fault,protocol_fault,correlated,safe,expected,reserved) in [
            (true,false,false,false,None,0),
            (false,true,false,false,Some(FailureCode::EffectUncertain),1),
            (false,true,false,true,Some(FailureCode::McpProtocol),1),
            (false,true,true,false,Some(FailureCode::McpProtocol),0),
        ]{
            let (compiler,registry)=support::native_compiler();let graph=compiler.compile_source(SOURCE,&support::catalogs()).unwrap();let adapter=Arc::new(Adapter{descriptors:graph.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),graph.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),connections:AtomicUsize::new(0),sent:AtomicUsize::new(0),connect_fault,protocol_fault});let dispatch:Arc<dyn McpAdapter>=if correlated{Arc::new(Correlated(adapter.clone()))}else{adapter.clone()};let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let runtime=Arc::new(Runtime::new(store.clone(),Arc::new(registry),Arc::new(Policy{safe}),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)).with_mcp(dispatch));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,graph.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();
            let mut done=None;for _ in 0..2000{if let Ok(view)=runtime.inspect_run(&caller,&run.run_id).await&&view.header.status.is_terminal(){done=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}let view=done.expect("delivery reducer stalled");assert_eq!(view.header.status,if expected.is_none(){RunStatus::Succeeded}else{RunStatus::Failed});assert_eq!(adapter.sent.load(Ordering::SeqCst),1);
            let Value::Map(usage)=view.usage else{panic!()};assert_eq!(usage.get("calls_spent"),Some(&Value::Integer(1)));assert_eq!(usage.get("concurrency_reserved"),Some(&Value::Integer(reserved)));
            let nodes=store.records(&run.run_id,RecordFamily::Invocation,None,100).await.unwrap();let node=nodes.iter().find(|n|matches!(&n.payload,Value::Map(m) if m.get("local_id")==Some(&Value::Text("lookup".into())))).unwrap();let Value::Map(node)=&node.payload else{panic!()};if let Some(code)=expected{assert_eq!(RuntimeFailure::from_value(node.get("failure").unwrap(),&Limits::default()).unwrap().code(),code);}
            let attempts=store.records(&run.run_id,RecordFamily::Attempt,None,100).await.unwrap();assert_eq!(attempts.len(),if connect_fault{2}else{1});
            if connect_fault{let responses=store.records(&run.run_id,RecordFamily::Response,None,100).await.unwrap();assert!(responses.iter().any(|r|matches!(&r.payload,Value::Map(m) if m.get("delivery")==Some(&Value::Text("not_sent".into())))));}
            assert!(attempts.iter().any(|a|matches!(&a.payload,Value::Map(m) if m.get("policy_version")==Some(&Value::Text("test-policy".into())))));
            engine.shutdown().await.unwrap();
        }
    })
}
struct Ignoring {
    descriptors: BTreeMap<Digest, JsonDocument>,
    started: AtomicBool,
    dropped: AtomicBool,
}
struct ScopedPolicy;
impl RuntimePolicy for ScopedPolicy {
    fn authorize(&self, _: &HostContext, action: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if let Authorization::DispatchMcp {
            method,
            parameters,
            encoded_parameters,
            ..
        } = action
        {
            assert_eq!(method, "tools/call");
            assert_eq!(
                JsonDocument::from_value(parameters, &Limits::default())
                    .unwrap()
                    .as_bytes(),
                encoded_parameters
            );
            assert!(
                matches!(parameters,Value::Map(m) if matches!(m.get("arguments"),Some(Value::Map(a)) if a.get("query")==Some(&Value::Text("question".into()))))
            );
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
    fn mcp_policy_for_request(
        &self,
        _: &HostContext,
        _: &McpBinding,
        parameters: &Value,
    ) -> Result<McpPolicy, RuntimeFailure> {
        assert!(
            matches!(parameters,Value::Map(m) if m.get("name")==Some(&Value::Text("lookup".into())))
        );
        Ok(McpPolicy::default())
    }
}
#[test]
fn exact_parameter_scope_is_authorized_before_opening_any_connection() {
    actix::System::new().block_on(async {
        let (compiler, registry) = support::native_compiler();
        let graph = compiler
            .compile_source(SOURCE, &support::catalogs())
            .unwrap();
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
            connections: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
            connect_fault: false,
            protocol_fault: false,
        });
        let runtime = Arc::new(
            Runtime::new(
                SqliteStore::in_memory(StoreConfig::default())
                    .await
                    .unwrap(),
                Arc::new(registry),
                Arc::new(ScopedPolicy),
                RuntimeConfig::default(),
            )
            .unwrap()
            .with_clock(Arc::new(Clock))
            .with_mcp(adapter.clone()),
        );
        let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
        runtime
            .register_graph(&caller, &graph.executable_bytes, &[])
            .await
            .unwrap();
        let engine = ExecutionEngine::launch(
            runtime.clone(),
            ExecutionConfig {
                poll_interval: Duration::from_millis(1),
                ..Default::default()
            },
        )
        .unwrap();
        let run = runtime
            .start_run(&caller, graph.fingerprint, &BTreeMap::new(), "start", None)
            .await
            .unwrap();
        let mut done = None;
        for _ in 0..1000 {
            if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await
                && view.header.status.is_terminal()
            {
                done = Some(view);
                break;
            }
            actix::clock::sleep(Duration::from_millis(2)).await;
        }
        let view = done.expect("denied dispatch stalled");
        assert_eq!(view.header.status, RunStatus::Failed);
        assert_eq!(adapter.connections.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.sent.load(Ordering::SeqCst), 0);
        assert!(
            matches!(view.usage,Value::Map(m) if m.get("calls_spent")==Some(&Value::Integer(0)))
        );
        engine.shutdown().await.unwrap();
    })
}
impl McpAdapter for Ignoring {
    fn descriptor<'a>(&'a self, b: &'a McpBinding, _: &'a Limits) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&b.descriptor()].clone()) })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, _: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            struct Finished<'a>(&'a AtomicBool);
            impl Drop for Finished<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst)
                }
            }
            let _finished = Finished(&self.dropped);
            self.started.store(true, Ordering::SeqCst);
            std::future::pending().await
        })
    }
}
#[test]
fn cancellation_and_shutdown_bound_an_ignoring_adapter_without_refunding_ambiguity() {
    actix::System::new().block_on(async {
        for shutdown in [false, true] {
            let (compiler, registry) = support::native_compiler_with_timeouts(10000, 5000);
            let graph = compiler
                .compile_source(SOURCE, &support::catalogs())
                .unwrap();
            let adapter = Arc::new(Ignoring {
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
                started: AtomicBool::new(false),
                dropped: AtomicBool::new(false),
            });
            let runtime = Arc::new(
                Runtime::new(
                    SqliteStore::in_memory(StoreConfig::default())
                        .await
                        .unwrap(),
                    Arc::new(registry),
                    Arc::new(Policy { safe: false }),
                    RuntimeConfig {
                        cancellation_grace_ms: 20,
                        ..Default::default()
                    },
                )
                .unwrap()
                .with_clock(Arc::new(Clock))
                .with_mcp(adapter.clone()),
            );
            let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
            runtime
                .register_graph(&caller, &graph.executable_bytes, &[])
                .await
                .unwrap();
            let engine = ExecutionEngine::launch(
                runtime.clone(),
                ExecutionConfig {
                    poll_interval: Duration::from_millis(1),
                    ..Default::default()
                },
            )
            .unwrap();
            let run = runtime
                .start_run(&caller, graph.fingerprint, &BTreeMap::new(), "start", None)
                .await
                .unwrap();
            for _ in 0..1000 {
                if adapter.started.load(Ordering::SeqCst) {
                    break;
                }
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            assert!(adapter.started.load(Ordering::SeqCst));
            let started = Instant::now();
            if shutdown {
                engine.shutdown().await.unwrap();
            } else {
                assert_eq!(
                    runtime
                        .cancel_run(&caller, &run.run_id, "stop", "stop")
                        .await
                        .unwrap()
                        .status,
                    RunStatus::Cancelled
                );
            }
            while !adapter.dropped.load(Ordering::SeqCst)
                && started.elapsed() < Duration::from_millis(500)
            {
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            assert!(
                adapter.dropped.load(Ordering::SeqCst),
                "adapter outlived the finite cancellation grace"
            );
            let view = runtime.inspect_run(&caller, &run.run_id).await.unwrap();
            assert_eq!(
                view.header.status,
                if shutdown {
                    RunStatus::Running
                } else {
                    RunStatus::Cancelled
                }
            );
            assert!(view.outputs.is_none());
            let Value::Map(usage) = view.usage else {
                panic!()
            };
            assert_eq!(usage.get("calls_spent"), Some(&Value::Integer(1)));
            assert_eq!(usage.get("concurrency_reserved"), Some(&Value::Integer(1)));
            if !shutdown {
                engine.shutdown().await.unwrap();
            }
        }
    })
}
