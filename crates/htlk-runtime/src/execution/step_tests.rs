//! Ordered host-reply interleavings against actual fenced SQLite transitions.
use super::*;
use crate::{
    Authorization, NativeRegistry, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{host, recovery},
    failure::RuntimeFailure,
    identity::{WaitId, host_request_digest},
    records::InputValue,
    store::{ReplayState, SqliteStore, StoreConfig},
};
use htlk_executable::{
    BuiltinType, CanonicalDocument, DocumentFields, Edge, EvaluatorLimits, Expression,
    ExpressionKind, NodeFields, PolicyDocument, PolicyFields, Port, ScopeFields, ValueType,
    cbor::Limits,
};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    BeforeWaitScan,
    BeforeInvocationScan,
    AfterExternalScans,
}
pub(super) struct Probe {
    stage: Stage,
    reached: Notify,
    release: Notify,
}
impl Probe {
    pub(super) async fn pause(&self, stage: Stage) {
        if self.stage == stage {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: htlk_executable::digest::Digest,
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
fn ports(name: &str, ty: BuiltinType) -> PortTable {
    PortTable::new(
        vec![(
            name.parse().unwrap(),
            Port::new(ValueType::builtin(ty), true),
        )],
        &Limits::default(),
    )
    .unwrap()
}
async fn waiting_run() -> (Arc<Runtime>, HostContext, RunLease, WaitId) {
    let limits = Limits::default();
    let policy = PolicyDocument::new(
        PolicyFields {
            cost_unit: "credits".into(),
            defaults: ExecutionLimits::new()
                .with_timeout_ms(1000)
                .unwrap()
                .with_attempt_timeout_ms(100)
                .unwrap()
                .with_max_concurrency(1)
                .unwrap(),
            evaluator_limits: EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 65536,
                max_collection_visits: 10000,
                max_regex_bytes: 1024,
                max_regex_compiled_bytes: 4096,
                max_output_bytes: 65536,
                max_steps: 100000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &limits,
    )
    .unwrap();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    let profile = registry.link_policy(&policy).unwrap();
    let mut review = NodeFields::new(
        "review".parse().unwrap(),
        Operation::Wait {
            topic: "review".into(),
            timeout_ms: 500,
        },
    );
    review.inputs = ports("request", BuiltinType::Json);
    review.outputs = ports("value", BuiltinType::Boolean);
    let mut consume = NodeFields::new(
        "consume".parse().unwrap(),
        Operation::Eval(
            Expression::new(
                ExpressionKind::Ref {
                    source: ValueReference::Input("answer".parse().unwrap()),
                    path: vec![],
                },
                &limits,
            )
            .unwrap(),
        ),
    );
    consume.inputs = ports("answer", BuiltinType::Boolean);
    consume.outputs = ports("value", BuiltinType::Boolean);
    let scope = Scope::new(
        ScopeFields {
            inputs: ports("request", BuiltinType::Json),
            outputs: ports("result", BuiltinType::Boolean),
            nodes: vec![
                Node::new(review, &limits).unwrap(),
                Node::new(consume, &limits).unwrap(),
            ],
            edges: vec![
                Edge::new(
                    "request".parse().unwrap(),
                    EdgeSource::Input("request".parse().unwrap()),
                    EdgeDestination::Input {
                        node: "review".parse().unwrap(),
                        port: "request".parse().unwrap(),
                    },
                ),
                Edge::new(
                    "answer".parse().unwrap(),
                    EdgeSource::Output {
                        node: "review".parse().unwrap(),
                        port: "value".parse().unwrap(),
                    },
                    EdgeDestination::Input {
                        node: "consume".parse().unwrap(),
                        port: "answer".parse().unwrap(),
                    },
                ),
                Edge::new(
                    "result".parse().unwrap(),
                    EdgeSource::Output {
                        node: "consume".parse().unwrap(),
                        port: "value".parse().unwrap(),
                    },
                    EdgeDestination::Output("result".parse().unwrap()),
                ),
            ],
            ..ScopeFields::default()
        },
        &limits,
    )
    .unwrap();
    let root = scope.digest(&limits).unwrap();
    let mut fields = DocumentFields::new("test.reply_race".into(), profile, root);
    fields.scopes.insert(root, scope);
    fields
        .documents
        .insert(policy.digest(), policy.document().clone());
    let bytes = CanonicalDocument::new(fields, &limits)
        .unwrap()
        .envelope(&limits)
        .unwrap()
        .encode(&limits)
        .unwrap();
    // Single-record pages exercise the complete paginated observation boundary.
    let store = SqliteStore::in_memory(StoreConfig {
        max_page_size: 1,
        ..StoreConfig::default()
    })
    .await
    .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store,
            Arc::new(registry),
            Arc::new(Policy),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(Arc::new(Clock)),
    );
    let context = HostContext::new("alice".into(), &limits).unwrap();
    let graph = runtime.register_graph(&context, &bytes, &[]).await.unwrap();
    let run = runtime
        .start_run(
            &context,
            graph.fingerprint,
            &BTreeMap::from([("request".parse().unwrap(), InputValue::Inline(Value::Null))]),
            "start",
            None,
        )
        .await
        .unwrap()
        .run_id;
    let lease = runtime
        .store
        .acquire(&run, "owner".into(), 1000, 10000)
        .await
        .unwrap();
    for _ in 0..32 {
        match tick(runtime.clone(), lease.clone()).await.unwrap() {
            Progress::Changed => {}
            Progress::Blocked => {
                let waits = super::super::control::all(&runtime, &lease, RecordFamily::Wait)
                    .await
                    .unwrap();
                assert_eq!(waits.len(), 1);
                assert_eq!(string(&waits[0].payload, "state").unwrap(), "open");
                return (runtime, context, lease, waits[0].key.id.parse().unwrap());
            }
            Progress::Terminal => panic!("run terminated before opening its wait"),
        }
    }
    panic!("run did not become blocked")
}
async fn accept_reply(runtime: &Runtime, context: &HostContext, lease: &RunLease, wait: WaitId) {
    let value = Value::Bool(false);
    let command = host::Command {
        run: lease.run_id().clone(),
        context: context.clone(),
        method: "submit_reply".into(),
        target: wait.to_string(),
        key: "reply".into(),
        digest: host_request_digest(
            context.principal(),
            "submit_reply",
            &wait.to_string(),
            &value,
            &runtime.config.limits,
        )
        .unwrap(),
        operation: host::Operation::Reply {
            wait,
            value: value.clone(),
        },
    };
    // The same authorization, typing, receipt and fenced commit used by the host
    // actor, without a scheduler introducing any uncontrolled execution ticks.
    let (response, writes) = host::reply_writes(runtime, lease, &command, wait, &value)
        .await
        .unwrap();
    assert_eq!(string(&response, "disposition").unwrap(), "accepted");
    commit(
        runtime,
        lease,
        format!("reply:{wait}:{}", command.digest),
        writes,
        None,
    )
    .await
    .unwrap();
}
async fn assert_replay(runtime: &Runtime, lease: &RunLease) {
    let mut sequence = 0;
    let mut replay = None;
    loop {
        let page = runtime
            .store
            .journal(lease.run_id(), sequence, 1)
            .await
            .unwrap();
        let Some(entry) = page.first() else { break };
        sequence = entry.sequence;
        if let Some(replay) = &mut replay {
            ReplayState::apply(replay, entry, &runtime.config.limits).unwrap();
        } else {
            replay = Some(ReplayState::begin(entry, &runtime.config.limits).unwrap());
        }
    }
    let replay = replay.unwrap();
    assert_eq!(
        replay.header(),
        &runtime.store.run_header(lease.run_id()).await.unwrap()
    );
    for family in [
        RecordFamily::Scope,
        RecordFamily::Invocation,
        RecordFamily::Wait,
        RecordFamily::Reply,
    ] {
        for record in super::super::control::all(runtime, lease, family)
            .await
            .unwrap()
        {
            assert_eq!(replay.record(&record.key), Some(&record));
        }
    }
}
#[test]
fn host_reply_during_external_work_scans_invalidates_the_entire_progress_observation() {
    actix::System::new().block_on(async {
        for stage in [
            Stage::BeforeWaitScan,
            Stage::BeforeInvocationScan,
            Stage::AfterExternalScans,
        ] {
            let (runtime, context, lease, wait) = waiting_run().await;
            let before = runtime.store.run_header(lease.run_id()).await.unwrap();
            runtime.quiescent.lock().unwrap().clear();
            let probe = Probe {
                stage,
                reached: Notify::new(),
                release: Notify::new(),
            };
            // Timeout is only a watchdog; Notify establishes the interleaving.
            let (progress, ()) = tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(
                    tick_observed(runtime.clone(), lease.clone(), Some(&probe)),
                    async {
                        probe.reached.notified().await;
                        assert_eq!(
                            runtime
                                .store
                                .run_header(lease.run_id())
                                .await
                                .unwrap()
                                .sequence,
                            before.sequence
                        );
                        accept_reply(&runtime, &context, &lease, wait).await;
                        assert_eq!(
                            runtime
                                .store
                                .run_header(lease.run_id())
                                .await
                                .unwrap()
                                .sequence,
                            before.sequence + 1
                        );
                        probe.release.notify_one();
                    }
                )
            })
            .await
            .expect("execution did not reach and resume its observation checkpoint");
            assert!(
                matches!(progress, Ok(Progress::Changed)),
                "{stage:?}: {progress:?}"
            );
            assert!(runtime.quiescent.lock().unwrap().is_empty());
            for _ in 0..32 {
                if matches!(
                    recovery::drive(runtime.clone(), lease.clone(), false)
                        .await
                        .unwrap(),
                    Progress::Terminal
                ) {
                    break;
                }
            }
            let header = runtime.store.run_header(lease.run_id()).await.unwrap();
            assert_eq!(header.status, RunStatus::Succeeded);
            let root = runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Scope, header.root_scope))
                .await
                .unwrap();
            let output = entry(field(field(&root.payload, "outputs").unwrap(), "result").unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(
                runtime.read_artifact(&context, output).await.unwrap().value,
                Value::Bool(false)
            );
            let accepted = runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Wait, wait))
                .await
                .unwrap();
            assert!(accepted.final_record);
            assert_eq!(string(&accepted.payload, "state").unwrap(), "accepted");
            let terminal = runtime.store.run_header(lease.run_id()).await.unwrap();
            assert_eq!(
                runtime
                    .submit_reply(&context, wait, Value::Bool(false), "reply")
                    .await
                    .unwrap()
                    .disposition,
                "accepted"
            );
            assert_eq!(
                runtime
                    .submit_reply(&context, wait, Value::Bool(true), "reply")
                    .await
                    .unwrap_err()
                    .code(),
                FailureCode::IdempotencyConflict
            );
            assert_eq!(
                runtime.store.run_header(lease.run_id()).await.unwrap(),
                terminal
            );
            assert_replay(&runtime, &lease).await;
        }
    });
}
#[test]
fn stable_open_wait_blocks_but_stable_impossible_wait_state_still_fails_invariant() {
    actix::System::new().block_on(async {
        let (runtime, _, lease, wait) = waiting_run().await;
        let before = runtime.store.run_header(lease.run_id()).await.unwrap();
        assert!(matches!(
            tick(runtime.clone(), lease.clone()).await.unwrap(),
            Progress::Blocked
        ));
        assert_eq!(
            runtime.store.run_header(lease.run_id()).await.unwrap(),
            before
        );
        // Fault injection: an active wait invocation with a terminal cancelled wait
        // has neither runnable work nor an external event that can unblock it.
        let record = runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Wait, wait))
            .await
            .unwrap();
        commit(
            &runtime,
            &lease,
            "break-wait".into(),
            vec![update(
                &record,
                replace(
                    &record.payload,
                    [("state", Value::Text("cancelled".into()))],
                )
                .unwrap(),
                true,
            )],
            None,
        )
        .await
        .unwrap();
        let stable = runtime.store.run_header(lease.run_id()).await.unwrap();
        assert_eq!(
            tick(runtime.clone(), lease.clone())
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Invariant
        );
        assert_eq!(
            runtime.store.run_header(lease.run_id()).await.unwrap(),
            stable
        );
        assert!(matches!(
            recovery::drive(runtime.clone(), lease.clone(), false)
                .await
                .unwrap(),
            Progress::Terminal
        ));
        let header = runtime.store.run_header(lease.run_id()).await.unwrap();
        assert_eq!(header.status, RunStatus::Failed);
        let root = runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Scope, header.root_scope))
            .await
            .unwrap();
        assert_eq!(
            failure(&root.payload, &runtime.config.limits)
                .unwrap()
                .code(),
            FailureCode::Invariant
        );
        assert_replay(&runtime, &lease).await;
    });
}
