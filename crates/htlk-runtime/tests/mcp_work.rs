//! Protected work snapshots and reconciled outcomes stay distinct from wire replies.
use htlk_executable::{
    JsonDocument,
    cbor::{self, Limits, Value},
    digest::hash_bytes,
};
use htlk_runtime::{
    HostContext,
    clock::CoordinatorClock,
    failure::{FailureCode, RuntimeFailure},
    mcp::client::*,
    records::SecurityLabels,
    store::{SqliteStore, StoreConfig},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(
            format!("htlk-work-audit-{}-{}.sqlite", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()),
        ))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(10)
    }
}
fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn labels() -> SecurityLabels {
    SecurityLabels::new(vec!["model-private".into()], &Limits::default()).unwrap()
}
fn actor(name: &str) -> HostContext {
    HostContext::new(name.into(), &Limits::default()).unwrap()
}
fn config(store: SqliteStore, policy: Arc<dyn McpClientAuthorizer>) -> McpClientConfig {
    McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), None)
        .unwrap()
        .with_persistence(McpClientPersistence::new(store, Arc::new(Clock)))
        .with_authorizer(policy)
}
struct Policy;
impl McpClientAuthorizer for Policy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if actor.principal() == "reader" && !action.labels.as_slice().is_empty() {
            Err(RuntimeFailure::new(FailureCode::Policy))
        } else {
            Ok(())
        }
    }
}

#[test]
fn reconciled_sampling_recovery_uses_exact_settlement_and_usage_across_takeover_and_reopen() {
    let path = Database::new();
    actix::System::new().block_on(async {
        let store = SqliteStore::open(&path.0, StoreConfig::default()).await.unwrap();
        let owner = store.acquire_mcp_owner(&actor("alice"), "model", 1, 1000).await.unwrap();
        let session = store.open_mcp_epoch(&owner, hash_bytes(b"server"), 2).await.unwrap();
        let mut selected = Vec::new();
        for id in 1..=2 {
            let request = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"sampling/createMessage","params":{{"messages":[],"maxTokens":10}}}}"#).into_bytes();
            let record = store.receive_mcp_interaction(&owner, session.id, request, SecurityLabels::default(), 900, 3, 8).await.unwrap();
            store.begin_mcp_work(&owner, record.id, 4, 8, 2).await.unwrap();
            let original = if id == 1 {
                let bytes = format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32000,"message":"Provider timeout"}}}}"#).into_bytes();
                store.complete_mcp_work(&owner, record.id, &actor("alice"), "timeout", bytes.clone(), SecurityLabels::default(), None, McpWorkDelivery::Uncertain, 5).await.unwrap();
                Some(bytes)
            } else {
                store.cancel_mcp_work(&owner, record.id, 5).await.unwrap();
                None
            };
            let result = doc(&format!(r#"{{"role":"assistant","model":"reconciled-{id}","content":{{"type":"text","text":"actual completion"}}}}"#));
            let reply = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{}}}"#, std::str::from_utf8(result.as_bytes()).unwrap()).into_bytes();
            let usage = McpUsage { output_tokens: Some(17), provider_operation_id: Some(format!("provider-{id}")), ..Default::default() }.to_value(&Limits::default()).unwrap();
            let evidence = store.retain_mcp_late_reply(&owner, record.id, reply, labels(), McpWorkDelivery::Completed, Some(usage), 4).await.unwrap();
            store.settle_mcp_work(&owner, record.id, evidence, 6).await.unwrap();
            // A later retained result must not replace the selected settlement.
            let other = format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-1,"message":"different observation"}}}}"#).into_bytes();
            store.retain_mcp_late_reply(&owner, record.id, other, SecurityLabels::default(), McpWorkDelivery::Completed, None, 4).await.unwrap();
            selected.push((record.id, original, result.digest(), format!("provider-{id}")));
        }
        store.release_mcp_owner(&owner, 20).await.unwrap();
        let successor = store.acquire_mcp_owner(&actor("alice"), "model", 21, 1000).await.unwrap();
        store.close().await.unwrap();
        let store = SqliteStore::open(&path.0, StoreConfig::default()).await.unwrap();
        let host = McpInteractionHost::new(config(store.clone(), Arc::new(Policy))).unwrap();
        for (id, original, expected, provider) in selected {
            assert_eq!(host.recover_work(&actor("reader"), &successor, id).await.unwrap_err().code(), FailureCode::Policy);
            let recovered = host.recover_work(&actor("alice"), &successor, id).await.unwrap();
            assert_eq!(recovered.delivery, McpWorkDelivery::Completed);
            assert_eq!(recovered.result.unwrap().digest(), expected);
            assert_eq!(recovered.labels, labels());
            let usage = recovered.usage.unwrap();
            assert_eq!(usage.output_tokens, Some(17));
            assert_eq!(usage.provider_operation_id.as_deref(), Some(provider.as_str()));
            let saved = store.mcp_interaction(&successor, id).await.unwrap();
            assert_eq!(saved.reply, original, "recovery must not overwrite original wire history");
            assert!(saved.usage.is_none());
        }
        store.close().await.unwrap();
    });
}

// Inject a real independent writer exactly at authorization, without changing
// the reader's snapshot. Before the fix, the second read disclosed this outcome.
struct RacingPolicy {
    mutation: Mutex<Option<Mutation>>,
}
struct Mutation {
    path: PathBuf,
    id: String,
    protection: Vec<u8>,
    payload: Vec<u8>,
    owner: String,
}
impl McpClientAuthorizer for RacingPolicy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        Policy.authorize(actor, action)?;
        if let Some(Mutation {
            path,
            id,
            protection,
            payload,
            owner,
        }) = self.mutation.lock().unwrap().take()
        {
            let mut db = tokio_rusqlite::rusqlite::Connection::open(path).unwrap();
            let tx = db.transaction().unwrap();
            tx.execute("UPDATE mcp_interactions SET protection=?2,revision=revision+1 WHERE interaction_id=?1", tokio_rusqlite::params![id, protection]).unwrap();
            tx.execute("INSERT INTO mcp_events(owner_key,interaction_id,kind,generation,timestamp_ms,payload) VALUES(?1,?2,'native_operation_outcome',1,8,?3)", tokio_rusqlite::params![owner, id, payload]).unwrap();
            tx.commit().unwrap();
        }
        Ok(())
    }
}

#[test]
fn work_inspection_authorizes_the_same_snapshot_as_the_returned_evidence() {
    let path = Database::new();
    actix::System::new().block_on(async {
        let store = SqliteStore::open(&path.0, StoreConfig::default()).await.unwrap();
        let owner = store.acquire_mcp_owner(&actor("alice"), "model", 1, 1000).await.unwrap();
        let session = store.open_mcp_epoch(&owner, hash_bytes(b"server"), 2).await.unwrap();
        let record = store.receive_mcp_interaction(&owner, session.id, br#"{"jsonrpc":"2.0","id":1,"method":"sampling/createMessage","params":{"messages":[],"maxTokens":10}}"#.to_vec(), SecurityLabels::default(), 900, 3, 8).await.unwrap();
        store.begin_mcp_work(&owner, record.id, 4, 8, 2).await.unwrap();
        store.claim_mcp_host_operation(&owner, record.id, SecurityLabels::default(), doc(r#"{"model":"public routing"}"#), 5).await.unwrap();
        let policy = Arc::new(RacingPolicy { mutation: Mutex::new(Some(Mutation {
            path: path.0.clone(), id: record.id.to_string(),
            protection: cbor::encode(&labels().to_value(&Limits::default()).unwrap(), &Limits::default()).unwrap(),
            payload: cbor::encode(&Value::Text("protected native outcome".into()), &Limits::default()).unwrap(),
            owner: owner.key().to_string(),
        })) });
        let host = McpInteractionHost::new(config(store.clone(), policy)).unwrap();
        let snapshot = host.inspect_work(&actor("reader"), &owner, record.id).await.unwrap().unwrap();
        assert!(snapshot.native_outcome.is_none(), "an authorized older snapshot cannot include newly private bytes");
        assert_eq!(host.inspect_work(&actor("reader"), &owner, record.id).await.err().unwrap().code(), FailureCode::Policy);
        assert!(host.inspect_work(&actor("alice"), &owner, record.id).await.unwrap().unwrap().native_outcome.is_some());
        store.close().await.unwrap();
    });
}
