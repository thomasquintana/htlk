//! Independent SQLite connections exercise fenced public URL storage, durable
//! callback replies, and direct SQL corruption/transaction fault injection.
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Value},
    digest::{Digest, hash_bytes},
};
use htlk_runtime::{HostContext, mcp::client::*, records::SecurityLabels, store::*};
pub use htlk_runtime::{failure, identity, mcp, records, store};
use rusqlite::{Connection as Db, TransactionBehavior};
use std::{
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
};

// Include the actual SQL core for explicit corruption/transaction fault injection.
#[path = "../src/store_mcp_url_state.rs"]
mod state;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "htlk-url-state-{}-{}-{}.sqlite", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        )))
    }
    fn connect(&self) -> Db {
        connect(&self.0)
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn connect(path: &PathBuf) -> Db {
    let db = Db::open(path).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db
}
fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn labels(value: &str) -> SecurityLabels {
    SecurityLabels::new(vec![value.into()], &Limits::default()).unwrap()
}
fn request(id: &str) -> McpUrlRequest {
    McpUrlRequest::from_params(doc(&format!(r#"{{"mode":"url","elicitationId":"{id}","url":"https://mcp.example.test/flow?opaque=protected","message":"private explanation","_meta":{{"private":"original-metadata"}}}}"#)).value(), &Limits::default()).unwrap()
}
fn required(raw: &str) -> McpUrlRequiredError {
    McpUrlRequiredError::parse(
        &McpRpcError::from_bytes(raw.as_bytes(), &Limits::default()).unwrap(),
        16,
        &Limits::default(),
    )
    .unwrap()
    .unwrap()
}
fn required_one() -> McpUrlRequiredError {
    required(
        r#"{ "code" : -32042, "message":"private original outcome", "data":{"elicitations":[{"mode":"url","elicitationId":"needed","url":"https://mcp.example.test/flow?opaque=protected","message":"private explanation","_meta":{"private":"original-metadata"}}],"private":"opaque original error metadata"} }"#,
    )
}
struct Fixture {
    path: Database,
    owner: McpOwner,
    server: Digest,
    session: Digest,
}
impl Fixture {
    fn new() -> Self {
        let path = Database::new();
        let (owner, server, session) = actix::System::new().block_on(async {
            let store = SqliteStore::open(&path.0, StoreConfig::default())
                .await
                .unwrap();
            let host = HostContext::new("alice".into(), &Limits::default()).unwrap();
            let owner = store
                .acquire_mcp_owner(&host, "url-tests", 10, 10_000)
                .await
                .unwrap();
            let server = hash_bytes(b"compound-server");
            let session = store.open_mcp_epoch(&owner, server, 20).await.unwrap();
            store.close().await.unwrap();
            (owner, server, session.id)
        });
        Self {
            path,
            owner,
            server,
            session,
        }
    }
    fn scope(&self) -> state::Scope<'_> {
        state::Scope {
            owner: &self.owner,
            server: self.server,
            session: self.session,
        }
    }
    fn register(&self, request: &McpUrlRequest) -> Digest {
        let mut db = self.path.connect();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let id = state::register(
            &tx,
            self.scope(),
            request,
            None,
            &labels("request"),
            900,
            16,
            &Limits::default(),
        )
        .unwrap();
        state::set_origin(&tx, id, None, false, &Limits::default()).unwrap();
        tx.commit().unwrap();
        id
    }
    fn record_error(&self, required: &McpUrlRequiredError, rpc: &McpRequestId) -> Vec<Digest> {
        let mut db = self.path.connect();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let ids = state::required(
            &tx,
            self.scope(),
            rpc,
            required,
            &labels("original-operation"),
            900,
            16,
            &Limits::default(),
        )
        .unwrap();
        state::required_origin(&tx, self.scope(), rpc, None, &Limits::default()).unwrap();
        tx.commit().unwrap();
        ids
    }
}

#[test]
fn protected_errors_validate_every_required_item_and_leave_other_codes_lazy() {
    let original = required_one();
    assert_eq!(
        original.interactions()[0].url(),
        "https://mcp.example.test/flow?opaque=protected"
    );
    assert!(original.error().as_bytes().starts_with(b"{ \"code\" :"));
    let debug = format!(
        "{original:?} {:?} {:?}",
        original.interactions()[0],
        original.error()
    );
    for secret in [
        "private",
        "opaque",
        "mcp.example",
        "needed",
        "original-metadata",
    ] {
        assert!(
            !debug.contains(secret),
            "protected data leaked through Debug: {debug}"
        );
    }
    for raw in [
        r#"{"code":-32042,"message":"x"}"#,
        r#"{"code":-32042,"message":"x","data":{"elicitations":null}}"#,
        r#"{"code":-32042,"message":"x","data":{"elicitations":[{"mode":"form","elicitationId":"x","url":"https://example.test","message":"x"}]}}"#,
        r#"{"code":-32042,"message":"x","data":{"elicitations":[{"mode":"url","url":"https://example.test","message":"x"}]}}"#,
        r#"{"code":-32042,"message":"x","data":{"elicitations":[{"mode":"url","elicitationId":"x","url":"not a URL","message":"x"}]}}"#,
        r#"{"code":-32042,"message":"x","data":{"elicitations":[{"mode":"url","elicitationId":"x","url":"https://example.test","message":"x"},{"mode":"url","elicitationId":"x","url":"https://other.test","message":"x"}]}}"#,
    ] {
        let error = McpRpcError::from_bytes(raw.as_bytes(), &Limits::default()).unwrap();
        let before = error.as_bytes().to_vec();
        assert!(McpUrlRequiredError::parse(&error, 16, &Limits::default()).is_err());
        assert_eq!(
            error.as_bytes(),
            before,
            "malformed URL requirements must not rewrite the RPC outcome"
        );
    }
    let other = McpRpcError::from_bytes(
        br#"{"code":-32000,"message":"other","data":false}"#,
        &Limits::default(),
    )
    .unwrap();
    assert!(
        McpUrlRequiredError::parse(&other, 0, &Limits::default())
            .unwrap()
            .is_none()
    );
    assert!(
        required(r#"{"code":-32042,"message":"x","data":{"elicitations":[]}}"#)
            .interactions()
            .is_empty(),
        "the selected schema does not impose minItems on the required list"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let local = doc(&format!(
        r#"{{"mode":"url","elicitationId":"no-fetch","url":"http://{}/private","message":"x"}}"#,
        listener.local_addr().unwrap()
    ));
    let fixture = Fixture::new();
    fixture.register(&McpUrlRequest::from_params(local.value(), &Limits::default()).unwrap());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "registration must not fetch URL or metadata"
    );
}

#[test]
fn restart_preserves_original_error_and_completion_never_supplies_consent_or_retries() {
    let fixture = Fixture::new();
    let original = required_one();
    let rpc = McpRequestId::String("original-rpc".into());
    let id = fixture.record_error(&original, &rpc)[0];
    let mut db = fixture.path.connect();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(
        state::complete(&tx, fixture.scope(), "needed", 100, &Limits::default()).unwrap(),
        McpUrlCompletion::Recorded { interaction_id: id }
    );
    tx.commit().unwrap();
    drop(db);
    let mut reopened = fixture.path.connect();
    let before = state::inspect(&reopened, fixture.scope(), id, &Limits::default()).unwrap();
    assert_eq!(before.consent, McpUrlConsent::Pending);
    assert_eq!(before.completed_ms, Some(100));
    let result = doc(r#"{"action":"accept"}"#);
    let tx = reopened
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let receipt = state::respond(
        &tx,
        fixture.scope(),
        id,
        before.request.params().digest(),
        "alice",
        "consent",
        &result,
        &labels("response"),
        110,
        &Limits::default(),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(reopened);
    let mut db = fixture.path.connect();
    let after = state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap();
    assert_eq!(after.consent, McpUrlConsent::Accepted);
    assert_eq!(after.completed_ms, Some(100));
    assert_eq!(
        after.labels,
        labels("original-operation")
            .union(&labels("response"), &Limits::default())
            .unwrap()
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(
        state::complete(&tx, fixture.scope(), "needed", 200, &Limits::default()).unwrap(),
        McpUrlCompletion::Ignored
    );
    assert_eq!(
        state::respond(
            &tx,
            fixture.scope(),
            id,
            after.request.params().digest(),
            "alice",
            "consent",
            &result,
            &SecurityLabels::default(),
            950,
            &Limits::default()
        )
        .unwrap(),
        receipt,
        "same receipt survives the deadline"
    );
    tx.commit().unwrap();
    let error = state::original_error(&db, fixture.scope(), &rpc, &Limits::default()).unwrap();
    assert_eq!(error.error.as_bytes(), original.error().as_bytes());
    assert_eq!(error.interactions, vec![id]);
    assert_eq!(error.request_id, rpc);
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM mcp_interactions", [], |r| r.get(0))
            .unwrap(),
        0,
        "URL completion must not manufacture callbacks or redispatch original work"
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT calls_spent FROM mcp_owners", [], |r| r.get(0))
            .unwrap(),
        0
    );
}

#[test]
fn acceptance_without_completion_and_expiry_are_durable_independent_facts() {
    let fixture = Fixture::new();
    let id = fixture.register(&request("consent-only"));
    let mut db = fixture.path.connect();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    state::respond(
        &tx,
        fixture.scope(),
        id,
        request("consent-only").params().digest(),
        "alice",
        "yes",
        &doc(r#"{"action":"accept"}"#),
        &SecurityLabels::default(),
        100,
        &Limits::default(),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let db = fixture.path.connect();
    let saved = state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap();
    assert_eq!(saved.consent, McpUrlConsent::Accepted);
    assert_eq!(saved.completed_ms, None);
    assert!(saved.expired(900));
    assert!(!saved.expired(899));
    db.execute(
        "UPDATE mcp_epochs SET closed=1 WHERE session_id=?1",
        [fixture.session.to_string()],
    )
    .unwrap();
    assert!(
        state::inspect(&db, fixture.scope(), id, &Limits::default())
            .unwrap()
            .session_closed
    );
    let expired = fixture.register(&request("expired"));
    let mut db = fixture.path.connect();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(
        state::respond(
            &tx,
            fixture.scope(),
            expired,
            request("expired").params().digest(),
            "alice",
            "too-late",
            &doc(r#"{"action":"accept"}"#),
            &SecurityLabels::default(),
            900,
            &Limits::default()
        )
        .unwrap_err(),
        StoreError::Deadline(failure::FailureCode::Timeout)
    );
    assert!(matches!(
        state::complete(&tx, fixture.scope(), "expired", 901, &Limits::default()).unwrap(),
        McpUrlCompletion::Recorded { .. }
    ));
    tx.commit().unwrap();
    assert_eq!(
        state::inspect(&db, fixture.scope(), expired, &Limits::default())
            .unwrap()
            .consent,
        McpUrlConsent::Pending
    );
}

#[test]
fn concurrent_completion_notifications_commit_once_across_connections() {
    let fixture = Fixture::new();
    let id = fixture.register(&request("raced"));
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for time in [100, 101] {
        let (path, owner, barrier, server, session) = (
            fixture.path.0.clone(),
            fixture.owner.clone(),
            barrier.clone(),
            fixture.server,
            fixture.session,
        );
        threads.push(std::thread::spawn(move || {
            let mut db = connect(&path);
            barrier.wait();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let observed = state::complete(
                &tx,
                state::Scope {
                    owner: &owner,
                    server,
                    session,
                },
                "raced",
                time,
                &Limits::default(),
            )
            .unwrap();
            tx.commit().unwrap();
            observed
        }));
    }
    barrier.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == McpUrlCompletion::Recorded { interaction_id: id })
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == McpUrlCompletion::Ignored)
            .count(),
        1
    );
    let saved = state::inspect(
        &fixture.path.connect(),
        fixture.scope(),
        id,
        &Limits::default(),
    )
    .unwrap();
    assert!([Some(100), Some(101)].contains(&saved.completed_ms));
    assert_eq!(saved.consent, McpUrlConsent::Pending);
}

#[test]
fn competing_responders_have_one_immutable_consent_and_receipt() {
    let fixture = Fixture::new();
    let url = request("two-responders");
    let id = fixture.register(&url);
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for (actor, action) in [("interactive", "accept"), ("headless", "decline")] {
        let (path, owner, barrier, server, session, request_digest) = (
            fixture.path.0.clone(),
            fixture.owner.clone(),
            barrier.clone(),
            fixture.server,
            fixture.session,
            url.params().digest(),
        );
        threads.push(std::thread::spawn(move || {
            let mut db = connect(&path);
            barrier.wait();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let result = state::respond(
                &tx,
                state::Scope {
                    owner: &owner,
                    server,
                    session,
                },
                id,
                request_digest,
                actor,
                "same-local-key",
                &doc(&format!(r#"{{"action":"{action}"}}"#)),
                &SecurityLabels::default(),
                100,
                &Limits::default(),
            );
            if result.is_ok() {
                tx.commit().unwrap();
            }
            result
        }));
    }
    barrier.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(StoreError::IdempotencyConflict))
            .count(),
        1
    );
    let db = fixture.path.connect();
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM mcp_url_receipts", [], |r| r.get(0))
            .unwrap(),
        1
    );
    let saved = state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap();
    assert!(matches!(
        saved.consent,
        McpUrlConsent::Accepted | McpUrlConsent::Declined
    ));
    assert_eq!(saved.completed_ms, None);
}

#[test]
fn completion_ids_are_scoped_and_unknown_notifications_do_not_read_protected_values() {
    let fixture = Fixture::new();
    let id = fixture.register(&request("same-remote-id"));
    let mut db = fixture.path.connect();
    // Deliberately corrupt protected bytes: completing an unknown scoped ID must
    // neither parse this unrelated request nor disclose it in an error.
    db.execute(
        "UPDATE mcp_url_interactions SET params=x'ff' WHERE url_id=?1",
        [id.to_string()],
    )
    .unwrap();
    let other_owner = McpOwner::new(
        HostContext::new("bob".into(), &Limits::default()).unwrap(),
        McpOwnerScope::Host("url-tests".into()),
        1,
        &Limits::default(),
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    for scope in [
        state::Scope {
            owner: &other_owner,
            ..fixture.scope()
        },
        state::Scope {
            server: hash_bytes(b"other-server"),
            ..fixture.scope()
        },
        state::Scope {
            session: hash_bytes(b"other-session"),
            ..fixture.scope()
        },
    ] {
        assert_eq!(
            state::complete(&tx, scope, "same-remote-id", 100, &Limits::default()).unwrap(),
            McpUrlCompletion::Ignored
        );
        assert_eq!(
            state::inspect(&tx, scope, id, &Limits::default()).unwrap_err(),
            StoreError::Missing
        );
    }
    assert_eq!(
        state::complete(&tx, fixture.scope(), "unknown", 100, &Limits::default()).unwrap(),
        McpUrlCompletion::Ignored
    );
    tx.commit().unwrap();
    assert_eq!(
        db.query_row::<Option<i64>, _, _>(
            "SELECT completed_ms FROM mcp_url_interactions",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        None
    );
    assert!(matches!(
        state::inspect(&db, fixture.scope(), id, &Limits::default()),
        Err(StoreError::Corrupt)
    ));
}

#[test]
fn required_list_capacity_or_conflict_rolls_back_every_id_and_original_error() {
    let fixture = Fixture::new();
    let two = required(
        r#"{"code":-32042,"message":"two","data":{"elicitations":[{"mode":"url","elicitationId":"first","url":"https://example.test/1","message":"x"},{"mode":"url","elicitationId":"last","url":"https://example.test/2","message":"x"}]}}"#,
    );
    let mut db = fixture.path.connect();
    {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            state::required(
                &tx,
                fixture.scope(),
                &McpRequestId::Integer(7),
                &two,
                &SecurityLabels::default(),
                900,
                1,
                &Limits::default()
            )
            .unwrap_err(),
            StoreError::Invalid
        );
    }
    for table in [
        "mcp_url_interactions",
        "mcp_url_required_errors",
        "mcp_url_required_links",
    ] {
        assert_eq!(
            db.query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            0
        );
    }
    let first = fixture.record_error(&required_one(), &McpRequestId::Integer(7));
    {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            state::required(
                &tx,
                fixture.scope(),
                &McpRequestId::Integer(7),
                &two,
                &SecurityLabels::default(),
                900,
                16,
                &Limits::default()
            )
            .unwrap_err(),
            StoreError::IdempotencyConflict
        );
    }
    assert_eq!(
        state::original_error(
            &db,
            fixture.scope(),
            &McpRequestId::Integer(7),
            &Limits::default()
        )
        .unwrap()
        .interactions,
        first
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM mcp_url_interactions", [], |r| r
            .get(0))
            .unwrap(),
        1
    );
    assert!(
        state::original_error(
            &db,
            fixture.scope(),
            &McpRequestId::String("7".into()),
            &Limits::default()
        )
        .is_err(),
        "integer and string original RPC identities differ"
    );
}

#[test]
fn real_callback_reply_remains_sole_consent_authority_after_completion() {
    let fixture = Fixture::new();
    let callback = actix::System::new().block_on(async {
        let store = SqliteStore::open(&fixture.path.0, StoreConfig::default())
            .await
            .unwrap();
        let raw = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"elicitation/create","params":{}}}"#,
            std::str::from_utf8(request("callback-id").params().as_bytes()).unwrap()
        )
        .into_bytes();
        let callback = store
            .receive_mcp_interaction(
                &fixture.owner,
                fixture.session,
                raw,
                labels("callback"),
                900,
                30,
                16,
            )
            .await
            .unwrap();
        store
            .begin_mcp_work(&fixture.owner, callback.id, 40, 16, 16)
            .await
            .unwrap();
        store.close().await.unwrap();
        callback
    });
    let mut db = fixture.path.connect();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let id = state::register(
        &tx,
        fixture.scope(),
        &request("callback-id"),
        Some(callback.id),
        &callback.labels,
        900,
        16,
        &Limits::default(),
    )
    .unwrap();
    state::complete(&tx, fixture.scope(), "callback-id", 50, &Limits::default()).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        state::inspect(&db, fixture.scope(), id, &Limits::default())
            .unwrap()
            .consent,
        McpUrlConsent::Pending
    );
    drop(db);
    let reply = br#"{"jsonrpc":"2.0","id":1,"result":{"action":"accept"}}"#.to_vec();
    actix::System::new().block_on(async {
        let store = SqliteStore::open(&fixture.path.0, StoreConfig::default())
            .await
            .unwrap();
        store
            .submit_mcp_reply(
                &fixture.owner,
                callback.id,
                fixture.owner.host(),
                "accepted",
                reply.clone(),
                labels("operator"),
                60,
            )
            .await
            .unwrap();
        let saved = store
            .mcp_interaction(&fixture.owner, callback.id)
            .await
            .unwrap();
        assert_eq!(saved.reply, Some(reply));
        assert_eq!(saved.state, McpWorkState::Result);
        assert_eq!(
            store.mcp_owner_usage(&fixture.owner).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 1,
                outstanding: 0
            }
        );
        store.close().await.unwrap();
    });
    let saved = state::inspect(
        &fixture.path.connect(),
        fixture.scope(),
        id,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(saved.consent, McpUrlConsent::Accepted);
    assert_eq!(saved.completed_ms, Some(50));
    assert_eq!(
        saved.labels,
        labels("callback")
            .union(&labels("operator"), &Limits::default())
            .unwrap()
    );
}

#[test]
fn completion_ingress_and_consent_reject_invalid_envelopes_and_in_band_credentials() {
    let notification = doc(
        r#"{"jsonrpc":"2.0","method":"notifications/elicitation/complete","params":{"elicitationId":"known"}}"#,
    );
    assert_eq!(validate_url_completion(&notification).unwrap(), "known");
    for raw in [
        r#"{"jsonrpc":"2.0","id":1,"method":"notifications/elicitation/complete","params":{"elicitationId":"known"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/elicitation/complete","params":{"elicitationId":1}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/elicitation/complete","params":{"elicitationId":"known","_meta":false}}"#,
    ] {
        assert!(validate_url_completion(&doc(raw)).is_err());
    }
    for raw in [
        r#"{"action":"accept","content":{"apiKey":"secret"}}"#,
        r#"{"action":"decline","content":{}}"#,
        r#"{"action":"completed"}"#,
    ] {
        assert!(McpUrlConsent::from_result(&doc(raw)).is_err());
    }
    assert_eq!(
        McpUrlConsent::from_result(&doc(r#"{"action":"cancel"}"#)).unwrap(),
        McpUrlConsent::Cancelled
    );
    assert_eq!(McpUrlConsent::parse("unrecognized"), None);
    assert_eq!(
        McpUrlConsent::parse(McpUrlConsent::Pending.as_str()),
        Some(McpUrlConsent::Pending)
    );
    assert!(matches!(
        doc(r#"{"action":"accept"}"#).value(),
        Value::Map(_)
    ));
}

#[test]
fn replay_cannot_extend_deadlines_lower_protection_or_replace_url_metadata() {
    let fixture = Fixture::new();
    let original = request("immutable");
    let id = fixture.register(&original);
    let mut db = fixture.path.connect();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(
        state::register(
            &tx,
            fixture.scope(),
            &original,
            None,
            &labels("additional"),
            5000,
            16,
            &Limits::default()
        )
        .unwrap(),
        id
    );
    tx.commit().unwrap();
    let saved = state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap();
    assert_eq!(saved.deadline_ms, 900);
    assert_eq!(
        saved.request.params().as_bytes(),
        original.params().as_bytes()
    );
    assert_eq!(
        saved.labels,
        labels("request")
            .union(&labels("additional"), &Limits::default())
            .unwrap()
    );
    let changed = McpUrlRequest::from_params(doc(r#"{"mode":"url","elicitationId":"immutable","url":"https://attacker.test/changed","message":"changed"}"#).value(), &Limits::default()).unwrap();
    {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            state::register(
                &tx,
                fixture.scope(),
                &changed,
                None,
                &SecurityLabels::default(),
                5000,
                16,
                &Limits::default()
            )
            .unwrap_err(),
            StoreError::IdempotencyConflict
        );
    }
    assert_eq!(
        state::inspect(&db, fixture.scope(), id, &Limits::default())
            .unwrap()
            .request
            .url(),
        original.url()
    );
    db.execute(
        "UPDATE mcp_url_interactions SET params=zeroblob(?1) WHERE url_id=?2",
        rusqlite::params![Limits::default().max_document_bytes / 8 + 1, id.to_string()],
    )
    .unwrap();
    assert_eq!(
        state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap_err(),
        StoreError::Corrupt
    );
}

#[test]
fn completion_racing_a_real_callback_response_preserves_both_facts() {
    let fixture = Fixture::new();
    let callback = actix::System::new().block_on(async {
        let store = SqliteStore::open(&fixture.path.0, StoreConfig::default())
            .await
            .unwrap();
        let raw = format!(
            r#"{{"jsonrpc":"2.0","id":"race","method":"elicitation/create","params":{}}}"#,
            std::str::from_utf8(request("reply-race").params().as_bytes()).unwrap()
        )
        .into_bytes();
        let callback = store
            .receive_mcp_interaction(
                &fixture.owner,
                fixture.session,
                raw,
                labels("callback"),
                900,
                30,
                16,
            )
            .await
            .unwrap();
        store
            .begin_mcp_work(&fixture.owner, callback.id, 40, 16, 16)
            .await
            .unwrap();
        store.close().await.unwrap();
        callback
    });
    let mut db = fixture.path.connect();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let id = state::register(
        &tx,
        fixture.scope(),
        &request("reply-race"),
        Some(callback.id),
        &callback.labels,
        900,
        16,
        &Limits::default(),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let barrier = Arc::new(Barrier::new(2));
    let (path, owner, gate, server, session) = (
        fixture.path.0.clone(),
        fixture.owner.clone(),
        barrier.clone(),
        fixture.server,
        fixture.session,
    );
    let completion = std::thread::spawn(move || {
        let mut db = connect(&path);
        gate.wait();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let completed = state::complete(
            &tx,
            state::Scope {
                owner: &owner,
                server,
                session,
            },
            "reply-race",
            70,
            &Limits::default(),
        )
        .unwrap();
        tx.commit().unwrap();
        completed
    });
    actix::System::new().block_on(async {
        let store = SqliteStore::open(&fixture.path.0, StoreConfig::default())
            .await
            .unwrap();
        barrier.wait();
        store
            .submit_mcp_reply(
                &fixture.owner,
                callback.id,
                fixture.owner.host(),
                "raced-response",
                br#"{"jsonrpc":"2.0","id":"race","result":{"action":"decline"}}"#.to_vec(),
                labels("human"),
                70,
            )
            .await
            .unwrap();
        store.close().await.unwrap();
    });
    assert_eq!(
        completion.join().unwrap(),
        McpUrlCompletion::Recorded { interaction_id: id }
    );
    let db = fixture.path.connect();
    let saved = state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap();
    assert_eq!(
        saved.consent,
        McpUrlConsent::Declined,
        "completion cannot change a refusal into consent"
    );
    assert_eq!(saved.completed_ms, Some(70));
    db.execute(
        "UPDATE mcp_interactions SET reply=?1 WHERE interaction_id=?2",
        rusqlite::params![
            br#"{"jsonrpc":"2.0","id":"race","result":{"action":"accept"}}"#,
            callback.id.to_string()
        ],
    )
    .unwrap();
    assert_eq!(
        state::inspect(&db, fixture.scope(), id, &Limits::default()).unwrap_err(),
        StoreError::Corrupt,
        "a tampered callback reply cannot create URL consent"
    );
}

#[test]
fn fenced_url_store_persists_races_receipts_and_epoch_loss_without_reinterpreting_old_schema() {
    let fixture = Fixture::new();
    actix::System::new().block_on(async {
        let first = SqliteStore::open(&fixture.path.0, StoreConfig::default())
            .await
            .unwrap();
        let second = SqliteStore::open(&fixture.path.0, StoreConfig::default())
            .await
            .unwrap();
        let original = required_one();
        let ids = first
            .record_mcp_url_required_error(
                &fixture.owner,
                fixture.server,
                fixture.session,
                McpRequestId::Integer(1),
                original.error().clone(),
                None,
                labels("original"),
                900,
                30,
                16,
            )
            .await
            .unwrap();
        let id = ids[0];
        assert_eq!(
            second
                .mcp_url_addresses(&fixture.owner, None, 8)
                .await
                .unwrap(),
            vec![(id, fixture.server, fixture.session)]
        );
        let (one, two) = tokio::join!(
            first.complete_mcp_url_interaction(
                &fixture.owner,
                fixture.server,
                fixture.session,
                "needed",
                40
            ),
            second.complete_mcp_url_interaction(
                &fixture.owner,
                fixture.server,
                fixture.session,
                "needed",
                40
            ),
        );
        let observations = [one.unwrap(), two.unwrap()];
        assert_eq!(
            observations
                .iter()
                .filter(|value| matches!(value, McpUrlCompletion::Recorded { .. }))
                .count(),
            1
        );
        assert_eq!(
            observations
                .iter()
                .filter(|value| **value == McpUrlCompletion::Ignored)
                .count(),
            1
        );
        let view = second
            .mcp_url_interaction(&fixture.owner, fixture.server, fixture.session, id)
            .await
            .unwrap();
        assert_eq!(view.consent, McpUrlConsent::Pending);
        let (one, two) = tokio::join!(
            first.submit_mcp_url_consent(
                &fixture.owner,
                fixture.server,
                fixture.session,
                id,
                view.request.params().digest(),
                fixture.owner.host(),
                "interactive",
                doc(r#"{"action":"accept"}"#),
                labels("human"),
                50
            ),
            second.submit_mcp_url_consent(
                &fixture.owner,
                fixture.server,
                fixture.session,
                id,
                view.request.params().digest(),
                fixture.owner.host(),
                "headless",
                doc(r#"{"action":"decline"}"#),
                labels("human"),
                50
            ),
        );
        assert_eq!(
            [one.as_ref(), two.as_ref()]
                .iter()
                .filter(|result| result.is_ok())
                .count(),
            1
        );
        let (key, result, receipt) = if let Ok(receipt) = one {
            ("interactive", doc(r#"{"action":"accept"}"#), receipt)
        } else {
            ("headless", doc(r#"{"action":"decline"}"#), two.unwrap())
        };
        first
            .close_mcp_epoch(&fixture.owner, fixture.session, 60)
            .await
            .unwrap();
        assert_eq!(
            second
                .submit_mcp_url_consent(
                    &fixture.owner,
                    fixture.server,
                    fixture.session,
                    id,
                    view.request.params().digest(),
                    fixture.owner.host(),
                    key,
                    result.clone(),
                    labels("human"),
                    950
                )
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(
            second
                .complete_mcp_url_interaction(
                    &fixture.owner,
                    fixture.server,
                    fixture.session,
                    "unknown",
                    70
                )
                .await
                .unwrap_err(),
            StoreError::Fenced,
            "transport closure fences new notifications"
        );
        let saved = second
            .mcp_url_interaction(&fixture.owner, fixture.server, fixture.session, id)
            .await
            .unwrap();
        assert!(saved.session_closed);
        assert_eq!(saved.completed_ms, Some(40));
        assert_eq!(
            second
                .mcp_url_required_error(
                    &fixture.owner,
                    fixture.server,
                    fixture.session,
                    McpRequestId::Integer(1)
                )
                .await
                .unwrap()
                .error
                .as_bytes(),
            original.error().as_bytes()
        );
        first.release_mcp_owner(&fixture.owner, 1000).await.unwrap();
        let fresh = second
            .acquire_mcp_owner(fixture.owner.host(), "url-tests", 1001, 20_000)
            .await
            .unwrap();
        assert_eq!(
            first
                .complete_mcp_url_interaction(
                    &fixture.owner,
                    fixture.server,
                    fixture.session,
                    "needed",
                    1002
                )
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        assert_eq!(
            first
                .complete_mcp_url_interaction(
                    &fresh,
                    fixture.server,
                    fixture.session,
                    "needed",
                    1002
                )
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        assert_eq!(
            second
                .submit_mcp_url_consent(
                    &fresh,
                    fixture.server,
                    fixture.session,
                    id,
                    view.request.params().digest(),
                    fixture.owner.host(),
                    key,
                    result,
                    labels("human"),
                    1002
                )
                .await
                .unwrap(),
            receipt,
            "historical exact receipts grant no new delivery"
        );
        first.close().await.unwrap();
        second.close().await.unwrap();
        fixture
            .path
            .connect()
            .pragma_update(None, "user_version", 11)
            .unwrap();
        assert!(matches!(
            SqliteStore::open(&fixture.path.0, StoreConfig::default()).await,
            Err(StoreError::Schema)
        ));
    });
}

#[test]
fn fenced_store_rejects_partial_requirements_and_tracks_callback_consent_from_existing_reply() {
    let fixture = Fixture::new();
    actix::System::new().block_on(async {
        let store = SqliteStore::open(&fixture.path.0, StoreConfig::default()).await.unwrap();
        let original = required(r#"{"code":-32042,"message":"two","data":{"elicitations":[{"mode":"url","elicitationId":"first","url":"https://example.test/1","message":"x"},{"mode":"url","elicitationId":"last","url":"https://example.test/2","message":"x"}]}}"#);
        assert_eq!(store.record_mcp_url_required_error(&fixture.owner, fixture.server, fixture.session, McpRequestId::Integer(4), original.error().clone(), None, labels("original"), 900, 30, 1).await.unwrap_err(), StoreError::Invalid);
        assert!(store.mcp_url_addresses(&fixture.owner, None, 8).await.unwrap().is_empty());
        assert!(matches!(store.mcp_url_required_error(&fixture.owner, fixture.server, fixture.session, McpRequestId::Integer(4)).await, Err(StoreError::Missing)));
        let raw = format!(r#"{{"jsonrpc":"2.0","id":7,"method":"elicitation/create","params":{}}}"#, std::str::from_utf8(request("public-callback").params().as_bytes()).unwrap()).into_bytes();
        let callback = store.receive_mcp_interaction(&fixture.owner, fixture.session, raw, labels("callback"), 900, 40, 16).await.unwrap();
        assert_eq!(store.track_mcp_url_callback(&fixture.owner, hash_bytes(b"wrong-server"), fixture.session, callback.id, 41, 16).await.unwrap_err(), StoreError::Fenced);
        let id = store.track_mcp_url_callback(&fixture.owner, fixture.server, fixture.session, callback.id, 42, 16).await.unwrap();
        store.begin_mcp_work(&fixture.owner, callback.id, 43, 16, 16).await.unwrap();
        store.submit_mcp_reply(&fixture.owner, callback.id, fixture.owner.host(), "human", br#"{"jsonrpc":"2.0","id":7,"result":{"action":"accept"}}"#.to_vec(), labels("human"), 44).await.unwrap();
        let view = store.mcp_url_interaction(&fixture.owner, fixture.server, fixture.session, id).await.unwrap();
        assert_eq!(view.callback_id, Some(callback.id));
        assert_eq!(view.consent, McpUrlConsent::Accepted);
        assert_eq!(view.completed_ms, None);
        assert_eq!(store.submit_mcp_url_consent(&fixture.owner, fixture.server, fixture.session, id, view.request.params().digest(), fixture.owner.host(), "second-ledger", doc(r#"{"action":"accept"}"#), SecurityLabels::default(), 45).await.unwrap_err(), StoreError::Invalid, "callbacks retain a single response ledger");
        store.close().await.unwrap();
        let reopened = SqliteStore::open(&fixture.path.0, StoreConfig::default()).await.unwrap();
        assert_eq!(reopened.mcp_url_interaction(&fixture.owner, fixture.server, fixture.session, id).await.unwrap().consent, McpUrlConsent::Accepted);
        reopened.close().await.unwrap();
    });
}
