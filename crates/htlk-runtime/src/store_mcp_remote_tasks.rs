//! Outbound creation intent and accepted identity are separate durable facts.
//! Existing intent never grants a second send, even after ownership/session change.
use super::*;
use crate::mcp::{McpAttemptContext, client::McpTaskState};

/// Retained remote task attribution, independently bound to actual session authority.
#[derive(Clone)]
pub struct McpRemoteTaskRecord {
    /// Stable logical creation intent identity.
    pub key: Digest,
    /// Accepted task state, absent for an interrupted/unacknowledged creation.
    pub task: Option<McpTaskState>,
    /// Trusted originating attempt; never inferred from the latest pending RPC.
    pub origin: Option<McpOperationOrigin>,
    /// Conservative original operation protection.
    pub labels: SecurityLabels,
    /// Original operation deadline, never renewed by polling or progress.
    pub deadline_ms: i64,
    /// Local retention deadline fixed when acceptance was committed.
    pub expires_ms: Option<i64>,
    /// Exact original acceptance bytes, separate from the current status snapshot.
    pub acceptance: Option<Vec<u8>>,
    /// Exact final result/error bytes; bool distinguishes JSON-RPC error payloads.
    pub final_payload: Option<(bool, Vec<u8>)>,
    /// Terminal execution proven by a successful deferred tool result, distinct
    /// from the last receiver-supplied timestamped task observation.
    pub result_status: Option<crate::mcp::client::McpTaskStatus>,
    /// Exact error rejecting the original creating RPC, never a deferred result.
    pub create_error: Option<Vec<u8>>,
}
impl fmt::Debug for McpRemoteTaskRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpRemoteTaskRecord")
            .field("key", &self.key)
            .field("task", &self.task)
            .field("has_final", &self.final_payload.is_some())
            .finish_non_exhaustive()
    }
}
impl SqliteStore {
    /// Retains a correlated creation error separately from acceptance and deferred
    /// results. The logical intent remains non-recreatable.
    ///
    /// # Errors
    /// Returns stale authority, invalid error, conflict or exhausted byte retention.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_mcp_remote_task_create_error(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        key: Digest,
        bytes: Vec<u8>,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let key = remote_key(owner, server, key, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        if bytes.len() > limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        crate::mcp::client::McpRpcError::from_bytes(&bytes, &limits)
            .map_err(|_| StoreError::Invalid)?;
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;remote_session(&tx,&owner,session,server)?;
            let old=remote_record(&tx,&owner,session,server,key,&limits)?;
            if old.task.is_some() || old.create_error.is_some_and(|old|old!=bytes){return Err(StoreError::IdempotencyConflict)}
            tx.execute("UPDATE mcp_remote_tasks SET create_error=?2,create_error_digest=?3 WHERE intent_key=?1",params![key.to_string(),bytes,hash_bytes(&bytes).to_string()])?;
            remote_bytes(&tx,&owner,&limits)?;
            fact(&tx,&owner,None,"remote_task_create_error",now,&Value::Text(key.to_string()),&limits)?;
            tx.commit()?;Ok(())
        }).await.map_err(StoreError::from_worker)
    }
    /// Reads protected creation evidence without dispatch or restoration. Historical
    /// evidence remains readable after transport closure; callers must authorize
    /// it just like interaction history. This grants no new epoch attribution.
    ///
    /// # Errors
    /// Returns a missing/cross-owner intent or corrupt stored identity.
    pub async fn inspect_mcp_remote_task_intent(
        &self,
        owner: &McpOwner,
        server: Digest,
        key: Digest,
    ) -> Result<McpRemoteTaskRecord, StoreError> {
        let key = remote_key(owner, server, key, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move|db|->Result<_,StoreError>{
            let session:String=db.query_row("SELECT session_id FROM mcp_remote_tasks WHERE intent_key=?1 AND owner_key=?2 AND server_digest=?3",params![key.to_string(),owner.key().to_string(),server.to_string()],|r|r.get(0)).optional()?.ok_or(StoreError::Missing)?;
            remote_record(db,&owner,session.parse().map_err(|_|StoreError::Corrupt)?,server,key,&limits)
        }).await.map_err(StoreError::from_worker)
    }
    /// Commits a unique creation intent before send. False means the logical key
    /// already exists and MUST NOT be sent again. Different params/descriptor/origin
    /// under that key are an integrity conflict, not permission for another task.
    ///
    /// # Errors
    /// Returns stale authority, malformed pins, exhausted retention or conflict.
    #[allow(clippy::too_many_arguments)]
    pub async fn claim_mcp_remote_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        key: Digest,
        descriptor: JsonDocument,
        params_doc: JsonDocument,
        context: Option<McpAttemptContext>,
        now_ms: i64,
        deadline_ms: i64,
        maximum: usize,
    ) -> Result<bool, StoreError> {
        timestamp(now_ms)?;
        let key = remote_key(owner, server, key, &self.config.limits)?;
        if maximum == 0
            || maximum > 65_536
            || descriptor.as_bytes().len() > self.config.limits.max_document_bytes / 8
            || params_doc.as_bytes().len() > self.config.limits.max_document_bytes / 8
        {
            return Err(StoreError::Invalid);
        }
        htlk_analyzer::validate_mcp_descriptor(
            htlk_analyzer::McpDescriptorKind::Tool,
            &descriptor,
            &self.config.limits,
        )
        .map_err(|_| StoreError::Invalid)?;
        let descriptor_fields = map(descriptor.value())?;
        let params = map(params_doc.value())?;
        if params.get("name") != descriptor_fields.get("name")
            || params
                .get("arguments")
                .is_some_and(|value| !matches!(value, Value::Map(_)))
        {
            return Err(StoreError::Invalid);
        }
        let support = descriptor_fields
            .get("execution")
            .and_then(|value| map(value).ok()?.get("taskSupport"));
        if !matches!(support,Some(Value::Text(support)) if support=="required" || support=="optional")
        {
            return Err(StoreError::Invalid);
        }
        let metadata = map(field(params, "task")?)?;
        if metadata
            .get("ttl")
            .is_some_and(|value| !matches!(value,Value::Integer(n) if *n>=0))
        {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_,StoreError> {
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;
            remote_session(&tx,&owner,session,server)?;
            let origin=context.as_ref().map(|c|c.origin.clone());
            let labels=context.as_ref().map_or_else(SecurityLabels::default,|c|c.labels.clone());
            if context.as_ref().is_some_and(|c|c.server!=server) {return Err(StoreError::Fenced)}
            let (labels,deadline)=run::admit(&tx,&owner,session,origin.as_ref(),"tools/call",labels,deadline_ms,now,&limits)?;
            let existing:Option<(Vec<u8>,Vec<u8>,Option<String>)>=tx.query_row("SELECT descriptor,params,origin_attempt FROM mcp_remote_tasks WHERE intent_key=?1 AND owner_key=?2 AND server_digest=?3",params![key.to_string(),owner.key().to_string(),server.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((d,p,a))=existing {
                if d!=descriptor.as_bytes() || p!=params_doc.as_bytes() || a!=origin.as_ref().map(|o|o.attempt_id.to_string()) {return Err(StoreError::IdempotencyConflict)}
                tx.commit()?;return Ok(false)
            }
            let count:usize=tx.query_row("SELECT count(*) FROM mcp_remote_tasks WHERE owner_key=?1",[owner.key().to_string()],|r|r.get(0))?;
            if count>=maximum {return Err(StoreError::Budget)}
            let protection=cbor::encode(&labels.to_value(&limits).map_err(|_|StoreError::Invalid)?,&limits)?;
            if protection.len()>limits.max_document_bytes/8 {return Err(StoreError::Invalid)}
            tx.execute("INSERT INTO mcp_remote_tasks(intent_key,owner_key,server_digest,session_id,generation,descriptor,params,origin_run,origin_invocation,origin_attempt,protection,deadline_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![key.to_string(),owner.key().to_string(),server.to_string(),session.to_string(),owner.generation(),descriptor.as_bytes(),params_doc.as_bytes(),origin.as_ref().map(|o|o.run_id.as_str()),origin.as_ref().map(|o|o.invocation_id.to_string()),origin.as_ref().map(|o|o.attempt_id.to_string()),protection,deadline])?;
            remote_bytes(&tx,&owner,&limits)?;
            fact(&tx,&owner,None,"remote_task_intent",now,&Value::Text(key.to_string()),&limits)?;
            tx.commit()?;Ok(true)
        }).await.map_err(StoreError::from_worker)
    }

    /// Saves acceptance before any poll/result request. Repeated acceptance must
    /// preserve both identity and exact acceptance bytes. Lifetime is not reset.
    ///
    /// # Errors
    /// Returns stale/unknown intent, conflicting identity, malformed state or bounds.
    #[allow(clippy::too_many_arguments)]
    pub async fn accept_mcp_remote_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        key: Digest,
        task: McpTaskState,
        acceptance: Vec<u8>,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let key = remote_key(owner, server, key, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        if acceptance.len() > limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;
            remote_session(&tx,&owner,session,server)?;
            let old=remote_record(&tx,&owner,session,server,key,&limits)?;
            if old.create_error.is_some(){return Err(StoreError::IdempotencyConflict)}
            if let Some(previous)=old.task {
                if previous.task_id()!=task.task_id() || old.acceptance.as_deref()!=Some(acceptance.as_slice()){return Err(StoreError::IdempotencyConflict)}
                tx.commit()?;return Ok(())
            }
            if task.status()!=crate::mcp::client::McpTaskStatus::Working{return Err(StoreError::Invalid)}
            let parsed=JsonDocument::new(&acceptance,&limits).map_err(|_|StoreError::Invalid)?;
            htlk_analyzer::validate_mcp_message(htlk_analyzer::McpMessageKind::CreateTaskResult,&parsed,&limits).map_err(|_|StoreError::Invalid)?;
            let fields=map(parsed.value())?;
            if fields.get("content").is_some() || fields.get("isError").is_some() {return Err(StoreError::Invalid)}
            if let Some(related)=crate::mcp::client::mcp_related_task_id(parsed.value()).map_err(|_|StoreError::Invalid)? && related!=task.task_id(){return Err(StoreError::Invalid)}
            if map(parsed.value())?.get("task")!=Some(task.document().value()){return Err(StoreError::Invalid)}
            let expires=task.remaining_ttl_ms(now).map(|ttl| now.checked_add(ttl as i64).ok_or(StoreError::Invalid)).transpose()?;
            tx.execute("UPDATE mcp_remote_tasks SET task_id=?2,snapshot=?3,acceptance=?4,expires_ms=?5,snapshot_digest=?6,acceptance_digest=?7 WHERE intent_key=?1",params![key.to_string(),task.task_id(),task.document().as_bytes(),acceptance,expires,hash_bytes(task.document().as_bytes()).to_string(),hash_bytes(&acceptance).to_string()])?;
            remote_bytes(&tx,&owner,&limits)?;
            fact(&tx,&owner,None,"remote_task_accepted",now,&Value::Text(key.to_string()),&limits)?;
            tx.commit()?;Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    /// Saves an independently admitted successor or immutable final result/error.
    /// Original result/error bytes are separate from acceptance and status state.
    ///
    /// # Errors
    /// Returns terminal regression, conflicting final bytes, expiry or stale authority.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_mcp_remote_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        key: Digest,
        task: Option<McpTaskState>,
        final_payload: Option<(bool, Vec<u8>)>,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let key = remote_key(owner, server, key, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;remote_session(&tx,&owner,session,server)?;
            let old=remote_record(&tx,&owner,session,server,key,&limits)?;
            if old.expires_ms.is_some_and(|e|now>=e){return Err(StoreError::Expired)}
            let previous=old.task.ok_or(StoreError::Missing)?;
            if let Some(task)=task {
                previous.validate_successor(&task).map_err(|_|StoreError::Terminal)?;
                if old.result_status.is_some_and(|status|status!=task.status()){return Err(StoreError::Terminal)}
                tx.execute("UPDATE mcp_remote_tasks SET snapshot=?2,snapshot_digest=?3 WHERE intent_key=?1",params![key.to_string(),task.document().as_bytes(),hash_bytes(task.document().as_bytes()).to_string()])?;
            }
            if let Some((error,bytes))=final_payload {
                if bytes.len()>limits.max_document_bytes/8{return Err(StoreError::Invalid)}
                JsonDocument::new(&bytes,&limits).map_err(|_|StoreError::Invalid)?;
                if !error {
                    let document=JsonDocument::new(&bytes,&limits).map_err(|_|StoreError::Invalid)?;
                    crate::validate_mcp_tool_result(&document,&limits).map_err(|_|StoreError::Invalid)?;
                    if crate::mcp::client::mcp_related_task_id(document.value()).map_err(|_|StoreError::Invalid)?!=Some(previous.task_id()){return Err(StoreError::Invalid)}
                    let failed=map(document.value())?.get("isError")==Some(&Value::Bool(true));
                    let status=if previous.status()==crate::mcp::client::McpTaskStatus::Cancelled {"cancelled"}else if failed {"failed"}else{"completed"};
                    if (previous.status()==crate::mcp::client::McpTaskStatus::Completed && failed) || (previous.status()==crate::mcp::client::McpTaskStatus::Failed && !failed){return Err(StoreError::Terminal)}
                    tx.execute("UPDATE mcp_remote_tasks SET result_status=?2 WHERE intent_key=?1",params![key.to_string(),status])?;
                }
                let previous:Option<(String,Vec<u8>)>=tx.query_row("SELECT final_kind,final_payload FROM mcp_remote_tasks WHERE intent_key=?1 AND final_kind IS NOT NULL",[key.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                let kind=if error{"error"}else{"result"};
                if previous.is_some_and(|p|p!=(kind.into(),bytes.clone())){return Err(StoreError::IdempotencyConflict)}
                tx.execute("UPDATE mcp_remote_tasks SET final_kind=?2,final_payload=?3,final_digest=?4 WHERE intent_key=?1",params![key.to_string(),kind,bytes,hash_bytes(&bytes).to_string()])?;
            }
            remote_bytes(&tx,&owner,&limits)?;
            fact(&tx,&owner,None,"remote_task_observed",now,&Value::Text(key.to_string()),&limits)?;
            tx.commit()?;Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    /// Resolves explicit task metadata in this exact epoch. No last-active-operation
    /// guess is used. Session/owner generation, server, TTL and deadline are checked.
    ///
    /// # Errors
    /// Returns unknown/expired task, stale authority or corrupt attribution.
    pub async fn resolve_mcp_remote_task(
        &self,
        owner: &McpOwner,
        session: Digest,
        server: Digest,
        task_id: &str,
        now_ms: i64,
    ) -> Result<McpRemoteTaskRecord, StoreError> {
        timestamp(now_ms)?;
        self.check_text(task_id)?;
        let (owner, task_id, limits) = (
            owner.clone(),
            task_id.to_owned(),
            self.config.limits.clone(),
        );
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;remote_session(&tx,&owner,session,server)?;
            let key:String=tx.query_row("SELECT intent_key FROM mcp_remote_tasks WHERE owner_key=?1 AND session_id=?2 AND server_digest=?3 AND task_id=?4",params![owner.key().to_string(),session.to_string(),server.to_string(),task_id],|r|r.get(0)).optional()?.ok_or(StoreError::Missing)?;
            let record=remote_record(&tx,&owner,session,server,key.parse().map_err(|_|StoreError::Corrupt)?,&limits)?;
            if now>=record.deadline_ms || record.expires_ms.is_some_and(|e|now>=e){return Err(StoreError::Expired)}
            if record.result_status.is_some() || record.task.as_ref().is_some_and(|task|task.status().is_terminal()){return Err(StoreError::Terminal)}
            tx.commit()?;Ok(record)
        }).await.map_err(StoreError::from_worker)
    }
}
fn remote_session(
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
fn remote_key(
    owner: &McpOwner,
    server: Digest,
    key: Digest,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    identity(
        "remote-task-intent",
        vec![
            Value::Text(owner.key().to_string()),
            Value::Text(server.to_string()),
            Value::Text(key.to_string()),
        ],
        limits,
    )
}
fn remote_record(
    db: &Db,
    owner: &McpOwner,
    session: Digest,
    server: Digest,
    key: Digest,
    limits: &Limits,
) -> Result<McpRemoteTaskRecord, StoreError> {
    let mut statement=db.prepare("SELECT snapshot,origin_run,origin_invocation,origin_attempt,protection,deadline_ms,expires_ms,acceptance,final_kind,final_payload,snapshot_digest,acceptance_digest,final_digest,result_status,create_error,create_error_digest FROM mcp_remote_tasks WHERE intent_key=?1 AND owner_key=?2 AND session_id=?3 AND server_digest=?4 AND generation=?5")?;
    let mut rows = statement.query(params![
        key.to_string(),
        owner.key().to_string(),
        session.to_string(),
        server.to_string(),
        owner.generation()
    ])?;
    let row = rows.next()?.ok_or(StoreError::Missing)?;
    let origin = match (
        row.get::<_, Option<String>>(1)?,
        row.get::<_, Option<String>>(2)?,
        row.get::<_, Option<String>>(3)?,
    ) {
        (None, None, None) => None,
        (Some(run), Some(invocation), Some(attempt)) => Some(McpOperationOrigin {
            run_id: RunId::new(run, limits).map_err(|_| StoreError::Corrupt)?,
            invocation_id: invocation.parse().map_err(|_| StoreError::Corrupt)?,
            attempt_id: attempt.parse().map_err(|_| StoreError::Corrupt)?,
        }),
        _ => return Err(StoreError::Corrupt),
    };
    let checked = |index, digest_index| -> Result<Option<Vec<u8>>, StoreError> {
        let bytes = bounded_blob(row, index, limits)?;
        if bytes.as_ref().map(|b| hash_bytes(b).to_string())
            != row.get::<_, Option<String>>(digest_index)?
        {
            return Err(StoreError::Corrupt);
        }
        Ok(bytes)
    };
    let task = checked(0, 10)?
        .map(|bytes| {
            McpTaskState::from_document(
                JsonDocument::new(&bytes, limits).map_err(|_| StoreError::Corrupt)?,
                limits,
            )
            .map_err(|_| StoreError::Corrupt)
        })
        .transpose()?;
    let acceptance = checked(7, 11)?;
    if let (Some(task), Some(acceptance)) = (&task, &acceptance) {
        let parsed = JsonDocument::new(acceptance, limits).map_err(|_| StoreError::Corrupt)?;
        let initial = map(parsed.value())?
            .get("task")
            .ok_or(StoreError::Corrupt)?;
        let initial = McpTaskState::from_document(
            JsonDocument::from_value(initial, limits).map_err(|_| StoreError::Corrupt)?,
            limits,
        )
        .map_err(|_| StoreError::Corrupt)?;
        initial
            .validate_successor(task)
            .map_err(|_| StoreError::Corrupt)?;
    } else if task.is_some() != acceptance.is_some() {
        return Err(StoreError::Corrupt);
    }
    let final_payload = match (row.get::<_, Option<String>>(8)?, checked(9, 12)?) {
        (None, None) => None,
        (Some(kind), Some(bytes)) => {
            JsonDocument::new(&bytes, limits).map_err(|_| StoreError::Corrupt)?;
            Some((
                match kind.as_str() {
                    "result" => false,
                    "error" => true,
                    _ => return Err(StoreError::Corrupt),
                },
                bytes,
            ))
        }
        _ => return Err(StoreError::Corrupt),
    };
    let protection = bounded_blob(row, 4, limits)?.ok_or(StoreError::Corrupt)?;
    let result_status = match row.get::<_, Option<String>>(13)?.as_deref() {
        None => None,
        Some("completed") => Some(crate::mcp::client::McpTaskStatus::Completed),
        Some("failed") => Some(crate::mcp::client::McpTaskStatus::Failed),
        Some("cancelled") => Some(crate::mcp::client::McpTaskStatus::Cancelled),
        _ => return Err(StoreError::Corrupt),
    };
    let create_error = checked(14, 15)?;
    if let Some(error) = &create_error {
        crate::mcp::client::McpRpcError::from_bytes(error, limits)
            .map_err(|_| StoreError::Corrupt)?;
        if task.is_some() {
            return Err(StoreError::Corrupt);
        }
    }
    Ok(McpRemoteTaskRecord {
        key,
        task,
        origin,
        labels: SecurityLabels::from_value(&cbor::decode(&protection, limits)?, limits)
            .map_err(|_| StoreError::Corrupt)?,
        deadline_ms: row.get(5)?,
        expires_ms: row.get(6)?,
        acceptance,
        final_payload,
        result_status,
        create_error,
    })
}
fn remote_bytes(db: &Db, owner: &McpOwner, limits: &Limits) -> Result<(), StoreError> {
    let bytes:usize=db.query_row("SELECT coalesce(sum(length(descriptor)+length(params)+coalesce(length(snapshot),0)+coalesce(length(acceptance),0)+coalesce(length(final_payload),0)+coalesce(length(create_error),0)+1024),0) FROM mcp_remote_tasks WHERE owner_key=?1",[owner.key().to_string()],|r|r.get(0))?;
    if bytes > limits.max_document_bytes {
        return Err(StoreError::Budget);
    }
    Ok(())
}
