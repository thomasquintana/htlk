//! Trusted SQLite coordination infrastructure. This is a storage boundary for
//! coordinators, not an authenticated host API. It exposes no raw SQL connection.
//! Database transactions, not actor mailboxes, establish fenced run authority.

use crate::{
    failure::FailureCode,
    identity::{RunId, ScopeInstanceId},
    records::RunStatus,
};
use htlk_executable::{
    cbor::{self, Limits, Value},
    digest::Digest,
};
use std::{fmt, path::Path, time::Duration};
use tokio_rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const SCHEMA_VERSION: i64 = 1;
const APPLICATION_ID: i64 = 0x48544c4b;
const SCHEMA: &str = include_str!("store.sql");

/// Trusted database settings. SQLite value data and journal payloads are bounded
/// by these codec limits; queue/read page limits are independent and finite.
#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// Complete record/journal byte and depth ceilings.
    pub limits: Limits,
    /// How long a competing SQLite writer may delay an operation.
    pub busy_timeout: Duration,
    /// Maximum rows returned by a single journal or outbox read.
    pub max_page_size: usize,
}
impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            busy_timeout: Duration::from_secs(5),
            max_page_size: 256,
        }
    }
}

/// Cloneable asynchronous SQLite storage handle. Clones share the database worker;
/// separately opened handles are protected by SQLite transactions and run fences.
#[derive(Clone)]
pub struct SqliteStore {
    connection: Connection,
    config: StoreConfig,
}
impl SqliteStore {
    /// Opens and migrates a durable store using WAL, FULL synchronization and
    /// foreign keys. An unrelated/nonempty or newer database is rejected.
    ///
    /// # Errors
    /// Returns invalid configuration, database identity/version or SQLite failures.
    pub async fn open(path: impl AsRef<Path>, config: StoreConfig) -> Result<Self, StoreError> {
        validate_config(&config)?;
        let connection = Connection::open(path).await?;
        Self::initialize(connection, config, false).await
    }
    /// Opens an isolated ephemeral store with the same transactional semantics.
    /// Its contents disappear when all handles close; it is not durable recovery.
    ///
    /// # Errors
    /// Returns invalid configuration or SQLite failures.
    pub async fn in_memory(config: StoreConfig) -> Result<Self, StoreError> {
        validate_config(&config)?;
        Self::initialize(Connection::open_in_memory().await?, config, true).await
    }
    async fn initialize(
        connection: Connection,
        config: StoreConfig,
        memory: bool,
    ) -> Result<Self, StoreError> {
        let busy = config.busy_timeout;
        connection
            .call(move |db| -> Result<(), StoreError> {
                db.busy_timeout(busy)?;
                db.pragma_update(None, "foreign_keys", true)?;
                db.pragma_update(None, "synchronous", "FULL")?;
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let application: i64 =
                    tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
                let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
                if version == 0 && application == 0 {
                    let tables: i64 = tx.query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                        [],
                        |r| r.get(0),
                    )?;
                    if tables != 0 {
                        return Err(StoreError::Schema);
                    }
                    tx.execute_batch(SCHEMA)?;
                    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
                    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                } else if application != APPLICATION_ID || version != SCHEMA_VERSION {
                    return Err(StoreError::Schema);
                }
                tx.commit()?;
                if !memory {
                    db.pragma_update(None, "journal_mode", "WAL")?;
                    let mode: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
                    if mode != "wal" {
                        return Err(StoreError::Schema);
                    }
                }
                Ok(())
            })
            .await
            .map_err(StoreError::from_worker)?;
        Ok(Self { connection, config })
    }
    /// Returns the host-selected bounded database settings.
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }
    /// Closes this connection and its clones after queued operations finish.
    /// Separately opened database connections remain usable.
    ///
    /// # Errors
    /// Returns an underlying connection close failure.
    pub async fn close(self) -> Result<(), StoreError> {
        self.connection
            .close()
            .await
            .map_err(|_| StoreError::Closed)
    }

    /// Creates the unfenced run authority row and initial journal/outbox atomically.
    /// The calling service must already validate graph, inputs, principal and host
    /// idempotency. Run IDs are unique; this low-level operation does not infer a
    /// principal-scoped host receipt from equal arguments.
    ///
    /// # Errors
    /// Returns duplicate run, invalid time, codec or database failures.
    pub async fn create_run(&self, seed: RunSeed) -> Result<(), StoreError> {
        timestamp(seed.timestamp_ms)?;
        self.check_text(seed.run_id.as_str())?;
        self.check_text(&seed.actor)?;
        self.check_text(&seed.causation)?;
        let limits = self.config.limits.clone();
        let payload = cbor::encode(
            &Value::Array(vec![
                Value::Text(seed.fingerprint.to_string()),
                Value::Text(seed.root_scope.to_string()),
            ]),
            &self.config.limits,
        )?;
        self.connection.call(move |db| -> Result<(), StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let inserted = tx.execute("INSERT OR IGNORE INTO runs(run_id,fingerprint,root_scope,status,sequence,fence,last_time_ms) VALUES(?1,?2,?3,'running',1,0,?4)", params![seed.run_id.as_str(), seed.fingerprint.to_string(), seed.root_scope.to_string(), seed.timestamp_ms])?;
            if inserted != 1 { return Err(StoreError::Conflict); }
            append_event(&tx, seed.run_id.as_str(), 1, EventMeta { kind: "run_created", actor: &seed.actor, timestamp: seed.timestamp_ms, causation: &seed.causation, command: None }, &payload, &limits)?;
            tx.commit()?;
            Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    /// Acquires an unowned or expired run with a strictly higher fence. Owner names
    /// must identify coordinator instances, not reusable user/session identities.
    /// Time is clamped to the last committed coordinator timestamp.
    ///
    /// # Errors
    /// Returns active ownership, terminal run, invalid time or database failures.
    pub async fn acquire(
        &self,
        run: &RunId,
        owner: String,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> Result<RunLease, StoreError> {
        check_lease_times(now_ms, expires_at_ms)?;
        self.check_text(&owner)?;
        let limits = self.config.limits.clone();
        let run = run.clone();
        self.connection.call(move |db| -> Result<RunLease, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let header = load_header(&tx, &run)?;
            if header.status.is_terminal() { return Err(StoreError::Terminal); }
            let now = now_ms.max(header.last_time_ms);
            if expires_at_ms <= now { return Err(StoreError::Invalid); }
            if header.owner.is_some() && header.expires_at_ms.is_some_and(|expiry| expiry > now) { return Err(StoreError::Fenced); }
            let fence = header.fence.checked_add(1).ok_or(StoreError::Invalid)?;
            let sequence = header.sequence.checked_add(1).ok_or(StoreError::Invalid)?;
            tx.execute("UPDATE runs SET owner=?2,fence=?3,lease_expires_ms=?4,sequence=?5,last_time_ms=?6 WHERE run_id=?1", params![run.as_str(), owner, fence, expires_at_ms, sequence, now])?;
            let payload = lease_payload(&owner, fence, expires_at_ms, &limits)?;
            append_event(&tx, run.as_str(), sequence, EventMeta { kind: "ownership_acquired", actor: &owner, timestamp: now, causation: "ownership", command: None }, &payload, &limits)?;
            tx.commit()?;
            Ok(RunLease { run_id: run, owner, fence })
        }).await.map_err(StoreError::from_worker)
    }

    /// Extends current ownership without changing its fence. Expired or superseded
    /// owners cannot renew; reacquisition requires a new fence.
    ///
    /// # Errors
    /// Returns stale authority, terminal run, invalid time or database failures.
    pub async fn renew(
        &self,
        lease: &RunLease,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> Result<(), StoreError> {
        check_lease_times(now_ms, expires_at_ms)?;
        let limits = self.config.limits.clone();
        let lease = lease.clone();
        self.connection.call(move |db| -> Result<(), StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let header = load_header(&tx, &lease.run_id)?;
            let now = now_ms.max(header.last_time_ms);
            check_authority(&header, &lease, now)?;
            if header.status.is_terminal() { return Err(StoreError::Terminal); }
            if expires_at_ms <= now || header.expires_at_ms.is_some_and(|old| expires_at_ms < old) { return Err(StoreError::Invalid); }
            let sequence = header.sequence.checked_add(1).ok_or(StoreError::Invalid)?;
            tx.execute("UPDATE runs SET lease_expires_ms=?2,sequence=?3,last_time_ms=?4 WHERE run_id=?1", params![lease.run_id.as_str(), expires_at_ms, sequence, now])?;
            let payload = lease_payload(&lease.owner, lease.fence, expires_at_ms, &limits)?;
            append_event(&tx, lease.run_id.as_str(), sequence, EventMeta { kind: "ownership_renewed", actor: &lease.owner, timestamp: now, causation: "ownership", command: None }, &payload, &limits)?;
            tx.commit()?;
            Ok(())
        }).await.map_err(StoreError::from_worker)
    }
    /// Reads the authoritative run authority header.
    ///
    /// # Errors
    /// Returns unknown run, corrupt record or database failure.
    pub async fn run_header(&self, run: &RunId) -> Result<RunHeader, StoreError> {
        let run = run.clone();
        self.connection
            .call(move |db| load_header(db, &run))
            .await
            .map_err(StoreError::from_worker)
    }
    fn check_text(&self, text: &str) -> Result<(), StoreError> {
        htlk_executable::record_accounting::RecordAccounting::new(&self.config.limits)?
            .text(text, 0)
            .map_err(|_| StoreError::Invalid)
    }
}

/// Validated run identity and graph selected by the calling runtime service.
#[derive(Clone, Debug)]
pub struct RunSeed {
    /// Unique host-generated run ID.
    pub run_id: RunId,
    /// Registered initial executable fingerprint.
    pub fingerprint: Digest,
    /// Deterministic initial root identity.
    pub root_scope: ScopeInstanceId,
    /// Authenticated or trusted actor attribution.
    pub actor: String,
    /// Host request/command causation identity.
    pub causation: String,
    /// Authoritative nonnegative coordinator timestamp.
    pub timestamp_ms: i64,
}
/// Fenced authority returned by a successful ownership acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLease {
    run_id: RunId,
    owner: String,
    fence: i64,
}
impl RunLease {
    /// Owned run.
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    /// Exact coordinator instance name.
    pub fn owner(&self) -> &str {
        &self.owner
    }
    /// Monotonically increasing authority generation.
    pub const fn fence(&self) -> i64 {
        self.fence
    }
}
/// Authoritative run header; no derived scheduler state is stored here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunHeader {
    /// Unique run identity.
    pub run_id: RunId,
    /// Active registered executable.
    pub fingerprint: Digest,
    /// Current root scope instance.
    pub root_scope: ScopeInstanceId,
    /// Immutable if terminal.
    pub status: RunStatus,
    /// Last committed journal sequence.
    pub sequence: i64,
    /// Current coordinator instance, if acquired.
    pub owner: Option<String>,
    /// Current fencing generation.
    pub fence: i64,
    /// Persisted coordinator lease expiry.
    pub expires_at_ms: Option<i64>,
    /// Nondecreasing authoritative timestamp.
    pub last_time_ms: i64,
}

fn load_header(
    db: &tokio_rusqlite::rusqlite::Connection,
    run: &RunId,
) -> Result<RunHeader, StoreError> {
    let row = db.query_row("SELECT fingerprint,root_scope,status,sequence,owner,fence,lease_expires_ms,last_time_ms FROM runs WHERE run_id=?1", [run.as_str()], |r| {
        Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,i64>(7)?))
    }).optional()?.ok_or(StoreError::Missing)?;
    let status = match row.2.as_str() {
        "running" => RunStatus::Running,
        "succeeded" => RunStatus::Succeeded,
        "failed" => RunStatus::Failed,
        "cancelled" => RunStatus::Cancelled,
        _ => return Err(StoreError::Corrupt),
    };
    Ok(RunHeader {
        run_id: run.clone(),
        fingerprint: row.0.parse().map_err(|_| StoreError::Corrupt)?,
        root_scope: row.1.parse().map_err(|_| StoreError::Corrupt)?,
        status,
        sequence: row.3,
        owner: row.4,
        fence: row.5,
        expires_at_ms: row.6,
        last_time_ms: row.7,
    })
}
fn check_authority(header: &RunHeader, lease: &RunLease, now: i64) -> Result<(), StoreError> {
    if header.fence != lease.fence
        || header.owner.as_deref() != Some(&lease.owner)
        || header.expires_at_ms.is_none_or(|expiry| now >= expiry)
    {
        return Err(StoreError::Fenced);
    }
    Ok(())
}
fn timestamp(time: i64) -> Result<(), StoreError> {
    if time < 0 {
        Err(StoreError::Invalid)
    } else {
        Ok(())
    }
}
fn check_lease_times(now: i64, expiry: i64) -> Result<(), StoreError> {
    timestamp(now)?;
    if expiry <= now {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
fn validate_config(config: &StoreConfig) -> Result<(), StoreError> {
    config.limits.validate()?;
    if config.max_page_size == 0
        || config.max_page_size > 4096
        || config.busy_timeout > Duration::from_secs(60)
    {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
fn lease_payload(
    owner: &str,
    fence: i64,
    expiry: i64,
    limits: &Limits,
) -> Result<Vec<u8>, StoreError> {
    Ok(cbor::encode(
        &Value::Array(vec![
            Value::Text(owner.into()),
            Value::Integer(fence),
            Value::Integer(expiry),
        ]),
        limits,
    )?)
}
struct EventMeta<'a> {
    kind: &'a str,
    actor: &'a str,
    timestamp: i64,
    causation: &'a str,
    command: Option<&'a str>,
}
fn append_event(
    tx: &tokio_rusqlite::Transaction<'_>,
    run: &str,
    sequence: i64,
    event: EventMeta<'_>,
    payload: &[u8],
    limits: &Limits,
) -> Result<(), StoreError> {
    // Journal rows are private records. Bound the complete logical row before it
    // becomes authoritative, including the opaque already-bounded payload bytes.
    let representation = Value::Array(vec![
        Value::Text(run.into()),
        Value::Integer(sequence),
        Value::Text(event.kind.into()),
        Value::Text("0.1".into()),
        Value::Text(event.actor.into()),
        Value::Integer(event.timestamp),
        Value::Text(event.causation.into()),
        event.command.map_or(Value::Null, |s| Value::Text(s.into())),
        Value::Bytes(payload.to_vec()),
    ]);
    htlk_executable::record_accounting::RecordAccounting::new(limits)?
        .value(&representation, 0)
        .map_err(|_| StoreError::Invalid)?;
    tx.execute("INSERT INTO journal(run_id,sequence,event_type,record_version,actor,timestamp_ms,causation,command_id,payload) VALUES(?1,?2,?3,'0.1',?4,?5,?6,?7,?8)", params![run,sequence,event.kind,event.actor,event.timestamp,event.causation,event.command,payload])?;
    tx.execute(
        "INSERT INTO outbox(run_id,sequence) VALUES(?1,?2)",
        params![run, sequence],
    )?;
    Ok(())
}

/// Storage failure categories. Display/Debug omit database paths, record contents
/// and backend diagnostic text; the trusted host chooses diagnostic logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreError {
    /// Backend database operation failed.
    Database,
    /// Connection worker is unavailable.
    Closed,
    /// Database identity/version is incompatible.
    Schema,
    /// Requested record does not exist.
    Missing,
    /// Duplicate identity or stale expected revision.
    Conflict,
    /// Coordinator authority expired or was superseded.
    Fenced,
    /// Run or record already committed its terminal state.
    Terminal,
    /// Invalid configuration, timestamp or bounded metadata.
    Invalid,
    /// Stored representation violates an integrity invariant.
    Corrupt,
}
impl StoreError {
    /// Runtime classification for operational handling; missing/conflict detail
    /// remains a host API concern rather than being guessed from backend text.
    pub const fn code(self) -> FailureCode {
        match self {
            Self::Fenced => FailureCode::Fenced,
            Self::Corrupt => FailureCode::Digest,
            Self::Invalid => FailureCode::Input,
            _ => FailureCode::Invariant,
        }
    }
    fn from_worker(error: tokio_rusqlite::Error<Self>) -> Self {
        match error {
            tokio_rusqlite::Error::Error(error) => error,
            _ => Self::Closed,
        }
    }
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database => "runtime database operation failed",
            Self::Closed => "runtime database connection is closed",
            Self::Schema => "runtime database schema is incompatible",
            Self::Missing => "runtime record was not found",
            Self::Conflict => "runtime record conflicts with committed state",
            Self::Fenced => "runtime coordinator authority is stale",
            Self::Terminal => "runtime state is already terminal",
            Self::Invalid => "invalid runtime storage request",
            Self::Corrupt => "runtime stored data failed integrity checks",
        })
    }
}
impl std::error::Error for StoreError {}
impl From<tokio_rusqlite::rusqlite::Error> for StoreError {
    fn from(_: tokio_rusqlite::rusqlite::Error) -> Self {
        Self::Database
    }
}
impl From<cbor::Error> for StoreError {
    fn from(_: cbor::Error) -> Self {
        Self::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TempDb(PathBuf);
    impl TempDb {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "htlk-store-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn path(&self) -> PathBuf {
            self.0.join("runtime.sqlite")
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn seed(name: &str) -> RunSeed {
        let run_id = RunId::new(name.into(), &Limits::default()).unwrap();
        RunSeed {
            root_scope: ScopeInstanceId::root(&run_id, &Limits::default()).unwrap(),
            run_id,
            fingerprint: Digest::from_bytes([1; 32]),
            actor: "principal".into(),
            causation: "start-request".into(),
            timestamp_ms: 0,
        }
    }
    async fn counts(store: &SqliteStore) -> (i64, i64, i64) {
        store
            .connection
            .call(|db| -> Result<_, StoreError> {
                Ok((
                    db.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?,
                    db.query_row("SELECT count(*) FROM journal", [], |r| r.get(0))?,
                    db.query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?,
                ))
            })
            .await
            .unwrap()
    }

    #[test]
    fn creation_is_atomic_with_journal_and_outbox_even_when_notification_insert_fails() {
        actix::System::new().block_on(async {
            let store = SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
            store.connection.call(|db| db.execute_batch("CREATE TRIGGER fail_outbox BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT,'injected'); END;")).await.unwrap();
            assert_eq!(store.create_run(seed("run")).await, Err(StoreError::Database));
            assert_eq!(counts(&store).await,(0,0,0));
            store.connection.call(|db| db.execute_batch("DROP TRIGGER fail_outbox;")).await.unwrap();
            store.create_run(seed("run")).await.unwrap();
            assert_eq!(counts(&store).await,(1,1,1));
            assert_eq!(store.create_run(seed("run")).await, Err(StoreError::Conflict));
            assert_eq!(counts(&store).await,(1,1,1));
        });
    }

    #[test]
    fn durable_reopen_and_separate_connections_preserve_fenced_ownership() {
        actix::System::new().block_on(async {
            let temporary = TempDb::new();
            let first = SqliteStore::open(temporary.path(), StoreConfig::default())
                .await
                .unwrap();
            let run = seed("run");
            first.create_run(run.clone()).await.unwrap();
            let old = first
                .acquire(&run.run_id, "coordinator-a".into(), 10, 20)
                .await
                .unwrap();
            first.renew(&old, 11, 30).await.unwrap();
            let second = SqliteStore::open(temporary.path(), StoreConfig::default())
                .await
                .unwrap();
            assert_eq!(
                second
                    .acquire(&run.run_id, "coordinator-b".into(), 29, 40)
                    .await,
                Err(StoreError::Fenced)
            );
            let current = second
                .acquire(&run.run_id, "coordinator-b".into(), 30, 40)
                .await
                .unwrap();
            assert_eq!(current.fence(), old.fence() + 1);
            assert_eq!(first.renew(&old, 31, 50).await, Err(StoreError::Fenced));
            // A backwards wall-clock observation cannot move committed time back.
            second.renew(&current, 25, 50).await.unwrap();
            let expected = second.run_header(&run.run_id).await.unwrap();
            assert_eq!(expected.last_time_ms, 30);
            assert_eq!(expected.expires_at_ms, Some(50));
            first.close().await.unwrap();
            second.close().await.unwrap();
            let reopened = SqliteStore::open(temporary.path(), StoreConfig::default())
                .await
                .unwrap();
            assert_eq!(reopened.run_header(&run.run_id).await.unwrap(), expected);
            let settings = reopened
                .connection
                .call(|db| -> Result<_, StoreError> {
                    Ok((
                        db.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))?,
                        db.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))?,
                        db.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))?,
                    ))
                })
                .await
                .unwrap();
            assert_eq!(settings, ("wal".into(), 2, true));
            reopened.close().await.unwrap();
        });
    }

    #[test]
    fn concurrent_acquisitions_have_one_winner_and_expired_owners_cannot_renew() {
        actix::System::new().block_on(async {
            let temporary = TempDb::new();
            let a = SqliteStore::open(temporary.path(), StoreConfig::default())
                .await
                .unwrap();
            let b = SqliteStore::open(temporary.path(), StoreConfig::default())
                .await
                .unwrap();
            let run = seed("run");
            a.create_run(run.clone()).await.unwrap();
            let run_a = run.run_id.clone();
            let run_b = run.run_id.clone();
            let a = a.clone();
            let b = b.clone();
            let left = actix::spawn(async move { a.acquire(&run_a, "a".into(), 0, 10).await });
            let right = actix::spawn(async move { b.acquire(&run_b, "b".into(), 0, 10).await });
            let left = left.await.unwrap();
            let right = right.await.unwrap();
            assert_ne!(left.is_ok(), right.is_ok());
            let winner = left.or(right).unwrap();
            let store = SqliteStore::open(temporary.path(), StoreConfig::default())
                .await
                .unwrap();
            assert_eq!(store.renew(&winner, 10, 20).await, Err(StoreError::Fenced));
            assert_eq!(counts(&store).await, (1, 2, 2));
            store.close().await.unwrap();
        });
    }

    #[test]
    fn failed_fence_commit_rolls_back_owner_sequence_and_timestamp_together() {
        actix::System::new().block_on(async {
            let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
            let run=seed("run");
            store.create_run(run.clone()).await.unwrap();
            let original=store.run_header(&run.run_id).await.unwrap();
            store.connection.call(|db| db.execute_batch("CREATE TRIGGER fail_journal BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT,'injected'); END;")).await.unwrap();
            assert_eq!(store.acquire(&run.run_id,"owner".into(),10,20).await,Err(StoreError::Database));
            assert_eq!(store.run_header(&run.run_id).await.unwrap(),original);
            assert_eq!(counts(&store).await,(1,1,1));
        });
    }

    #[test]
    fn foreign_and_future_databases_are_rejected_without_relabeling() {
        actix::System::new().block_on(async {
            let temporary = TempDb::new();
            let raw = Connection::open(temporary.path()).await.unwrap();
            raw.call(|db| {
                db.execute_batch(
                    "CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES('keep');",
                )
            })
            .await
            .unwrap();
            assert!(matches!(
                SqliteStore::open(temporary.path(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
            let identity = raw
                .call(|db| db.pragma_query_value(None, "application_id", |r| r.get::<_, i64>(0)))
                .await
                .unwrap();
            assert_eq!(identity, 0);
            raw.close().await.unwrap();
            let future = TempDb::new();
            let store = SqliteStore::open(future.path(), StoreConfig::default())
                .await
                .unwrap();
            store
                .connection
                .call(|db| db.pragma_update(None, "user_version", SCHEMA_VERSION + 1))
                .await
                .unwrap();
            store.close().await.unwrap();
            assert!(matches!(
                SqliteStore::open(future.path(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
        });
    }
}
