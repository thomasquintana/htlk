//! Worked-example execution using pinned scripted external results and actual actors.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::RuntimeFailure,
    mcp::{McpAdapter, McpFuture, McpPolicy, McpRequest},
    records::{InputValue, RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding, McpBindingKind,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{catalogs, native_compiler};
struct Policy {
    approval: bool,
    version: AtomicUsize,
}

struct FaultAdapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    replies: Mutex<VecDeque<Option<Vec<u8>>>>,
    requests: Mutex<Vec<Value>>,
    release: AtomicBool,
}
impl McpAdapter for FaultAdapter {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&binding.descriptor()].clone()) })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, request: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.params);
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra dispatch");
            match reply {
                Some(raw) => Ok(raw),
                None => {
                    while !self.release.load(Ordering::SeqCst) {
                        actix::clock::sleep(Duration::from_millis(1)).await;
                    }
                    Ok(br#"{"content":[],"structuredContent":{"evidence_text":"late"}}"#.to_vec())
                }
            }
        })
    }
}
struct MovingClock(AtomicI64);
impl CoordinatorClock for MovingClock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct UnsafePolicy;
impl RuntimePolicy for UnsafePolicy {
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
#[test]
fn owner_takeover_never_replays_unsafe_possibly_sent_work_or_accepts_late_success() {
    actix::System::new().block_on(async {
        let (compiler, registry) = native_compiler();
        let result = compiler.compile_source(r#"
            ir_version="0.1" graph test.recovery {
                inputs={} outputs={value=json}
                nodes {args=eval(json,{query="question"}) lookup=call(mcp.tool("research","lookup"))}
                edges {
                    edge args {from=args.outputs.value to=lookup.inputs.arguments}
                    edge result {from=lookup.outputs.value to=outputs.value}
                }
            }
        "#, &catalogs()).unwrap();
        let adapter = Arc::new(FaultAdapter {
            descriptors: result.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),result.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),
            replies: Mutex::new(VecDeque::from([None])), requests: Mutex::new(vec![]), release: AtomicBool::new(false),
        });
        let clock = Arc::new(MovingClock(AtomicI64::new(1000)));
        let store = SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
        let runtime = Arc::new(Runtime::new(store.clone(),Arc::new(registry),Arc::new(UnsafePolicy),RuntimeConfig::default()).unwrap().with_clock(clock.clone()).with_mcp(adapter.clone()));
        let caller = HostContext::new("alice".into(),&Limits::default()).unwrap();
        runtime.register_graph(&caller,&result.executable_bytes,&[]).await.unwrap();
        let config = ExecutionConfig{lease_ms:100,poll_interval:Duration::from_millis(1),..Default::default()};
        let engine = ExecutionEngine::launch(runtime.clone(),config.clone()).unwrap();
        let run = runtime.start_run(&caller,result.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();
        for _ in 0..1000 {
            if !adapter.requests.lock().unwrap().is_empty(){break}
            actix::clock::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(adapter.requests.lock().unwrap().len(),1);
        engine.shutdown().await.unwrap();
        clock.0.store(1200,Ordering::SeqCst);
        let recovered = ExecutionEngine::launch(runtime.clone(),config).unwrap();
        assert_eq!(terminal(&runtime,&caller,&run.run_id).await.header.status,RunStatus::Failed);
        let nodes = store.records(&run.run_id,htlk::runtime::store::RecordFamily::Invocation,None,100).await.unwrap();
        assert!(nodes.iter().any(|n| matches!(&n.payload,Value::Map(m) if matches!(m.get("failure"),Some(Value::Map(f)) if f.get("code")==Some(&Value::Text("E_EFFECT_UNCERTAIN".into()))))));
        adapter.release.store(true,Ordering::SeqCst);
        actix::clock::sleep(Duration::from_millis(30)).await;
        assert_eq!(adapter.requests.lock().unwrap().len(),1);
        assert_eq!(runtime.inspect_run(&caller,&run.run_id).await.unwrap().header.status,RunStatus::Failed);
        let usage = store.record(&run.run_id,&htlk::runtime::store::RecordKey{family:htlk::runtime::store::RecordFamily::Usage,id:"run".into()}).await.unwrap();
        assert!(matches!(usage.payload,Value::Map(m) if m.get("calls_spent")==Some(&Value::Integer(1))&&m.get("concurrency_reserved")==Some(&Value::Integer(1))));
        recovered.shutdown().await.unwrap();
    })
}
#[test]
fn malformed_result_retries_only_with_explicit_safe_policy_and_spends_each_call() {
    actix::System::new().block_on(async {
        for (max_calls, expected_calls, expected_status) in [
            (2, 2, RunStatus::Succeeded),
            (1, 1, RunStatus::Failed),
            (0, 0, RunStatus::Failed),
        ] {
            let (compiler, registry) = native_compiler();
            let result = compiler
                .compile_source(
                    r#"
                ir_version="0.1" graph test.retry {
                    inputs={} outputs={value=json}
                    nodes {
                        args=eval(json,{query="question"})
                        lookup=call(mcp.tool("research","lookup")) {
                            retry {max_attempts=2 on=["E_OUTPUT_SCHEMA"] backoff_ms=[0]}
                        }
                    }
                    edges {
                        edge args {from=args.outputs.value to=lookup.inputs.arguments}
                        edge result {from=lookup.outputs.value to=outputs.value}
                    }
                }
            "#,
                    &catalogs(),
                )
                .unwrap();
            let adapter = Arc::new(FaultAdapter {
                descriptors: result
                    .canonical_document
                    .fields()
                    .bindings
                    .values()
                    .map(|b| {
                        (
                            b.descriptor(),
                            result.canonical_document.fields().documents[&b.descriptor()].clone(),
                        )
                    })
                    .collect(),
                replies: Mutex::new(VecDeque::from([
                    Some(br#"{"content":[],"structuredContent":{}}"#.to_vec()),
                    Some(
                        br#"{"content":[],"structuredContent":{"evidence_text":"accepted"}}"#
                            .to_vec(),
                    ),
                ])),
                requests: Mutex::new(vec![]),
                release: AtomicBool::new(false),
            });
            let store = SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap();
            let runtime = Arc::new(
                Runtime::new(
                    store.clone(),
                    Arc::new(registry),
                    Arc::new(Policy {
                        approval: false,
                        version: AtomicUsize::new(1),
                    }),
                    RuntimeConfig::default(),
                )
                .unwrap()
                .with_clock(Arc::new(Clock))
                .with_mcp(adapter.clone()),
            );
            let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
            runtime
                .register_graph(&caller, &result.executable_bytes, &[])
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
                .start_run(
                    &caller,
                    result.fingerprint,
                    &BTreeMap::new(),
                    "start",
                    Some(
                        &htlk_executable::ExecutionLimits::new()
                            .with_max_mcp_calls(max_calls)
                            .unwrap(),
                    ),
                )
                .await
                .unwrap();
            let view = terminal(&runtime, &caller, &run.run_id).await;
            assert_eq!(view.header.status, expected_status);
            let requests = adapter.requests.lock().unwrap().clone();
            assert_eq!(requests.len(), expected_calls);
            assert!(requests.windows(2).all(|w| w[0] == w[1]));
            let usage = store
                .record(
                    &run.run_id,
                    &htlk::runtime::store::RecordKey {
                        family: htlk::runtime::store::RecordFamily::Usage,
                        id: "run".into(),
                    },
                )
                .await
                .unwrap();
            let Value::Map(usage) = usage.payload else {
                panic!()
            };
            assert_eq!(
                usage.get("calls_spent"),
                Some(&Value::Integer(expected_calls as i64))
            );
            assert_eq!(usage.get("concurrency_reserved"), Some(&Value::Integer(0)));
            engine.shutdown().await.unwrap();
        }
    })
}
async fn terminal(
    runtime: &Runtime,
    caller: &HostContext,
    run: &htlk::runtime::identity::RunId,
) -> htlk::runtime::execution::RunView {
    for _ in 0..2000 {
        if let Ok(view) = runtime.inspect_run(caller, run).await
            && view.header.status.is_terminal()
        {
            return view;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("run did not settle")
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if matches!(action, Authorization::Approve { .. }) && context.principal() != "approver" {
            return Err(RuntimeFailure::new(
                htlk::runtime::failure::FailureCode::Policy,
            ));
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
    fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            replay_safe: true,
            approval_authority: if self.approval {
                Some("exact-tool".into())
            } else {
                None
            },
            policy_version: self.version.load(Ordering::SeqCst).to_string(),
            ..Default::default()
        })
    }
}
struct Clock;
#[test]
fn approval_generations_are_authenticated_and_old_allow_cannot_authorize_new_request() {
    actix::System::new().block_on(async {
        let (compiler, registry) = native_compiler();
        let result = compiler
            .compile_source(
                r#"
            ir_version="0.1"
            graph test.approval {
                inputs={} outputs={value=json}
                nodes {
                    args=eval(json,{query="question"})
                    lookup=call(mcp.tool("research","lookup"))
                }
                edges {
                    edge args {from=args.outputs.value to=lookup.inputs.arguments}
                    edge result {from=lookup.outputs.value to=outputs.value}
                }
            }
        "#,
                &catalogs(),
            )
            .unwrap();
        let descriptors = result
            .canonical_document
            .fields()
            .bindings
            .values()
            .map(|b| {
                (
                    b.descriptor(),
                    result.canonical_document.fields().documents[&b.descriptor()].clone(),
                )
            })
            .collect();
        let script = Arc::new(Script {
            descriptors,
            calls: AtomicUsize::new(0),
            reviews: AtomicUsize::new(0),
            accept_at: 1,
            pause_discovery: AtomicBool::new(false),
            waiting_discovery: AtomicBool::new(false),
        });
        let policy = Arc::new(Policy {
            approval: true,
            version: AtomicUsize::new(1),
        });
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let runtime = Arc::new(
            Runtime::new(
                store,
                Arc::new(registry),
                policy.clone(),
                RuntimeConfig::default(),
            )
            .unwrap()
            .with_clock(Arc::new(Clock))
            .with_mcp(script.clone()),
        );
        let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
        let approver = HostContext::new("approver".into(), &Limits::default()).unwrap();
        runtime
            .register_graph(&caller, &result.executable_bytes, &[])
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
            .start_run(&caller, result.fingerprint, &BTreeMap::new(), "start", None)
            .await
            .unwrap();
        let first = pending_approval(&runtime, &caller, &run.run_id, None).await;
        let digest = first.request_digest.unwrap();
        assert!(
            runtime
                .submit_reply(&caller, first.wait_id, Value::Bool(true), "not-approval")
                .await
                .is_err()
        );
        assert!(
            runtime
                .submit_approval(&caller, first.wait_id, digest, true, "unauthorized")
                .await
                .is_err()
        );
        script.pause_discovery.store(true, Ordering::SeqCst);
        for _ in 0..1000 {
            if script.waiting_discovery.load(Ordering::SeqCst) {
                break;
            }
            actix::clock::sleep(Duration::from_millis(1)).await;
        }
        assert!(script.waiting_discovery.load(Ordering::SeqCst));
        runtime
            .submit_approval(&approver, first.wait_id, digest, true, "old")
            .await
            .unwrap();
        policy.version.store(2, Ordering::SeqCst);
        script.pause_discovery.store(false, Ordering::SeqCst);
        let second = pending_approval(&runtime, &caller, &run.run_id, Some(first.wait_id)).await;
        assert_ne!(second.request_digest, first.request_digest);
        runtime
            .submit_approval(&approver, first.wait_id, digest, true, "old")
            .await
            .unwrap();
        assert_eq!(script.calls.load(Ordering::SeqCst), 0);
        runtime
            .submit_approval(
                &approver,
                second.wait_id,
                second.request_digest.unwrap(),
                true,
                "current",
            )
            .await
            .unwrap();
        for _ in 0..1000 {
            if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await
                && view.header.status.is_terminal()
            {
                break;
            }
            actix::clock::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            runtime
                .inspect_run(&caller, &run.run_id)
                .await
                .unwrap()
                .header
                .status,
            RunStatus::Succeeded
        );
        assert_eq!(script.calls.load(Ordering::SeqCst), 1);
        let denied = runtime
            .start_run(
                &caller,
                result.fingerprint,
                &BTreeMap::new(),
                "denied",
                None,
            )
            .await
            .unwrap();
        let request = pending_approval(&runtime, &caller, &denied.run_id, None).await;
        assert!(
            runtime
                .submit_approval(&approver, request.wait_id, digest, true, "wrong-request")
                .await
                .is_err()
        );
        runtime
            .submit_approval(
                &approver,
                request.wait_id,
                request.request_digest.unwrap(),
                false,
                "deny",
            )
            .await
            .unwrap();
        assert_eq!(
            terminal(&runtime, &caller, &denied.run_id)
                .await
                .header
                .status,
            RunStatus::Failed
        );
        assert_eq!(script.calls.load(Ordering::SeqCst), 1);
        assert!(
            runtime
                .submit_approval(
                    &approver,
                    request.wait_id,
                    request.request_digest.unwrap(),
                    true,
                    "deny"
                )
                .await
                .is_err()
        );
        engine.shutdown().await.unwrap();
    })
}
async fn pending_approval(
    runtime: &Runtime,
    caller: &HostContext,
    run: &htlk::runtime::identity::RunId,
    except: Option<htlk::runtime::identity::WaitId>,
) -> htlk::runtime::execution::WaitView {
    for _ in 0..1000 {
        if let Ok(view) = runtime.inspect_run(caller, run).await
            && let Some(wait) = view
                .waits
                .into_iter()
                .find(|w| w.purpose == "approval" && Some(w.wait_id) != except && w.state == "open")
        {
            return wait;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("approval did not open")
}
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
struct Script {
    descriptors: BTreeMap<Digest, JsonDocument>,
    calls: AtomicUsize,
    reviews: AtomicUsize,
    accept_at: usize,
    pause_discovery: AtomicBool,
    waiting_discovery: AtomicBool,
}
impl McpAdapter for Script {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            while self.pause_discovery.load(Ordering::SeqCst) {
                self.waiting_discovery.store(true, Ordering::SeqCst);
                actix::clock::sleep(Duration::from_millis(1)).await;
            }
            Ok(self.descriptors[&binding.descriptor()].clone())
        })
    }
    fn invoke<'a>(&'a self, binding: &'a McpBinding, _: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let McpBindingKind::Tool { name, .. } = binding.kind() else {
                panic!()
            };
            let response=match name.as_str(){"lookup"=>r#"{"content":[],"structuredContent":{"evidence_text":"Evidence with sources and uncertainty."}}"#.to_string(),"revise_brief"=>r#"{"content":[],"structuredContent":{"draft":"A bounded evidence-based pilot."}}"#.to_string(),"review_brief"=>{let n=self.reviews.fetch_add(1,Ordering::SeqCst)+1;format!(r#"{{"content":[],"structuredContent":{{"accepted":{},"feedback":"Improve the draft."}}}}"#,n>=self.accept_at)},_=>panic!()};
            Ok(response.into_bytes())
        })
    }
}
#[test]
fn decision_brief_accepts_at_each_bound_and_falls_back_after_loop_limit() {
    actix::System::new().block_on(async {
        for accept_at in [1, 2, 3, 4] {
            let (compiler, registry) = native_compiler();
            let result = compiler
                .compile_ir(
                    include_str!("../specs/examples/decision_brief.htlk"),
                    &catalogs(),
                )
                .unwrap();
            let descriptors = result
                .canonical_document
                .fields()
                .bindings
                .values()
                .map(|binding| {
                    (
                        binding.descriptor(),
                        result.canonical_document.fields().documents[&binding.descriptor()].clone(),
                    )
                })
                .collect();
            let script = Arc::new(Script {
                descriptors,
                calls: AtomicUsize::new(0),
                reviews: AtomicUsize::new(0),
                accept_at,
                pause_discovery: AtomicBool::new(false),
                waiting_discovery: AtomicBool::new(false),
            });
            let store = SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap();
            let runtime = Arc::new(
                Runtime::new(
                    store.clone(),
                    Arc::new(registry),
                    Arc::new(Policy {
                        approval: false,
                        version: AtomicUsize::new(1),
                    }),
                    RuntimeConfig::default(),
                )
                .unwrap()
                .with_clock(Arc::new(Clock))
                .with_mcp(script.clone()),
            );
            let context = HostContext::new("alice".into(), &Limits::default()).unwrap();
            runtime
                .register_graph(&context, &result.executable_bytes, &[])
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
                .start_run(
                    &context,
                    result.fingerprint,
                    &BTreeMap::from([(
                        "question".parse().unwrap(),
                        InputValue::Inline(Value::Text("Should we run a pilot?".into())),
                    )]),
                    "start",
                    None,
                )
                .await
                .unwrap();
            let mut done = None;
            for _ in 0..4000 {
                let view = runtime.inspect_run(&context, &run.run_id).await;
                if let Ok(view) = view
                    && view.header.status.is_terminal()
                {
                    done = Some(view);
                    break;
                }
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            let view = done.expect("worked example stalled");
            assert_eq!(view.header.status, RunStatus::Succeeded);
            if script.calls.load(Ordering::SeqCst) != 2 + 2 * accept_at.min(3) {
                let nodes = store
                    .records(
                        &run.run_id,
                        htlk::runtime::store::RecordFamily::Invocation,
                        None,
                        100,
                    )
                    .await
                    .unwrap();
                for node in nodes {
                    if let Value::Map(data) = node.payload
                        && data.get("status") == Some(&Value::Text("failed".into()))
                    {
                        eprintln!(
                            "failed {:?}: {:?}",
                            data.get("local_id"),
                            data.get("failure")
                        );
                    }
                }
            }
            assert_eq!(
                script.calls.load(Ordering::SeqCst),
                2 + 2 * accept_at.min(3)
            );
            let Value::Map(outputs) = view.outputs.unwrap() else {
                panic!()
            };
            let Some(Value::Map(entry)) = outputs.get("result") else {
                panic!()
            };
            let Some(Value::Text(id)) = entry.get("artifact_id") else {
                panic!()
            };
            let value = runtime
                .read_artifact(&context, id.parse().unwrap())
                .await
                .unwrap();
            let Value::Map(value) = value.value else {
                panic!()
            };
            assert_eq!(
                value.get("status"),
                Some(&Value::Text(
                    if accept_at <= 3 {
                        "accepted"
                    } else {
                        "needs_attention"
                    }
                    .into()
                ))
            );
            engine.shutdown().await.unwrap();
        }
    })
}
