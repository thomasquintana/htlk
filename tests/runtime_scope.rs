//! Explicit engine attachment excludes all automatic work on unrelated saved runs.
mod support;
use htlk::{
    compiler::CompilationSuccess,
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        clock::CoordinatorClock,
        execution::{ExecutionConfig, ExecutionEngine, ExecutionScope, RunView},
        failure::{FailureCode, RuntimeFailure},
        identity::RunId,
        mcp::{McpAdapter, McpFuture, McpRequest, McpRunContext},
        records::{RunStatus, SecurityLabels},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    ExecutionLimits, Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::Duration,
};

struct Policy {
    deny_dispatch: AtomicBool,
}
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, action: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if self.deny_dispatch.load(Ordering::SeqCst)
            && matches!(action, Authorization::DispatchMcp { .. })
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
}
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Adapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    calls: Mutex<BTreeMap<RunId, usize>>,
}
impl Adapter {
    fn calls(&self, run: &RunId) -> usize {
        self.calls.lock().unwrap().get(run).copied().unwrap_or(0)
    }
}
impl McpAdapter for Adapter {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&binding.descriptor()].clone()) })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        context: &'a McpRunContext,
        _: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            *self
                .calls
                .lock()
                .unwrap()
                .entry(context.run_id.clone())
                .or_default() += 1;
            Ok(br#"{"content":[],"structuredContent":{"evidence_text":"one original observation"}}"#.to_vec())
        })
    }
}
fn caller() -> HostContext {
    HostContext::new("alice".into(), &Limits::default()).unwrap()
}
fn config() -> ExecutionConfig {
    ExecutionConfig {
        scope: ExecutionScope::Attached,
        lease_ms: 100,
        poll_interval: Duration::from_millis(1),
        ..Default::default()
    }
}
const EFFECT: &str = r#"ir_version="0.1" graph scope.effect {
    inputs={} outputs={value=McpToolResult}
    nodes{args=eval(json,{query="scoped"}) effect=call(mcp.tool("research","lookup"))}
    edges{edge args{from=args.outputs.value to=effect.inputs.arguments}edge value{from=effect.outputs.value to=outputs.value}}
}"#;
#[actix::test]
async fn duplicate_launch_preserves_command_routing_and_shutdown_allows_replacement() {
    let (compiler, registry) = support::native_compiler_with_timeouts(60000, 1000);
    let graph = compiler.compile_source(WAIT, &support::catalogs()).unwrap();
    let runtime = Arc::new(
        Runtime::new(
            SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap(),
            Arc::new(registry),
            Arc::new(Policy {
                deny_dispatch: AtomicBool::new(false),
            }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_mcp(adapter(&graph)),
    );
    runtime
        .register_graph(&caller(), &graph.executable_bytes, &[])
        .await
        .unwrap();
    let engine = ExecutionEngine::launch(runtime.clone(), config()).unwrap();
    assert_eq!(
        ExecutionEngine::launch(runtime.clone(), config())
            .err()
            .unwrap()
            .code(),
        FailureCode::Profile
    );
    let run = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "engine-lifecycle",
            None,
        )
        .await
        .unwrap()
        .run_id;
    engine.attach_run(&run).await.unwrap();
    let wait = waiting(&runtime, &run).await;
    runtime
        .submit_reply(&caller(), wait, Value::Bool(true), "reply")
        .await
        .unwrap();
    assert_eq!(
        terminal(&runtime, &run).await.header.status,
        RunStatus::Succeeded
    );
    engine.shutdown().await.unwrap();
    let replacement = ExecutionEngine::launch(runtime.clone(), config()).unwrap();
    drop(engine);
    let run = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "replacement",
            None,
        )
        .await
        .unwrap()
        .run_id;
    replacement.attach_run(&run).await.unwrap();
    let wait = waiting(&runtime, &run).await;
    runtime
        .submit_reply(&caller(), wait, Value::Bool(false), "reply")
        .await
        .unwrap();
    assert_eq!(
        terminal(&runtime, &run).await.header.status,
        RunStatus::Succeeded
    );
    replacement.shutdown().await.unwrap();
}
const WAIT: &str = r#"ir_version="0.1" graph scope.wait {
    inputs={} outputs={value=boolean}
    nodes{args=eval(json,{query="once"}) effect=call(mcp.tool("research","lookup")) answer=wait(boolean){topic="answer" timeout_ms=5000}}
    edges{edge args{from=args.outputs.value to=effect.inputs.arguments}edge question{from=effect.outputs.value to=answer.inputs.request}edge value{from=answer.outputs.value to=outputs.value}}
}"#;
fn adapter(graph: &CompilationSuccess) -> Arc<Adapter> {
    Arc::new(Adapter {
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
        calls: Mutex::new(BTreeMap::new()),
    })
}
async fn terminal(runtime: &Runtime, run: &RunId) -> RunView {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime.inspect_run(&caller(), run).await {
                Ok(view) if view.header.status.is_terminal() => return view,
                Ok(_) => {}
                Err(error) if error.code() == FailureCode::Fenced => {}
                Err(error) => panic!("inspection: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("selected run stalled")
}
async fn waiting(runtime: &Runtime, run: &RunId) -> htlk::runtime::identity::WaitId {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime.inspect_run(&caller(), run).await {
                Ok(view) => {
                    assert!(!view.header.status.is_terminal(), "{view:?}");
                    if view.blocked
                        && let Some(wait) = view.waits.first()
                    {
                        return wait.wait_id;
                    }
                }
                Err(error) if error.code() == FailureCode::Fenced => {}
                Err(error) => panic!("inspection: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("selected wait stalled")
}

#[actix::test]
async fn empty_scope_and_selected_execution_leave_ready_and_overdue_runs_unchanged() {
    let (compiler, registry) = support::native_compiler_with_timeouts(60000, 1000);
    let graph = compiler
        .compile_source(EFFECT, &support::catalogs())
        .unwrap();
    let adapter = adapter(&graph);
    let clock = Arc::new(Clock(AtomicI64::new(1000)));
    let store = SqliteStore::in_memory(StoreConfig::default())
        .await
        .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store.clone(),
            Arc::new(registry),
            Arc::new(Policy {
                deny_dispatch: AtomicBool::new(false),
            }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(clock.clone())
        .with_mcp(adapter.clone()),
    );
    runtime
        .register_graph(&caller(), &graph.executable_bytes, &[])
        .await
        .unwrap();
    let ready = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "unselected-ready",
            None,
        )
        .await
        .unwrap()
        .run_id;
    let overdue = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "unselected-overdue",
            Some(&ExecutionLimits::new().with_timeout_ms(1).unwrap()),
        )
        .await
        .unwrap()
        .run_id;
    let snapshots = [
        (
            ready.clone(),
            store.run_header(&ready).await.unwrap(),
            store.journal(&ready, 0, 100).await.unwrap(),
        ),
        (
            overdue.clone(),
            store.run_header(&overdue).await.unwrap(),
            store.journal(&overdue, 0, 100).await.unwrap(),
        ),
    ];
    clock.0.store(2000, Ordering::SeqCst);
    let engine = ExecutionEngine::launch(runtime.clone(), config()).unwrap();
    let selected = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "selected",
            None,
        )
        .await
        .unwrap()
        .run_id;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(adapter.calls.lock().unwrap().is_empty());
    assert!(store.run_header(&selected).await.unwrap().owner.is_none());
    let missing = RunId::new("unknown-run".into(), &Limits::default()).unwrap();
    assert!(engine.attach_run(&missing).await.is_err());
    assert_eq!(
        runtime
            .cancel_run(&caller(), &ready, "unselected command", "cancel")
            .await
            .unwrap_err()
            .code(),
        FailureCode::Fenced
    );
    engine.attach_run(&selected).await.unwrap();
    engine.attach_run(&selected).await.unwrap();
    assert_eq!(
        terminal(&runtime, &selected).await.header.status,
        RunStatus::Succeeded
    );
    assert_eq!(adapter.calls(&selected), 1);
    for (run, header, journal) in snapshots {
        assert_eq!(store.run_header(&run).await.unwrap(), header);
        assert_eq!(store.journal(&run, 0, 100).await.unwrap(), journal);
        assert_eq!(adapter.calls(&run), 0);
    }
    engine.shutdown().await.unwrap();
    assert!(engine.attach_run(&selected).await.is_err());
}

#[actix::test]
async fn selected_run_restart_preserves_effect_and_exact_addressed_wait() {
    let directory = std::env::temp_dir().join(format!(
        "htlk-scope-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (compiler, registry) = support::native_compiler_with_timeouts(60000, 1000);
    let graph = compiler.compile_source(WAIT, &support::catalogs()).unwrap();
    let registry = Arc::new(registry);
    let adapter = adapter(&graph);
    let clock = Arc::new(Clock(AtomicI64::new(1000)));
    let store = SqliteStore::open(&directory, StoreConfig::default())
        .await
        .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store.clone(),
            registry.clone(),
            Arc::new(Policy {
                deny_dispatch: AtomicBool::new(false),
            }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(clock.clone())
        .with_mcp(adapter.clone()),
    );
    runtime
        .register_graph(&caller(), &graph.executable_bytes, &[])
        .await
        .unwrap();
    let run = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "original-admission",
            None,
        )
        .await
        .unwrap()
        .run_id;
    let engine = ExecutionEngine::launch(runtime.clone(), config()).unwrap();
    engine.attach_run(&run).await.unwrap();
    let wait = waiting(&runtime, &run).await;
    assert_eq!(adapter.calls(&run), 1);
    engine.shutdown().await.unwrap();
    store.close().await.unwrap();
    drop(runtime);
    let store = SqliteStore::open(&directory, StoreConfig::default())
        .await
        .unwrap();
    let before = store.run_header(&run).await.unwrap();
    clock.0.store(1200, Ordering::SeqCst);
    let runtime = Arc::new(
        Runtime::new(
            store.clone(),
            registry,
            Arc::new(Policy {
                deny_dispatch: AtomicBool::new(false),
            }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(clock)
        .with_mcp(adapter.clone()),
    );
    let engine = ExecutionEngine::launch(runtime.clone(), config()).unwrap();
    assert_eq!(
        runtime
            .start_run(
                &caller(),
                graph.fingerprint,
                &BTreeMap::new(),
                "original-admission",
                None
            )
            .await
            .unwrap()
            .run_id,
        run
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(store.run_header(&run).await.unwrap(), before);
    engine.attach_run(&run).await.unwrap();
    assert_eq!(waiting(&runtime, &run).await, wait);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match runtime
                .submit_reply(&caller(), wait, Value::Bool(true), "exact-reply")
                .await
            {
                Ok(_) => break,
                Err(error) if error.code() == FailureCode::Fenced => {
                    tokio::time::sleep(Duration::from_millis(2)).await
                }
                Err(error) => panic!("reply: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        terminal(&runtime, &run).await.header.status,
        RunStatus::Succeeded
    );
    runtime
        .submit_reply(&caller(), wait, Value::Bool(true), "exact-reply")
        .await
        .unwrap();
    assert_eq!(adapter.calls(&run), 1);
    engine.shutdown().await.unwrap();
    store.close().await.unwrap();
    std::fs::remove_file(directory).unwrap();
}

#[actix::test]
async fn bounded_attachment_is_additive_and_cannot_override_dispatch_policy() {
    let (compiler, registry) = support::native_compiler_with_timeouts(60000, 1000);
    let graph = compiler
        .compile_source(EFFECT, &support::catalogs())
        .unwrap();
    let adapter = adapter(&graph);
    let store = SqliteStore::in_memory(StoreConfig::default())
        .await
        .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store.clone(),
            Arc::new(registry),
            Arc::new(Policy {
                deny_dispatch: AtomicBool::new(true),
            }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(Arc::new(Clock(AtomicI64::new(1000))))
        .with_mcp(adapter.clone()),
    );
    runtime
        .register_graph(&caller(), &graph.executable_bytes, &[])
        .await
        .unwrap();
    let first = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "first",
            None,
        )
        .await
        .unwrap()
        .run_id;
    let second = runtime
        .start_run(
            &caller(),
            graph.fingerprint,
            &BTreeMap::new(),
            "second",
            None,
        )
        .await
        .unwrap()
        .run_id;
    for bound in [0, 4097] {
        assert!(
            ExecutionEngine::launch(
                runtime.clone(),
                ExecutionConfig {
                    max_attached_runs: bound,
                    ..config()
                }
            )
            .is_err()
        );
    }
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            max_attached_runs: 1,
            ..config()
        },
    )
    .unwrap();
    engine.attach_run(&first).await.unwrap();
    engine.attach_run(&first).await.unwrap();
    assert_eq!(
        engine.attach_run(&second).await.unwrap_err().code(),
        FailureCode::Budget
    );
    assert_eq!(
        terminal(&runtime, &first).await.header.status,
        RunStatus::Failed
    );
    assert_eq!(adapter.calls(&first), 0, "attachment is not a policy grant");
    let failed = compiler.compile_source(r#"ir_version="0.1" graph scope.rejected{inputs={}outputs={}nodes{}edges{}preconditions=false}"#, &support::catalogs()).unwrap();
    runtime
        .register_graph(&caller(), &failed.executable_bytes, &[])
        .await
        .unwrap();
    let terminal_run = runtime
        .start_run(
            &caller(),
            failed.fingerprint,
            &BTreeMap::new(),
            "terminal-at-admission",
            None,
        )
        .await
        .unwrap()
        .run_id;
    engine.attach_run(&terminal_run).await.unwrap();
    engine.attach_run(&first).await.unwrap(); // terminal attachment needs no capacity
    assert_eq!(
        engine.attach_run(&second).await.unwrap_err().code(),
        FailureCode::Budget,
        "a terminal run must not silently free/reassign the additive selection"
    );
    engine.shutdown().await.unwrap();
    // A host may intentionally choose global coordination instead.
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            scope: ExecutionScope::All,
            ..config()
        },
    )
    .unwrap();
    engine.attach_run(&second).await.unwrap();
    assert_eq!(
        terminal(&runtime, &second).await.header.status,
        RunStatus::Failed
    );
    engine.shutdown().await.unwrap();
}

#[actix::test]
async fn selected_id_paging_progresses_with_small_capacity_and_many_unselected_runs() {
    let (compiler, registry) = support::native_compiler();
    let graph = compiler
        .compile_source(EFFECT, &support::catalogs())
        .unwrap();
    let adapter = adapter(&graph);
    let store = SqliteStore::in_memory(StoreConfig {
        max_page_size: 2,
        ..Default::default()
    })
    .await
    .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store.clone(),
            Arc::new(registry),
            Arc::new(Policy {
                deny_dispatch: AtomicBool::new(false),
            }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(Arc::new(Clock(AtomicI64::new(1000))))
        .with_mcp(adapter.clone()),
    );
    runtime
        .register_graph(&caller(), &graph.executable_bytes, &[])
        .await
        .unwrap();
    let mut ignored = Vec::new();
    for i in 0..12 {
        ignored.push(
            runtime
                .start_run(
                    &caller(),
                    graph.fingerprint,
                    &BTreeMap::new(),
                    &format!("ignored-{i}"),
                    None,
                )
                .await
                .unwrap()
                .run_id,
        );
    }
    let mut selected = Vec::new();
    for i in 0..7 {
        selected.push(
            runtime
                .start_run(
                    &caller(),
                    graph.fingerprint,
                    &BTreeMap::new(),
                    &format!("selected-{i}"),
                    None,
                )
                .await
                .unwrap()
                .run_id,
        );
    }
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            max_runs: 1,
            ..config()
        },
    )
    .unwrap();
    for run in selected.iter().rev() {
        engine.attach_run(run).await.unwrap();
    }
    for run in &selected {
        assert_eq!(
            terminal(&runtime, run).await.header.status,
            RunStatus::Succeeded
        );
        assert_eq!(adapter.calls(run), 1);
    }
    for run in ignored {
        assert!(store.run_header(&run).await.unwrap().owner.is_none());
        assert_eq!(adapter.calls(&run), 0);
    }
    engine.shutdown().await.unwrap();
}
