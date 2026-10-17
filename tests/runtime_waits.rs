//! Real wait/reply/cancellation transactions through the fenced actor owner.
mod support;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    identity::RunId,
    records::{InputValue, RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use support::{catalogs, native_compiler};
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(&self, c: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if c.principal() == "alice" {
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
        Ok(SecurityLabels::new(vec!["private".into()], &Limits::default()).unwrap())
    }
}
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
fn context(name: &str) -> HostContext {
    HostContext::new(name.into(), &Limits::default()).unwrap()
}
async fn setup() -> (
    Arc<Runtime>,
    ExecutionEngine,
    Arc<Clock>,
    RunId,
    SqliteStore,
) {
    let (compiler, registry) = native_compiler();
    let result=compiler.compile_source(r#"ir_version="0.1" graph test.wait{inputs={request=json} outputs={value=string}nodes{review=wait(string){topic="review" timeout_ms=500}}edges{edge request{from=inputs.request to=review.inputs.request} edge reply{from=review.outputs.value to=outputs.value}}}"#,&catalogs()).unwrap();
    let clock = Arc::new(Clock(AtomicI64::new(1000)));
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
        .with_clock(clock.clone()),
    );
    runtime
        .register_graph(&context("alice"), &result.executable_bytes, &[])
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
        .start_run(
            &context("alice"),
            result.fingerprint,
            &BTreeMap::from([(
                "request".parse().unwrap(),
                InputValue::Inline(Value::Text("question".into())),
            )]),
            "start",
            None,
        )
        .await
        .unwrap()
        .run_id;
    (runtime, engine, clock, run, store)
}
async fn open(runtime: &Runtime, run: &RunId) -> htlk::runtime::identity::WaitId {
    for _ in 0..1000 {
        let view = runtime.inspect_run(&context("alice"), run).await.unwrap();
        if let Some(wait) = view.waits.first() {
            return wait.wait_id;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("wait did not open")
}
async fn terminal(runtime: &Runtime, run: &RunId) -> RunStatus {
    for _ in 0..1000 {
        let view = runtime.inspect_run(&context("alice"), run).await.unwrap();
        if view.header.status.is_terminal() {
            return view.header.status;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("run did not terminate")
}
#[test]
fn reply_type_policy_and_idempotency_are_checked_without_holding_workers() {
    actix::System::new().block_on(async {
        let (runtime, engine, clock, run, store) = setup().await;
        let wait = open(&runtime, &run).await;
        assert_eq!(
            runtime
                .submit_reply(&context("bob"), wait, Value::Text("answer".into()), "reply")
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
        assert_eq!(
            runtime
                .submit_reply(&context("alice"), wait, Value::Integer(7), "reply")
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Input
        );
        assert_eq!(
            runtime
                .submit_reply(
                    &context("alice"),
                    wait,
                    Value::Text("answer".into()),
                    "reply"
                )
                .await
                .unwrap()
                .disposition,
            "accepted"
        );
        assert_eq!(terminal(&runtime, &run).await, RunStatus::Succeeded);
        engine.shutdown().await.unwrap();
        let before = store.run_header(&run).await.unwrap();
        let receipt = runtime
            .cancel_run(&context("alice"), &run, "already complete", "terminal-noop")
            .await
            .unwrap();
        assert_eq!(receipt.status, RunStatus::Succeeded);
        let after = store.run_header(&run).await.unwrap();
        assert_eq!(after.sequence, before.sequence + 1);
        assert_eq!(
            runtime
                .cancel_run(&context("alice"), &run, "already complete", "terminal-noop")
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(
            store.run_header(&run).await.unwrap().sequence,
            after.sequence
        );
        assert_eq!(
            runtime
                .cancel_run(&context("alice"), &run, "changed", "terminal-noop")
                .await
                .unwrap_err()
                .code(),
            FailureCode::IdempotencyConflict
        );
        let entries = store.journal(&run, 0, 100).await.unwrap();
        let mut replay =
            htlk::runtime::store::ReplayState::begin(&entries[0], &Limits::default()).unwrap();
        for entry in &entries[1..] {
            replay.apply(entry, &Limits::default()).unwrap();
        }
        assert_eq!(replay.header(), &after);
        clock.0.store(9000, Ordering::SeqCst);
        assert_eq!(
            runtime
                .submit_reply(
                    &context("alice"),
                    wait,
                    Value::Text("answer".into()),
                    "reply"
                )
                .await
                .unwrap()
                .disposition,
            "accepted"
        );
        assert_eq!(
            runtime
                .submit_reply(
                    &context("alice"),
                    wait,
                    Value::Text("changed".into()),
                    "reply"
                )
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::IdempotencyConflict
        );
    })
}
#[test]
fn cancellation_closes_waits_and_expiry_is_not_an_approval_or_success() {
    actix::System::new().block_on(async {
        let (runtime, engine, _, run, _) = setup().await;
        let wait = open(&runtime, &run).await;
        assert_eq!(
            runtime
                .cancel_run(&context("alice"), &run, "stop", "cancel")
                .await
                .unwrap()
                .status,
            RunStatus::Cancelled
        );
        assert_eq!(
            runtime
                .submit_reply(&context("alice"), wait, Value::Text("late".into()), "late")
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Cancelled
        );
        assert_eq!(
            runtime
                .cancel_run(&context("alice"), &run, "different", "cancel")
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::IdempotencyConflict
        );
        engine.shutdown().await.unwrap();
        let (runtime, engine, clock, run, _) = setup().await;
        open(&runtime, &run).await;
        clock.0.store(1500, Ordering::SeqCst);
        assert_eq!(terminal(&runtime, &run).await, RunStatus::Failed);
        engine.shutdown().await.unwrap();
    })
}
