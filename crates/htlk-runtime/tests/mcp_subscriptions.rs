//! Durable lifecycle evidence from independent Node peers and competing SQLite
//! connections. Notifications never authorize resource acquisition or tool calls.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
    digest::{Digest, hash_bytes},
};
use htlk_runtime::{
    HostContext,
    clock::CoordinatorClock,
    failure::RuntimeFailure,
    mcp::{McpCancellation, McpEndpoint, client::*},
    store::{
        McpSubscriptionClaim, McpSubscriptionIntent, McpSubscriptionLimits,
        McpSubscriptionOperation, McpSubscriptionOutcome, McpSubscriptionState, SqliteStore,
        StoreError,
    },
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};

const PEER: &str = r#"
import {createInterface} from 'node:readline';
const mode=process.argv[1];
const send=v=>process.stdout.write(JSON.stringify(v)+'\n');
const reply=(r,result)=>send({jsonrpc:'2.0',id:r.id,result});
const update=(uri,phase)=>send({jsonrpc:'2.0',method:'notifications/resources/updated',params:{uri,phase}});
let held;const counts={subscribe:0,unsubscribe:0,content:0,cancel:0};
for await(const line of createInterface({input:process.stdin})) {
 const r=JSON.parse(line);
 if(r.method==='initialize')reply(r,{protocolVersion:'2025-11-25',serverInfo:{name:'subscriptions',version:'1'},capabilities:{resources:{subscribe:true}}});
 else if(r.method==='notifications/initialized')update('test://idle','idle');
 else if(r.method==='resources/subscribe') {
   counts.subscribe++;update(r.params.uri,'subscribe');
   if(mode==='held')held=r;
   else if(mode==='error')send({jsonrpc:'2.0',id:r.id,error:{code:-32000,message:'Uncertain remote state'}});
   else if(mode==='disconnect')process.exit(0);
   else reply(r,{});
 } else if(r.method==='resources/unsubscribe') {
   counts.unsubscribe++;update(r.params.uri,'before-unsubscribe-ack');reply(r,{});update(r.params.uri,'after-unsubscribe-ack');
 } else if(r.method==='ping') {
   if(r.params.release&&held){reply(held,{});held=undefined;}
   reply(r,counts);
 } else if(r.method==='notifications/cancelled')counts.cancel++;
 else if(['resources/read','tools/call','prompts/get'].includes(r.method)) {counts.content++;reply(r,{});}
 else process.exit(41);
}
"#;
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture {
    path: PathBuf,
    store: SqliteStore,
    clock: Arc<Clock>,
    persistence: McpClientPersistence,
    owner: McpOwner,
}
impl Fixture {
    async fn new() -> Self {
        htlk_analyzer::mcp_schema_validators().unwrap();
        let path = std::env::temp_dir().join(format!(
            "htlk-mcp-subscriptions-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = SqliteStore::open(&path, Default::default()).await.unwrap();
        let clock = Arc::new(Clock(AtomicI64::new(10)));
        let persistence = McpClientPersistence::new(store.clone(), clock.clone());
        let owner = persistence
            .acquire_owner(
                &HostContext::new("alice".into(), &Limits::default()).unwrap(),
                "monitor",
                10_000,
            )
            .await
            .unwrap();
        Self {
            path,
            store,
            clock,
            persistence,
            owner,
        }
    }
    fn host(&self) -> McpSubscriptionHost {
        McpSubscriptionHost::new(self.persistence.clone(), budget())
    }
    fn client(&self) -> NativeMcpClient {
        NativeMcpClient::new(
            McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), None)
                .unwrap()
                .with_persistence(self.persistence.clone()),
        )
        .unwrap()
    }
    async fn open(&self, client: &NativeMcpClient, mode: &str) -> McpClientSession {
        client
            .open(
                self.owner.clone(),
                server(),
                McpEndpoint::Stdio {
                    program: "node".into(),
                    arguments: vec![
                        "--input-type=module".into(),
                        "-e".into(),
                        PEER.into(),
                        mode.into(),
                    ],
                    environment: BTreeMap::new(),
                },
                Limits::default(),
                cancel(),
            )
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
fn budget() -> McpSubscriptionLimits {
    McpSubscriptionLimits::new(100, 200, 1000, 1024 * 1024, 4).unwrap()
}
fn rpc_limits() -> McpServiceLimits {
    McpServiceLimits::new(4096, 2000).unwrap()
}
fn cancel() -> Arc<McpCancellation> {
    Arc::new(McpCancellation::new())
}
fn server() -> ServerIdentity {
    ServerIdentity::new(
        "subscriptions".into(),
        McpTransport::Stdio,
        "subscriptions".into(),
        "1".into(),
        &Limits::default(),
    )
    .unwrap()
}
fn consumer(key: &str) -> McpSubscriptionConsumer {
    McpSubscriptionConsumer::new("test://resource".into(), key.into(), &Limits::default()).unwrap()
}
fn document(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
async fn stats(session: &McpClientSession, release: bool) -> JsonDocument {
    session
        .request(
            "ping",
            document(if release { r#"{"release":true}"# } else { "{}" }),
            cancel(),
        )
        .await
        .result
        .unwrap()
        .unwrap()
}
fn count(doc: &JsonDocument, key: &str) -> i64 {
    let Value::Map(fields) = doc.value() else {
        panic!()
    };
    let Some(Value::Integer(value)) = fields.get(key) else {
        panic!()
    };
    *value
}
async fn wait_subscribe(session: &McpClientSession) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if count(&stats(session, false).await, "subscribe") > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn dispatched(claim: McpSubscriptionClaim) -> McpSubscriptionOperation {
    let McpSubscriptionClaim::Dispatch { operation, .. } = claim else {
        panic!("expected unique dispatch claim")
    };
    operation
}
fn intent(session: Digest, server: Digest, key: &str, desired: bool) -> McpSubscriptionIntent {
    McpSubscriptionIntent::new(
        session,
        server,
        "test://resource".into(),
        key.into(),
        desired,
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn duplicate_consumers_share_one_remote_subscription_and_last_release_unsubscribes() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let client = f.client();
        let session = f.open(&client, "normal").await;
        let host = f.host();
        let second = f.host();
        let first = host
            .subscribe(&session, &consumer("ui"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(first.record().state(), McpSubscriptionState::Active);
        assert!(first.record().converged());
        assert_eq!(first.attempted_rpcs(), 1);
        let duplicate = second
            .subscribe(&session, &consumer("ui"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(duplicate.attempted_rpcs(), 0);
        assert_eq!(duplicate.record().desired_consumers(), 1);
        let added = second
            .subscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(added.attempted_rpcs(), 0);
        assert_eq!(added.record().desired_consumers(), 2);
        let removed = host
            .unsubscribe(&session, &consumer("ui"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(removed.attempted_rpcs(), 0);
        assert_eq!(removed.record().desired_consumers(), 1);
        let last = second
            .unsubscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(last.attempted_rpcs(), 1);
        assert_eq!(last.record().state(), McpSubscriptionState::Inactive);
        assert!(last.record().converged());
        let again = host
            .unsubscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(again.attempted_rpcs(), 0);
        let observed = stats(&session, false).await;
        assert_eq!(count(&observed, "subscribe"), 1);
        assert_eq!(count(&observed, "unsubscribe"), 1);
        assert_eq!(count(&observed, "content"), 0);
        let history = f.store.mcp_journal(&f.owner, 0, 100).await.unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|e| e.kind == "subscription_intent")
                .count(),
            2
        );
        assert_eq!(
            history
                .iter()
                .filter(|e| e.kind == "subscription_acknowledged")
                .count(),
            2
        );
        client.shutdown().await.unwrap();
    });
}

#[test]
fn in_flight_reference_races_persist_before_rpc_and_converge_without_duplicate_dispatch() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let client = f.client();
        let session = f.open(&client, "held").await;
        let one = f.host();
        let two = f.host();
        let ui = consumer("ui");
        let pending = one.subscribe(&session, &ui, rpc_limits(), cancel());
        let control = async {
            wait_subscribe(&session).await;
            let server = server().digest(&Limits::default()).unwrap();
            let observed = two
                .inspect(&f.owner, server, "test://resource")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(observed.state(), McpSubscriptionState::Uncertain);
            assert!(observed.operation().is_some());
            assert_eq!(observed.desired_consumers(), 1);
            let add = two
                .subscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(add.attempted_rpcs(), 0);
            let a = two
                .unsubscribe(&session, &ui, rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(a.attempted_rpcs(), 0);
            let b = two
                .unsubscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(b.attempted_rpcs(), 0);
            assert_eq!(b.record().desired_consumers(), 0);
            stats(&session, true).await;
        };
        let (finished, ()) = tokio::join!(pending, control);
        let finished = finished.unwrap();
        assert!(finished.record().converged());
        assert_eq!(finished.record().state(), McpSubscriptionState::Inactive);
        assert_eq!(finished.attempted_rpcs(), 2);
        let observed = stats(&session, false).await;
        assert_eq!(count(&observed, "subscribe"), 1);
        assert_eq!(count(&observed, "unsubscribe"), 1);
        assert_eq!(count(&observed, "content"), 0);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn errors_and_dropped_callers_retain_uncertainty_without_blind_retries() {
    actix::System::new().block_on(async {
        for mode in ["error", "held"] {
            let f = Fixture::new().await;
            let client = f.client();
            let session = f.open(&client, mode).await;
            let host = f.host();
            if mode == "error" {
                let report = host
                    .subscribe(&session, &consumer("ui"), rpc_limits(), cancel())
                    .await
                    .unwrap();
                assert!(report.failure().is_some());
                assert_eq!(report.record().state(), McpSubscriptionState::Uncertain);
            } else {
                let host = host.clone();
                let session_copy = session.clone();
                let task = actix::spawn(async move {
                    host.subscribe(&session_copy, &consumer("ui"), rpc_limits(), cancel())
                        .await
                });
                wait_subscribe(&session).await;
                task.abort();
                let _ = task.await;
            }
            let before = host.list(&f.owner, None, 10).await.unwrap().remove(0);
            assert_eq!(before.state(), McpSubscriptionState::Uncertain);
            let operation = before.operation().unwrap();
            let added = host
                .subscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(added.attempted_rpcs(), 0);
            assert_eq!(added.record().operation(), Some(operation));
            let reconcile = host
                .reconcile(&session, before.id(), rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(reconcile.attempted_rpcs(), 0);
            host.unsubscribe(&session, &consumer("ui"), rpc_limits(), cancel())
                .await
                .unwrap();
            let removed = host
                .unsubscribe(&session, &consumer("daemon"), rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(removed.record().desired_consumers(), 0);
            assert_eq!(removed.record().state(), McpSubscriptionState::Uncertain);
            let observed = stats(&session, false).await;
            assert_eq!(count(&observed, "subscribe"), 1);
            assert_eq!(count(&observed, "unsubscribe"), 0);
            assert_eq!(count(&observed, "content"), 0);
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn reconnect_requires_explicit_epoch_bound_restoration_and_exposes_durable_gap() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let client = f.client();
        let old = f.open(&client, "normal").await;
        let host = f.host();
        let subscribed = host
            .subscribe(&old, &consumer("ui"), rpc_limits(), cancel())
            .await
            .unwrap();
        let id = subscribed.record().id();
        let prior = old.durable_identity().unwrap();
        host.disconnect(&old).await.unwrap();
        let reopened_store = SqliteStore::open(&f.path, Default::default())
            .await
            .unwrap();
        let reopened = McpSubscriptionHost::new(
            McpClientPersistence::new(reopened_store, f.clock.clone()),
            budget(),
        );
        let records = reopened.list(&f.owner, None, 10).await.unwrap();
        assert_eq!(records[0].state(), McpSubscriptionState::Gap);
        assert_eq!(records[0].gap_count(), 1);
        assert_eq!(records[0].desired_consumers(), 1);
        let fresh = f.open(&client, "normal").await;
        assert_ne!(fresh.durable_identity(), Some(prior));
        let deferred = reopened
            .subscribe(&fresh, &consumer("daemon"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(deferred.attempted_rpcs(), 0);
        assert_eq!(deferred.record().state(), McpSubscriptionState::Gap);
        assert_eq!(count(&stats(&fresh, false).await, "subscribe"), 0);
        assert!(
            reopened
                .restore(
                    &fresh,
                    id,
                    hash_bytes(b"wrong prior epoch"),
                    rpc_limits(),
                    cancel()
                )
                .await
                .is_err()
        );
        let restored = reopened
            .restore(&fresh, id, prior, rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(restored.attempted_rpcs(), 1);
        assert_eq!(restored.record().state(), McpSubscriptionState::Active);
        assert_eq!(restored.record().gap_count(), 1);
        assert_eq!(restored.record().desired_consumers(), 2);
        assert!(
            reopened
                .restore(&fresh, id, prior, rpc_limits(), cancel())
                .await
                .is_err()
        );
        let observed = stats(&fresh, false).await;
        assert_eq!(count(&observed, "subscribe"), 1);
        assert_eq!(count(&observed, "content"), 0);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn transactional_claims_coordinate_connections_and_fence_closed_or_superseded_acks() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let other = SqliteStore::open(&f.path, Default::default())
            .await
            .unwrap();
        let server = server().digest(&Limits::default()).unwrap();
        let epoch = f.store.open_mcp_epoch(&f.owner, server, 10).await.unwrap();
        let (one, two) = tokio::join!(
            f.store.set_mcp_subscription_intent(
                &f.owner,
                intent(epoch.id, server, "a", true),
                budget(),
                10
            ),
            other.set_mcp_subscription_intent(
                &f.owner,
                intent(epoch.id, server, "b", true),
                budget(),
                10
            )
        );
        let operation = match (one.unwrap(), two.unwrap()) {
            (
                McpSubscriptionClaim::Dispatch { operation, .. },
                McpSubscriptionClaim::Retained(record),
            )
            | (
                McpSubscriptionClaim::Retained(record),
                McpSubscriptionClaim::Dispatch { operation, .. },
            ) => {
                assert_eq!(record.state(), McpSubscriptionState::Uncertain);
                operation
            }
            _ => panic!("only one transaction may dispatch"),
        };
        let record = f
            .store
            .mcp_subscription(&f.owner, server, "test://resource", 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.desired_consumers(), 2);
        f.store
            .close_mcp_epoch(&f.owner, epoch.id, 10)
            .await
            .unwrap();
        assert!(matches!(
            other
                .settle_mcp_subscription(
                    &f.owner,
                    &operation,
                    McpSubscriptionOutcome::Acknowledged,
                    10
                )
                .await,
            Err(StoreError::Fenced)
        ));
        let next = f.store.open_mcp_epoch(&f.owner, server, 10).await.unwrap();
        let new_operation = dispatched(
            f.store
                .restore_mcp_subscription(&f.owner, record.id(), epoch.id, next.id, budget(), 10)
                .await
                .unwrap(),
        );
        f.store
            .settle_mcp_subscription(
                &f.owner,
                &new_operation,
                McpSubscriptionOutcome::Acknowledged,
                10,
            )
            .await
            .unwrap();
        assert!(
            other
                .settle_mcp_subscription(
                    &f.owner,
                    &operation,
                    McpSubscriptionOutcome::Acknowledged,
                    10
                )
                .await
                .is_err()
        );
        f.clock.0.store(10_011, Ordering::SeqCst);
        let successor = f
            .persistence
            .acquire_owner(f.owner.host(), "monitor", 10_000)
            .await
            .unwrap();
        assert!(successor.generation() > f.owner.generation());
        assert!(matches!(
            other
                .settle_mcp_subscription(
                    &f.owner,
                    &new_operation,
                    McpSubscriptionOutcome::Acknowledged,
                    10
                )
                .await,
            Err(StoreError::Fenced)
        ));
        let old = f
            .store
            .mcp_subscription(&successor, server, "test://resource", 10_011)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.state(), McpSubscriptionState::Gap);
        assert_eq!(old.gap_count(), 2);
        let takeover_epoch = f
            .store
            .open_mcp_epoch(&successor, server, 10_011)
            .await
            .unwrap();
        let restored = dispatched(
            f.store
                .restore_mcp_subscription(
                    &successor,
                    record.id(),
                    next.id,
                    takeover_epoch.id,
                    budget(),
                    10_011,
                )
                .await
                .unwrap(),
        );
        let record = f
            .store
            .settle_mcp_subscription(
                &successor,
                &restored,
                McpSubscriptionOutcome::Acknowledged,
                10_011,
            )
            .await
            .unwrap();
        assert_eq!(record.state(), McpSubscriptionState::Active);
        assert_eq!(record.gap_count(), 2);
        assert_eq!(record.desired_consumers(), 2);
    });
}

#[test]
fn session_loss_keeps_uncertain_operation_evidence_and_never_auto_restores() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let client = f.client();
        let session = f.open(&client, "disconnect").await;
        let host = f.host();
        // Depending on teardown ordering, storing uncertainty can itself be
        // fenced by the already-closed epoch. Either path retains the pre-send claim.
        let _ = host
            .subscribe(&session, &consumer("ui"), rpc_limits(), cancel())
            .await;
        client.shutdown().await.unwrap();
        let record = host.list(&f.owner, None, 10).await.unwrap().remove(0);
        assert_eq!(record.state(), McpSubscriptionState::Gap);
        assert_eq!(record.gap_count(), 1);
        assert_eq!(record.desired_consumers(), 1);
        assert!(record.operation().is_some());
        let replacement = f.client();
        let fresh = f.open(&replacement, "normal").await;
        let pending = host
            .subscribe(&fresh, &consumer("ui"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert_eq!(pending.record().state(), McpSubscriptionState::Gap);
        assert_eq!(pending.attempted_rpcs(), 0);
        assert_eq!(count(&stats(&fresh, false).await, "subscribe"), 0);
        replacement.shutdown().await.unwrap();
    });
}

#[test]
fn independent_authority_checks_reject_cross_owner_server_and_corrupt_epoch_bindings() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let server = server().digest(&Limits::default()).unwrap();
        let epoch = f.store.open_mcp_epoch(&f.owner, server, 10).await.unwrap();
        let bob = f
            .persistence
            .acquire_owner(
                &HostContext::new("bob".into(), &Limits::default()).unwrap(),
                "monitor",
                10_000,
            )
            .await
            .unwrap();
        let bob_epoch = f.store.open_mcp_epoch(&bob, server, 10).await.unwrap();
        assert!(matches!(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(bob_epoch.id, server, "a", true),
                    budget(),
                    10
                )
                .await,
            Err(StoreError::Fenced)
        ));
        assert!(matches!(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(epoch.id, hash_bytes(b"other-server"), "a", true),
                    budget(),
                    10
                )
                .await,
            Err(StoreError::Fenced)
        ));
        let operation = dispatched(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(epoch.id, server, "a", true),
                    budget(),
                    10,
                )
                .await
                .unwrap(),
        );
        let alice_record = f
            .store
            .settle_mcp_subscription(
                &f.owner,
                &operation,
                McpSubscriptionOutcome::Acknowledged,
                10,
            )
            .await
            .unwrap();
        let other_live = f.store.open_mcp_epoch(&f.owner, server, 10).await.unwrap();
        assert!(matches!(
            f.store
                .restore_mcp_subscription(
                    &f.owner,
                    alice_record.id(),
                    epoch.id,
                    other_live.id,
                    budget(),
                    10
                )
                .await,
            Err(StoreError::Conflict)
        ));
        let operation = dispatched(
            f.store
                .set_mcp_subscription_intent(
                    &bob,
                    intent(bob_epoch.id, server, "a", true),
                    budget(),
                    10,
                )
                .await
                .unwrap(),
        );
        let bob_record = f
            .store
            .settle_mcp_subscription(&bob, &operation, McpSubscriptionOutcome::Acknowledged, 10)
            .await
            .unwrap();
        assert_ne!(alice_record.id(), bob_record.id());
        // Mutate the independent SQLite boundary, preserving foreign keys and a
        // valid session identity. The read must reject a cross-owner epoch binding.
        let db = tokio_rusqlite::Connection::open(&f.path).await.unwrap();
        let id = alice_record.id().to_string();
        db.call(move |db| {
            db.execute(
                "UPDATE mcp_resource_subscriptions SET session_id=?2 WHERE subscription_id=?1",
                tokio_rusqlite::params![id, bob_epoch.id.to_string()],
            )
        })
        .await
        .unwrap();
        assert!(matches!(
            f.store
                .mcp_subscription(&f.owner, server, "test://resource", 10)
                .await,
            Err(StoreError::Corrupt)
        ));
    });
}

#[test]
fn known_not_sent_is_retryable_and_quotas_rollback_entire_desire_transaction() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let server = server().digest(&Limits::default()).unwrap();
        let epoch = f.store.open_mcp_epoch(&f.owner, server, 10).await.unwrap();
        let small = McpSubscriptionLimits::new(1, 1, 10, 100_000, 2).unwrap();
        let operation = dispatched(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(epoch.id, server, "a", true),
                    small,
                    10,
                )
                .await
                .unwrap(),
        );
        assert!(matches!(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(epoch.id, server, "b", true),
                    small,
                    10
                )
                .await,
            Err(StoreError::Budget)
        ));
        let record = f
            .store
            .settle_mcp_subscription(&f.owner, &operation, McpSubscriptionOutcome::NotSent, 10)
            .await
            .unwrap();
        assert_eq!(record.state(), McpSubscriptionState::Inactive);
        assert_eq!(record.desired_consumers(), 1);
        let retry = dispatched(
            f.store
                .claim_mcp_subscription(&f.owner, epoch.id, record.id(), small, 10)
                .await
                .unwrap(),
        );
        assert_ne!(retry.id(), operation.id());
        let active = f
            .store
            .settle_mcp_subscription(&f.owner, &retry, McpSubscriptionOutcome::Acknowledged, 10)
            .await
            .unwrap();
        assert!(active.converged());
        assert!(matches!(
            f.store
                .settle_mcp_subscription(
                    &f.owner,
                    &operation,
                    McpSubscriptionOutcome::Acknowledged,
                    10
                )
                .await,
            Err(StoreError::IdempotencyConflict)
        ));
        let too_small = McpSubscriptionLimits::new(1, 1, 1, 100_000, 2).unwrap();
        assert!(matches!(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(epoch.id, server, "a", false),
                    too_small,
                    10
                )
                .await,
            Err(StoreError::Budget)
        ));
        assert_eq!(
            f.store
                .mcp_subscription(&f.owner, server, "test://resource", 10)
                .await
                .unwrap()
                .unwrap()
                .desired_consumers(),
            1
        );
    });
}

#[test]
fn transition_ceiling_exposes_unconverged_desire_for_explicit_follow_up() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let client = f.client();
        let session = f.open(&client, "held").await;
        let single = McpSubscriptionHost::new(
            f.persistence.clone(),
            McpSubscriptionLimits::new(100, 200, 1000, 1024 * 1024, 1).unwrap(),
        );
        let other = f.host();
        let ui = consumer("ui");
        let subscribe = single.subscribe(&session, &ui, rpc_limits(), cancel());
        let release = async {
            wait_subscribe(&session).await;
            let removed = other
                .unsubscribe(&session, &ui, rpc_limits(), cancel())
                .await
                .unwrap();
            assert_eq!(removed.attempted_rpcs(), 0);
            stats(&session, true).await;
        };
        let (report, ()) = tokio::join!(subscribe, release);
        let report = report.unwrap();
        assert_eq!(report.attempted_rpcs(), 1);
        assert_eq!(report.record().state(), McpSubscriptionState::Active);
        assert_eq!(report.record().desired_consumers(), 0);
        assert!(!report.record().converged());
        assert_eq!(count(&stats(&session, false).await, "unsubscribe"), 0);
        let settled = single
            .reconcile(&session, report.record().id(), rpc_limits(), cancel())
            .await
            .unwrap();
        assert!(settled.record().converged());
        let observed = stats(&session, false).await;
        assert_eq!(count(&observed, "subscribe"), 1);
        assert_eq!(count(&observed, "unsubscribe"), 1);
        assert_eq!(count(&observed, "content"), 0);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn cancelled_command_retains_desire_without_dispatch_and_storage_bytes_are_bounded() {
    actix::System::new().block_on(async {
        let f = Fixture::new().await;
        let server = server().digest(&Limits::default()).unwrap();
        let epoch = f.store.open_mcp_epoch(&f.owner, server, 10).await.unwrap();
        let tiny = McpSubscriptionLimits::new(100, 200, 1000, 100, 2).unwrap();
        assert!(matches!(
            f.store
                .set_mcp_subscription_intent(
                    &f.owner,
                    intent(epoch.id, server, "a", true),
                    tiny,
                    10
                )
                .await,
            Err(StoreError::Budget)
        ));
        assert!(f.host().list(&f.owner, None, 10).await.unwrap().is_empty());
        let client = f.client();
        let session = f.open(&client, "normal").await;
        let cancelled = cancel();
        cancelled.cancel();
        let report = f
            .host()
            .subscribe(&session, &consumer("ui"), rpc_limits(), cancelled)
            .await
            .unwrap();
        assert!(report.failure().is_some());
        assert_eq!(report.attempted_rpcs(), 0);
        assert_eq!(report.record().state(), McpSubscriptionState::Inactive);
        assert_eq!(report.record().desired_consumers(), 1);
        assert!(report.record().operation().is_none());
        assert!(!report.record().converged());
        assert_eq!(count(&stats(&session, false).await, "subscribe"), 0);
        let report = f
            .host()
            .subscribe(&session, &consumer("ui"), rpc_limits(), cancel())
            .await
            .unwrap();
        assert!(report.record().converged());
        assert_eq!(report.record().desired_consumers(), 1);
        assert_eq!(count(&stats(&session, false).await, "subscribe"), 1);
        client.shutdown().await.unwrap();
    });
}
