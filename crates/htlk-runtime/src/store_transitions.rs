//! Atomic revisioned record transitions and recoverable journal/outbox delivery.
use super::*;
use htlk_executable::record_accounting::RecordAccounting;
use std::collections::BTreeSet;

macro_rules! families {
    ($($name:ident => $wire:literal),+ $(,)?) => {
        /// Closed authoritative runtime record families; each payload is validated
        /// by its coordinator consumer before committing this storage transition.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum RecordFamily { $(#[doc = $wire] $name),+ }
        impl RecordFamily {
            /// Stable private database family spelling.
            pub const fn as_str(self) -> &'static str { match self { $(Self::$name => $wire),+ } }
            pub(crate) fn parse(value: &str) -> Result<Self, StoreError> {
                match value { $($wire => Ok(Self::$name)),+, _ => Err(StoreError::Corrupt) }
            }
        }
    };
}
families! {
    Scope => "scope", Invocation => "invocation", Guard => "guard", Binding => "binding",
    Manifest => "manifest", Attempt => "attempt", Response => "response", Wait => "wait",
    Reply => "reply", Approval => "approval", Reservation => "reservation", Usage => "usage",
    Artifact => "artifact", Cancellation => "cancellation", Extension => "extension",
    Loop => "loop", Timer => "timer", Observation => "observation", Tombstone => "tombstone",
}
/// Exact record key within a run. IDs are private names, not source-language paths.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordKey {
    /// Typed record family.
    pub family: RecordFamily,
    /// Exact opaque record name.
    pub id: String,
}
/// A trusted coordinator's compare-and-swap proposal. A finalized record cannot
/// subsequently be revised. This supplies storage integrity, not host authorization.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordWrite {
    /// Exact run-local key.
    pub key: RecordKey,
    /// None inserts; Some(n) requires an existing nonfinal revision n.
    pub expected_revision: Option<i64>,
    /// Make this record immutable after the transaction commits.
    pub final_record: bool,
    /// Bounded, consumer-validated canonical runtime record.
    pub payload: Value,
}
/// An authoritative bounded runtime record.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRecord {
    /// Exact run-local key.
    pub key: RecordKey,
    /// Initial insertion is revision one.
    pub revision: i64,
    /// Whether further mutation is prohibited.
    pub final_record: bool,
    /// Protected canonical runtime record data.
    pub payload: Value,
}
/// Trusted attribution and internal idempotent command identity.
#[derive(Clone, Debug)]
pub struct TransitionContext {
    /// Stable unique command key scoped by the target run.
    pub command_id: String,
    /// Authenticated principal or trusted runtime actor.
    pub actor: String,
    /// Stable cause/request identity retained across command retries.
    pub causation: String,
    /// Trusted observed time, clamped to committed coordinator time.
    pub timestamp_ms: i64,
}
/// An already validated additive installation's new active root pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootReplacement {
    /// Registered composed executable.
    pub fingerprint: Digest,
    /// Fresh extended-root scope instance.
    pub root_scope: ScopeInstanceId,
}
/// One atomic decision, complete record changes and optional run terminal/root
/// update. The service validates operation semantics before this trusted boundary.
#[derive(Clone, Debug)]
pub struct Transition {
    /// Attribution and idempotent command key.
    pub context: TransitionContext,
    /// Unique record keys; storage canonicalizes their order.
    pub writes: Vec<RecordWrite>,
    /// Optional immutable terminal run result.
    pub next_status: Option<RunStatus>,
    /// Optional new root, after runtime additive validation.
    pub root: Option<RootReplacement>,
    /// Compare active graph before a root installation or other guarded decision.
    pub expected_fingerprint: Option<Digest>,
}
/// Stable result of an internal command, including repeated identical commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitReceipt {
    /// The one authoritative journal sequence for this command.
    pub sequence: i64,
}
/// Closed version-0.1 journal transition schema selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalEvent {
    /// Terminal audit-window expiry records one immutable artifact tombstone.
    ArtifactExpired,
    /// Immutable host receipt appended after a terminal outcome, without work.
    TerminalReceipt,
    /// Initial root/inputs, contract result and authenticated host start receipt.
    RunInitialized,
    /// Initial fingerprint/root; sequence one, no owner.
    RunCreated,
    /// New owner, higher fence and expiry.
    OwnershipAcquired,
    /// Same owner/fence with renewed expiry.
    OwnershipRenewed,
    /// Complete revisioned records and optional terminal/root update.
    RecordsCommitted,
}
impl JournalEvent {
    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "artifact_expired" => Ok(Self::ArtifactExpired),
            "terminal_receipt" => Ok(Self::TerminalReceipt),
            "run_initialized" => Ok(Self::RunInitialized),
            "run_created" => Ok(Self::RunCreated),
            "ownership_acquired" => Ok(Self::OwnershipAcquired),
            "ownership_renewed" => Ok(Self::OwnershipRenewed),
            "records_committed" => Ok(Self::RecordsCommitted),
            _ => Err(StoreError::Corrupt),
        }
    }
}
/// One complete committed journal entry. Payloads are protected runtime data.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalEntry {
    /// Target run.
    pub run_id: RunId,
    /// Strictly increasing per-run sequence.
    pub sequence: i64,
    /// Closed event schema.
    pub event: JournalEvent,
    /// Version, always exactly 0.1.
    pub record_version: &'static str,
    /// Authenticated or trusted actor attribution.
    pub actor: String,
    /// Authoritative persisted nondecreasing time.
    pub timestamp_ms: i64,
    /// Stable causal identity.
    pub causation: String,
    /// Internal idempotent command identity, if applicable.
    pub command_id: Option<String>,
    /// Complete typed-event canonical payload.
    pub payload: Value,
}
/// Recoverable hint for journal consumers. Redelivery cannot create a new result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxNotice {
    /// Target run.
    pub run_id: RunId,
    /// Committed journal event to inspect.
    pub sequence: i64,
}

impl SqliteStore {
    /// Atomically compare/revise records, update run status/root, append the complete
    /// decision and enqueue its outbox hint. An identical command returns its old
    /// receipt; a changed request under that key fails. Timestamp and worker lease
    /// are excluded from request identity so recovery can retry with new authority.
    ///
    /// # Errors
    /// Returns stale authority/revision, final record/run, idempotency conflict,
    /// active-fingerprint conflict, complete record limits or database failure.
    pub async fn commit(
        &self,
        lease: &RunLease,
        mut transition: Transition,
    ) -> Result<CommitReceipt, StoreError> {
        timestamp(transition.context.timestamp_ms)?;
        let limits = self.config.limits.clone();
        for text in [
            &transition.context.command_id,
            &transition.context.actor,
            &transition.context.causation,
        ] {
            self.check_text(text)?;
        }
        let body = transition_body(&mut transition, &limits)?;
        let request_digest = command_digest(
            lease.run_id(),
            &transition.context.actor,
            &transition.context.causation,
            &body,
            &limits,
        )?;
        let event_payload = cbor::encode(
            &Value::Array(vec![
                Value::Text(lease.owner.clone()),
                Value::Integer(lease.fence),
                body,
            ]),
            &limits,
        )?;
        let writes = transition
            .writes
            .iter()
            .map(|write| Ok(cbor::encode(&write.payload, &limits)?))
            .collect::<Result<Vec<_>, StoreError>>()?;
        let lease = lease.clone();
        self.connection.call(move |db| -> Result<CommitReceipt,StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let header = load_header(&tx, &lease.run_id)?;
            let now = transition.context.timestamp_ms.max(header.last_time_ms);
            check_authority(&header, &lease, now)?;
            let old = tx.query_row("SELECT request_digest,sequence FROM commands WHERE run_id=?1 AND command_id=?2",params![lease.run_id.as_str(),transition.context.command_id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).optional()?;
            if let Some((digest,sequence))=old {
                if digest != request_digest.to_string() { return Err(StoreError::IdempotencyConflict); }
                return Ok(CommitReceipt {sequence});
            }
            if header.status.is_terminal() { return Err(StoreError::Terminal); }
            if transition.expected_fingerprint.is_some_and(|fp|fp!=header.fingerprint) { return Err(StoreError::Conflict); }
            if transition.next_status.is_some_and(|status|!header.status.can_transition_to(status)) { return Err(StoreError::Terminal); }
            for (write,payload) in transition.writes.iter().zip(&writes) {
                artifacts::check_artifact_write(&tx,&lease.run_id,write,&limits)?;
                let old=tx.query_row("SELECT revision,final FROM records WHERE run_id=?1 AND family=?2 AND record_id=?3",params![lease.run_id.as_str(),write.key.family.as_str(),write.key.id],|r| Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).optional()?;
                if old.is_some_and(|(_,final_record)|final_record) { return Err(StoreError::Terminal); }
                if old.map(|(revision,_)|revision) != write.expected_revision { return Err(StoreError::Conflict); }
                let revision=next_revision(write.expected_revision)?;
                tx.execute("INSERT INTO records(run_id,family,record_id,revision,final,payload) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(run_id,family,record_id) DO UPDATE SET revision=excluded.revision,final=excluded.final,payload=excluded.payload",params![lease.run_id.as_str(),write.key.family.as_str(),write.key.id,revision,write.final_record,payload])?;
            }
            retention::check_references(&tx,&transition.writes,&limits)?;
            artifacts::check_output_publication(&tx,&lease.run_id,&transition.writes,&limits)?;
            let sequence=header.sequence.checked_add(1).ok_or(StoreError::Invalid)?;
            let fingerprint=transition.root.as_ref().map_or(header.fingerprint,|r|r.fingerprint);
            let root=transition.root.as_ref().map_or(header.root_scope,|r|r.root_scope);
            tx.execute("UPDATE runs SET fingerprint=?2,root_scope=?3,status=?4,sequence=?5,last_time_ms=?6 WHERE run_id=?1",params![lease.run_id.as_str(),fingerprint.to_string(),root.to_string(),transition.next_status.unwrap_or(header.status).as_str(),sequence,now])?;
            append_event(&tx,lease.run_id.as_str(),sequence,EventMeta {kind:"records_committed",actor:&transition.context.actor,timestamp:now,causation:&transition.context.causation,command:Some(&transition.context.command_id)},&event_payload,&limits)?;
            tx.execute("INSERT INTO commands(run_id,command_id,request_digest,sequence) VALUES(?1,?2,?3,?4)",params![lease.run_id.as_str(),transition.context.command_id,request_digest.to_string(),sequence])?;
            tx.commit()?;
            Ok(CommitReceipt {sequence})
        }).await.map_err(StoreError::from_worker)
    }
    /// Reads one authoritative record under the configured representation limits.
    ///
    /// # Errors
    /// Returns missing record, invalid/corrupt stored representation or SQLite failure.
    pub async fn record(&self, run: &RunId, key: &RecordKey) -> Result<StoredRecord, StoreError> {
        self.check_text(&key.id)?;
        let (run, key, limits) = (run.clone(), key.clone(), self.config.limits.clone());
        self.connection.call(move |db|->Result<StoredRecord,StoreError>{
            let mut statement=db.prepare("SELECT revision,final,payload FROM records WHERE run_id=?1 AND family=?2 AND record_id=?3")?;
            let mut rows=statement.query(params![run.as_str(),key.family.as_str(),key.id])?;
            let row=rows.next()?.ok_or(StoreError::Missing)?;
            let revision:i64=row.get(0)?;
            let final_record:bool=row.get(1)?;
            let bytes=row.get_ref(2)?.as_blob().map_err(|_|StoreError::Corrupt)?;
            let payload=decode_stored(bytes,&limits)?;
            if revision<1 {return Err(StoreError::Corrupt)}
            Ok(StoredRecord{key,revision,final_record,payload})
        }).await.map_err(StoreError::from_worker)
    }
    /// Reads a bounded journal page after an exclusive sequence cursor. A page may
    /// contain fewer than the requested rows to respect the aggregate byte budget.
    /// Continue from its last sequence. This never executes external operations.
    ///
    /// # Errors
    /// Returns invalid cursor/page size, corrupt records or SQLite failures.
    pub async fn journal(
        &self,
        run: &RunId,
        after: i64,
        page_size: usize,
    ) -> Result<Vec<JournalEntry>, StoreError> {
        let count = self.page_size(page_size)?;
        if after < 0 {
            return Err(StoreError::Invalid);
        }
        let (run, limits) = (run.clone(), self.config.limits.clone());
        self.connection.call(move |db|->Result<Vec<JournalEntry>,StoreError>{
            let mut statement=db.prepare("SELECT sequence,event_type,record_version,actor,timestamp_ms,causation,command_id,payload FROM journal WHERE run_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            let mut rows=statement.query(params![run.as_str(),after,count])?;
            let mut result=Vec::new();
            let mut bytes=0usize;
            while let Some(row)=rows.next()? {
                if text(row,2,&limits)?!="0.1" {return Err(StoreError::Corrupt)}
                let raw=row.get_ref(7)?.as_blob().map_err(|_|StoreError::Corrupt)?;
                if raw.len()>limits.max_document_bytes{return Err(StoreError::Invalid)}
                let actor=text(row,3,&limits)?;
                let causation=text(row,5,&limits)?;
                let command_id=if matches!(row.get_ref(6)?,tokio_rusqlite::types::ValueRef::Null){None}else{Some(text(row,6,&limits)?)};
                let kind=text(row,1,&limits)?;
                let event=JournalEvent::parse(&kind)?;
                let sequence:i64=row.get(0)?;
                let timestamp_ms:i64=row.get(4)?;
                if sequence<1||timestamp_ms<0{return Err(StoreError::Corrupt)}
                let metadata=EventMeta{kind:&kind,actor:&actor,timestamp:timestamp_ms,causation:&causation,command:command_id.as_deref()};
                let size=cbor::encode(&event_value(run.as_str(),sequence,&metadata,raw),&limits)?.len();
                if bytes.checked_add(size).is_none_or(|n|n>limits.max_document_bytes) {
                    if result.is_empty(){return Err(StoreError::Invalid)}else{break}
                }
                bytes+=size;
                result.push(JournalEntry{run_id:run.clone(),sequence,event,record_version:"0.1",actor,timestamp_ms,causation,command_id,payload:decode_stored(raw,&limits)?});
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
    /// Lists bounded pending delivery hints. Repeated reads intentionally return
    /// the same hints until their delivery is acknowledged.
    ///
    /// # Errors
    /// Returns invalid page size, corrupt IDs or SQLite failure.
    pub async fn pending_outbox(&self, page_size: usize) -> Result<Vec<OutboxNotice>, StoreError> {
        let count = self.page_size(page_size)?;
        let limits = self.config.limits.clone();
        self.connection.call(move |db|->Result<Vec<OutboxNotice>,StoreError>{
            let mut statement=db.prepare("SELECT run_id,sequence FROM outbox WHERE delivered=0 ORDER BY run_id,sequence LIMIT ?1")?;
            let mut rows=statement.query([count])?;
            let mut result=Vec::new();
            let mut bytes=1usize;
            while let Some(row)=rows.next()? {
                let id=text(row,0,&limits)?;
                let sequence:i64=row.get(1)?;
                if sequence<1{return Err(StoreError::Corrupt)}
                let size=cbor::encode(&Value::Array(vec![Value::Text(id.clone()),Value::Integer(sequence)]),&limits)?.len();
                let header_growth=page_header(result.len()+1)-page_header(result.len());
                if bytes.checked_add(size).and_then(|n|n.checked_add(header_growth)).is_none_or(|n|n>limits.max_document_bytes) {
                    if result.is_empty(){return Err(StoreError::Invalid)}else{break}
                }
                bytes+=size+header_growth;
                result.push(OutboxNotice{run_id:RunId::new(id,&limits).map_err(|_|StoreError::Corrupt)?,sequence});
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
    /// Idempotently acknowledges notification delivery. This changes no run state
    /// or journal outcome and grants no graph-execution authority.
    ///
    /// # Errors
    /// Returns an unknown notice or SQLite failure.
    pub async fn acknowledge(&self, notice: &OutboxNotice) -> Result<(), StoreError> {
        let notice = notice.clone();
        self.connection
            .call(move |db| -> Result<(), StoreError> {
                let changed = db.execute(
                    "UPDATE outbox SET delivered=1 WHERE run_id=?1 AND sequence=?2",
                    params![notice.run_id.as_str(), notice.sequence],
                )?;
                if changed != 1 {
                    return Err(StoreError::Missing);
                }
                Ok(())
            })
            .await
            .map_err(StoreError::from_worker)
    }
    fn page_size(&self, page_size: usize) -> Result<i64, StoreError> {
        if page_size == 0 || page_size > self.config.max_page_size {
            return Err(StoreError::Invalid);
        }
        i64::try_from(page_size).map_err(|_| StoreError::Invalid)
    }
}

// Pages are capped at 4096 rows by StoreConfig validation.
fn page_header(count: usize) -> usize {
    if count < 24 {
        1
    } else if count < 256 {
        2
    } else {
        3
    }
}

pub(super) fn next_revision(previous: Option<i64>) -> Result<i64, StoreError> {
    match previous {
        None => Ok(1),
        Some(n) if n >= 1 => n.checked_add(1).ok_or(StoreError::Invalid),
        _ => Err(StoreError::Invalid),
    }
}
pub(super) fn transition_body(
    transition: &mut Transition,
    limits: &Limits,
) -> Result<Value, StoreError> {
    transition.writes.sort_by(|a, b| a.key.cmp(&b.key));
    let mut unique = BTreeSet::new();
    let mut accounting = RecordAccounting::new(limits)?;
    accounting
        .collection(4, 0)
        .map_err(|_| StoreError::Invalid)?;
    accounting
        .collection(transition.writes.len(), 1)
        .map_err(|_| StoreError::Invalid)?;
    let mut writes = Vec::new();
    for write in &transition.writes {
        next_revision(write.expected_revision)?;
        if !unique.insert(&write.key) {
            return Err(StoreError::Conflict);
        }
        accounting
            .collection(5, 2)
            .map_err(|_| StoreError::Invalid)?;
        accounting
            .text(write.key.family.as_str(), 3)
            .map_err(|_| StoreError::Invalid)?;
        accounting
            .text(&write.key.id, 3)
            .map_err(|_| StoreError::Invalid)?;
        accounting
            .value(
                &write.expected_revision.map_or(Value::Null, Value::Integer),
                3,
            )
            .map_err(|_| StoreError::Invalid)?;
        accounting
            .value(&Value::Bool(write.final_record), 3)
            .map_err(|_| StoreError::Invalid)?;
        accounting
            .value(&write.payload, 3)
            .map_err(|_| StoreError::Invalid)?;
        writes.push(Value::Array(vec![
            Value::Text(write.key.family.as_str().into()),
            Value::Text(write.key.id.clone()),
            write.expected_revision.map_or(Value::Null, Value::Integer),
            Value::Bool(write.final_record),
            write.payload.clone(),
        ]));
    }
    let status = transition
        .next_status
        .map_or(Value::Null, |s| Value::Text(s.as_str().into()));
    let root = transition.root.as_ref().map_or(Value::Null, |r| {
        Value::Array(vec![
            Value::Text(r.fingerprint.to_string()),
            Value::Text(r.root_scope.to_string()),
        ])
    });
    let expected = transition
        .expected_fingerprint
        .map_or(Value::Null, |fp| Value::Text(fp.to_string()));
    let body = Value::Array(vec![Value::Array(writes), status, root, expected]);
    RecordAccounting::new(limits)?
        .value(&body, 0)
        .map_err(|_| StoreError::Invalid)?;
    Ok(body)
}
pub(super) fn command_digest(
    run: &RunId,
    actor: &str,
    causation: &str,
    body: &Value,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    crate::identity::hash(
        "store_command",
        &[
            &Value::Text(actor.into()),
            &Value::Text(causation.into()),
            &Value::Text(run.as_str().into()),
            body,
        ],
        limits,
    )
    .map_err(|_| StoreError::Invalid)
}
fn decode_stored(bytes: &[u8], limits: &Limits) -> Result<Value, StoreError> {
    cbor::decode(bytes, limits).map_err(|e| {
        if matches!(e.kind(), cbor::ErrorKind::LimitExceeded { .. }) {
            StoreError::Invalid
        } else {
            StoreError::Corrupt
        }
    })
}
fn text(
    row: &tokio_rusqlite::Row<'_>,
    index: usize,
    limits: &Limits,
) -> Result<String, StoreError> {
    let text = row
        .get_ref(index)?
        .as_str()
        .map_err(|_| StoreError::Corrupt)?;
    if text.len() > limits.max_document_bytes {
        return Err(StoreError::Invalid);
    }
    Ok(text.into())
}
