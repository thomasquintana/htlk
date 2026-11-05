//! Transactional host-session ownership and client-work facts. As with the other
//! store APIs, these are trusted infrastructure, not authenticated frontend APIs.
use super::*;
use crate::{
    HostContext,
    mcp::client::{McpOwner, McpOwnerScope, McpRequestId},
    records::SecurityLabels,
};
use htlk_executable::{JsonDocument, digest::hash_bytes};
use tokio_rusqlite::rusqlite::Connection as Db;

/// Durable session identity. Epochs never repeat within a store, and the session
/// digest includes the store namespace as well as owner and server identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpStoredSession {
    /// Stable identity for this exact session epoch.
    pub id: Digest,
    /// Monotonic database epoch number.
    pub epoch: u64,
    /// Exact compound server digest.
    pub server: Digest,
}

/// Durable callback work state. Intent without a result is uncertain after loss of
/// the executor; receipt of that state never authorizes redispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpWorkState {
    /// Received but no host work has been dispatched.
    Pending,
    /// Host work may have been dispatched; no result is durably known.
    Intent,
    /// A complete protected reply has been recorded before transmission.
    Result,
    /// Local cancellation committed; outstanding work may still be uncertain.
    Cancelled,
}
impl McpWorkState {
    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "intent" => Ok(Self::Intent),
            "result" => Ok(Self::Result),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(StoreError::Corrupt),
        }
    }
}

/// Protected stored callback facts. Values are intentionally absent from Debug.
#[derive(Clone)]
pub struct McpStoredInteraction {
    /// Stable identity derived from session and typed server request ID.
    pub id: Digest,
    /// Session epoch identity; not a remote handle or restoration permission.
    pub session: Digest,
    /// Canonical request identity for duplicate/conflicting message checks.
    pub request_digest: Digest,
    /// Original admitted wire request bytes.
    pub request: Vec<u8>,
    /// Unioned immutable request/result protection.
    pub labels: SecurityLabels,
    /// Owner generation that admitted this work.
    pub generation: i64,
    /// Optimistic-concurrency revision.
    pub revision: i64,
    /// Current durable work state.
    pub state: McpWorkState,
    /// Absolute callback deadline.
    pub deadline_ms: i64,
    /// Whether a possibly dispatched work reservation remains outstanding.
    pub outstanding: bool,
    /// Exact saved reply, including result/error envelope, before transmission.
    pub reply: Option<Vec<u8>>,
    /// Hash of the saved reply, independent of delivery acknowledgement.
    pub reply_digest: Option<Digest>,
    /// Optional host-supplied usage receipt; absence is unknown usage, not zero.
    pub usage: Option<Value>,
    /// Number of attempts to send this same saved reply.
    pub send_attempts: u64,
    /// Transport accepted a reply; this does not prove application consumption.
    pub transport_accepted: bool,
    /// Explicitly reconciled late evidence for cancelled work. This releases only
    /// concurrency; it cannot publish a reply or refund the spent call.
    pub settlement_digest: Option<Digest>,
}
impl fmt::Debug for McpStoredInteraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpStoredInteraction")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("revision", &self.revision)
            .field("outstanding", &self.outstanding)
            .finish_non_exhaustive()
    }
}

/// A work claim distinguishes first dispatch authority from existing work. Only
/// `Started` permits calling a handler. `Existing(Intent)` requires reconciliation.
#[derive(Debug)]
pub enum McpWorkClaim {
    /// Work intent and its budget reservation were committed by this call.
    Started(McpStoredInteraction),
    /// This interaction already has work/terminal facts; never dispatch it again.
    Existing(McpStoredInteraction),
}

/// One immutable journal fact; consumers resume from its sequence number.
#[derive(Clone, Debug)]
pub struct McpJournalFact {
    /// Exact interaction affected, absent for owner/session lifecycle facts.
    pub interaction_id: Option<Digest>,
    /// Store-monotonic sequence; gaps between an owner's facts are permitted.
    pub sequence: i64,
    /// Event type without protected request/result bytes.
    pub kind: String,
    /// Committing ownership generation.
    pub generation: i64,
    /// Trusted monotonic commit time.
    pub timestamp_ms: i64,
    /// Bounded event data. Applications must preserve the source record protection.
    pub payload: Value,
}

/// Cumulative host client-work accounting, retained across ownership generations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpOwnerUsage {
    /// Calls whose work intent was committed; never refunded on cancellation.
    pub calls_spent: u64,
    /// Work without a durably accepted completion, including uncertain cancellation.
    pub outstanding: u64,
}

/// Protected late reply evidence. Retention neither accepts the result nor releases
/// an uncertain work reservation. The service must authorize its labels before use.
#[derive(Clone)]
pub struct McpLateReply {
    /// Exact reply content identity.
    pub digest: Digest,
    /// Complete bounded reply bytes.
    pub reply: Vec<u8>,
    /// Union of original and late-result protection.
    pub labels: SecurityLabels,
}
impl fmt::Debug for McpLateReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpLateReply")
            .field("digest", &self.digest)
            .field("bytes", &self.reply.len())
            .finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Acquires an unowned/expired host-session namespace. The generation increases;
    /// existing spent calls and unresolved reservations do not reset on takeover.
    ///
    /// # Errors
    /// Returns live ownership, invalid metadata/time, or storage failure.
    pub async fn acquire_mcp_owner(
        &self,
        context: &HostContext,
        name: &str,
        now_ms: i64,
        expires_ms: i64,
    ) -> Result<McpOwner, StoreError> {
        check_lease_times(now_ms, expires_ms)?;
        let prototype = McpOwner::new(
            context.clone(),
            McpOwnerScope::Host(name.into()),
            1,
            &self.config.limits,
        )
        .map_err(|_| StoreError::Invalid)?;
        let (context, name, limits) = (
            context.clone(),
            name.to_string(),
            self.config.limits.clone(),
        );
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let previous: Option<(i64, i64, i64)> = tx.query_row(
                "SELECT generation,expires_ms,last_time_ms FROM mcp_owners WHERE owner_key=?1", [prototype.key().to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
            let now = now_ms.max(previous.map_or(0, |p| p.2));
            if previous.is_some_and(|p| p.1 > now) { return Err(StoreError::Fenced); }
            if expires_ms <= now { return Err(StoreError::Invalid); }
            let generation = previous.map_or(0, |p| p.0).checked_add(1).ok_or(StoreError::Invalid)?;
            let owner = McpOwner::new(context.clone(), McpOwnerScope::Host(name.clone()), generation, &limits).map_err(|_| StoreError::Invalid)?;
            tx.execute("INSERT INTO mcp_owners(owner_key,principal,name,generation,expires_ms,last_time_ms) VALUES(?1,?2,?3,?4,?5,?6)
                ON CONFLICT(owner_key) DO UPDATE SET generation=excluded.generation,expires_ms=excluded.expires_ms,last_time_ms=excluded.last_time_ms",
                params![owner.key().to_string(), context.principal(), name, generation, expires_ms, now])?;
            fact(&tx, &owner, None, "owner_acquired", now, &Value::Integer(expires_ms), &limits)?;
            tx.commit()?;
            Ok(owner)
        }).await.map_err(StoreError::from_worker)
    }

    /// Extends a live host lease without changing generation or shortening expiry.
    ///
    /// # Errors
    /// Returns stale ownership, invalid expiry, or storage failure.
    pub async fn renew_mcp_owner(
        &self,
        owner: &McpOwner,
        now_ms: i64,
        expires_ms: i64,
    ) -> Result<(), StoreError> {
        check_lease_times(now_ms, expires_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                let old: i64 = tx.query_row(
                    "SELECT expires_ms FROM mcp_owners WHERE owner_key=?1",
                    [owner.key().to_string()],
                    |r| r.get(0),
                )?;
                if expires_ms <= now || expires_ms < old {
                    return Err(StoreError::Invalid);
                }
                tx.execute(
                    "UPDATE mcp_owners SET expires_ms=?2,last_time_ms=?3 WHERE owner_key=?1",
                    params![owner.key().to_string(), expires_ms, now],
                )?;
                fact(
                    &tx,
                    &owner,
                    None,
                    "owner_renewed",
                    now,
                    &Value::Integer(expires_ms),
                    &limits,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Allocates a durable host session epoch. This performs no transport I/O.
    ///
    /// # Errors
    /// Returns stale ownership, exhausted storage representation, or database failure.
    pub async fn open_mcp_epoch(
        &self,
        owner: &McpOwner,
        server: Digest,
        now_ms: i64,
    ) -> Result<McpStoredSession, StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                let namespace: String = tx.query_row(
                    "SELECT namespace FROM runtime_identity WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO mcp_epochs(owner_key,generation,server_digest) VALUES(?1,?2,?3)",
                    params![
                        owner.key().to_string(),
                        owner.generation(),
                        server.to_string()
                    ],
                )?;
                let epoch = tx.last_insert_rowid();
                let id = identity(
                    "session",
                    vec![
                        Value::Text(namespace),
                        Value::Text(owner.key().to_string()),
                        Value::Text(server.to_string()),
                        Value::Integer(epoch),
                    ],
                    &limits,
                )?;
                tx.execute(
                    "UPDATE mcp_epochs SET session_id=?2 WHERE epoch=?1",
                    params![epoch, id.to_string()],
                )?;
                fact(
                    &tx,
                    &owner,
                    None,
                    "session_opened",
                    now,
                    &Value::Text(id.to_string()),
                    &limits,
                )?;
                tx.commit()?;
                Ok(McpStoredSession {
                    id,
                    epoch: epoch as u64,
                    server,
                })
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Closes an exact host epoch without erasing interactions or proving their
    /// effects were undone. Old-generation cleanup cannot close a new epoch.
    ///
    /// # Errors
    /// Returns stale ownership/session or storage failure.
    pub async fn close_mcp_epoch(
        &self,
        owner: &McpOwner,
        session: Digest,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                session_authority(&tx, &owner, session, false)?;
                let changed = tx.execute(
                    "UPDATE mcp_epochs SET closed=1 WHERE session_id=?1 AND closed=0",
                    [session.to_string()],
                )?;
                if changed != 0 {
                    fact(
                        &tx,
                        &owner,
                        None,
                        "session_closed",
                        now,
                        &Value::Text(session.to_string()),
                        &limits,
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Persists an admitted server request once, preserving original bytes. Equal
    /// typed request IDs within an epoch reuse the saved record; changed content
    /// conflicts. Retention is bounded by an explicit cumulative owner row ceiling.
    ///
    /// # Errors
    /// Returns stale authority, invalid ingress/deadline, identity conflict or limits.
    #[allow(clippy::too_many_arguments)]
    pub async fn receive_mcp_interaction(
        &self,
        owner: &McpOwner,
        session: Digest,
        request: Vec<u8>,
        labels: SecurityLabels,
        deadline_ms: i64,
        now_ms: i64,
        maximum_records: usize,
    ) -> Result<McpStoredInteraction, StoreError> {
        check_lease_times(now_ms, deadline_ms)?;
        if maximum_records == 0
            || maximum_records > i64::MAX as usize
            || request.len() > self.config.limits.max_document_bytes / 8
        {
            return Err(StoreError::Invalid);
        }
        let document =
            JsonDocument::new(&request, &self.config.limits).map_err(|_| StoreError::Invalid)?;
        let fields = map(document.value())?;
        let request_id =
            McpRequestId::from_value(field(fields, "id")?).map_err(|_| StoreError::Invalid)?;
        if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
            || !matches!(fields.get("method"), Some(Value::Text(_)))
            || fields.get("result").is_some()
            || fields.get("error").is_some()
            || fields
                .get("params")
                .is_some_and(|p| !matches!(p, Value::Map(_)))
        {
            return Err(StoreError::Invalid);
        }
        let id = identity(
            "interaction",
            vec![Value::Text(session.to_string()), request_id.to_value()],
            &self.config.limits,
        )?;
        let request_digest = document.digest();
        let raw_digest = hash_bytes(&request);
        let protection = cbor::encode(
            &labels
                .to_value(&self.config.limits)
                .map_err(|_| StoreError::Invalid)?,
            &self.config.limits,
        )?;
        if protection.len() > self.config.limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            session_authority(&tx, &owner, session, true)?;
            if let Some(old) = interaction(&tx, &owner, id, &limits)? {
                if old.request_digest != request_digest { return Err(StoreError::IdempotencyConflict); }
                tx.commit()?;
                return Ok(old);
            }
            if now >= deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            let count: i64 = tx.query_row("SELECT count(*) FROM mcp_interactions WHERE owner_key=?1", [owner.key().to_string()], |r| r.get(0))?;
            if count >= maximum_records as i64 { return Err(StoreError::Invalid); }
            tx.execute("INSERT INTO mcp_interactions(interaction_id,owner_key,session_id,request_digest,raw_digest,request,protection,generation,revision,state,deadline_ms)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,'pending',?9)", params![id.to_string(), owner.key().to_string(), session.to_string(), request_digest.to_string(), raw_digest.to_string(), request, protection, owner.generation(), deadline_ms])?;
            fact(&tx, &owner, Some(id), "interaction_received", now, &Value::Text(request_digest.to_string()), &limits)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            tx.commit()?; Ok(record)
        }).await.map_err(StoreError::from_worker)
    }

    /// Atomically reserves a host call and concurrency before first dispatch.
    /// Existing intents/results never grant another dispatch, even after restart.
    ///
    /// # Errors
    /// Returns stale authority, expired interaction, exhausted budget or store failure.
    pub async fn begin_mcp_work(
        &self,
        owner: &McpOwner,
        id: Digest,
        now_ms: i64,
        maximum_calls: u64,
        maximum_concurrency: u64,
    ) -> Result<McpWorkClaim, StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            if old.state != McpWorkState::Pending { tx.commit()?; return Ok(McpWorkClaim::Existing(old)); }
            session_authority(&tx, &owner, old.session, true)?;
            if now >= old.deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            let (calls, outstanding): (i64, i64) = tx.query_row("SELECT calls_spent,outstanding FROM mcp_owners WHERE owner_key=?1", [owner.key().to_string()], |r| Ok((r.get(0)?,r.get(1)?)))?;
            if calls as u64 >= maximum_calls || outstanding as u64 >= maximum_concurrency { return Err(StoreError::Budget); }
            let calls = calls.checked_add(1).ok_or(StoreError::Invalid)?;
            let outstanding = outstanding.checked_add(1).ok_or(StoreError::Invalid)?;
            tx.execute("UPDATE mcp_owners SET calls_spent=?2,outstanding=?3 WHERE owner_key=?1", params![owner.key().to_string(), calls, outstanding])?;
            tx.execute("UPDATE mcp_interactions SET state='intent',outstanding=1,revision=revision+1 WHERE interaction_id=?1", [id.to_string()])?;
            fact(&tx, &owner, Some(id), "work_intent", now, &Value::Integer(calls), &limits)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            tx.commit()?; Ok(McpWorkClaim::Started(record))
        }).await.map_err(StoreError::from_worker)
    }

    /// Records an immutable complete wire reply before sending, with a durable
    /// responder receipt. An equal receipt/result returns the saved facts. This
    /// method cannot revive cancelled, expired or stale-generation work.
    ///
    /// # Errors
    /// Returns stale authority, invalid/corrupt result, receipt conflict or expiry.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_mcp_work(
        &self,
        owner: &McpOwner,
        id: Digest,
        responder: &HostContext,
        idempotency_key: &str,
        reply: Vec<u8>,
        labels: SecurityLabels,
        usage: Option<Value>,
        now_ms: i64,
    ) -> Result<McpStoredInteraction, StoreError> {
        timestamp(now_ms)?;
        self.check_text(responder.principal())?;
        self.check_text(idempotency_key)?;
        if idempotency_key.is_empty() || reply.len() > self.config.limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        let usage = usage
            .map(|value| cbor::encode(&value, &self.config.limits))
            .transpose()?;
        if usage
            .as_ref()
            .is_some_and(|value| value.len() > self.config.limits.max_document_bytes / 8)
        {
            return Err(StoreError::Invalid);
        }
        let (owner, responder, idempotency_key, limits) = (
            owner.clone(),
            responder.clone(),
            idempotency_key.to_string(),
            self.config.limits.clone(),
        );
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            validate_reply(&old.request, &reply, &limits)?;
            let digest = hash_bytes(&reply);
            let receipt: Option<(String, String)> = tx.query_row("SELECT interaction_id,reply_digest FROM mcp_receipts WHERE owner_key=?1 AND principal=?2 AND idempotency_key=?3",
                params![owner.key().to_string(), responder.principal(), idempotency_key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some(receipt) = receipt {
                if receipt != (id.to_string(), digest.to_string()) { return Err(StoreError::IdempotencyConflict); }
                if old.reply_digest != Some(digest) { return Err(StoreError::Corrupt); }
                tx.commit()?;
                return Ok(old);
            }
            if old.state == McpWorkState::Result {
                if old.reply_digest != Some(digest) { return Err(StoreError::IdempotencyConflict); }
                tx.execute("INSERT INTO mcp_receipts VALUES(?1,?2,?3,?4,?5)", params![owner.key().to_string(), responder.principal(), idempotency_key, id.to_string(), digest.to_string()])?;
                fact(&tx, &owner, Some(id), "reply_receipt", now, &Value::Text(digest.to_string()), &limits)?;
                tx.commit()?;
                return Ok(old);
            }
            if old.state != McpWorkState::Intent { return Err(StoreError::Terminal); }
            if old.generation != owner.generation() { return Err(StoreError::Fenced); }
            if now >= old.deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            let protection = old.labels.union(&labels, &limits).map_err(|_| StoreError::Invalid)?;
            let protection = cbor::encode(&protection.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            tx.execute("UPDATE mcp_interactions SET state='result',outstanding=0,reply=?2,reply_digest=?3,protection=?4,usage=?5,revision=revision+1 WHERE interaction_id=?1",
                params![id.to_string(), reply, digest.to_string(), protection, usage])?;
            if old.outstanding && tx.execute("UPDATE mcp_owners SET outstanding=outstanding-1 WHERE owner_key=?1 AND outstanding>0", [owner.key().to_string()])? != 1 { return Err(StoreError::Corrupt); }
            tx.execute("INSERT INTO mcp_receipts VALUES(?1,?2,?3,?4,?5)", params![owner.key().to_string(), responder.principal(), idempotency_key, id.to_string(), digest.to_string()])?;
            fact(&tx, &owner, Some(id), "reply_recorded", now, &Value::Text(digest.to_string()), &limits)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            tx.commit()?; Ok(record)
        }).await.map_err(StoreError::from_worker)
    }

    /// Records cancellation without releasing possibly dispatched work reservations.
    /// A saved result remains immutable. No protocol response is sent by storage.
    ///
    /// # Errors
    /// Returns stale authority, missing interaction or storage failure.
    pub async fn cancel_mcp_work(
        &self,
        owner: &McpOwner,
        id: Digest,
        now_ms: i64,
    ) -> Result<McpStoredInteraction, StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            if matches!(old.state, McpWorkState::Result | McpWorkState::Cancelled) { tx.commit()?; return Ok(old); }
            tx.execute("UPDATE mcp_interactions SET state='cancelled',revision=revision+1 WHERE interaction_id=?1", [id.to_string()])?;
            fact(&tx, &owner, Some(id), "work_cancelled", now, &Value::Bool(old.outstanding), &limits)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            tx.commit()?; Ok(record)
        }).await.map_err(StoreError::from_worker)
    }

    /// Records attempted send or transport acceptance of an existing immutable
    /// reply. This does not authorize rerunning its handler or original operation.
    ///
    /// # Errors
    /// Returns stale authority/session, missing result, or storage failure.
    pub async fn record_mcp_delivery(
        &self,
        owner: &McpOwner,
        id: Digest,
        accepted: bool,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            session_authority(&tx, &owner, old.session, true)?;
            if old.state != McpWorkState::Result || (accepted && old.send_attempts == 0) { return Err(StoreError::Invalid); }
            if accepted && old.transport_accepted { tx.commit()?; return Ok(()); }
            let attempts = old.send_attempts.checked_add(u64::from(!accepted)).filter(|v| *v <= i64::MAX as u64).ok_or(StoreError::Invalid)?;
            tx.execute("UPDATE mcp_interactions SET send_attempts=?2,transport_accepted=?3,revision=revision+1 WHERE interaction_id=?1", params![id.to_string(), attempts as i64, accepted || old.transport_accepted])?;
            fact(&tx, &owner, Some(id), if accepted { "reply_transport_accepted" } else { "reply_send_attempted" }, now, &Value::Integer(attempts as i64), &limits)?;
            tx.commit()?; Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    /// Reads protected historical evidence by exact owner and interaction identity.
    /// The calling service must authorize current access to the returned labels.
    /// Ownership expiry does not erase history or authorize new work.
    ///
    /// # Errors
    /// Returns missing/cross-owner identity, corrupt evidence, or storage failure.
    pub async fn mcp_interaction(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<McpStoredInteraction, StoreError> {
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| interaction(db, &owner, id, &limits)?.ok_or(StoreError::Missing))
            .await
            .map_err(StoreError::from_worker)
    }

    /// Reads cumulative host usage without changing authority or reservations.
    ///
    /// # Errors
    /// Returns missing owner, corrupt counters, or storage failure.
    pub async fn mcp_owner_usage(&self, owner: &McpOwner) -> Result<McpOwnerUsage, StoreError> {
        let key = owner.key().to_string();
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                db.query_row(
                    "SELECT calls_spent,outstanding FROM mcp_owners WHERE owner_key=?1",
                    [key],
                    |row| {
                        Ok(McpOwnerUsage {
                            calls_spent: row.get(0)?,
                            outstanding: row.get(1)?,
                        })
                    },
                )
                .optional()?
                .ok_or(StoreError::Missing)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Preserves a late reply even after expiry/fencing. The exact known interaction
    /// and request ID are checked, labels are unioned, and no authoritative result,
    /// receipt, or work accounting changes. Retention is explicitly bounded.
    ///
    /// # Errors
    /// Returns missing identity, malformed reply, exhausted retention or storage failure.
    pub async fn retain_mcp_late_reply(
        &self,
        owner: &McpOwner,
        id: Digest,
        reply: Vec<u8>,
        labels: SecurityLabels,
        maximum_replies: usize,
    ) -> Result<Digest, StoreError> {
        if reply.len() > self.config.limits.max_document_bytes / 8
            || maximum_replies == 0
            || maximum_replies > self.config.max_page_size
        {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            validate_reply(&old.request, &reply, &limits)?;
            let digest = hash_bytes(&reply);
            let existing: Option<Vec<u8>> = tx.query_row("SELECT protection FROM mcp_late_results WHERE interaction_id=?1 AND digest=?2", params![id.to_string(), digest.to_string()], |row| row.get(0)).optional()?;
            let mut protection = old.labels.union(&labels, &limits).map_err(|_| StoreError::Invalid)?;
            if let Some(existing) = existing {
                let previous = SecurityLabels::from_value(&cbor::decode(&existing, &limits)?, &limits).map_err(|_| StoreError::Corrupt)?;
                protection = protection.union(&previous, &limits).map_err(|_| StoreError::Invalid)?;
            } else {
                let count: i64 = tx.query_row("SELECT count(*) FROM mcp_late_results WHERE interaction_id=?1", [id.to_string()], |row| row.get(0))?;
                if count >= maximum_replies as i64 { return Err(StoreError::Invalid); }
            }
            let protection = cbor::encode(&protection.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            tx.execute("INSERT INTO mcp_late_results VALUES(?1,?2,?3,?4) ON CONFLICT(interaction_id,digest) DO UPDATE SET protection=excluded.protection",
                params![id.to_string(), digest.to_string(), reply, protection])?;
            tx.commit()?; Ok(digest)
        }).await.map_err(StoreError::from_worker)
    }

    /// Reads a bounded late-evidence page by digest, without adopting it as a result.
    ///
    /// # Errors
    /// Returns missing owner/interaction, invalid bounds, corruption or storage failure.
    pub async fn mcp_late_replies(
        &self,
        owner: &McpOwner,
        id: Digest,
        after: Option<Digest>,
        page_size: usize,
    ) -> Result<Vec<McpLateReply>, StoreError> {
        let count = self.page_size(page_size)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let old = interaction(db, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            let mut statement = db.prepare("SELECT digest,reply,protection FROM mcp_late_results WHERE interaction_id=?1 AND digest>?2 ORDER BY digest LIMIT ?3")?;
            let mut rows = statement.query(params![id.to_string(), after.map_or_else(String::new, |id| id.to_string()), count])?;
            let mut replies = Vec::new(); let mut retained = 0usize;
            while let Some(row) = rows.next()? {
                let raw = row.get_ref(1)?.as_blob().map_err(|_| StoreError::Corrupt)?;
                let protection = row.get_ref(2)?.as_blob().map_err(|_| StoreError::Corrupt)?;
                if raw.len() > limits.max_document_bytes / 8 || protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Corrupt); }
                retained = retained.checked_add(raw.len() + protection.len() + 128).ok_or(StoreError::Invalid)?;
                if retained > limits.max_document_bytes { break; }
                let digest = row.get::<_, String>(0)?.parse().map_err(|_| StoreError::Corrupt)?;
                if hash_bytes(raw) != digest { return Err(StoreError::Corrupt); }
                validate_reply(&old.request, raw, &limits).map_err(|_| StoreError::Corrupt)?;
                let labels = SecurityLabels::from_value(&cbor::decode(protection, &limits)?, &limits).map_err(|_| StoreError::Corrupt)?;
                replies.push(McpLateReply { digest, reply: raw.to_vec(), labels });
            }
            Ok(replies)
        }).await.map_err(StoreError::from_worker)
    }

    /// Explicitly reconciles cancelled uncertain work against an exact retained
    /// late reply. The caller must authenticate and authorize reconciliation.
    /// Cancellation and spent calls remain immutable; no reply is published.
    ///
    /// # Errors
    /// Returns stale ownership, missing/conflicting evidence, wrong state or storage failure.
    pub async fn settle_mcp_work(
        &self,
        owner: &McpOwner,
        id: Digest,
        late_digest: Digest,
        now_ms: i64,
    ) -> Result<McpStoredInteraction, StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            if let Some(settlement) = old.settlement_digest {
                if settlement != late_digest { return Err(StoreError::IdempotencyConflict); }
                tx.commit()?; return Ok(old);
            }
            if old.state != McpWorkState::Cancelled || !old.outstanding { return Err(StoreError::Invalid); }
            let mut statement = tx.prepare("SELECT reply,protection FROM mcp_late_results WHERE interaction_id=?1 AND digest=?2")?;
            let mut rows = statement.query(params![id.to_string(), late_digest.to_string()])?;
            let row = rows.next()?.ok_or(StoreError::Missing)?;
            let reply = bounded_blob(row, 0, &limits)?.ok_or(StoreError::Corrupt)?;
            if hash_bytes(&reply) != late_digest { return Err(StoreError::Corrupt); }
            validate_reply(&old.request, &reply, &limits).map_err(|_| StoreError::Corrupt)?;
            let protection = bounded_blob(row, 1, &limits)?.ok_or(StoreError::Corrupt)?;
            let late_labels = SecurityLabels::from_value(&cbor::decode(&protection, &limits)?, &limits).map_err(|_| StoreError::Corrupt)?;
            let labels = old.labels.union(&late_labels, &limits).map_err(|_| StoreError::Invalid)?;
            let protection = cbor::encode(&labels.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            drop(rows); drop(statement);
            tx.execute("UPDATE mcp_interactions SET outstanding=0,settlement_digest=?2,protection=?3,revision=revision+1 WHERE interaction_id=?1", params![id.to_string(), late_digest.to_string(), protection])?;
            if tx.execute("UPDATE mcp_owners SET outstanding=outstanding-1 WHERE owner_key=?1 AND outstanding>0", [owner.key().to_string()])? != 1 { return Err(StoreError::Corrupt); }
            fact(&tx, &owner, Some(id), "cancelled_work_settled", now, &Value::Text(late_digest.to_string()), &limits)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            tx.commit()?; Ok(record)
        }).await.map_err(StoreError::from_worker)
    }

    /// Reads a bounded owner journal page. This performs no dispatch or recovery.
    ///
    /// # Errors
    /// Returns invalid paging bounds, corrupt data or storage failure.
    pub async fn mcp_journal(
        &self,
        owner: &McpOwner,
        after: i64,
        page_size: usize,
    ) -> Result<Vec<McpJournalFact>, StoreError> {
        let count = self.page_size(page_size)?;
        if after < 0 {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let mut statement = db.prepare("SELECT sequence,kind,generation,timestamp_ms,payload,interaction_id FROM mcp_events WHERE owner_key=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            let mut rows = statement.query(params![owner.key().to_string(), after, count])?;
            let mut result = Vec::new(); let mut retained = 0usize;
            while let Some(row) = rows.next()? {
                let bytes = row.get_ref(4)?.as_blob().map_err(|_| StoreError::Corrupt)?;
                let kind: String = row.get(1)?;
                retained = retained.checked_add(bytes.len()).and_then(|n| n.checked_add(kind.len() + 128)).ok_or(StoreError::Invalid)?;
                if retained > limits.max_document_bytes { if result.is_empty() { return Err(StoreError::Invalid); } break; }
                result.push(McpJournalFact { interaction_id: row.get::<_, Option<String>>(5)?.map(|id| id.parse().map_err(|_| StoreError::Corrupt)).transpose()?, sequence: row.get(0)?, kind, generation: row.get(2)?, timestamp_ms: row.get(3)?, payload: cbor::decode(bytes, &limits)? });
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
}

fn authority(db: &Db, owner: &McpOwner, observed: i64) -> Result<i64, StoreError> {
    let McpOwnerScope::Host(name) = owner.scope() else {
        return Err(StoreError::Invalid);
    };
    let row: (String, String, i64, i64, i64) = db.query_row("SELECT principal,name,generation,expires_ms,last_time_ms FROM mcp_owners WHERE owner_key=?1", [owner.key().to_string()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(StoreError::Missing)?;
    let now = observed.max(row.4);
    if row.0 != owner.host().principal()
        || row.1 != *name
        || row.2 != owner.generation()
        || now >= row.3
    {
        return Err(StoreError::Fenced);
    }
    db.execute(
        "UPDATE mcp_owners SET last_time_ms=?2 WHERE owner_key=?1",
        params![owner.key().to_string(), now],
    )?;
    Ok(now)
}
fn session_authority(db: &Db, owner: &McpOwner, id: Digest, open: bool) -> Result<(), StoreError> {
    let row: (String, i64, bool) = db
        .query_row(
            "SELECT owner_key,generation,closed FROM mcp_epochs WHERE session_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(StoreError::Missing)?;
    if row.0 != owner.key().to_string() || row.1 != owner.generation() || (open && row.2) {
        return Err(StoreError::Fenced);
    }
    Ok(())
}
fn identity(domain: &str, mut values: Vec<Value>, limits: &Limits) -> Result<Digest, StoreError> {
    values.insert(0, Value::Text(format!("htlk.mcp-{domain}/1")));
    Ok(hash_bytes(&cbor::encode(&Value::Array(values), limits)?))
}
fn map(value: &Value) -> Result<&cbor::Map, StoreError> {
    if let Value::Map(map) = value {
        Ok(map)
    } else {
        Err(StoreError::Invalid)
    }
}
fn field<'a>(map: &'a cbor::Map, name: &str) -> Result<&'a Value, StoreError> {
    map.get(name).ok_or(StoreError::Invalid)
}
fn validate_reply(request: &[u8], reply: &[u8], limits: &Limits) -> Result<(), StoreError> {
    let request = JsonDocument::new(request, limits).map_err(|_| StoreError::Corrupt)?;
    let reply = JsonDocument::new(reply, limits).map_err(|_| StoreError::Invalid)?;
    let fields = map(reply.value())?;
    if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
        || fields.get("id") != map(request.value())?.get("id")
        || fields.get("method").is_some()
    {
        return Err(StoreError::Invalid);
    }
    match (fields.get("result"), fields.get("error")) {
        (Some(Value::Map(_)), None) => Ok(()),
        (None, Some(Value::Map(error)))
            if matches!(error.get("code"), Some(Value::Integer(_)))
                && matches!(error.get("message"), Some(Value::Text(_))) =>
        {
            Ok(())
        }
        _ => Err(StoreError::Invalid),
    }
}
fn interaction(
    db: &Db,
    owner: &McpOwner,
    id: Digest,
    limits: &Limits,
) -> Result<Option<McpStoredInteraction>, StoreError> {
    let mut statement = db.prepare("SELECT session_id,request_digest,raw_digest,request,protection,generation,revision,state,deadline_ms,outstanding,reply,reply_digest,usage,send_attempts,transport_accepted,settlement_digest FROM mcp_interactions WHERE owner_key=?1 AND interaction_id=?2")?;
    let mut rows = statement.query(params![owner.key().to_string(), id.to_string()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let request = row.get_ref(3)?.as_blob().map_err(|_| StoreError::Corrupt)?;
    if request.len() > limits.max_document_bytes / 8 {
        return Err(StoreError::Corrupt);
    }
    let raw_digest: String = row.get(2)?;
    let request_digest: String = row.get(1)?;
    if hash_bytes(request).to_string() != raw_digest
        || JsonDocument::new(request, limits)
            .map_err(|_| StoreError::Corrupt)?
            .digest()
            .to_string()
            != request_digest
    {
        return Err(StoreError::Corrupt);
    }
    let protection = bounded_blob(row, 4, limits)?.ok_or(StoreError::Corrupt)?;
    let protection = cbor::decode(&protection, limits)?;
    let reply = bounded_blob(row, 10, limits)?;
    let reply_digest: Option<String> = row.get(11)?;
    if reply.as_ref().map(|bytes| hash_bytes(bytes).to_string()) != reply_digest {
        return Err(StoreError::Corrupt);
    }
    if let Some(reply) = &reply {
        if reply.len() > limits.max_document_bytes / 8 {
            return Err(StoreError::Corrupt);
        }
        validate_reply(request, reply, limits).map_err(|_| StoreError::Corrupt)?;
    }
    let usage = bounded_blob(row, 12, limits)?;
    let record = McpStoredInteraction {
        id,
        session: row
            .get::<_, String>(0)?
            .parse()
            .map_err(|_| StoreError::Corrupt)?,
        request_digest: request_digest.parse().map_err(|_| StoreError::Corrupt)?,
        request: request.to_vec(),
        labels: SecurityLabels::from_value(&protection, limits).map_err(|_| StoreError::Corrupt)?,
        generation: row.get(5)?,
        revision: row.get(6)?,
        state: McpWorkState::parse(&row.get::<_, String>(7)?)?,
        deadline_ms: row.get(8)?,
        outstanding: row.get(9)?,
        reply,
        reply_digest: reply_digest
            .map(|d| d.parse().map_err(|_| StoreError::Corrupt))
            .transpose()?,
        usage: usage
            .map(|bytes| cbor::decode(&bytes, limits))
            .transpose()?,
        send_attempts: row.get(13)?,
        transport_accepted: row.get(14)?,
        settlement_digest: row
            .get::<_, Option<String>>(15)?
            .map(|id| id.parse().map_err(|_| StoreError::Corrupt))
            .transpose()?,
    };
    let request_doc =
        JsonDocument::new(&record.request, limits).map_err(|_| StoreError::Corrupt)?;
    let request_id = McpRequestId::from_value(field(map(request_doc.value())?, "id")?)
        .map_err(|_| StoreError::Corrupt)?;
    if identity(
        "interaction",
        vec![
            Value::Text(record.session.to_string()),
            request_id.to_value(),
        ],
        limits,
    )? != id
        || (record.state == McpWorkState::Result) != record.reply.is_some()
        || (matches!(record.state, McpWorkState::Pending | McpWorkState::Result)
            && record.outstanding)
        || (record.state == McpWorkState::Intent && !record.outstanding)
        || record.generation <= 0
        || record.revision <= 0
        || record.deadline_ms <= 0
        || (record.transport_accepted && record.send_attempts == 0)
        || (record.settlement_digest.is_some()
            && (record.state != McpWorkState::Cancelled || record.outstanding))
    {
        return Err(StoreError::Corrupt);
    }
    Ok(Some(record))
}
fn bounded_blob(
    row: &tokio_rusqlite::rusqlite::Row<'_>,
    index: usize,
    limits: &Limits,
) -> Result<Option<Vec<u8>>, StoreError> {
    match row.get_ref(index)? {
        tokio_rusqlite::types::ValueRef::Null => Ok(None),
        tokio_rusqlite::types::ValueRef::Blob(bytes)
            if bytes.len() <= limits.max_document_bytes / 8 =>
        {
            Ok(Some(bytes.to_vec()))
        }
        _ => Err(StoreError::Corrupt),
    }
}
fn fact(
    db: &Db,
    owner: &McpOwner,
    interaction: Option<Digest>,
    kind: &str,
    now: i64,
    payload: &Value,
    limits: &Limits,
) -> Result<(), StoreError> {
    let payload = cbor::encode(payload, limits)?;
    db.execute("INSERT INTO mcp_events(owner_key,interaction_id,kind,generation,timestamp_ms,payload) VALUES(?1,?2,?3,?4,?5,?6)",
        params![owner.key().to_string(), interaction.map(|id| id.to_string()), kind, owner.generation(), now, payload])?;
    Ok(())
}
