//! Native PostgreSQL execution, lifecycle, and bounded cursor handling.

use crate::{
    sqlite_worker,
    store::{Store, StoreConfig, StoreError},
    store_db::{self, Error, Value},
    store_postgres_client::{Client, Error as ClientError},
};
use postgres::types::{ToSql, Type};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    fmt,
    ops::Deref,
    sync::OnceLock,
    time::{Duration, Instant},
};

const SCHEMA_VERSION: i64 = 1;
const APPLICATION_ID: &str = "htlk.runtime.postgres";
const SCHEMA: &str = include_str!("store_postgres.sql");

/// Host-selected PostgreSQL connection settings. Debug output omits credentials.
#[derive(Clone)]
pub struct PostgresConfig {
    connection: String,
    /// Dedicated HTLK schema; an ASCII identifier of at most 63 bytes.
    pub schema: String,
    /// Maximum time establishing a server connection.
    pub connect_timeout: Duration,
    /// Maximum time waiting for competing database authority.
    pub lock_timeout: Duration,
    /// Maximum duration of a database statement.
    pub statement_timeout: Duration,
    /// Maximum duration of an admitted storage operation, excluding queue wait.
    pub operation_timeout: Duration,
}
impl PostgresConfig {
    /// Creates explicit connection settings. Use `sslmode=disable` only for an
    /// intentionally unencrypted connection; the default requires verified TLS.
    pub fn new(connection: impl Into<String>) -> Self {
        Self {
            connection: connection.into(),
            schema: "htlk".into(),
            connect_timeout: Duration::from_secs(5),
            lock_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(60),
        }
    }
    fn validate(&self) -> Result<(), StoreError> {
        if self.schema.is_empty()
            || self.schema.len() > 63
            || !self
                .schema
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
            || [
                self.connect_timeout,
                self.lock_timeout,
                self.statement_timeout,
                self.operation_timeout,
            ]
            .iter()
            .any(|d| d.is_zero() || *d > Duration::from_secs(300))
        {
            return Err(StoreError::Invalid);
        }
        Ok(())
    }
}
impl fmt::Debug for PostgresConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresConfig")
            .field("connection", &"[redacted]")
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

/// Asynchronous PostgreSQL store backed by native tables and a bounded worker.
/// Clones share one connection; separately opened handles coordinate in PostgreSQL.
#[derive(Clone)]
pub struct PostgresStore(Store);
impl PostgresStore {
    /// Initializes an empty dedicated schema or opens its current recognized layout.
    /// Unrecognized nonempty schemas are never relabeled, migrated, or deleted.
    ///
    /// # Errors
    /// Returns invalid settings, incompatible schema, connection, or database failure.
    pub async fn initialize(
        settings: PostgresConfig,
        config: StoreConfig,
    ) -> Result<Self, StoreError> {
        Self::connect(settings, config, true).await
    }
    /// Opens an existing recognized schema without creating database objects.
    ///
    /// # Errors
    /// Returns invalid settings, missing/incompatible schema, or database failure.
    pub async fn open(settings: PostgresConfig, config: StoreConfig) -> Result<Self, StoreError> {
        Self::connect(settings, config, false).await
    }
    async fn connect(
        settings: PostgresConfig,
        config: StoreConfig,
        initialize: bool,
    ) -> Result<Self, StoreError> {
        settings.validate()?;
        super::store::validate_config(&config)?;
        let maximum =
            i64::try_from(config.limits.max_document_bytes).map_err(|_| StoreError::Invalid)?;
        let connection = sqlite_worker::Connection::start(move || {
            Database::connect(settings, initialize, maximum)
                .map(|db| store_db::Connection::Postgres(Box::new(db)))
        })
        .await?;
        Ok(Self(Store { connection, config }))
    }
    /// Drains queued work and closes this connection and its clones.
    ///
    /// # Errors
    /// Returns a worker shutdown failure.
    pub async fn close(self) -> Result<(), StoreError> {
        self.0.close().await
    }
}
impl Deref for PostgresStore {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.0
    }
}
impl From<PostgresStore> for Store {
    fn from(store: PostgresStore) -> Self {
        store.0
    }
}

pub(crate) struct Database {
    client: RefCell<Client>,
    settings: PostgresConfig,
    cursor: Cell<u64>,
    last_insert: Cell<i64>,
    started: Cell<Option<Instant>>,
    broken: Cell<bool>,
    cursor_failed: Cell<bool>,
    statements: RefCell<BTreeMap<String, postgres::Statement>>,
    maximum_field_bytes: i64,
}
impl Database {
    fn connect(
        settings: PostgresConfig,
        initialize: bool,
        maximum_field_bytes: i64,
    ) -> store_db::Result<Self> {
        let mut options: tokio_postgres::Config =
            settings.connection.parse().map_err(|_| Error::Invalid)?;
        options.connect_timeout(settings.connect_timeout);
        if options.get_ssl_mode() != postgres::config::SslMode::Disable {
            options.ssl_mode(postgres::config::SslMode::Require);
        }
        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|_| Error::Database)?;
        let client = Client::connect(
            options,
            postgres_native_tls::MakeTlsConnector::new(tls),
            settings.connect_timeout,
            settings.statement_timeout,
        )
        .map_err(pg_error)?;
        client.set_deadline(Some(Instant::now() + settings.operation_timeout));
        let version: i32 = client
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .map_err(pg_error)?
            .get(0);
        let readonly: String = client
            .query_one("SHOW transaction_read_only", &[])
            .map_err(pg_error)?
            .get(0);
        let fsync: String = client
            .query_one("SHOW fsync", &[])
            .map_err(pg_error)?
            .get(0);
        let encoding: String = client
            .query_one("SHOW server_encoding", &[])
            .map_err(pg_error)?
            .get(0);
        if version < 160000 || readonly != "off" || fsync != "on" || encoding != "UTF8" {
            return Err(Error::Invalid);
        }
        client.batch_execute(&format!(
            "SET synchronous_commit=on; SET lock_timeout='{}ms'; SET statement_timeout='{}ms'; SET idle_in_transaction_session_timeout='{}ms';",
            settings.lock_timeout.as_millis(), settings.statement_timeout.as_millis(), settings.operation_timeout.as_millis()
        )).map_err(pg_error)?;
        client.batch_execute("BEGIN").map_err(pg_error)?;
        client.query_one("SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || $1, 1213484107))", &[&settings.schema]).map_err(pg_error)?;
        let exists: bool = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)",
                &[&settings.schema],
            )
            .map_err(pg_error)?
            .get(0);
        if !exists {
            if !initialize {
                return Err(Error::Schema);
            }
            client
                .batch_execute(&format!("CREATE SCHEMA \"{}\"", settings.schema))
                .map_err(pg_error)?;
        }
        client
            .batch_execute(&format!(
                "SET search_path TO \"{}\", pg_catalog",
                settings.schema
            ))
            .map_err(pg_error)?;
        let recognized: bool = client
            .query_one("SELECT to_regclass('htlk_metadata') IS NOT NULL", &[])
            .map_err(pg_error)?
            .get(0);
        if !recognized {
            let objects: i64 = client.query_one("SELECT (SELECT count(*) FROM pg_class WHERE relnamespace=n.oid)+(SELECT count(*) FROM pg_proc WHERE pronamespace=n.oid)+(SELECT count(*) FROM pg_type WHERE typnamespace=n.oid) FROM pg_namespace n WHERE nspname=$1", &[&settings.schema]).map_err(pg_error)?.get(0);
            if !initialize || objects != 0 {
                return Err(Error::Schema);
            }
            client.batch_execute(SCHEMA).map_err(pg_error)?;
            client
                .execute(
                    "INSERT INTO htlk_metadata VALUES(1,$1,$2,$3)",
                    &[
                        &APPLICATION_ID,
                        &SCHEMA_VERSION,
                        &htlk_executable::digest::hash_bytes(SCHEMA.as_bytes()).to_string(),
                    ],
                )
                .map_err(pg_error)?;
        }
        let metadata = client
            .query_one(
                "SELECT left(application,64),version,left(layout_digest,128),octet_length(application),octet_length(layout_digest) FROM htlk_metadata WHERE singleton=1",
                &[],
            )
            .map_err(|_| Error::Schema)?;
        if metadata
            .try_get::<_, String>(0)
            .map_err(|_| Error::Schema)?
            != APPLICATION_ID
            || metadata.try_get::<_, i64>(1).map_err(|_| Error::Schema)? != SCHEMA_VERSION
            || metadata
                .try_get::<_, String>(2)
                .map_err(|_| Error::Schema)?
                != htlk_executable::digest::hash_bytes(SCHEMA.as_bytes()).to_string()
            || metadata.try_get::<_, i32>(3).map_err(|_| Error::Schema)?
                != APPLICATION_ID.len() as i32
            || metadata.try_get::<_, i32>(4).map_err(|_| Error::Schema)? != 71
        {
            return Err(Error::Schema);
        }
        client.batch_execute("COMMIT").map_err(pg_error)?;
        client.set_deadline(None);
        Ok(Self {
            client: RefCell::new(client),
            settings,
            cursor: Cell::new(0),
            last_insert: Cell::new(0),
            started: Cell::new(None),
            broken: Cell::new(false),
            cursor_failed: Cell::new(false),
            statements: RefCell::new(BTreeMap::new()),
            maximum_field_bytes,
        })
    }
    pub(crate) fn begin(&self) -> store_db::Result<()> {
        if self.broken.get() {
            return Err(Error::Database);
        }
        self.started.set(Some(Instant::now()));
        self.client
            .borrow()
            .set_deadline(Some(Instant::now() + self.settings.operation_timeout));
        self.cursor_failed.set(false);
        self.batch("BEGIN ISOLATION LEVEL READ COMMITTED")?;
        if let Err(error) = self.batch("SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || current_schema(), 1213484107))") {
            let _ = self.client.borrow_mut().batch_execute("ROLLBACK");
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn finish(&self, success: bool) -> store_db::Result<()> {
        if success && self.cursor_failed.get() {
            let _ = self.client.borrow_mut().batch_execute("ROLLBACK");
            self.started.set(None);
            return Err(Error::Database);
        }
        let result =
            self.client
                .borrow_mut()
                .batch_execute(if success { "COMMIT" } else { "ROLLBACK" });
        self.started.set(None);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let unknown = success && !error.known_commit_rollback();
                if unknown || self.client.borrow_mut().batch_execute("ROLLBACK").is_err() {
                    self.broken.set(true);
                }
                if success && unknown {
                    Err(Error::CommitUnknown)
                } else {
                    Err(pg_error(error))
                }
            }
        }
    }
    fn deadline(&self) -> store_db::Result<()> {
        if self
            .started
            .get()
            .is_some_and(|at| at.elapsed() >= self.settings.operation_timeout)
        {
            Err(Error::Database)
        } else {
            Ok(())
        }
    }
    pub(crate) fn batch(&self, sql: &str) -> store_db::Result<()> {
        self.deadline()?;
        self.client
            .borrow_mut()
            .batch_execute(sql)
            .map_err(pg_error)
    }
    pub(crate) fn execute(&self, sql: &str, values: Vec<Value>) -> store_db::Result<usize> {
        self.deadline()?;
        let mut sql = dialect(sql)?;
        let epoch = sql.starts_with("INSERT INTO mcp_epochs(");
        if epoch {
            sql.push_str(" RETURNING epoch");
        }
        let mut client = self.client.borrow_mut();
        let statement = self.prepare(&mut client, &sql)?;
        let params = parameters(statement.params(), values)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|v| &**v).collect();
        if epoch {
            let row = client.query_one(&statement, &refs).map_err(pg_error)?;
            self.last_insert.set(row.get(0));
            Ok(1)
        } else {
            usize::try_from(client.execute(&statement, &refs).map_err(pg_error)?)
                .map_err(|_| Error::Invalid)
        }
    }
    pub(crate) fn last_insert_id(&self) -> i64 {
        self.last_insert.get()
    }
    fn prepare(&self, client: &mut Client, sql: &str) -> store_db::Result<postgres::Statement> {
        if let Some(statement) = self.statements.borrow().get(sql) {
            return Ok(statement.clone());
        }
        let statement = client.prepare(sql).map_err(pg_error)?;
        let mut statements = self.statements.borrow_mut();
        if statements.len() >= 256 {
            statements.clear();
        }
        statements.insert(sql.into(), statement.clone());
        Ok(statement)
    }
    pub(crate) fn query_one(&self, sql: &str, values: Vec<Value>) -> store_db::Result<Vec<Value>> {
        self.deadline()?;
        let sql = bounded_projection(&dialect(sql)?, self.maximum_field_bytes)?;
        let mut client = self.client.borrow_mut();
        let statement = self.prepare(&mut client, &sql)?;
        let params = parameters(statement.params(), values)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|v| &**v).collect();
        let row = client
            .query_opt(&statement, &refs)
            .map_err(pg_error)?
            .ok_or(Error::Missing)?;
        decode_row(&row)
    }
    pub(crate) fn rows(&self, sql: &str, values: Vec<Value>) -> store_db::Result<Cursor<'_>> {
        self.deadline()?;
        let sql = bounded_projection(&dialect(sql)?, self.maximum_field_bytes)?;
        let next = self.cursor.get().checked_add(1).ok_or(Error::Invalid)?;
        self.cursor.set(next);
        let name = format!("htlk_cursor_{next}");
        let mut client = self.client.borrow_mut();
        let query = self.prepare(&mut client, &sql)?;
        let statement = client
            .prepare_typed(
                &format!("DECLARE {name} NO SCROLL CURSOR FOR {sql}"),
                query.params(),
            )
            .map_err(pg_error)?;
        let params = parameters(statement.params(), values)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|v| &**v).collect();
        client.execute(&statement, &refs).map_err(pg_error)?;
        Ok(Cursor {
            db: self,
            name,
            done: false,
            buffered: VecDeque::new(),
        })
    }
}

pub(crate) struct Cursor<'a> {
    db: &'a Database,
    name: String,
    done: bool,
    buffered: VecDeque<postgres::Row>,
}
impl Cursor<'_> {
    pub(crate) fn next(&mut self) -> store_db::Result<Option<Vec<Value>>> {
        if self.done {
            return Ok(None);
        }
        self.db.deadline()?;
        if self.buffered.is_empty() {
            self.buffered.extend(
                self.db
                    .client
                    .borrow_mut()
                    .query(&format!("FETCH FORWARD 16 FROM {}", self.name), &[])
                    .map_err(pg_error)?,
            );
        }
        let Some(row) = self.buffered.pop_front() else {
            self.done = true;
            return Ok(None);
        };
        decode_row(&row).map(Some)
    }
}
fn decode_row(row: &postgres::Row) -> store_db::Result<Vec<Value>> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            Ok(match *column.type_() {
                Type::BOOL => row
                    .try_get::<_, Option<bool>>(index)
                    .map_err(pg_error)?
                    .map_or(Value::Null, |v| Value::Integer(i64::from(v))),
                Type::INT8 => row
                    .try_get::<_, Option<i64>>(index)
                    .map_err(pg_error)?
                    .map_or(Value::Null, Value::Integer),
                Type::INT4 => row
                    .try_get::<_, Option<i32>>(index)
                    .map_err(pg_error)?
                    .map_or(Value::Null, |v| Value::Integer(i64::from(v))),
                Type::BYTEA => match row.try_get::<_, Option<Vec<u8>>>(index).map_err(pg_error)? {
                    None => Value::Null,
                    Some(bytes) if opaque_column(column.name()) => {
                        Value::Text(String::from_utf8(bytes).map_err(|_| Error::Invalid)?)
                    }
                    Some(bytes) => Value::Blob(bytes),
                },
                Type::TEXT | Type::VARCHAR | Type::NAME => row
                    .try_get::<_, Option<String>>(index)
                    .map_err(pg_error)?
                    .map_or(Value::Null, Value::Text),
                _ => return Err(Error::Invalid),
            })
        })
        .collect::<store_db::Result<Vec<_>>>()
}
impl Drop for Cursor<'_> {
    fn drop(&mut self) {
        if self
            .db
            .client
            .borrow_mut()
            .batch_execute(&format!("CLOSE {}", self.name))
            .is_err()
        {
            self.db.cursor_failed.set(true);
        }
    }
}

fn opaque_column(name: &str) -> bool {
    matches!(
        name,
        "run_id"
            | "owner"
            | "actor"
            | "causation"
            | "command_id"
            | "record_id"
            | "graph_id"
            | "principal"
            | "name"
            | "target"
            | "idempotency_key"
            | "producer_id"
            | "port"
            | "subscriber_key"
            | "uri"
            | "origin_run"
            | "task_id"
    )
}
fn parameters(types: &[Type], values: Vec<Value>) -> store_db::Result<Vec<Box<dyn ToSql + Sync>>> {
    if types.len() != values.len() {
        return Err(Error::Invalid);
    }
    types
        .iter()
        .zip(values)
        .map(|(ty, value)| -> store_db::Result<Box<dyn ToSql + Sync>> {
            Ok(match value {
                Value::Null => match *ty {
                    Type::BYTEA => Box::new(None::<Vec<u8>>),
                    Type::INT8 => Box::new(None::<i64>),
                    Type::INT4 => Box::new(None::<i32>),
                    _ => Box::new(None::<String>),
                },
                Value::Text(value) if *ty == Type::BYTEA => Box::new(value.into_bytes()),
                Value::Text(value) => Box::new(value),
                Value::Blob(value) => Box::new(value),
                Value::Integer(value) if *ty == Type::INT4 => {
                    Box::new(i32::try_from(value).map_err(|_| Error::Invalid)?)
                }
                Value::Integer(value) => Box::new(value),
                Value::Real(value) => Box::new(value),
            })
        })
        .collect()
}

// The private statement vocabulary is deliberately bounded: parameter markers,
// conflict targets for generated lookup keys, and three SQLite scalar spellings.
// Schema DDL is independently authored PostgreSQL SQL, never translated here.
fn dialect(sql: &str) -> store_db::Result<String> {
    let mut result = String::with_capacity(sql.len());
    let mut quoted = false;
    for ch in sql.chars() {
        if ch == '\'' {
            quoted = !quoted;
        }
        if ch == '?' && !quoted {
            result.push('$');
        } else {
            result.push(ch);
        }
    }
    if quoted {
        return Err(Error::Invalid);
    }
    if result.starts_with("INSERT OR IGNORE INTO ") {
        result = result.replacen("INSERT OR IGNORE INTO ", "INSERT INTO ", 1);
        result.push_str(" ON CONFLICT DO NOTHING");
    }
    result = result.replace("ON CONFLICT(run_id,family,record_id)", "ON CONFLICT(run_key,family,record_key)")
        .replace("$3 IS NULL OR record_id>$3", "$3::bytea IS NULL OR record_id>$3")
        .replace("$1 IS NULL OR run_id>$1", "$1::bytea IS NULL OR run_id>$1")
        .replace("$1 IS NULL OR blob_digest>$1", "$1::text IS NULL OR blob_digest>$1")
        .replace("$2 IS NULL OR provenance_digest>$2", "$2::text IS NULL OR provenance_digest>$2")
        .replace("$2 IS NULL OR origin_invocation=$2", "$2::text IS NULL OR origin_invocation=$2")
        .replace("$3 IS NULL OR origin_attempt=$3", "$3::text IS NULL OR origin_attempt=$3")
        .replace("INSERT INTO mcp_receipts VALUES", "INSERT INTO mcp_receipts(owner_key,principal,idempotency_key,interaction_id,reply_digest,work_delivery) VALUES")
        .replace("INSERT INTO mcp_url_receipts VALUES", "INSERT INTO mcp_url_receipts(owner_key,principal,idempotency_key,url_id,result_digest) VALUES")
        .replace("INSERT INTO mcp_subscription_consumers VALUES", "INSERT INTO mcp_subscription_consumers(subscription_id,subscriber_key,desired) VALUES")
        .replace("ON CONFLICT(principal,method,target,idempotency_key)", "ON CONFLICT(principal_key,method,target_key,idempotency_key_hash)")
        .replace("ON CONFLICT(subscription_id,subscriber_key)", "ON CONFLICT(subscription_id,subscriber_key_hash)")
        .replace("CAST(uri AS BLOB)", "uri")
        .replace("CAST(c.subscriber_key AS BLOB)", "c.subscriber_key")
        .replace("last_time_ms=max(last_time_ms,excluded.last_time_ms)", "last_time_ms=greatest(mcp_owners.last_time_ms,excluded.last_time_ms)")
        .replace("r.record_id LIKE 'client:%'", "convert_from(r.record_id,'UTF8') LIKE 'client:%'")
        .replace("r.record_id='client:'||i.interaction_id", "r.record_id=convert_to('client:'||i.interaction_id,'UTF8')")
        .replace("SELECT lower(hex(randomblob(32)))", "SELECT encode(sha256(convert_to(gen_random_uuid()::text || gen_random_uuid()::text,'UTF8')),'hex')");
    // PostgreSQL sum(bigint) is NUMERIC; retained counts must fit signed i64.
    let mut search = 0;
    while let Some(offset) = result[search..].find("sum(") {
        let start = search + offset + 4;
        let mut depth = 1;
        let mut end = None;
        for (offset, ch) in result[start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = Some(start + offset + 1);
                break;
            }
        }
        let end = end.ok_or(Error::Invalid)?;
        result.insert_str(end, "::bigint");
        search = end + 8;
    }
    Ok(indexed_predicates(&result))
}
fn indexed_predicates(sql: &str) -> String {
    let Some(where_at) = sql.find(" WHERE ") else {
        return sql.into();
    };
    let (prefix, predicates) = sql.split_at(where_at);
    static KEYS: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = KEYS.get_or_init(|| regex::Regex::new(r"\b([a-zA-Z_][a-zA-Z_0-9]*\.)?(run_id|record_id|producer_id|port|command_id|principal|target|idempotency_key|subscriber_key|uri|task_id|origin_run)=\$([0-9]+)\b").expect("fixed internal key vocabulary"));
    let optimized = pattern
        .replace_all(predicates, |capture: &regex::Captures<'_>| {
            let alias = capture.get(1).map_or("", |m| m.as_str());
            let key = match &capture[2] {
                "origin_run" if sql.contains("mcp_interactions") => {
                    return format!(
                        "(sha256({alias}origin_run)=sha256(${}) AND {})",
                        &capture[3], &capture[0]
                    );
                }
                "run_id" => "run_key",
                "record_id" => "record_key",
                "producer_id" => "producer_key",
                "port" => "port_key",
                "command_id" if sql.contains("FROM commands") => "command_key",
                "principal" if sql.contains("receipts") => "principal_key",
                "target" if sql.contains("host_receipts") => "target_key",
                "idempotency_key" => "idempotency_key_hash",
                "subscriber_key" => "subscriber_key_hash",
                "uri" if sql.contains("mcp_resource_subscriptions") => "uri_key",
                "task_id" if sql.contains("mcp_remote_tasks") => "task_key",
                _ => return capture[0].to_owned(),
            };
            format!(
                "({alias}{key}=sha256(${}) AND {})",
                &capture[3], &capture[0]
            )
        })
        .into_owned();
    format!("{prefix}{optimized}")
}
fn pg_error(error: impl Into<ClientError>) -> Error {
    match error.into() {
        ClientError::Protocol(error) if error.code().is_some_and(|code| code.code() == "HT001") => {
            Error::Corrupt
        }
        ClientError::Protocol(error) if error.code().is_some_and(|code| code.code() == "22001") => {
            Error::Invalid
        }
        _ => Error::Database,
    }
}

// Only simple fields in the closed SELECT vocabulary are guarded. Expressions
// (counts, comparisons, fixed-size generated identities) retain their SQL meaning.
fn bounded_projection(sql: &str, maximum: i64) -> store_db::Result<String> {
    let Some(body) = sql.strip_prefix("SELECT ") else {
        return Err(Error::Invalid);
    };
    let mut depth = 0usize;
    let mut quoted = false;
    let mut from = None;
    for (index, ch) in body.char_indices() {
        match ch {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.checked_sub(1).ok_or(Error::Invalid)?,
            _ => {}
        }
        if !quoted && depth == 0 && body[index..].starts_with(" FROM ") {
            from = Some(index);
            break;
        }
    }
    let Some(from) = from else {
        return Ok(sql.into());
    };
    let fields = &body[..from];
    let mut guarded = Vec::new();
    let mut start = 0;
    depth = 0;
    quoted = false;
    for (index, ch) in fields
        .char_indices()
        .chain(std::iter::once((fields.len(), ',')))
    {
        match ch {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.checked_sub(1).ok_or(Error::Invalid)?,
            _ => {}
        }
        if ch == ',' && depth == 0 && !quoted {
            let field = fields[start..index].trim();
            if field
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && field
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
            {
                let name = field.rsplit('.').next().ok_or(Error::Invalid)?;
                guarded.push(format!("htlk_field({field},{maximum}) AS {name}"));
            } else {
                guarded.push(field.into());
            }
            start = index + 1;
        }
    }
    Ok(format!("SELECT {}{}", guarded.join(","), &body[from..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{RunId, ScopeInstanceId},
        store::{RecordFamily, RecordKey, RecordWrite, RunSeed, Transition, TransitionContext},
    };
    use htlk_executable::{
        cbor::{Limits, Value as Cbor},
        digest::Digest,
    };
    use postgres::Client;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn settings() -> PostgresConfig {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let url = std::env::var("HTLK_TEST_POSTGRES_URL")
            .expect("postgres feature tests require HTLK_TEST_POSTGRES_URL (see docs/testing.md)");
        let mut settings = PostgresConfig::new(url);
        settings.schema = format!(
            "htlk_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        settings
    }
    fn seed(name: &str) -> RunSeed {
        let limits = Limits::default();
        let run_id = RunId::new(name.into(), &limits).unwrap();
        let root_scope = ScopeInstanceId::root(&run_id, &limits).unwrap();
        RunSeed {
            run_id,
            root_scope,
            fingerprint: Digest::from_bytes([1; 32]),
            actor: "host".into(),
            causation: "test".into(),
            timestamp_ms: 1,
        }
    }
    fn change(id: &str, revision: Option<i64>) -> Transition {
        Transition {
            context: TransitionContext {
                command_id: id.into(),
                actor: "host".into(),
                causation: "test".into(),
                timestamp_ms: 3,
            },
            writes: vec![RecordWrite {
                key: RecordKey {
                    family: RecordFamily::Invocation,
                    id: "node".into(),
                },
                expected_revision: revision,
                final_record: false,
                payload: Cbor::Null,
            }],
            next_status: None,
            root: None,
            expected_fingerprint: None,
        }
    }
    #[test]
    fn native_layout_reopen_fencing_cas_and_exact_opaque_identities() {
        actix::System::new().block_on(async {
            let settings = settings();
            let store = PostgresStore::initialize(settings.clone(), StoreConfig::default())
                .await
                .unwrap();
            let other = PostgresStore::open(settings.clone(), StoreConfig::default())
                .await
                .unwrap();
            let run = seed(&format!("nul\0é{}", "x".repeat(8192)));
            store.create_run(run.clone()).await.unwrap();
            assert_eq!(
                other.run_header(&run.run_id).await.unwrap().run_id,
                run.run_id
            );
            let lease = store
                .acquire(&run.run_id, "coordinator\0one".into(), 2, 100)
                .await
                .unwrap();
            assert_eq!(
                other.acquire(&run.run_id, "other".into(), 2, 100).await,
                Err(StoreError::Fenced)
            );
            let receipt = store.commit(&lease, change("first", None)).await.unwrap();
            assert_eq!(
                other.commit(&lease, change("first", None)).await.unwrap(),
                receipt
            );
            assert_eq!(
                other.commit(&lease, change("stale", None)).await,
                Err(StoreError::Conflict)
            );
            assert_eq!(
                other
                    .records(&run.run_id, RecordFamily::Invocation, None, 2)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(other.journal(&run.run_id, 0, 10).await.unwrap().len(), 3);
            assert_eq!(
                other.running_runs(None, 2).await.unwrap(),
                vec![run.run_id.clone()]
            );
            store.close().await.unwrap();
            other.close().await.unwrap();
            let reopened = PostgresStore::open(settings, StoreConfig::default())
                .await
                .unwrap();
            let current = reopened
                .acquire(&run.run_id, "new".into(), 100, 200)
                .await
                .unwrap();
            assert!(current.fence() > lease.fence());
            assert_eq!(
                reopened.commit(&lease, change("late", Some(1))).await,
                Err(StoreError::Fenced)
            );
            reopened.close().await.unwrap();
        });
    }

    #[test]
    fn incompatible_state_is_preserved_and_concurrent_initialization_is_atomic() {
        let settings = settings();
        actix::System::new().block_on(async {
            let left = settings.clone();
            let right = settings.clone();
            let (a, b) = tokio::join!(
                PostgresStore::initialize(left, StoreConfig::default()),
                PostgresStore::initialize(right, StoreConfig::default())
            );
            a.unwrap().close().await.unwrap();
            b.unwrap().close().await.unwrap();
        });
        let mut admin = Client::connect(&settings.connection, postgres::NoTls).unwrap();
        admin
            .batch_execute(&format!(
                "UPDATE \"{}\".htlk_metadata SET version=999",
                settings.schema
            ))
            .unwrap();
        actix::System::new().block_on(async {
            assert!(matches!(
                PostgresStore::open(settings.clone(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
            assert!(matches!(
                PostgresStore::initialize(settings.clone(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
        });
        assert_eq!(
            admin
                .query_one(
                    &format!("SELECT version FROM \"{}\".htlk_metadata", settings.schema),
                    &[]
                )
                .unwrap()
                .get::<_, i64>(0),
            999
        );
        let foreign = self::settings();
        admin.batch_execute(&format!("CREATE SCHEMA \"{}\"; CREATE TABLE \"{}\".unrelated(value text); INSERT INTO \"{}\".unrelated VALUES('retained')", foreign.schema, foreign.schema, foreign.schema)).unwrap();
        actix::System::new().block_on(async {
            assert!(matches!(
                PostgresStore::initialize(foreign.clone(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
        });
        assert_eq!(
            admin
                .query_one(
                    &format!("SELECT value FROM \"{}\".unrelated", foreign.schema),
                    &[]
                )
                .unwrap()
                .get::<_, String>(0),
            "retained"
        );
        let absent = self::settings();
        actix::System::new().block_on(async {
            assert!(matches!(
                PostgresStore::open(absent.clone(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
        });
        assert!(
            !admin
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)",
                    &[&absent.schema]
                )
                .unwrap()
                .get::<_, bool>(0)
        );
    }

    #[test]
    fn configuration_validation_and_debug_output_do_not_disclose_credentials() {
        let mut config =
            PostgresConfig::new("postgres://user:protected-password@localhost/database");
        assert!(!format!("{config:?}").contains("protected-password"));
        assert!(!format!("{config:?}").contains("postgres://"));
        config.schema = "unsafe; DROP SCHEMA public".into();
        assert_eq!(config.validate(), Err(StoreError::Invalid));
        config.schema = "htlk".into();
        config.operation_timeout = Duration::ZERO;
        assert_eq!(config.validate(), Err(StoreError::Invalid));
    }
}
