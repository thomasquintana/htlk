//! Independent connections prove callback intent/results are atomic and replay-safe.
use htlk_executable::{
    cbor::{Limits, Value},
    digest::hash_bytes,
};
use htlk_runtime::{
    HostContext,
    failure::FailureCode,
    records::SecurityLabels,
    store::{McpOwnerUsage, McpWorkClaim, McpWorkState, SqliteStore, StoreConfig, StoreError},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "htlk-mcp-ledger-{}-{}-{}.sqlite",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for path in [
            self.0.clone(),
            PathBuf::from(format!("{}-wal", self.0.display())),
            PathBuf::from(format!("{}-shm", self.0.display())),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }
}
fn host(name: &str) -> HostContext {
    HostContext::new(name.into(), &Limits::default()).unwrap()
}
fn labels(name: &str) -> SecurityLabels {
    SecurityLabels::new(vec![name.into()], &Limits::default()).unwrap()
}
fn request(id: &str) -> Vec<u8> {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"sampling/createMessage","params":{{"messages":[],"maxTokens":10}}}}"#).into_bytes()
}
fn reply(id: &str) -> Vec<u8> {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"role":"assistant","model":"fixture","content":{{"type":"text","text":"private-result"}}}}}}"#).into_bytes()
}

#[test]
fn competing_claims_and_lost_reply_acknowledgements_never_authorize_duplicate_work() {
    let path = Database::new();
    actix::System::new().block_on(async {
        let first = SqliteStore::open(&path.0, StoreConfig::default())
            .await
            .unwrap();
        let second = SqliteStore::open(&path.0, StoreConfig::default())
            .await
            .unwrap();
        let alice = host("alice");
        let owner = first
            .acquire_mcp_owner(&alice, "ide", 10, 1000)
            .await
            .unwrap();
        assert_eq!(
            second
                .acquire_mcp_owner(&alice, "ide", 20, 1000)
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        let session = first
            .open_mcp_epoch(&owner, hash_bytes(b"server"), 20)
            .await
            .unwrap();
        let original = request("1");
        let record = first
            .receive_mcp_interaction(
                &owner,
                session.id,
                original.clone(),
                labels("request"),
                900,
                30,
                8,
            )
            .await
            .unwrap();
        let replay = second
            .receive_mcp_interaction(
                &owner,
                session.id,
                original.clone(),
                SecurityLabels::default(),
                950,
                31,
                8,
            )
            .await
            .unwrap();
        assert_eq!(record.id, replay.id);
        assert_eq!(
            replay.deadline_ms, 900,
            "redelivery cannot reset the deadline"
        );
        let (one, two) = tokio::join!(
            first.begin_mcp_work(&owner, record.id, 40, 2, 1),
            second.begin_mcp_work(&owner, record.id, 40, 2, 1)
        );
        let claims = [one.unwrap(), two.unwrap()];
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, McpWorkClaim::Started(_)))
                .count(),
            1
        );
        assert_eq!(
            first.mcp_owner_usage(&owner).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 1,
                outstanding: 1
            }
        );
        let response = reply("1");
        let saved = first
            .complete_mcp_work(
                &owner,
                record.id,
                &alice,
                "answer",
                response.clone(),
                labels("result"),
                Some(Value::Integer(7)),
                50,
            )
            .await
            .unwrap();
        let repeated = second
            .complete_mcp_work(
                &owner,
                record.id,
                &alice,
                "answer",
                response.clone(),
                labels("result"),
                Some(Value::Integer(7)),
                51,
            )
            .await
            .unwrap();
        assert_eq!(saved.revision, repeated.revision);
        assert_eq!(
            saved.labels,
            labels("request")
                .union(&labels("result"), &Limits::default())
                .unwrap()
        );
        assert!(!format!("{saved:?}").contains("private-result"));
        first
            .record_mcp_delivery(&owner, record.id, false, 60)
            .await
            .unwrap();
        first.close().await.unwrap();
        second.close().await.unwrap();

        // A fresh connection sees saved work after a lost send acknowledgement.
        let recovered = SqliteStore::open(&path.0, StoreConfig::default())
            .await
            .unwrap();
        let McpWorkClaim::Existing(saved) = recovered
            .begin_mcp_work(&owner, record.id, 70, 2, 1)
            .await
            .unwrap()
        else {
            panic!("recovery must not dispatch")
        };
        assert_eq!(saved.state, McpWorkState::Result);
        assert_eq!(saved.reply.as_deref(), Some(response.as_slice()));
        assert_eq!(saved.send_attempts, 1);
        assert!(!saved.transport_accepted);
        recovered
            .record_mcp_delivery(&owner, record.id, false, 80)
            .await
            .unwrap();
        recovered
            .record_mcp_delivery(&owner, record.id, true, 81)
            .await
            .unwrap();
        let saved = recovered.mcp_interaction(&owner, record.id).await.unwrap();
        assert_eq!(saved.send_attempts, 2);
        assert!(saved.transport_accepted);
        assert_eq!(
            recovered.mcp_owner_usage(&owner).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 1,
                outstanding: 0
            }
        );
        let facts = recovered.mcp_journal(&owner, 0, 32).await.unwrap();
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.kind == "work_intent")
                .count(),
            1
        );
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.kind == "reply_recorded")
                .count(),
            1
        );
        assert!(
            facts
                .windows(2)
                .all(|p| p[0].sequence < p[1].sequence && p[0].timestamp_ms <= p[1].timestamp_ms)
        );
        recovered.close().await.unwrap();
    });
}

#[test]
fn takeover_preserves_uncertainty_and_late_evidence_without_refunding_reservations() {
    actix::System::new().block_on(async {
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let alice = host("alice");
        let old = store
            .acquire_mcp_owner(&alice, "ide", 1, 100)
            .await
            .unwrap();
        let session = store
            .open_mcp_epoch(&old, hash_bytes(b"server"), 2)
            .await
            .unwrap();
        let record = store
            .receive_mcp_interaction(&old, session.id, request("1"), labels("request"), 500, 3, 8)
            .await
            .unwrap();
        assert!(matches!(
            store
                .begin_mcp_work(&old, record.id, 4, 8, 1)
                .await
                .unwrap(),
            McpWorkClaim::Started(_)
        ));
        let current = store
            .acquire_mcp_owner(&alice, "ide", 100, 1000)
            .await
            .unwrap();
        assert_eq!(current.generation(), old.generation() + 1);
        let McpWorkClaim::Existing(unknown) = store
            .begin_mcp_work(&current, record.id, 101, 8, 1)
            .await
            .unwrap()
        else {
            panic!("unknown work cannot restart")
        };
        assert_eq!(unknown.state, McpWorkState::Intent);
        assert!(unknown.outstanding);
        assert_eq!(
            store
                .complete_mcp_work(
                    &old,
                    record.id,
                    &alice,
                    "late",
                    reply("1"),
                    labels("late"),
                    None,
                    102
                )
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        assert_eq!(
            store
                .complete_mcp_work(
                    &current,
                    record.id,
                    &alice,
                    "late",
                    reply("1"),
                    labels("late"),
                    None,
                    102
                )
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        let digest = store
            .retain_mcp_late_reply(&old, record.id, reply("1"), labels("late"), 2)
            .await
            .unwrap();
        let late = store
            .mcp_late_replies(&current, record.id, None, 2)
            .await
            .unwrap();
        assert_eq!(late.len(), 1);
        assert_eq!(late[0].digest, digest);
        assert_eq!(
            late[0].labels,
            labels("request")
                .union(&labels("late"), &Limits::default())
                .unwrap()
        );
        assert!(
            store
                .mcp_interaction(&current, record.id)
                .await
                .unwrap()
                .reply
                .is_none()
        );
        let cancelled = store
            .cancel_mcp_work(&current, record.id, 103)
            .await
            .unwrap();
        assert_eq!(cancelled.state, McpWorkState::Cancelled);
        assert!(cancelled.outstanding);
        assert_eq!(
            store.mcp_owner_usage(&current).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 1,
                outstanding: 1
            }
        );
        let fresh = store
            .open_mcp_epoch(&current, hash_bytes(b"server"), 104)
            .await
            .unwrap();
        assert_ne!(fresh.id, session.id);
        assert!(fresh.epoch > session.epoch);
        let next = store
            .receive_mcp_interaction(
                &current,
                fresh.id,
                request("1"),
                labels("request"),
                900,
                105,
                8,
            )
            .await
            .unwrap();
        assert_ne!(next.id, record.id);
        assert_eq!(
            store
                .begin_mcp_work(&current, next.id, 106, 8, 1)
                .await
                .unwrap_err(),
            StoreError::Budget
        );
        assert_eq!(
            store
                .close_mcp_epoch(&old, session.id, 107)
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        assert_eq!(
            store
                .mcp_interaction(&current, next.id)
                .await
                .unwrap()
                .state,
            McpWorkState::Pending
        );
        let settled = store
            .settle_mcp_work(&current, record.id, digest, 108)
            .await
            .unwrap();
        assert_eq!(settled.state, McpWorkState::Cancelled);
        assert!(!settled.outstanding);
        assert!(settled.reply.is_none());
        assert_eq!(
            store.mcp_owner_usage(&current).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 1,
                outstanding: 0
            }
        );
        assert!(matches!(
            store
                .begin_mcp_work(&current, next.id, 109, 8, 1)
                .await
                .unwrap(),
            McpWorkClaim::Started(_)
        ));
    });
}

#[test]
fn typed_request_ids_conflicts_deadlines_and_principal_isolation_are_independent() {
    actix::System::new().block_on(async {
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let alice = host("alice");
        let owner = store
            .acquire_mcp_owner(&alice, "ide", 1, 1000)
            .await
            .unwrap();
        let bob = store
            .acquire_mcp_owner(&host("bob"), "ide", 1, 1000)
            .await
            .unwrap();
        let session = store
            .open_mcp_epoch(&owner, hash_bytes(b"server"), 2)
            .await
            .unwrap();
        let first = store
            .receive_mcp_interaction(
                &owner,
                session.id,
                request("1"),
                SecurityLabels::default(),
                50,
                3,
                2,
            )
            .await
            .unwrap();
        let second = store
            .receive_mcp_interaction(
                &owner,
                session.id,
                request("\"1\""),
                SecurityLabels::default(),
                50,
                3,
                2,
            )
            .await
            .unwrap();
        assert_ne!(first.id, second.id);
        let changed = String::from_utf8(request("1"))
            .unwrap()
            .replace("10", "20")
            .into_bytes();
        assert_eq!(
            store
                .receive_mcp_interaction(
                    &owner,
                    session.id,
                    changed,
                    SecurityLabels::default(),
                    50,
                    4,
                    2
                )
                .await
                .unwrap_err(),
            StoreError::IdempotencyConflict
        );
        assert_eq!(
            store
                .receive_mcp_interaction(
                    &owner,
                    session.id,
                    request("3"),
                    SecurityLabels::default(),
                    50,
                    4,
                    2
                )
                .await
                .unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(
            store.mcp_interaction(&bob, first.id).await.unwrap_err(),
            StoreError::Missing
        );
        assert_eq!(
            store
                .receive_mcp_interaction(
                    &bob,
                    session.id,
                    request("4"),
                    SecurityLabels::default(),
                    50,
                    4,
                    2
                )
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        assert!(matches!(
            store
                .begin_mcp_work(&owner, first.id, 5, 2, 2)
                .await
                .unwrap(),
            McpWorkClaim::Started(_)
        ));
        assert_eq!(
            store
                .complete_mcp_work(
                    &owner,
                    first.id,
                    &alice,
                    "wrong-id",
                    reply("\"1\""),
                    SecurityLabels::default(),
                    None,
                    6
                )
                .await
                .unwrap_err(),
            StoreError::Invalid
        );
        assert_eq!(
            store
                .complete_mcp_work(
                    &owner,
                    first.id,
                    &alice,
                    "expired",
                    reply("1"),
                    SecurityLabels::default(),
                    None,
                    50
                )
                .await
                .unwrap_err(),
            StoreError::Deadline(FailureCode::Timeout)
        );
        assert!(
            store
                .mcp_interaction(&owner, first.id)
                .await
                .unwrap()
                .outstanding
        );
        assert_eq!(
            store
                .begin_mcp_work(&owner, second.id, 50, 2, 2)
                .await
                .unwrap_err(),
            StoreError::Deadline(FailureCode::Timeout)
        );
        assert!(store.mcp_journal(&owner, 0, 0).await.is_err());
    });
}
