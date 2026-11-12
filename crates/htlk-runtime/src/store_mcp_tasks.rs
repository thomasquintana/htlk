//! Child of store_mcp: durable receiver tasks reuse the real owner, epoch and
//! interaction authority checks in the same SQLite write transaction.
use super::*;
use htlk_executable::cbor::Map;

const RELATED_TASK: &str = "io.modelcontextprotocol/related-task";
const ROW_ALLOWANCE: usize = 1024;
const MAX_PROTOCOL_TIME: i64 = 253_402_300_799_999;

/// Frozen receiver retention and admission quotas. Expired rows remain bounded
/// deduplication tombstones; expiration does not authorize recreation or release
/// uncertain work. Result capacity plus a 1 KiB metadata/receipt allowance is
/// reserved at task acceptance; SQLite file/page overhead is not byte-accounted.
#[derive(Clone, Copy, Debug)]
pub struct McpReceivedTaskLimits {
    records: usize,
    active: usize,
    default_ttl_ms: u64,
    maximum_ttl_ms: u64,
    poll_interval_ms: u64,
    result_bytes: usize,
    retained_bytes: usize,
}
impl McpReceivedTaskLimits {
    /// Checks finite quotas, including capacity for at least one full task row.
    /// The store independently caps result representation by its codec limits.
    ///
    /// # Errors
    /// Returns invalid or inconsistent positive bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        records: usize,
        active: usize,
        default_ttl_ms: u64,
        maximum_ttl_ms: u64,
        poll_interval_ms: u64,
        result_bytes: usize,
        retained_bytes: usize,
    ) -> Result<Self, StoreError> {
        if [records, active].into_iter().any(|n| n == 0 || n > 65_536)
            || active > records
            || default_ttl_ms == 0
            || maximum_ttl_ms < default_ttl_ms
            || maximum_ttl_ms > MAX_PROTOCOL_TIME as u64
            || poll_interval_ms == 0
            || poll_interval_ms > i64::MAX as u64
            || result_bytes < 256
            || retained_bytes > isize::MAX as usize
            || result_bytes
                .checked_add(ROW_ALLOWANCE)
                .is_none_or(|n| n > retained_bytes)
        {
            return Err(StoreError::Invalid);
        }
        Ok(Self {
            records,
            active,
            default_ttl_ms,
            maximum_ttl_ms,
            poll_interval_ms,
            result_bytes,
            retained_bytes,
        })
    }
    /// Maximum retained task identities, including expired terminal tombstones.
    pub fn records(&self) -> usize {
        self.records
    }
    /// Maximum possibly active wrapped operations, including uncertain outcomes.
    pub fn active(&self) -> usize {
        self.active
    }
    /// Default granted retention from creation in milliseconds.
    pub fn default_ttl_ms(&self) -> u64 {
        self.default_ttl_ms
    }
    /// Maximum granted retention from creation in milliseconds.
    pub fn maximum_ttl_ms(&self) -> u64 {
        self.maximum_ttl_ms
    }
    /// Suggested requestor polling interval in milliseconds.
    pub fn poll_interval_ms(&self) -> u64 {
        self.poll_interval_ms
    }
    /// Reserved canonical final-payload bytes for each task.
    pub fn result_bytes(&self) -> usize {
        self.result_bytes
    }
}

/// Persisted protocol state, independent of host-work delivery and accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpReceivedTaskStatus {
    /// Accepted and executing, or awaiting its bounded execution worker.
    Working,
    /// The requestor must service a task-related message on a result stream.
    InputRequired,
    /// The original request produced a successful result.
    Completed,
    /// The original request failed or exceeded its execution deadline.
    Failed,
    /// Task cancellation committed; late work cannot change this status.
    Cancelled,
}
impl McpReceivedTaskStatus {
    /// Whether status and final payload are immutable.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    /// Selected-version wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "working" => Ok(Self::Working),
            "input_required" => Ok(Self::InputRequired),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(StoreError::Corrupt),
        }
    }
}

/// Immutable protected final payload. The original reply bytes remain untouched
/// in the interaction ledger; this is its task-associated protocol projection.
#[derive(Clone)]
pub struct McpReceivedTaskResult {
    is_error: bool,
    payload: JsonDocument,
    source_reply_digest: Option<Digest>,
}
impl fmt::Debug for McpReceivedTaskResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpReceivedTaskResult")
            .field("is_error", &self.is_error)
            .field("bytes", &self.payload.as_bytes().len())
            .finish_non_exhaustive()
    }
}
impl McpReceivedTaskResult {
    /// Whether this is the original operation's JSON-RPC error rather than result.
    pub fn is_error(&self) -> bool {
        self.is_error
    }
    /// Complete protected result/error object with related-task metadata.
    pub fn payload(&self) -> &JsonDocument {
        &self.payload
    }
    /// Original saved reply identity, absent for a task-local cancellation/timeout.
    pub fn source_reply_digest(&self) -> Option<Digest> {
        self.source_reply_digest
    }
}

/// Durable receiver-generated identity and state. Debug excludes protected IDs and
/// payloads. This is not a graph task or authority to execute another interaction.
#[derive(Clone)]
pub struct McpReceivedTask {
    task_id: String,
    interaction_id: Digest,
    session: Digest,
    server: Digest,
    method: String,
    generation: i64,
    created_ms: i64,
    updated_ms: i64,
    expires_ms: i64,
    ttl_ms: u64,
    poll_interval_ms: u64,
    status: McpReceivedTaskStatus,
    revision: i64,
    result_capacity: usize,
    result: Option<McpReceivedTaskResult>,
}
impl fmt::Debug for McpReceivedTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpReceivedTask")
            .field("status", &self.status)
            .field("revision", &self.revision)
            .field("expires_ms", &self.expires_ms)
            .finish_non_exhaustive()
    }
}
impl McpReceivedTask {
    /// Opaque receiver-generated identity; do not derive one from a request ID.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    /// Existing accounted interaction wrapped by this task.
    pub fn interaction_id(&self) -> Digest {
        self.interaction_id
    }
    /// Creation epoch, distinct from the current authorized retrieval epoch.
    pub fn session(&self) -> Digest {
        self.session
    }
    /// Exact initialized compound server digest.
    pub fn server(&self) -> Digest {
        self.server
    }
    /// Original task-augmented method.
    pub fn method(&self) -> &str {
        &self.method
    }
    /// Task protocol state, not underlying work delivery.
    pub fn status(&self) -> McpReceivedTaskStatus {
        self.status
    }
    /// Trusted absolute retention boundary; expiry does not imply work stopped.
    pub fn expires_ms(&self) -> i64 {
        self.expires_ms
    }
    /// Granted retention duration in milliseconds.
    pub fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }
    /// Stable final payload, present only in a terminal task state.
    pub fn result(&self) -> Option<&McpReceivedTaskResult> {
        self.result.as_ref()
    }
    /// Current Task object, with RFC3339 UTC timestamps and actual finite TTL.
    ///
    /// # Errors
    /// Returns bounded representation failure.
    pub fn protocol_task(&self, limits: &Limits) -> Result<JsonDocument, StoreError> {
        task_document(self, self.status, self.updated_ms, limits)
    }
    /// Stable initial CreateTaskResult. Redelivery after completion still returns
    /// the original working acceptance, never a second acceptance or final result.
    ///
    /// # Errors
    /// Returns bounded representation failure.
    pub fn acceptance(&self, limits: &Limits) -> Result<JsonDocument, StoreError> {
        let task = task_document(
            self,
            McpReceivedTaskStatus::Working,
            self.created_ms,
            limits,
        )?;
        let acceptance = document(vec![("task".into(), task.value().clone())], limits)?;
        related_payload(acceptance.value(), &self.task_id, limits)
    }
}

/// One bounded inventory page. The cursor identifies the last scanned row even
/// when the embedding host filters that row by current disclosure authorization.
pub struct McpReceivedTaskPage {
    /// Nonexpired observations in this owner/server/generation scope.
    pub tasks: Vec<McpReceivedTask>,
    /// Opaque server cursor, absent only on the final page.
    pub next_cursor: Option<String>,
}
impl fmt::Debug for McpReceivedTaskPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpReceivedTaskPage")
            .field("tasks", &self.tasks.len())
            .field("has_next", &self.next_cursor.is_some())
            .finish()
    }
}

impl SqliteStore {
    /// Atomically assigns a random receiver ID to an already-admitted interaction.
    /// This must precede begin_mcp_work and sending CreateTaskResult. Redelivery of
    /// the same interaction returns the same identity without another reservation.
    ///
    /// # Errors
    /// Returns malformed/unsupported augmentation, expired authority or quota failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_mcp_received_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        id: Digest,
        now_ms: i64,
        budget: McpReceivedTaskLimits,
    ) -> Result<McpReceivedTask, StoreError> {
        timestamp(now_ms)?;
        // Every legal ledger reply must fit after association metadata is added.
        // Reserving that capacity now avoids losing a final result to late quota
        // exhaustion. The original ledger already limits replies to one eighth.
        if budget.result_bytes < self.config.limits.max_document_bytes / 8 + 256
            || budget.result_bytes > self.config.limits.max_document_bytes / 4
            || self.config.limits.max_depth < 4
        {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            task_session(&tx, &owner, session, server)?;
            let work = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            if work.session != session || work.generation != owner.generation() { return Err(StoreError::Fenced); }
            let request = JsonDocument::new(&work.request, &limits).map_err(|_| StoreError::Corrupt)?;
            let fields = map(request.value())?;
            let Value::Text(method) = field(fields, "method")? else { return Err(StoreError::Invalid) };
            if !matches!(method.as_str(), "sampling/createMessage" | "elicitation/create") { return Err(StoreError::Invalid); }
            let params = map(field(fields, "params")?)?;
            let metadata = map(field(params, "task")?)?;
            let requested = match metadata.get("ttl") {
                None => budget.default_ttl_ms,
                Some(Value::Integer(n)) if *n >= 0 => *n as u64,
                _ => return Err(StoreError::Invalid),
            };
            let existing: Option<String> = tx.query_row("SELECT task_id FROM mcp_received_tasks WHERE owner_key=?1 AND interaction_id=?2", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?;
            if let Some(existing) = existing {
                let task = load_task(&tx, &owner, server, &existing, &limits)?.ok_or(StoreError::Corrupt)?;
                if now >= task.expires_ms { return Err(StoreError::Expired); }
                tx.commit()?; return Ok(task);
            }
            if work.state != McpWorkState::Pending || now >= work.deadline_ms { return Err(StoreError::Terminal); }
            checked_usage(&tx, &owner)?;
            let (records, reserved): (usize, usize) = tx.query_row("SELECT count(*),coalesce(sum(result_capacity+1024),0) FROM mcp_received_tasks WHERE owner_key=?1", [owner.key().to_string()], |r| Ok((r.get(0)?,r.get(1)?)))?;
            if records >= budget.records || reserved.checked_add(budget.result_bytes + ROW_ALLOWANCE).is_none_or(|n| n > budget.retained_bytes) { return Err(StoreError::Budget); }
            // Expired retention cannot release possibly dispatched work capacity.
            let active: usize = tx.query_row("SELECT count(*) FROM mcp_received_tasks t JOIN mcp_interactions i ON i.interaction_id=t.interaction_id WHERE t.owner_key=?1 AND (i.outstanding=1 OR (i.state IN ('pending','intent') AND i.deadline_ms>?2))", params![owner.key().to_string(),now], |r| r.get(0))?;
            if active >= budget.active { return Err(StoreError::Budget); }
            let ttl = requested.min(budget.maximum_ttl_ms);
            let expires = now.checked_add(ttl as i64).filter(|n| *n <= MAX_PROTOCOL_TIME).ok_or(StoreError::Invalid)?;
            // SQLite obtains fresh PRNG entropy; IDs are never requestor-selected.
            // Authorization is always owner/server bound, not bearer-ID secrecy.
            let task_id: String = tx.query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))?;
            tx.execute("INSERT INTO mcp_received_tasks(task_id,owner_key,server_digest,session_id,interaction_id,generation,method,created_ms,updated_ms,expires_ms,ttl_ms,poll_interval_ms,status,revision,result_capacity) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,?10,?11,'working',1,?12)", params![task_id,owner.key().to_string(),server.to_string(),session.to_string(),id.to_string(),owner.generation(),method,now,expires,ttl as i64,budget.poll_interval_ms as i64,budget.result_bytes as i64])?;
            fact(&tx, &owner, Some(id), "receiver_task_accepted", now, &Value::Text(task_id.clone()), &limits)?;
            let task = load_task(&tx, &owner, server, &task_id, &limits)?.ok_or(StoreError::Corrupt)?;
            tx.commit()?; Ok(task)
        }).await.map_err(StoreError::from_worker)
    }

    /// Reads and transactionally synchronizes a task with saved interaction facts.
    /// A new live epoch may retrieve an old epoch's tasks only for the same exact
    /// owner generation and server. Neither lookup nor recovery dispatches work.
    ///
    /// # Errors
    /// Returns stale authority, missing/expired task, corruption or store failure.
    pub async fn mcp_received_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        task_id: &str,
        now_ms: i64,
    ) -> Result<McpReceivedTask, StoreError> {
        timestamp(now_ms)?;
        self.check_text(task_id)?;
        let (owner, task_id, limits) = (
            owner.clone(),
            task_id.to_owned(),
            self.config.limits.clone(),
        );
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                task_session(&tx, &owner, session, server)?;
                let task = current_task(&tx, &owner, server, &task_id, now, &limits)?;
                tx.commit()?;
                Ok(task)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Lists one finite owner/server-scoped page, excluding expired retention.
    /// Cursor validation never parses or normalizes an opaque token.
    ///
    /// # Errors
    /// Returns invalid cursor/bounds, stale authority or storage failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn mcp_received_tasks(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        cursor: Option<&str>,
        page_size: usize,
        now_ms: i64,
    ) -> Result<McpReceivedTaskPage, StoreError> {
        let count = self.page_size(page_size)?;
        timestamp(now_ms)?;
        if let Some(cursor) = cursor {
            self.check_text(cursor)?;
        }
        let (owner, cursor, limits) = (
            owner.clone(),
            cursor.map(str::to_owned),
            self.config.limits.clone(),
        );
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            task_session(&tx, &owner, session, server)?;
            if let Some(cursor) = &cursor { load_task(&tx, &owner, server, cursor, &limits)?.ok_or(StoreError::Missing)?; }
            let mut statement = tx.prepare("SELECT task_id FROM mcp_received_tasks WHERE owner_key=?1 AND server_digest=?2 AND generation=?3 AND expires_ms>?4 AND task_id>?5 ORDER BY task_id LIMIT ?6")?;
            let ids = statement.query_map(params![owner.key().to_string(),server.to_string(),owner.generation(),now,cursor.unwrap_or_default(),count + 1], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            statement.finish();
            let more = ids.len() > count as usize;
            let mut tasks = Vec::new();
            let mut bytes = 0usize;
            for id in ids.iter().take(count as usize) {
                let task = current_task(&tx, &owner, server, id, now, &limits)?;
                bytes = bytes.checked_add(task.result.as_ref().map_or(0, |r| r.payload.as_bytes().len()) + ROW_ALLOWANCE).ok_or(StoreError::Invalid)?;
                if bytes > limits.max_document_bytes { return Err(StoreError::Invalid); }
                tasks.push(task);
            }
            let next_cursor = if more { tasks.last().map(|task| task.task_id.clone()) } else { None };
            tx.commit()?; Ok(McpReceivedTaskPage { tasks, next_cursor })
        }).await.map_err(StoreError::from_worker)
    }

    /// Commits terminal task cancellation and cancels the wrapped interaction in
    /// one transaction before any protocol response. Call charges, reservations
    /// and underlying uncertain delivery remain unchanged. A concurrent saved
    /// completion wins if it commits first; otherwise it can only be late evidence.
    ///
    /// # Errors
    /// Returns Terminal for completed/failed/cancelled tasks; never revives them.
    pub async fn cancel_mcp_received_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        task_id: &str,
        now_ms: i64,
    ) -> Result<McpReceivedTask, StoreError> {
        self.cancel_received_task_inner(owner, session, server, task_id, now_ms, None)
            .await
    }

    /// Cancels an exact task RPC with a receipt committed atomically with terminal
    /// cancellation. Identical redelivery returns the saved cancellation state;
    /// a different request ID against that terminal task still returns Terminal.
    ///
    /// # Errors
    /// Returns invalid parameters, changed request payload, terminal or stale task.
    pub async fn cancel_mcp_received_task_request(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        request: &JsonDocument,
        now_ms: i64,
    ) -> Result<McpReceivedTask, StoreError> {
        let (task_id, key, digest) = cancel_request(request, session, &self.config.limits)?;
        self.cancel_received_task_inner(
            owner,
            session,
            server,
            &task_id,
            now_ms,
            Some((key, digest)),
        )
        .await
    }

    /// Looks up a recorded cancellation RPC before authorization/disclosure. No
    /// mutation or fresh cancellation occurs. Saved receipts survive task expiry,
    /// while ownership/session fences still apply; this is replay, not revival.
    ///
    /// # Errors
    /// Returns changed payload under the same RPC ID, stale authority or corruption.
    pub async fn mcp_received_task_cancel_receipt(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        request: &JsonDocument,
        now_ms: i64,
    ) -> Result<Option<McpReceivedTask>, StoreError> {
        timestamp(now_ms)?;
        let (task_id, key, digest) = cancel_request(request, session, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                authority(&tx, &owner, now_ms)?;
                task_session(&tx, &owner, session, server)?;
                let result = cancel_receipt(&tx, &owner, server, &task_id, key, digest, &limits)?;
                tx.commit()?;
                Ok(result)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    #[allow(clippy::too_many_arguments)]
    async fn cancel_received_task_inner(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        task_id: &str,
        now_ms: i64,
        receipt: Option<(Digest, Digest)>,
    ) -> Result<McpReceivedTask, StoreError> {
        timestamp(now_ms)?;
        self.check_text(task_id)?;
        let (owner, task_id, limits) = (
            owner.clone(),
            task_id.to_owned(),
            self.config.limits.clone(),
        );
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                task_session(&tx, &owner, session, server)?;
                if let Some((key,digest)) = receipt
                    && let Some(saved) = cancel_receipt(&tx,&owner,server,&task_id,key,digest,&limits)?
                { tx.commit()?; return Ok(saved); }
                let task = current_task(&tx, &owner, server, &task_id, now, &limits)?;
                if task.status.is_terminal() {
                    return Err(StoreError::Terminal);
                }
                cancel_interaction(&tx, &owner, task.interaction_id, now, &limits)?;
                let payload = task_error(&task.task_id, -32800, "Task cancelled.", &limits)?;
                finish_task(
                    &tx,
                    &owner,
                    &task,
                    McpReceivedTaskStatus::Cancelled,
                    true,
                    payload,
                    None,
                    now,
                    &limits,
                )?;
                if let Some((key,digest)) = receipt {
                    tx.execute("INSERT INTO mcp_received_task_cancel_receipts(request_key,session_id,task_id,request_digest) VALUES(?1,?2,?3,?4)", params![key.to_string(),session.to_string(),task_id,digest.to_string()])?;
                    fact(&tx,&owner,Some(task.interaction_id),"receiver_task_cancel_receipt",now,&Value::Text(key.to_string()),&limits)?;
                }
                let saved = load_task(&tx, &owner, server, &task_id, &limits)?
                    .ok_or(StoreError::Corrupt)?;
                tx.commit()?;
                Ok(saved)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Records sending the stable CreateTaskResult separately from delivery of an
    /// underlying interaction reply. Lost transport acceptance never permits work
    /// redispatch. HTTP acceptance is not proof of application consumption.
    ///
    /// # Errors
    /// Returns stale authority, acceptance without a send attempt, or storage failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_mcp_received_task_acceptance_delivery(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        task_id: &str,
        accepted: bool,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        self.check_text(task_id)?;
        let (owner, task_id, limits) = (
            owner.clone(),
            task_id.to_owned(),
            self.config.limits.clone(),
        );
        self.connection.call(move |db| -> Result<_,StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx,&owner,now_ms)?;
            task_session(&tx,&owner,session,server)?;
            let task = load_task(&tx,&owner,server,&task_id,&limits)?.ok_or(StoreError::Missing)?;
            let (attempts,was_accepted): (i64,bool) = tx.query_row("SELECT acceptance_send_attempts,acceptance_accepted FROM mcp_received_tasks WHERE task_id=?1", [&task_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
            if accepted && attempts==0 { return Err(StoreError::Invalid); }
            if accepted && was_accepted { tx.commit()?; return Ok(()); }
            let attempts = attempts.checked_add(i64::from(!accepted)).ok_or(StoreError::Invalid)?;
            tx.execute("UPDATE mcp_received_tasks SET acceptance_send_attempts=?2,acceptance_accepted=?3 WHERE task_id=?1",params![task_id,attempts,accepted || was_accepted])?;
            fact(&tx,&owner,Some(task.interaction_id),if accepted {"receiver_task_acceptance_accepted"} else {"receiver_task_acceptance_send"},now,&Value::Integer(attempts),&limits)?;
            tx.commit()?; Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    /// Explicitly marks a live task as needing requestor-side input, or resumes
    /// working. Host-user input alone is not requestor-side input. The task must
    /// accompany the corresponding request with related-task metadata.
    ///
    /// # Errors
    /// Returns terminal/expired task or stale authority; terminal states never move.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_mcp_received_task_input(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        task_id: &str,
        input_required: bool,
        now_ms: i64,
    ) -> Result<McpReceivedTask, StoreError> {
        timestamp(now_ms)?;
        self.check_text(task_id)?;
        let (owner, task_id, limits) = (
            owner.clone(),
            task_id.to_owned(),
            self.config.limits.clone(),
        );
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            task_session(&tx, &owner, session, server)?;
            let task = current_task(&tx, &owner, server, &task_id, now, &limits)?;
            if task.status.is_terminal() { return Err(StoreError::Terminal); }
            let status = if input_required { McpReceivedTaskStatus::InputRequired } else { McpReceivedTaskStatus::Working };
            if status != task.status {
                tx.execute("UPDATE mcp_received_tasks SET status=?2,updated_ms=?3,revision=revision+1 WHERE task_id=?1", params![task_id,status.as_str(),now])?;
                fact(&tx, &owner, Some(task.interaction_id), "receiver_task_status", now, &Value::Text(status.as_str().into()), &limits)?;
            }
            let saved = load_task(&tx, &owner, server, &task_id, &limits)?.ok_or(StoreError::Corrupt)?;
            tx.commit()?; Ok(saved)
        }).await.map_err(StoreError::from_worker)
    }
}

fn cancel_request(
    request: &JsonDocument,
    session: Digest,
    limits: &Limits,
) -> Result<(String, Digest, Digest), StoreError> {
    let fields = map(request.value())?;
    if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
        || fields.get("method") != Some(&Value::Text("tasks/cancel".into()))
        || fields.get("result").is_some()
        || fields.get("error").is_some()
    {
        return Err(StoreError::Invalid);
    }
    let id = McpRequestId::from_value(field(fields, "id")?).map_err(|_| StoreError::Invalid)?;
    let Value::Text(task_id) = field(map(field(fields, "params")?)?, "taskId")? else {
        return Err(StoreError::Invalid);
    };
    let key = identity(
        "receiver-task-cancel",
        vec![Value::Text(session.to_string()), id.to_value()],
        limits,
    )?;
    Ok((task_id.clone(), key, request.digest()))
}
fn cancel_receipt(
    db: &Db,
    owner: &McpOwner,
    server: Digest,
    task_id: &str,
    key: Digest,
    digest: Digest,
    limits: &Limits,
) -> Result<Option<McpReceivedTask>, StoreError> {
    let saved: Option<(String,String)> = db.query_row("SELECT task_id,request_digest FROM mcp_received_task_cancel_receipts WHERE request_key=?1",[key.to_string()],|r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((saved_id, saved_digest)) = saved else {
        return Ok(None);
    };
    if saved_id != task_id || saved_digest != digest.to_string() {
        return Err(StoreError::IdempotencyConflict);
    }
    let task = load_task(db, owner, server, task_id, limits)?.ok_or(StoreError::Missing)?;
    if task.status != McpReceivedTaskStatus::Cancelled {
        return Err(StoreError::Corrupt);
    }
    Ok(Some(task))
}
fn task_session(
    db: &Db,
    owner: &McpOwner,
    session: Digest,
    server: Digest,
) -> Result<(), StoreError> {
    session_authority(db, owner, session, true)?;
    let actual: String = db.query_row(
        "SELECT server_digest FROM mcp_epochs WHERE session_id=?1",
        [session.to_string()],
        |r| r.get(0),
    )?;
    if actual != server.to_string() {
        return Err(StoreError::Fenced);
    }
    Ok(())
}
fn current_task(
    db: &Db,
    owner: &McpOwner,
    server: Digest,
    id: &str,
    now: i64,
    limits: &Limits,
) -> Result<McpReceivedTask, StoreError> {
    let task = load_task(db, owner, server, id, limits)?.ok_or(StoreError::Missing)?;
    if now >= task.expires_ms {
        return Err(StoreError::Expired);
    }
    if task.status.is_terminal() {
        return Ok(task);
    }
    let work = interaction(db, owner, task.interaction_id, limits)?.ok_or(StoreError::Corrupt)?;
    if work.session != task.session || work.generation != task.generation {
        return Err(StoreError::Corrupt);
    }
    if work.state == McpWorkState::Result
        || (work.state == McpWorkState::Cancelled && work.reply.is_some())
    {
        let reply = work.reply.as_ref().ok_or(StoreError::Corrupt)?;
        let parsed = JsonDocument::new(reply, limits).map_err(|_| StoreError::Corrupt)?;
        let fields = map(parsed.value())?;
        let (error, value) = match (fields.get("result"), fields.get("error")) {
            (Some(value), None) => (false, value),
            (None, Some(value)) => (true, value),
            _ => return Err(StoreError::Corrupt),
        };
        let payload = related_payload(value, &task.task_id, limits)?;
        let status = if work.state == McpWorkState::Cancelled {
            if !error {
                return Err(StoreError::Corrupt);
            }
            McpReceivedTaskStatus::Cancelled
        } else if error {
            McpReceivedTaskStatus::Failed
        } else {
            McpReceivedTaskStatus::Completed
        };
        finish_task(
            db,
            owner,
            &task,
            status,
            error,
            payload,
            work.reply_digest,
            now,
            limits,
        )?;
    } else if work.state == McpWorkState::Cancelled || now >= work.deadline_ms {
        let cancelled = work.state == McpWorkState::Cancelled;
        if !cancelled {
            cancel_interaction(db, owner, work.id, now, limits)?;
        }
        let status = if cancelled {
            McpReceivedTaskStatus::Cancelled
        } else {
            McpReceivedTaskStatus::Failed
        };
        let payload = task_error(
            &task.task_id,
            if cancelled { -32800 } else { -32000 },
            if cancelled {
                "Task cancelled."
            } else {
                "Task execution deadline exceeded."
            },
            limits,
        )?;
        finish_task(db, owner, &task, status, true, payload, None, now, limits)?;
    }
    load_task(db, owner, server, id, limits)?.ok_or(StoreError::Corrupt)
}
fn cancel_interaction(
    db: &Db,
    owner: &McpOwner,
    id: Digest,
    now: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    checked_usage(db, owner)?;
    let work = interaction(db, owner, id, limits)?.ok_or(StoreError::Missing)?;
    if work.state == McpWorkState::Result {
        return Err(StoreError::Terminal);
    }
    if work.state != McpWorkState::Cancelled {
        db.execute("UPDATE mcp_interactions SET state='cancelled',revision=revision+1 WHERE interaction_id=?1", [id.to_string()])?;
        fact(
            db,
            owner,
            Some(id),
            "work_cancelled",
            now,
            &Value::Bool(work.outstanding),
            limits,
        )?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn finish_task(
    db: &Db,
    owner: &McpOwner,
    task: &McpReceivedTask,
    status: McpReceivedTaskStatus,
    error: bool,
    payload: JsonDocument,
    source: Option<Digest>,
    now: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    if !status.is_terminal()
        || payload.as_bytes().len() > task.result_capacity
        || now > MAX_PROTOCOL_TIME
    {
        return Err(StoreError::Invalid);
    }
    let digest = payload.digest();
    if db.execute("UPDATE mcp_received_tasks SET status=?2,result_kind=?3,result=?4,result_digest=?5,source_reply_digest=?6,updated_ms=?7,revision=revision+1 WHERE task_id=?1 AND status IN ('working','input_required') AND revision=?8", params![task.task_id,status.as_str(),if error {"error"} else {"result"},payload.as_bytes(),digest.to_string(),source.map(|d| d.to_string()),now.max(task.updated_ms),task.revision])? != 1 { return Err(StoreError::Conflict); }
    fact(
        db,
        owner,
        Some(task.interaction_id),
        "receiver_task_terminal",
        now,
        &Value::Array(vec![
            Value::Text(task.task_id.clone()),
            Value::Text(status.as_str().into()),
            Value::Text(digest.to_string()),
        ]),
        limits,
    )
}
fn load_task(
    db: &Db,
    owner: &McpOwner,
    server: Digest,
    id: &str,
    limits: &Limits,
) -> Result<Option<McpReceivedTask>, StoreError> {
    let mut statement = db.prepare("SELECT interaction_id,session_id,method,generation,created_ms,updated_ms,expires_ms,ttl_ms,poll_interval_ms,status,revision,result_capacity,result_kind,result,result_digest,source_reply_digest FROM mcp_received_tasks WHERE task_id=?1 AND owner_key=?2 AND server_digest=?3 AND generation=?4")?;
    let mut rows = statement.query(params![
        id,
        owner.key().to_string(),
        server.to_string(),
        owner.generation()
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let result_capacity: usize = row.get(11)?;
    if result_capacity > limits.max_document_bytes / 4
        || id.len() != 64
        || !id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(StoreError::Corrupt);
    }
    let result = match (row.get::<_, Option<String>>(12)?, row.get_ref(13)?) {
        (None, rusqlite::types::ValueRef::Null) => None,
        (Some(kind), rusqlite::types::ValueRef::Blob(bytes)) if bytes.len() <= result_capacity => {
            let payload = JsonDocument::new(bytes, limits).map_err(|_| StoreError::Corrupt)?;
            if payload.digest().to_string() != row.get::<_, String>(14)? {
                return Err(StoreError::Corrupt);
            }
            let meta = map(field(map(payload.value())?, "_meta")?)?;
            if field(map(field(meta, RELATED_TASK)?)?, "taskId")? != &Value::Text(id.into()) {
                return Err(StoreError::Corrupt);
            }
            Some(McpReceivedTaskResult {
                is_error: match kind.as_str() {
                    "result" => false,
                    "error" => true,
                    _ => return Err(StoreError::Corrupt),
                },
                payload,
                source_reply_digest: row
                    .get::<_, Option<String>>(15)?
                    .map(|v| v.parse().map_err(|_| StoreError::Corrupt))
                    .transpose()?,
            })
        }
        _ => return Err(StoreError::Corrupt),
    };
    let task = McpReceivedTask {
        task_id: id.into(),
        interaction_id: row
            .get::<_, String>(0)?
            .parse()
            .map_err(|_| StoreError::Corrupt)?,
        session: row
            .get::<_, String>(1)?
            .parse()
            .map_err(|_| StoreError::Corrupt)?,
        server,
        method: row.get(2)?,
        generation: row.get(3)?,
        created_ms: row.get(4)?,
        updated_ms: row.get(5)?,
        expires_ms: row.get(6)?,
        ttl_ms: row.get(7)?,
        poll_interval_ms: row.get(8)?,
        status: McpReceivedTaskStatus::parse(&row.get::<_, String>(9)?)?,
        revision: row.get(10)?,
        result_capacity,
        result,
    };
    if task.status.is_terminal() != task.result.is_some()
        || task.created_ms < 0
        || task.updated_ms < task.created_ms
        || task.updated_ms > MAX_PROTOCOL_TIME
        || task.expires_ms > MAX_PROTOCOL_TIME
        || task.created_ms.checked_add(task.ttl_ms as i64) != Some(task.expires_ms)
        || task.poll_interval_ms == 0
    {
        return Err(StoreError::Corrupt);
    }
    if let Some(result) = &task.result {
        if result.is_error != (task.status != McpReceivedTaskStatus::Completed) {
            return Err(StoreError::Corrupt);
        }
        if let Some(source) = result.source_reply_digest {
            let work =
                interaction(db, owner, task.interaction_id, limits)?.ok_or(StoreError::Corrupt)?;
            if work.reply_digest != Some(source)
                || !(work.state == McpWorkState::Result
                    || (work.state == McpWorkState::Cancelled
                        && task.status == McpReceivedTaskStatus::Cancelled))
                || work.session != task.session
            {
                return Err(StoreError::Corrupt);
            }
            let reply =
                JsonDocument::new(work.reply.as_deref().ok_or(StoreError::Corrupt)?, limits)
                    .map_err(|_| StoreError::Corrupt)?;
            let original = field(
                map(reply.value())?,
                if result.is_error { "error" } else { "result" },
            )?;
            if related_payload(original, id, limits)?.digest() != result.payload.digest() {
                return Err(StoreError::Corrupt);
            }
        } else {
            let (code, message) = match task.status {
                McpReceivedTaskStatus::Cancelled => (-32800, "Task cancelled."),
                McpReceivedTaskStatus::Failed => (-32000, "Task execution deadline exceeded."),
                _ => return Err(StoreError::Corrupt),
            };
            if task_error(id, code, message, limits)?.digest() != result.payload.digest() {
                return Err(StoreError::Corrupt);
            }
        }
    }
    Ok(Some(task))
}
fn document(entries: Vec<(String, Value)>, limits: &Limits) -> Result<JsonDocument, StoreError> {
    JsonDocument::from_value(&Value::Map(Map::try_from_entries(entries)?), limits)
        .map_err(|_| StoreError::Invalid)
}
fn related_payload(
    value: &Value,
    task_id: &str,
    limits: &Limits,
) -> Result<JsonDocument, StoreError> {
    let fields = map(value)?;
    let mut entries: Vec<_> = fields
        .iter()
        .filter(|(key, _)| *key != "_meta")
        .map(|(key, value)| (key.into(), value.clone()))
        .collect();
    let mut meta: Vec<_> = match fields.get("_meta") {
        None => vec![],
        Some(value) => map(value)?
            .iter()
            .filter(|(key, _)| *key != RELATED_TASK)
            .map(|(key, value)| (key.into(), value.clone()))
            .collect(),
    };
    meta.push((
        RELATED_TASK.into(),
        Value::Map(Map::try_from_entries([(
            "taskId".into(),
            Value::Text(task_id.into()),
        )])?),
    ));
    entries.push(("_meta".into(), Value::Map(Map::try_from_entries(meta)?)));
    document(entries, limits)
}
fn task_error(
    task: &str,
    code: i64,
    message: &str,
    limits: &Limits,
) -> Result<JsonDocument, StoreError> {
    related_payload(
        document(
            vec![
                ("code".into(), Value::Integer(code)),
                ("message".into(), Value::Text(message.into())),
            ],
            limits,
        )?
        .value(),
        task,
        limits,
    )
}
fn task_document(
    task: &McpReceivedTask,
    status: McpReceivedTaskStatus,
    updated: i64,
    limits: &Limits,
) -> Result<JsonDocument, StoreError> {
    document(
        vec![
            ("taskId".into(), Value::Text(task.task_id.clone())),
            ("status".into(), Value::Text(status.as_str().into())),
            (
                "createdAt".into(),
                Value::Text(protocol_time(task.created_ms)?),
            ),
            ("lastUpdatedAt".into(), Value::Text(protocol_time(updated)?)),
            ("ttl".into(), Value::Integer(task.ttl_ms as i64)),
            (
                "pollInterval".into(),
                Value::Integer(task.poll_interval_ms as i64),
            ),
        ],
        limits,
    )
}
fn protocol_time(ms: i64) -> Result<String, StoreError> {
    if !(0..=MAX_PROTOCOL_TIME).contains(&ms) {
        return Err(StoreError::Invalid);
    }
    let seconds = ms / 1000;
    let z = seconds / 86400 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds % 86400 / 3600,
        seconds % 3600 / 60,
        seconds % 60,
        ms % 1000
    ))
}
