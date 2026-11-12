//! Read-only historical projections of journaled runtime facts.
//!
//! Positions cover run headers, all runtime record families (including recorded
//! MCP observations and artifact tombstones), and per-run journal upper bounds.
//! Mutable MCP owners, sessions, epochs, interactions, subscriptions, tasks,
//! receipts, URL state and transport evidence tables are **outside** this domain.
//! These methods never join those tables. Graph envelopes are separately immutable
//! by fingerprint. Detached source/provenance attachments are separately immutable
//! by digest; their attachment inventory is not pinned by these positions.
//!
//! A position pins facts, not bytes or authorization: it neither reads artifact
//! values nor prevents their subsequent retention expiry/garbage collection.
//! Projections are maintained in the authoritative journal transaction, never
//! replayed from sequence one on a query, and are not recovery authority.

use super::transitions::{decode_stored, text};
use super::*;
use htlk_executable::record_accounting::RecordAccounting;

/// A committed boundary in one store's journaled inspection domain.
/// Interior journal sequences are not positions. Zero denotes the empty domain.
/// Positions survive reopening that store; rebuilding creates a new namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePosition {
    namespace: String,
    position: i64,
}
impl StorePosition {
    /// Persistent store namespace, also used to reject cross-store pins.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    /// Monotonic commit number. Transactions without journaled facts do not advance it.
    pub const fn number(&self) -> i64 {
        self.position
    }
}

/// Immutable run view at a store-wide commit boundary. This carries no grants.
/// It covers headers, runtime records, artifact metadata/tombstones and journal
/// bounds. Mutable MCP adjunct tables and detached source attachment inventories
/// are outside the snapshot; immutable graph/source bytes can be separately
/// resolved by fingerprint/digest. Artifact bytes remain subject to current expiry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectionSnapshot {
    position: StorePosition,
    header: RunHeader,
}
impl InspectionSnapshot {
    /// Store-wide upper bound, shared by every included fact.
    pub fn position(&self) -> &StorePosition {
        &self.position
    }
    /// Historical header, including the exact committed journal upper bound.
    pub fn header(&self) -> &RunHeader {
        &self.header
    }
}

/// Structured historical header predicates. All specified predicates must match.
/// Filtering examines a bounded candidate page, so a page may have no matches
/// while still carrying a continuation cursor.
#[derive(Clone, Debug, Default)]
pub struct InspectionRunFilter {
    /// Select one exact run, if specified.
    pub run_id: Option<RunId>,
    /// Historical run status (active and terminal runs use the same index).
    pub status: Option<RunStatus>,
    /// Historical active executable fingerprint.
    pub fingerprint: Option<Digest>,
    /// Inclusive historical last-authoritative-time lower bound.
    pub since_ms: Option<i64>,
    /// Inclusive historical last-authoritative-time upper bound.
    pub until_ms: Option<i64>,
}
impl InspectionRunFilter {
    fn matches(&self, header: &RunHeader) -> bool {
        self.run_id.as_ref().is_none_or(|id| id == &header.run_id)
            && self.status.is_none_or(|status| status == header.status)
            && self
                .fingerprint
                .is_none_or(|fingerprint| fingerprint == header.fingerprint)
            && self.since_ms.is_none_or(|time| header.last_time_ms >= time)
            && self.until_ms.is_none_or(|time| header.last_time_ms <= time)
    }
}

/// Bounded historical discovery results in stable creation-ordinal order.
#[derive(Clone, Debug)]
pub struct InspectionRunPage {
    /// The common pin to reuse on every continuation page.
    pub position: StorePosition,
    /// Matching headers; no record payloads or artifact bytes are loaded.
    pub runs: Vec<RunHeader>,
    /// Last examined ordinal, exclusive on the next call. None means exhaustion.
    /// An empty `runs` vector does not imply exhaustion.
    pub next: Option<i64>,
}

/// Selected historical record and its stable paging ordinal.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoricalRecord {
    /// Exclusive continuation cursor within the selected run/family.
    pub cursor: i64,
    /// Historical canonical runtime record, without artifact value bytes.
    pub record: StoredRecord,
}

/// Artifact metadata and recorded expiry at one historical boundary. Current
/// value-byte availability is deliberately separate and must be checked on a
/// selected content read; this state does not imply a retention hold.
#[derive(Clone, Debug)]
pub struct InspectionArtifactState {
    /// Immutable accepted metadata, including protection and content reference.
    pub metadata: crate::artifact::ArtifactMetadata,
    /// Authoritative tombstone time if expiry was committed at or before the pin.
    pub expired_at_ms: Option<i64>,
}

/// One independently resumable committed change; acknowledging outbox hints has
/// no effect on this feed. A transaction can produce several changes at one pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectionChange {
    /// Exclusive feed continuation cursor; gaps are permitted.
    pub cursor: i64,
    /// Actual transaction boundary, suitable for opening a snapshot.
    pub position: StorePosition,
    /// Stable target run identity.
    pub run_id: RunId,
    /// Stable record identity; None denotes the run header.
    pub key: Option<RecordKey>,
    /// Final per-run journal bound for this transaction.
    pub sequence: i64,
}

impl Store {
    /// Reads the latest committed inspection-domain boundary without loading values.
    ///
    /// # Errors
    /// Returns database or corrupt store identity failures.
    pub async fn inspection_position(&self) -> Result<StorePosition, StoreError> {
        self.connection
            .call(|db| position(db, None))
            .await
            .map_err(StoreError::from_worker)
    }

    /// Resolves a retained commit number in this store. Every number from zero to
    /// the latest boundary exists; journal sequences cannot be passed as pins.
    /// Persist the namespace alongside numbers when transporting references.
    ///
    /// # Errors
    /// Returns invalid negative/future positions, database or corrupt identity failures.
    pub async fn inspection_position_at(&self, number: i64) -> Result<StorePosition, StoreError> {
        self.connection
            .call(move |db| position(db, Some(number)))
            .await
            .map_err(StoreError::from_worker)
    }

    /// Opens an immutable historical run view, or pins the latest committed state
    /// when `at` is None. An interior creation/initialization prefix is never exposed.
    /// Independently mutable MCP adjuncts and source inventories are outside this view.
    ///
    /// # Errors
    /// Returns unknown run at the pin, cross-store/future pin, corruption or database failure.
    pub async fn open_inspection_snapshot(
        &self,
        run: &RunId,
        at: Option<&StorePosition>,
    ) -> Result<InspectionSnapshot, StoreError> {
        let (run, at, limits) = (run.clone(), at.cloned(), self.config.limits.clone());
        self.connection
            .call(move |db| {
                let pin = checked_position(db, at.as_ref())?;
                let (_, header) = historical_header(db, &run, pin.position, &limits)?;
                Ok(InspectionSnapshot {
                    position: pin,
                    header,
                })
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Examines at most `page_size` historical headers, applying structured filters
    /// without reading runtime records. Continue using the returned pin and `next`,
    /// even on empty filtered pages. Ordinals are stable across later writes.
    ///
    /// # Errors
    /// Returns invalid filters/cursor/limit/pin, corruption or database failure.
    pub async fn inspection_runs(
        &self,
        at: Option<&StorePosition>,
        filter: InspectionRunFilter,
        after: i64,
        page_size: usize,
    ) -> Result<InspectionRunPage, StoreError> {
        let count = self.page_size(page_size)?;
        if after < 0
            || filter.since_ms.is_some_and(|n| n < 0)
            || filter.until_ms.is_some_and(|n| n < 0)
            || matches!((filter.since_ms,filter.until_ms),(Some(a),Some(b)) if a>b)
        {
            return Err(StoreError::Invalid);
        }
        let (at, limits) = (at.cloned(), self.config.limits.clone());
        self.connection.call(move |db| {
            let pin = checked_position(db, at.as_ref())?;
            let (upper,_,_) = uppers(db,pin.position)?;
            let mut statement = db.prepare("SELECT run_number,run_id FROM inspection_runs WHERE run_number>?1 AND run_number<=?2 ORDER BY run_number LIMIT ?3")?;
            let mut rows = statement.query(params![after,upper,count])?;
            let mut runs = Vec::new();
            let mut next = None;
            let mut budget = PageBudget::new(&limits);
            let mut exhausted = false;
            let mut scanned = 0;
            loop {
                let Some(row) = rows.next()? else { exhausted = true; break; };
                let ordinal: i64 = row.get(0)?;
                let run = RunId::new(text(row,1,&limits)?, &limits).map_err(|_| StoreError::Corrupt)?;
                let (_, header) = historical_header(db,&run,pin.position,&limits)?;
                if filter.matches(&header) {
                    if !budget.add(&header_value(&header))? { break; }
                    runs.push(header);
                }
                next = Some(ordinal);
                scanned += 1;
                if scanned == count { break; }
            }
            if exhausted { next = None; }
            Ok(InspectionRunPage { position: pin, runs, next })
        }).await.map_err(StoreError::from_worker)
    }

    /// Pages a record family at the snapshot boundary in stable insertion-ordinal
    /// order. Continue after the final returned cursor until an empty page. Later
    /// insertions/revisions cannot alter this page sequence. No value blobs are read.
    ///
    /// # Errors
    /// Returns invalid cursor/limit/pin, corrupt records or database failure.
    pub async fn inspection_record_page(
        &self,
        snapshot: &InspectionSnapshot,
        family: RecordFamily,
        after: i64,
        page_size: usize,
    ) -> Result<Vec<HistoricalRecord>, StoreError> {
        let count = self.page_size(page_size)?;
        if after < 0 {
            return Err(StoreError::Invalid);
        }
        let (snapshot, limits) = (snapshot.clone(), self.config.limits.clone());
        self.connection.call(move |db| {
            checked_position(db,Some(&snapshot.position))?;
            let (_,upper,_) = uppers(db,snapshot.position.position)?;
            // Bound candidate keys before looking up versions, rather than asking
            // the planner to find a bounded result in an arbitrarily long history.
            let mut statement = db.prepare("SELECT record_number,record_id FROM inspection_record_keys WHERE run_id=?1 AND family=?2 AND record_number>?3 AND record_number<=?4 ORDER BY record_number LIMIT ?5")?;
            let mut rows = statement.query(params![snapshot.header.run_id.as_str(),family.as_str(),after,upper,count])?;
            let mut result = Vec::new();
            let mut budget = PageBudget::new(&limits);
            while let Some(row) = rows.next()? {
                let cursor = row.get(0)?;
                let key = RecordKey { family,id:text(row,1,&limits)? };
                let record = historical_record(db,cursor,key,snapshot.position.position,&limits)?;
                let item = HistoricalRecord { cursor,record };
                if !budget.add(&Value::Array(vec![Value::Integer(cursor),record_value(&item.record)]))? { break; }
                result.push(item);
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }

    /// Reads one exact historical runtime record; never reads an artifact value.
    ///
    /// # Errors
    /// Returns missing historical record, invalid pin/key, corruption or database failure.
    pub async fn inspection_record(
        &self,
        snapshot: &InspectionSnapshot,
        key: &RecordKey,
    ) -> Result<StoredRecord, StoreError> {
        self.check_text(&key.id)?;
        let (snapshot, key, limits) = (snapshot.clone(), key.clone(), self.config.limits.clone());
        self.connection.call(move |db| {
            checked_position(db,Some(&snapshot.position))?;
            let number = db.query_row("SELECT record_number FROM inspection_record_keys WHERE run_id=?1 AND family=?2 AND record_id=?3",params![snapshot.header.run_id.as_str(),key.family.as_str(),key.id],|row| row.get(0))?;
            historical_record(db,number,key,snapshot.position.position,&limits)
        }).await.map_err(StoreError::from_worker)
    }

    /// Reads journal history after an exclusive sequence, bounded by the snapshot's
    /// final committed sequence. This cannot dispatch work or attach an engine.
    ///
    /// # Errors
    /// Returns invalid pin/cursor/limit, corrupt journal data or database failure.
    pub async fn inspection_journal(
        &self,
        snapshot: &InspectionSnapshot,
        after: i64,
        page_size: usize,
    ) -> Result<Vec<JournalEntry>, StoreError> {
        // Verify namespace before the shared bounded journal reader.
        let pin = snapshot.position.clone();
        self.connection
            .call(move |db| checked_position(db, Some(&pin)))
            .await
            .map_err(StoreError::from_worker)?;
        self.journal_until(
            &snapshot.header.run_id,
            after,
            snapshot.header.sequence,
            page_size,
        )
        .await
    }

    /// Reads accepted artifact metadata and its historical expiry tombstone in
    /// the artifact's producing run. No value bytes or latest adjunct facts are read.
    /// A current content read may still return `StoreError::Expired` for an older pin.
    ///
    /// # Errors
    /// Returns missing artifact, invalid pin, corrupt metadata/tombstone or database failure.
    pub async fn inspection_artifact(
        &self,
        snapshot: &InspectionSnapshot,
        id: crate::identity::ArtifactId,
    ) -> Result<InspectionArtifactState, StoreError> {
        let record = self
            .inspection_record(
                snapshot,
                &RecordKey {
                    family: RecordFamily::Artifact,
                    id: id.to_string(),
                },
            )
            .await?;
        let metadata =
            crate::artifact::ArtifactMetadata::from_value(&record.payload, &self.config.limits)
                .map_err(|_| StoreError::Corrupt)?;
        if !record.final_record || metadata.artifact_id() != id {
            return Err(StoreError::Corrupt);
        }
        let expired_at_ms = match self
            .inspection_record(
                snapshot,
                &RecordKey {
                    family: RecordFamily::Tombstone,
                    id: id.to_string(),
                },
            )
            .await
        {
            Err(StoreError::Missing) => None,
            Err(error) => return Err(error),
            Ok(tombstone) => {
                let Value::Map(fields) = tombstone.payload else {
                    return Err(StoreError::Corrupt);
                };
                let Some(Value::Integer(time)) = fields.get("expired_at_ms") else {
                    return Err(StoreError::Corrupt);
                };
                if !tombstone.final_record
                    || *time < 0
                    || fields.get("artifact_id") != Some(&Value::Text(id.to_string()))
                    || fields.get("reason") != Some(&Value::Text("retention_expired".into()))
                {
                    return Err(StoreError::Corrupt);
                }
                Some(*time)
            }
        };
        Ok(InspectionArtifactState {
            metadata,
            expired_at_ms,
        })
    }

    /// Reads a bounded independent store-wide change page after an exclusive feed
    /// cursor, optionally stopping at a committed pin. Consumers retain their own
    /// cursor; neither this operation nor outbox acknowledgment consumes changes.
    /// No runtime payloads, artifact bytes, or mutable MCP adjuncts are read.
    ///
    /// # Errors
    /// Returns invalid cursor/limit/pin, corruption or database failure.
    pub async fn inspection_changes(
        &self,
        after: i64,
        through: Option<&StorePosition>,
        page_size: usize,
    ) -> Result<Vec<InspectionChange>, StoreError> {
        let count = self.page_size(page_size)?;
        if after < 0 {
            return Err(StoreError::Invalid);
        }
        let (through, limits) = (through.cloned(), self.config.limits.clone());
        self.connection.call(move |db| {
            let pin = checked_position(db,through.as_ref())?;
            let (_,_,upper) = uppers(db,pin.position)?;
            let mut statement = db.prepare("SELECT c.change_id,c.position,r.run_id,k.family,k.record_id,c.sequence FROM inspection_changes c JOIN inspection_runs r ON r.run_number=c.run_number LEFT JOIN inspection_record_keys k ON k.record_number=c.record_number WHERE c.change_id>?1 AND c.change_id<=?2 ORDER BY c.change_id LIMIT ?3")?;
            let mut rows = statement.query(params![after,upper,count])?;
            let mut result = Vec::new();
            let mut budget = PageBudget::new(&limits);
            while let Some(row) = rows.next()? {
                let run = text(row,2,&limits)?;
                let key = if matches!(row.get_ref(3)?,rusqlite::types::ValueRef::Null) { None } else {
                    Some(RecordKey { family:RecordFamily::parse(&text(row,3,&limits)?)?,id:text(row,4,&limits)? })
                };
                let cursor = row.get(0)?;
                let number = row.get(1)?;
                let sequence = row.get(5)?;
                if cursor<1 || number<1 || sequence<1 { return Err(StoreError::Corrupt); }
                let value = Value::Array(vec![Value::Integer(cursor),Value::Integer(number),Value::Text(run.clone()),key.as_ref().map_or(Value::Null,|k| Value::Array(vec![Value::Text(k.family.as_str().into()),Value::Text(k.id.clone())])),Value::Integer(sequence)]);
                if !budget.add(&value)? { break; }
                result.push(InspectionChange { cursor,position:StorePosition { namespace:pin.namespace.clone(),position:number },run_id:RunId::new(run,&limits).map_err(|_| StoreError::Corrupt)?,key,sequence });
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
}

fn position(db: &rusqlite::Connection, number: Option<i64>) -> Result<StorePosition, StoreError> {
    let (namespace,latest) = db.query_row("SELECT i.namespace,c.position FROM runtime_identity i CROSS JOIN inspection_clock c WHERE i.singleton=1 AND c.singleton=1",[],|row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?;
    if namespace.len() != 32 || !namespace.bytes().all(|b| b.is_ascii_hexdigit()) || latest < 0 {
        return Err(StoreError::Corrupt);
    }
    let position = number.unwrap_or(latest);
    if position < 0 || position > latest {
        return Err(StoreError::Invalid);
    }
    Ok(StorePosition {
        namespace,
        position,
    })
}
fn checked_position(
    db: &rusqlite::Connection,
    at: Option<&StorePosition>,
) -> Result<StorePosition, StoreError> {
    let pin = position(db, at.map(|p| p.position))?;
    if at.is_some_and(|p| p.namespace != pin.namespace) {
        return Err(StoreError::Invalid);
    }
    Ok(pin)
}
fn uppers(db: &rusqlite::Connection, position: i64) -> Result<(i64, i64, i64), StoreError> {
    Ok(db.query_row(
        "SELECT run_upper,record_upper,change_upper FROM inspection_commits WHERE position=?1",
        [position],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}
fn historical_header(
    db: &rusqlite::Connection,
    run: &RunId,
    position: i64,
    limits: &Limits,
) -> Result<(i64, RunHeader), StoreError> {
    let row = db.query_row("SELECT r.run_number,h.fingerprint,h.root_scope,h.status,h.sequence,h.owner,h.fence,h.lease_expires_ms,h.last_time_ms FROM inspection_runs r JOIN inspection_headers h ON h.run_number=r.run_number WHERE r.run_id=?1 AND h.position<=?2 ORDER BY h.position DESC LIMIT 1",params![run.as_str(),position],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,i64>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,i64>(6)?,row.get::<_,Option<i64>>(7)?,row.get::<_,i64>(8)?)))?;
    let status = match row.3.as_str() {
        "running" => RunStatus::Running,
        "succeeded" => RunStatus::Succeeded,
        "failed" => RunStatus::Failed,
        "cancelled" => RunStatus::Cancelled,
        _ => return Err(StoreError::Corrupt),
    };
    let header = RunHeader {
        run_id: run.clone(),
        fingerprint: row.1.parse().map_err(|_| StoreError::Corrupt)?,
        root_scope: row.2.parse().map_err(|_| StoreError::Corrupt)?,
        status,
        sequence: row.4,
        owner: row.5,
        fence: row.6,
        expires_at_ms: row.7,
        last_time_ms: row.8,
    };
    if row.0 < 1
        || header.sequence < 1
        || header.fence < 0
        || header.last_time_ms < 0
        || header.owner.is_some() != header.expires_at_ms.is_some()
    {
        return Err(StoreError::Corrupt);
    }
    RecordAccounting::new(limits)?
        .value(&header_value(&header), 0)
        .map_err(|_| StoreError::Invalid)?;
    Ok((row.0, header))
}
fn historical_record(
    db: &rusqlite::Connection,
    number: i64,
    key: RecordKey,
    position: i64,
    limits: &Limits,
) -> Result<StoredRecord, StoreError> {
    let (revision,final_record,raw) = db.query_row("SELECT revision,final,payload FROM inspection_records WHERE record_number=?1 AND position<=?2 ORDER BY position DESC LIMIT 1",params![number,position],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,bool>(1)?,row.get::<_,Vec<u8>>(2)?)))?;
    if number < 1 || revision < 1 {
        return Err(StoreError::Corrupt);
    }
    Ok(StoredRecord {
        key,
        revision,
        final_record,
        payload: decode_stored(&raw, limits)?,
    })
}
fn header_value(header: &RunHeader) -> Value {
    Value::Array(vec![
        Value::Text(header.run_id.to_string()),
        Value::Text(header.fingerprint.to_string()),
        Value::Text(header.root_scope.to_string()),
        Value::Text(header.status.as_str().into()),
        Value::Integer(header.sequence),
        header.owner.clone().map_or(Value::Null, Value::Text),
        Value::Integer(header.fence),
        header.expires_at_ms.map_or(Value::Null, Value::Integer),
        Value::Integer(header.last_time_ms),
    ])
}
fn record_value(record: &StoredRecord) -> Value {
    Value::Array(vec![
        Value::Text(record.key.family.as_str().into()),
        Value::Text(record.key.id.clone()),
        Value::Integer(record.revision),
        Value::Bool(record.final_record),
        record.payload.clone(),
    ])
}
struct PageBudget<'a> {
    limits: &'a Limits,
    bytes: usize,
    count: usize,
}
impl<'a> PageBudget<'a> {
    fn new(limits: &'a Limits) -> Self {
        Self {
            limits,
            bytes: 1,
            count: 0,
        }
    }
    fn add(&mut self, value: &Value) -> Result<bool, StoreError> {
        let size = cbor::encode(value, self.limits)?.len();
        let growth = usize::from(self.count + 1 == 24 || self.count + 1 == 256);
        if self
            .bytes
            .checked_add(size)
            .and_then(|n| n.checked_add(growth))
            .is_none_or(|n| n > self.limits.max_document_bytes)
        {
            if self.count == 0 {
                return Err(StoreError::Invalid);
            }
            return Ok(false);
        }
        self.bytes += size + growth;
        self.count += 1;
        Ok(true)
    }
}

// Called only by append_event, after authoritative rows and outbox were written.
// Multiple events in the same transaction overwrite the same header boundary.
pub(super) fn project(
    tx: &rusqlite::Transaction<'_>,
    run: &str,
    kind: &str,
    payload: &[u8],
    limits: &Limits,
) -> Result<(), StoreError> {
    let position = if let Some(position) = tx.inspection_position() {
        position
    } else {
        let previous: i64 = tx.query_row(
            "SELECT position FROM inspection_clock WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let position = previous.checked_add(1).ok_or(StoreError::Invalid)?;
        tx.execute(
            "UPDATE inspection_clock SET position=?1 WHERE singleton=1",
            [position],
        )?;
        tx.set_inspection_position(Some(position));
        position
    };
    tx.execute(
        "INSERT OR IGNORE INTO inspection_runs(run_id,created_position) VALUES(?1,?2)",
        params![run, position],
    )?;
    let run_number: i64 = tx.query_row(
        "SELECT run_number FROM inspection_runs WHERE run_id=?1",
        [run],
        |r| r.get(0),
    )?;
    let sequence: i64 = tx.query_row("SELECT sequence FROM runs WHERE run_id=?1", [run], |r| {
        r.get(0)
    })?;
    tx.execute("INSERT INTO inspection_headers(run_number,position,fingerprint,root_scope,status,sequence,owner,fence,lease_expires_ms,last_time_ms) SELECT ?1,?2,fingerprint,root_scope,status,sequence,owner,fence,lease_expires_ms,last_time_ms FROM runs WHERE run_id=?3 ON CONFLICT(run_number,position) DO UPDATE SET fingerprint=excluded.fingerprint,root_scope=excluded.root_scope,status=excluded.status,sequence=excluded.sequence,owner=excluded.owner,fence=excluded.fence,lease_expires_ms=excluded.lease_expires_ms,last_time_ms=excluded.last_time_ms",params![run_number,position,run])?;
    tx.execute("INSERT INTO inspection_changes(position,run_number,record_number,sequence) VALUES(?1,?2,0,?3) ON CONFLICT(position,run_number,record_number) DO UPDATE SET sequence=excluded.sequence",params![position,run_number,sequence])?;
    let value = cbor::decode(payload, limits).map_err(|_| StoreError::Corrupt)?;
    let body = match kind {
        "run_initialized" => array(&value)?.first(),
        "records_committed" => array(&value)?.get(2),
        "terminal_receipt" | "artifact_expired" => Some(&value),
        "run_created" | "ownership_acquired" | "ownership_renewed" => None,
        _ => return Err(StoreError::Corrupt),
    };
    if let Some(body) = body {
        let writes = array(array(body)?.first().ok_or(StoreError::Corrupt)?)?;
        for write in writes {
            let fields = array(write)?;
            let (Some(Value::Text(family)), Some(Value::Text(id))) =
                (fields.first(), fields.get(1))
            else {
                return Err(StoreError::Corrupt);
            };
            RecordFamily::parse(family)?;
            tx.execute("INSERT OR IGNORE INTO inspection_record_keys(run_id,family,record_id,created_position) VALUES(?1,?2,?3,?4)",params![run,family,id,position])?;
            let record_number:i64 = tx.query_row("SELECT record_number FROM inspection_record_keys WHERE run_id=?1 AND family=?2 AND record_id=?3",params![run,family,id],|r| r.get(0))?;
            let copied = tx.execute("INSERT INTO inspection_records(record_number,position,revision,final,payload) SELECT ?1,?2,revision,final,payload FROM records WHERE run_id=?3 AND family=?4 AND record_id=?5 ON CONFLICT(record_number,position) DO UPDATE SET revision=excluded.revision,final=excluded.final,payload=excluded.payload",params![record_number,position,run,family,id])?;
            if copied != 1 {
                return Err(StoreError::Corrupt);
            }
            tx.execute("INSERT INTO inspection_changes(position,run_number,record_number,sequence) VALUES(?1,?2,?3,?4) ON CONFLICT(position,run_number,record_number) DO UPDATE SET sequence=excluded.sequence",params![position,run_number,record_number,sequence])?;
        }
    }
    // Earlier events in the transaction may also have changed records.
    tx.execute(
        "UPDATE inspection_changes SET sequence=?3 WHERE position=?1 AND run_number=?2",
        params![position, run_number, sequence],
    )?;
    tx.execute("INSERT INTO inspection_commits(position,run_upper,record_upper,change_upper) VALUES(?1,(SELECT coalesce(max(run_number),0) FROM inspection_runs),(SELECT coalesce(max(record_number),0) FROM inspection_record_keys),(SELECT coalesce(max(change_id),0) FROM inspection_changes)) ON CONFLICT(position) DO UPDATE SET run_upper=excluded.run_upper,record_upper=excluded.record_upper,change_upper=excluded.change_upper",[position])?;
    Ok(())
}
fn array(value: &Value) -> Result<&[Value], StoreError> {
    if let Value::Array(items) = value {
        Ok(items)
    } else {
        Err(StoreError::Corrupt)
    }
}

#[cfg(test)]
#[path = "store_inspection_tests.rs"]
mod tests;
