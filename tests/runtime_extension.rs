//! Live extension uses real compiler joins and mounts existing scope instances.
mod support;
use htlk::{
    compiler::{JoinEdge, JoinEndpoint, JoinInput, JoinOperand, JoinOutput, JoinSpec},
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        clock::CoordinatorClock,
        execution::{CloseWait, ExecutionConfig, ExecutionEngine, ExtensionRequest},
        failure::{FailureCode, RuntimeFailure},
        mcp::{McpAdapter, McpFuture, McpRequest},
        records::{InputValue, RunStatus, SecurityLabels},
        store::{RecordFamily, RecordKey, SqliteStore, StoreConfig},
    },
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
const PLANNING: &str = r#"
    ir_version="0.1" graph test.planning {
        inputs={context=string, optional_context=optional(string)} outputs={context=string}
        nodes {
            request=eval(json,{context=inputs.context}){inputs={context=string}}
            window=wait(boolean){topic="extend" timeout_ms=900}
        }
        edges {
            edge context {from=inputs.context to=request.inputs.context}
            edge request {from=request.outputs.value to=window.inputs.request}
            edge output {from=inputs.context to=outputs.context}
        }
        postconditions=status(@window)=="succeeded"
    }
"#;
const ADDED: &str = r#"
    ir_version="0.1" graph test.added {
        inputs={context=string} outputs={answer=string}
        nodes{answer=eval(string,inputs.context){inputs={context=string}}}
        edges{
            edge input {from=inputs.context to=answer.inputs.context}
            edge output {from=answer.outputs.value to=outputs.answer}
        }
    }
"#;
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
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
fn endpoint(graph: JoinOperand, port: &str) -> JoinEndpoint {
    JoinEndpoint {
        graph,
        port: port.into(),
    }
}
async fn open_wait(
    runtime: &Runtime,
    caller: &HostContext,
    run: &htlk::runtime::identity::RunId,
) -> htlk::runtime::identity::WaitId {
    for _ in 0..2000 {
        if let Ok(view) = runtime.inspect_run(caller, run).await
            && let Some(wait) = view.waits.iter().find(|w| w.state == "open")
        {
            return wait.wait_id;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("wait did not open")
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
#[test]
fn repeated_joins_preserve_prior_history_and_stale_requests_cannot_close_a_window() {
    actix::System::new().block_on(async {
        let (compiler, registry) = support::native_compiler();
        let catalogs = support::catalogs();
        let old = compiler.compile_source(PLANNING, &catalogs).unwrap();
        let added = compiler.compile_source(ADDED, &catalogs).unwrap();
        let mut spec = JoinSpec {
            id: "test.twice".into(),
            left_alias: "first".into(),
            right_alias: "second".into(),
            edges: vec![JoinEdge {
                id: "data".into(),
                from: endpoint(JoinOperand::Left, "context"),
                to: endpoint(JoinOperand::Right, "context"),
            }],
            inputs: vec![
                JoinInput {
                    name: "context".into(),
                    to: vec![endpoint(JoinOperand::Left, "context")],
                },
                JoinInput {
                    name: "optional_context".into(),
                    to: vec![endpoint(JoinOperand::Left, "optional_context")],
                },
            ],
            outputs: vec![JoinOutput {
                name: "context".into(),
                from: endpoint(JoinOperand::Right, "context"),
            }],
        };
        let middle = compiler
            .join_graphs(
                &old.canonical_document,
                &old.canonical_document,
                &spec,
                &catalogs,
            )
            .unwrap();
        spec.id = "test.thrice".into();
        spec.outputs = vec![JoinOutput {
            name: "answer".into(),
            from: endpoint(JoinOperand::Right, "answer"),
        }];
        let final_graph = compiler
            .join_graphs(
                &middle.canonical_document,
                &added.canonical_document,
                &spec,
                &catalogs,
            )
            .unwrap();
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
        let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
        for graph in [&old, &middle, &final_graph] {
            runtime
                .register_graph(&caller, &graph.executable_bytes, &[])
                .await
                .unwrap();
        }
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
                old.fingerprint,
                &BTreeMap::from([(
                    "context".parse().unwrap(),
                    InputValue::Inline(Value::Text("same".into())),
                )]),
                "start",
                None,
            )
            .await
            .unwrap();
        let wait = open_wait(&runtime, &caller, &run.run_id).await;
        let first = ExtensionRequest {
            expected_fingerprint: old.fingerprint,
            composed_fingerprint: middle.fingerprint,
            inputs: BTreeMap::new(),
            close_wait: Some(CloseWait {
                wait_id: wait,
                value: Value::Bool(true),
                idempotency_key: "close1".into(),
            }),
        };
        let receipt = runtime
            .extend_run(&caller, &run.run_id, &first, "extend1")
            .await
            .unwrap();
        let next = open_wait(&runtime, &caller, &run.run_id).await;
        assert_ne!(next, wait);
        let before = store.run_header(&run.run_id).await.unwrap();
        let mut second = ExtensionRequest {
            expected_fingerprint: old.fingerprint,
            composed_fingerprint: final_graph.fingerprint,
            inputs: BTreeMap::new(),
            close_wait: Some(CloseWait {
                wait_id: next,
                value: Value::Bool(true),
                idempotency_key: "close2".into(),
            }),
        };
        assert_eq!(
            runtime
                .extend_run(&caller, &run.run_id, &second, "stale")
                .await
                .unwrap_err()
                .code(),
            FailureCode::ExtensionConflict
        );
        assert_eq!(open_wait(&runtime, &caller, &run.run_id).await, next);
        assert_eq!(
            runtime
                .extend_run(&caller, &run.run_id, &first, "extend1")
                .await
                .unwrap(),
            receipt
        );
        second.expected_fingerprint = middle.fingerprint;
        assert_eq!(
            runtime
                .extend_run(&caller, &run.run_id, &second, "extend2")
                .await
                .unwrap()
                .extension_number,
            2
        );
        let view = terminal(&runtime, &caller, &run.run_id).await;
        assert_eq!(view.header.status, RunStatus::Succeeded);
        assert_ne!(before.root_scope, view.header.root_scope);
        let extensions = store
            .records(&run.run_id, RecordFamily::Extension, None, 100)
            .await
            .unwrap();
        assert_eq!(extensions.len(), 2);
        assert!(extensions.iter().all(|r| r.final_record));
        engine.shutdown().await.unwrap();
    })
}
struct HeldAdapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    sent: AtomicUsize,
    release: AtomicBool,
}
impl McpAdapter for HeldAdapter {
    fn descriptor<'a>(&'a self, b: &'a McpBinding, _: &'a Limits) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&b.descriptor()].clone()) })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, _: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.sent.fetch_add(1, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                actix::clock::sleep(Duration::from_millis(1)).await;
            }
            Ok(br#"{"content":[],"structuredContent":{"evidence_text":"accepted"}}"#.to_vec())
        })
    }
}
#[test]
fn inflight_dispatch_keeps_identity_and_new_ancestry_adopts_and_releases_its_reservations() {
    actix::System::new().block_on(async {
        let (compiler,registry)=support::native_compiler_with_timeouts(10000,5000);let catalogs=support::catalogs();
        let old=compiler.compile_source(r#"ir_version="0.1" graph test.inflight{inputs={}outputs={value=json}nodes{args=eval(json,{query="question"}) lookup=call(mcp.tool("research","lookup"))}edges{edge args{from=args.outputs.value to=lookup.inputs.arguments}edge result{from=lookup.outputs.value to=outputs.value}}}"#,&catalogs).unwrap();
        let added=compiler.compile_source(r#"ir_version="0.1" graph test.append{inputs={value=json}outputs={value=json}nodes{copy=eval(json,inputs.value){inputs={value=json}}}edges{edge input{from=inputs.value to=copy.inputs.value}edge output{from=copy.outputs.value to=outputs.value}}}"#,&catalogs).unwrap();
        let spec=JoinSpec{id:"test.inflight_extended".into(),left_alias:"preserved".into(),right_alias:"added".into(),edges:vec![JoinEdge{id:"data".into(),from:endpoint(JoinOperand::Left,"value"),to:endpoint(JoinOperand::Right,"value")}],inputs:vec![],outputs:vec![JoinOutput{name:"value".into(),from:endpoint(JoinOperand::Right,"value")}]};
        let composed=compiler.join_graphs(&old.canonical_document,&added.canonical_document,&spec,&catalogs).unwrap();
        let adapter=Arc::new(HeldAdapter{descriptors:old.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),old.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),sent:AtomicUsize::new(0),release:AtomicBool::new(false)});
        let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let runtime=Arc::new(Runtime::new(store.clone(),Arc::new(registry),Arc::new(Policy),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)).with_mcp(adapter.clone()));let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();
        for graph in [&old,&composed]{runtime.register_graph(&caller,&graph.executable_bytes,&[]).await.unwrap();}
        let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,old.fingerprint,&BTreeMap::new(),"start",Some(&htlk_executable::ExecutionLimits::new().with_max_mcp_calls(1).unwrap())).await.unwrap();
        for _ in 0..2000{if adapter.sent.load(Ordering::SeqCst)==1{break}actix::clock::sleep(Duration::from_millis(1)).await;}
        assert_eq!(adapter.sent.load(Ordering::SeqCst),1);
        let attempt=store.records(&run.run_id,RecordFamily::Attempt,None,100).await.unwrap().pop().unwrap();
        let request=ExtensionRequest{expected_fingerprint:old.fingerprint,composed_fingerprint:composed.fingerprint,inputs:BTreeMap::new(),close_wait:None};runtime.extend_run(&caller,&run.run_id,&request,"extend").await.unwrap();
        let reservations=store.records(&run.run_id,RecordFamily::Usage,None,100).await.unwrap();assert_eq!(reservations.len(),5);
        for usage in reservations{assert!(matches!(usage.payload,Value::Map(m) if m.get("calls_spent")==Some(&Value::Integer(1))&&m.get("concurrency_reserved")==Some(&Value::Integer(1))));}
        adapter.release.store(true,Ordering::SeqCst);assert_eq!(terminal(&runtime,&caller,&run.run_id).await.header.status,RunStatus::Succeeded);assert_eq!(adapter.sent.load(Ordering::SeqCst),1);
        let attempts=store.records(&run.run_id,RecordFamily::Attempt,None,100).await.unwrap();assert_eq!(attempts.len(),1);assert_eq!(attempts[0].key,attempt.key);
        for usage in store.records(&run.run_id,RecordFamily::Usage,None,100).await.unwrap(){assert!(matches!(usage.payload,Value::Map(m) if m.get("calls_spent")==Some(&Value::Integer(1))&&m.get("concurrency_reserved")==Some(&Value::Integer(0))));}
        engine.shutdown().await.unwrap();
    })
}
#[test]
fn compiler_join_mounts_either_operand_and_closes_planning_wait_atomically() {
    actix::System::new().block_on(async {
        for preserved_left in [true, false] {
            let (compiler, registry) = support::native_compiler();
            let catalogs = support::catalogs();
            let old = compiler.compile_source(PLANNING, &catalogs).unwrap();
            let added = compiler.compile_source(ADDED, &catalogs).unwrap();
            let (old_side, new_side) = if preserved_left {
                (JoinOperand::Left, JoinOperand::Right)
            } else {
                (JoinOperand::Right, JoinOperand::Left)
            };
            let spec = JoinSpec {
                id: "test.extended".into(),
                left_alias: "arbitrary_x".into(),
                right_alias: "arbitrary_y".into(),
                edges: vec![JoinEdge {
                    id: "context".into(),
                    from: endpoint(old_side, "context"),
                    to: endpoint(new_side, "context"),
                }],
                inputs: vec![
                    JoinInput {
                        name: "renamed".into(),
                        to: vec![endpoint(old_side, "context")],
                    },
                    JoinInput {
                        name: "optional".into(),
                        to: vec![endpoint(old_side, "optional_context")],
                    },
                ],
                outputs: vec![JoinOutput {
                    name: "answer".into(),
                    from: endpoint(new_side, "answer"),
                }],
            };
            let (left, right) = if preserved_left {
                (&old.canonical_document, &added.canonical_document)
            } else {
                (&added.canonical_document, &old.canonical_document)
            };
            let composed = compiler.join_graphs(left, right, &spec, &catalogs).unwrap();
            let mut nonadditive = composed.canonical_document.fields().clone();
            let mut definition = nonadditive.scopes[&nonadditive.root_scope].fields().clone();
            definition.preconditions = htlk_executable::Expression::literal(
                htlk_executable::ScalarLiteral::Boolean(false),
            );
            let definition = htlk_executable::Scope::new(definition, &Limits::default()).unwrap();
            nonadditive.scopes.remove(&nonadditive.root_scope);
            nonadditive.root_scope = definition.digest(&Limits::default()).unwrap();
            nonadditive
                .scopes
                .insert(nonadditive.root_scope, definition);
            let nonadditive = compiler
                .compile_document(
                    htlk_executable::CanonicalDocument::new(nonadditive, &Limits::default())
                        .unwrap(),
                    &catalogs,
                )
                .unwrap();
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
            let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
            runtime
                .register_graph(&caller, &old.executable_bytes, &[])
                .await
                .unwrap();
            runtime
                .register_graph(&caller, &composed.executable_bytes, &[])
                .await
                .unwrap();
            runtime
                .register_graph(&caller, &nonadditive.executable_bytes, &[])
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
                    old.fingerprint,
                    &BTreeMap::from([(
                        "context".parse().unwrap(),
                        InputValue::Inline(Value::Text("preserved".into())),
                    )]),
                    "start",
                    None,
                )
                .await
                .unwrap();
            let mut waiting = None;
            for _ in 0..2000 {
                if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await
                    && let Some(wait) = view.waits.iter().find(|w| w.state == "open")
                {
                    waiting = Some((view.header.clone(), wait.wait_id));
                    break;
                }
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            let (initial, wait) = waiting.expect("planning window did not open");
            let old_scope = store
                .record(
                    &run.run_id,
                    &RecordKey {
                        family: RecordFamily::Scope,
                        id: initial.root_scope.to_string(),
                    },
                )
                .await
                .unwrap();
            let old_nodes = store
                .records(&run.run_id, RecordFamily::Invocation, None, 100)
                .await
                .unwrap();
            let mut request = ExtensionRequest {
                expected_fingerprint: old.fingerprint,
                composed_fingerprint: composed.fingerprint,
                inputs: BTreeMap::new(),
                close_wait: Some(CloseWait {
                    wait_id: wait,
                    value: Value::Bool(true),
                    idempotency_key: "close".into(),
                }),
            };
            request.composed_fingerprint = nonadditive.fingerprint;
            assert_eq!(
                runtime
                    .extend_run(&caller, &run.run_id, &request, "nonadditive")
                    .await
                    .unwrap_err()
                    .code(),
                FailureCode::Extension
            );
            request.composed_fingerprint = composed.fingerprint;
            request.inputs.insert(
                "renamed".parse().unwrap(),
                InputValue::Inline(Value::Text("changed".into())),
            );
            assert_eq!(
                runtime
                    .extend_run(&caller, &run.run_id, &request, "invalid-input")
                    .await
                    .unwrap_err()
                    .code(),
                FailureCode::Extension
            );
            request.inputs.clear();
            request.inputs.insert(
                "optional".parse().unwrap(),
                InputValue::Inline(Value::Text("was absent".into())),
            );
            assert_eq!(
                runtime
                    .extend_run(&caller, &run.run_id, &request, "changed-presence")
                    .await
                    .unwrap_err()
                    .code(),
                FailureCode::Extension
            );
            request.inputs.clear();
            request.close_wait.as_mut().unwrap().value = Value::Text("wrong-type".into());
            assert!(
                runtime
                    .extend_run(&caller, &run.run_id, &request, "invalid-reply")
                    .await
                    .is_err()
            );
            assert_eq!(
                store.run_header(&run.run_id).await.unwrap().fingerprint,
                old.fingerprint
            );
            let view = runtime.inspect_run(&caller, &run.run_id).await.unwrap();
            assert_eq!(
                view.waits.iter().find(|w| w.wait_id == wait).unwrap().state,
                "open"
            );
            request.close_wait.as_mut().unwrap().value = Value::Bool(true);
            if preserved_left {
                request.inputs.insert(
                    "renamed".parse().unwrap(),
                    InputValue::Inline(Value::Text("preserved".into())),
                );
            }
            let receipt = runtime
                .extend_run(&caller, &run.run_id, &request, "install")
                .await
                .unwrap();
            assert_eq!(receipt.active_fingerprint, composed.fingerprint);
            assert_eq!(receipt.extension_number, 1);
            let mut final_view = None;
            for _ in 0..2000 {
                if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await
                    && view.header.status.is_terminal()
                {
                    final_view = Some(view);
                    break;
                }
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            let view = final_view.expect("extended run stalled");
            assert_eq!(view.header.status, RunStatus::Succeeded);
            assert_ne!(view.header.root_scope, initial.root_scope);
            let preserved = store.record(&run.run_id, &old_scope.key).await.unwrap();
            let (Value::Map(original), Value::Map(preserved)) =
                (&old_scope.payload, &preserved.payload)
            else {
                panic!()
            };
            for field in [
                "origin_executable",
                "definition_digest",
                "scope_use",
                "inputs",
                "input_checkpoint",
                "admitted_at_ms",
                "deadline_ms",
                "effective_limits",
            ] {
                assert_eq!(
                    original.get(field),
                    preserved.get(field),
                    "preserved {field}"
                );
            }
            for node in old_nodes {
                let current = store.record(&run.run_id, &node.key).await.unwrap();
                let (Value::Map(before), Value::Map(after)) = (&node.payload, current.payload)
                else {
                    panic!()
                };
                assert_eq!(before.get("input_hash"), after.get("input_hash"));
            }
            let Value::Map(outputs) = view.outputs.unwrap() else {
                panic!()
            };
            let Some(Value::Map(output)) = outputs.get("answer") else {
                panic!()
            };
            let Some(Value::Text(id)) = output.get("artifact_id") else {
                panic!()
            };
            assert_eq!(
                runtime
                    .read_artifact(&caller, id.parse().unwrap())
                    .await
                    .unwrap()
                    .value,
                Value::Text("preserved".into())
            );
            engine.shutdown().await.unwrap();
            assert_eq!(
                runtime
                    .extend_run(&caller, &run.run_id, &request, "install")
                    .await
                    .unwrap(),
                receipt
            );
            assert!(
                runtime
                    .extend_run(&caller, &run.run_id, &request, "terminal")
                    .await
                    .is_err()
            );
            runtime
                .submit_reply(&caller, wait, Value::Bool(true), "close")
                .await
                .unwrap();
        }
    })
}
