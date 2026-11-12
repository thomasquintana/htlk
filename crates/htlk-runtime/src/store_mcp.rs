//! Transactional host-session ownership and client-work facts. As with the other
//! store APIs, these are trusted infrastructure, not authenticated frontend APIs.
use super::*;
use crate::store_db::Connection as Db;
use crate::{
    HostContext,
    mcp::client::{McpOperationOrigin, McpOwner, McpOwnerScope, McpRequestId, McpWorkDelivery},
    records::SecurityLabels,
};
use htlk_executable::{JsonDocument, digest::hash_bytes};
#[path = "store_mcp_run.rs"]
mod run;
#[path = "store_mcp_tasks.rs"]
mod tasks;
pub use tasks::*;
#[path = "store_mcp_remote_tasks.rs"]
mod remote_tasks;
pub use remote_tasks::*;
#[path = "store_mcp_subscriptions.rs"]
mod subscriptions;
#[path = "store_mcp_url.rs"]
mod url_interactions;
pub use subscriptions::{
    McpStoredSubscription, McpSubscriptionAction, McpSubscriptionClaim, McpSubscriptionIntent,
    McpSubscriptionLimits, McpSubscriptionOperation, McpSubscriptionOutcome, McpSubscriptionState,
};

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
    /// Generation that opened this epoch.
    pub generation: i64,
    /// Explicitly closed transport epoch; reopening uses a different identity.
    pub closed: bool,
}

/// Durable callback protocol state. Underlying work delivery is separate: even a
/// recorded error reply can leave model work uncertain. Neither permits redispatch.
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
    /// Trusted native child-RPC parent, never inferred from server metadata.
    pub parent_interaction: Option<Digest>,
    /// Persisted interaction ancestry depth; top-level callbacks have depth one.
    pub depth: usize,
    /// Independently checked originating run operation, absent for host/root services.
    pub origin: Option<McpOperationOrigin>,
    /// Stable identity derived from session and typed server request ID.
    pub id: Digest,
    /// Session epoch identity; not a remote handle or restoration permission.
    pub session: Digest,
    /// Canonical request identity for duplicate/conflicting message checks.
    pub request_digest: Digest,
    /// Exact original request-byte digest, independent of canonical JSON identity.
    pub raw_digest: Digest,
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
    /// Underlying work completion, independent of callback reply transmission.
    pub work_delivery: McpWorkDelivery,
    /// Absolute callback deadline.
    pub deadline_ms: i64,
    /// Whether a possibly dispatched work reservation remains outstanding.
    pub outstanding: bool,
    /// Whether this interaction committed a spent call before host dispatch.
    pub call_charged: bool,
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
    /// Explicitly reconciled late evidence for uncertain work. This releases only
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
    /// Evidence identity binding bytes, work delivery, and usage observation.
    pub digest: Digest,
    /// Exact original reply-byte digest.
    pub raw_digest: Digest,
    /// Underlying work evidence; uncertainty cannot settle a reservation.
    pub work_delivery: McpWorkDelivery,
    /// Usage observed with this late result, independently of the original reply.
    pub usage: Option<Value>,
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
    pub(crate) async fn run_client_metadata(
        &self,
        run: &RunId,
        invocation: Option<crate::identity::InvocationId>,
        attempt: Option<crate::identity::AttemptId>,
    ) -> Result<(Vec<Digest>, SecurityLabels), StoreError> {
        let (run, limits) = (run.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| run_metadata(db, &run, invocation, attempt, &limits))
            .await
            .map_err(StoreError::from_worker)
    }
    pub(crate) async fn validate_run_mcp_interactions(
        &self,
        run_id: &RunId,
    ) -> Result<(), StoreError> {
        let (run_id, limits, page_size) = (
            run_id.clone(),
            self.config.limits.clone(),
            self.config.max_page_size,
        );
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction()?;
            let header = load_header(&tx, &run_id)?;
            let scope: Vec<u8> = tx.query_row("SELECT payload FROM records WHERE run_id=?1 AND family='scope' AND record_id=?2", params![run_id.as_str(), header.root_scope.to_string()], |row| row.get(0))?;
            let scope = cbor::decode(&scope, &limits)?;
            let Value::Text(principal) = field(map(&scope)?, "principal")? else { return Err(StoreError::Corrupt) };
            let owner = McpOwner::new(HostContext::new(principal.clone(), &limits).map_err(|_| StoreError::Corrupt)?, McpOwnerScope::Run(run_id.clone()), header.fence, &limits).map_err(|_| StoreError::Corrupt)?;
            // Codec limits apply to each retained document, not lifetime history.
            // Validate one record at a time in bounded identity pages, under one
            // snapshot. No request/reply history or identity set accumulates here.
            let mut after = String::new();
            loop {
                let mut statement = tx.prepare("SELECT interaction_id FROM mcp_interactions WHERE owner_key=?1 AND interaction_id>?2 ORDER BY interaction_id LIMIT ?3")?;
                let mut rows = statement.query(params![owner.key().to_string(), after, page_size as i64])?;
                let mut count = 0;
                while let Some(row) = rows.next()? {
                    after = row.get(0)?;
                    let id = after.parse().map_err(|_| StoreError::Corrupt)?;
                    let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Corrupt)?;
                    run::validate(&tx, &owner, &record, &limits)?;
                    count += 1;
                }
                if count < page_size { break; }
            }
            let orphan: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM records r WHERE r.run_id=?1 AND r.family='reservation' AND r.record_id LIKE 'client:%' AND NOT EXISTS(SELECT 1 FROM mcp_interactions i WHERE i.owner_key=?2 AND r.record_id='client:'||i.interaction_id))", params![run_id.as_str(), owner.key().to_string()], |row| row.get(0))?;
            if orphan { return Err(StoreError::Corrupt); }
            tx.commit()?;
            Ok(())
        }).await.map_err(StoreError::from_worker)
    }
    /// Enumerates a bounded page of interaction identities without loading values.
    /// The host service independently authorizes each selected record.
    ///
    /// # Errors
    /// Returns invalid paging bounds, corruption, or storage failure.
    pub async fn mcp_interaction_ids(
        &self,
        owner: &McpOwner,
        after: Option<Digest>,
        page_size: usize,
    ) -> Result<Vec<Digest>, StoreError> {
        let count = self.page_size(page_size)?;
        let key = owner.key().to_string();
        self.connection.call(move |db| -> Result<_, StoreError> {
            let mut statement = db.prepare("SELECT interaction_id FROM mcp_interactions WHERE owner_key=?1 AND interaction_id>?2 ORDER BY interaction_id LIMIT ?3")?;
            let mut rows = statement.query(params![key, after.map_or_else(String::new, |id| id.to_string()), count])?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next()? { ids.push(row.get::<_, String>(0)?.parse().map_err(|_| StoreError::Corrupt)?); }
            Ok(ids)
        }).await.map_err(StoreError::from_worker)
    }
    /// Computes the bounded identity of a server request without reading values or
    /// granting authority. The receive transaction independently uses this domain.
    ///
    /// # Errors
    /// Returns representation limits.
    pub fn mcp_interaction_id(
        &self,
        session: Digest,
        request: &McpRequestId,
    ) -> Result<Digest, StoreError> {
        identity(
            "interaction",
            vec![Value::Text(session.to_string()), request.to_value()],
            &self.config.limits,
        )
    }

    /// Reads only callback control state, without loading request/result values.
    ///
    /// # Errors
    /// Returns corrupt state or storage failure; an absent interaction returns None.
    pub async fn mcp_interaction_state(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<Option<McpWorkState>, StoreError> {
        let owner = owner.clone();
        self.connection.call(move |db| -> Result<_, StoreError> {
            let value: Option<String> = db.query_row("SELECT state FROM mcp_interactions WHERE owner_key=?1 AND interaction_id=?2", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?;
            value.map(|value| McpWorkState::parse(&value)).transpose()
        }).await.map_err(StoreError::from_worker)
    }
    /// Reads historical session identity under the exact owner namespace. This
    /// does not assert that a transport or its ownership lease remains live.
    ///
    /// # Errors
    /// Returns missing/cross-owner session, corruption, or storage failure.
    pub async fn mcp_session(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<McpStoredSession, StoreError> {
        let owner = owner.clone();
        self.connection.call(move |db| -> Result<_, StoreError> {
            let (epoch, server, generation, closed): (u64, String, i64, bool) = db.query_row("SELECT epoch,server_digest,generation,closed FROM mcp_epochs WHERE owner_key=?1 AND session_id=?2", params![owner.key().to_string(), id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?.ok_or(StoreError::Missing)?;
            if epoch == 0 { return Err(StoreError::Corrupt); }
            Ok(McpStoredSession { id, epoch, server: server.parse().map_err(|_| StoreError::Corrupt)?, generation, closed })
        }).await.map_err(StoreError::from_worker)
    }
    /// Checks current host/run ownership independently of local transport state.
    /// Run owners are checked against the actual run fence and lifetime.
    ///
    /// # Errors
    /// Returns missing/stale authority or storage failure.
    pub async fn validate_mcp_owner(
        &self,
        owner: &McpOwner,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let owner = owner.clone();
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                authority(&tx, &owner, now_ms)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Releases an explicitly owned host namespace. Outstanding work and spent
    /// calls remain retained; a later acquisition receives a higher generation.
    ///
    /// # Errors
    /// Returns stale or run-owned authority, or storage failure.
    pub async fn release_mcp_owner(&self, owner: &McpOwner, now_ms: i64) -> Result<(), StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                tx.execute(
                    "UPDATE mcp_owners SET expires_ms=?2 WHERE owner_key=?1",
                    params![owner.key().to_string(), now],
                )?;
                fact(
                    &tx,
                    &owner,
                    None,
                    "owner_released",
                    now,
                    &Value::Null,
                    &limits,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(StoreError::from_worker)
    }
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
        host_owner(owner)?;
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
                register_run_owner(&tx, &owner, now_ms, &limits)?;
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
                    generation: owner.generation(),
                    closed: false,
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
        host_owner(owner)?;
        self.receive_mcp_interaction_inner(
            owner,
            session,
            None,
            None,
            request,
            labels,
            deadline_ms,
            now_ms,
            maximum_records,
        )
        .await
    }

    /// Admits a run-owned callback against its actual outstanding operation and
    /// ancestor protection/deadlines. Only roots inquiries may omit an operation.
    ///
    /// # Errors
    /// Returns invalid attribution, stale authority, deadline or representation limits.
    #[allow(clippy::too_many_arguments)]
    pub async fn receive_run_mcp_interaction(
        &self,
        owner: &McpOwner,
        session: Digest,
        origin: Option<McpOperationOrigin>,
        request: Vec<u8>,
        labels: SecurityLabels,
        deadline_ms: i64,
        now_ms: i64,
        maximum_records: usize,
    ) -> Result<McpStoredInteraction, StoreError> {
        if !matches!(owner.scope(), McpOwnerScope::Run(_)) {
            return Err(StoreError::Invalid);
        }
        self.receive_mcp_interaction_inner(
            owner,
            session,
            origin,
            None,
            request,
            labels,
            deadline_ms,
            now_ms,
            maximum_records,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn receive_mcp_child_interaction(
        &self,
        owner: &McpOwner,
        session: Digest,
        origin: Option<McpOperationOrigin>,
        parent: Option<Digest>,
        request: Vec<u8>,
        labels: SecurityLabels,
        deadline_ms: i64,
        now_ms: i64,
        maximum_records: usize,
    ) -> Result<McpStoredInteraction, StoreError> {
        self.receive_mcp_interaction_inner(
            owner,
            session,
            origin,
            parent,
            request,
            labels,
            deadline_ms,
            now_ms,
            maximum_records,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn receive_mcp_interaction_inner(
        &self,
        owner: &McpOwner,
        session: Digest,
        origin: Option<McpOperationOrigin>,
        parent: Option<Digest>,
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
        let Value::Text(method) = field(fields, "method")? else {
            return Err(StoreError::Invalid);
        };
        let method = method.clone();
        let raw_digest = hash_bytes(&request);
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            session_authority(&tx, &owner, session, true)?;
            if let Some(old) = interaction(&tx, &owner, id, &limits)? {
                if old.request_digest != request_digest || old.origin != origin || old.parent_interaction != parent { return Err(StoreError::IdempotencyConflict); }
                tx.commit()?;
                return Ok(old);
            }
            let (mut labels, mut deadline_ms) = run::admit(&tx, &owner, session, origin.as_ref(), &method, labels, deadline_ms, now, &limits)?;
            let depth = if let Some(parent) = parent {
                if parent == id { return Err(StoreError::Invalid); }
                let previous = interaction(&tx, &owner, parent, &limits)?.ok_or(StoreError::Missing)?;
                let claimed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='host_operation_claim')", params![owner.key().to_string(), parent.to_string()], |row| row.get(0))?;
                if !claimed || previous.session != session || previous.origin != origin || previous.state != McpWorkState::Intent || !previous.outstanding || previous.generation != owner.generation() { return Err(StoreError::Fenced); }
                deadline_ms = deadline_ms.min(previous.deadline_ms);
                labels = labels.union(&previous.labels, &limits).map_err(|_| StoreError::Invalid)?;
                previous.depth.checked_add(1).filter(|depth| *depth <= 1024).ok_or(StoreError::Invalid)?
            } else { 1 };
            let protection = cbor::encode(&labels.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            if now >= deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            let count: i64 = tx.query_row("SELECT count(*) FROM mcp_interactions WHERE owner_key=?1", [owner.key().to_string()], |r| r.get(0))?;
            if count >= maximum_records as i64 { return Err(StoreError::Invalid); }
            tx.execute("INSERT INTO mcp_interactions(interaction_id,owner_key,session_id,request_digest,raw_digest,request,protection,generation,revision,state,deadline_ms,origin_invocation,origin_attempt,origin_run)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,'pending',?9,?10,?11,?12)", params![id.to_string(), owner.key().to_string(), session.to_string(), request_digest.to_string(), raw_digest.to_string(), request, protection, owner.generation(), deadline_ms,
                origin.as_ref().map(|origin| origin.invocation_id.to_string()), origin.as_ref().map(|origin| origin.attempt_id.to_string()), origin.as_ref().map(|origin| origin.run_id.as_str())])?;
            fact(&tx, &owner, Some(id), "interaction_received", now, &Value::Text(request_digest.to_string()), &limits)?;
            if let Some(parent) = parent {
                fact(&tx, &owner, Some(id), "interaction_parent", now, &Value::Array(vec![Value::Text(parent.to_string()), Value::Integer(depth as i64)]), &limits)?;
            }
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
            let totals = checked_usage(&tx, &owner)?;
            let (calls, outstanding) = (totals.calls_spent as i64, totals.outstanding as i64);
            if calls as u64 >= maximum_calls || outstanding as u64 >= maximum_concurrency { return Err(StoreError::Budget); }
            run::reserve(&tx, &owner, &old, now, &limits)?;
            let calls = calls.checked_add(1).ok_or(StoreError::Invalid)?;
            let outstanding = outstanding.checked_add(1).ok_or(StoreError::Invalid)?;
            tx.execute("UPDATE mcp_owners SET calls_spent=?2,outstanding=?3 WHERE owner_key=?1", params![owner.key().to_string(), calls, outstanding])?;
            tx.execute("UPDATE mcp_interactions SET state='intent',work_delivery='uncertain',outstanding=1,call_charged=1,revision=revision+1 WHERE interaction_id=?1", [id.to_string()])?;
            fact(&tx, &owner, Some(id), "work_intent", now, &Value::Integer(calls), &limits)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            tx.commit()?; Ok(McpWorkClaim::Started(record))
        }).await.map_err(StoreError::from_worker)
    }

    /// Claims the single actual host operation covered by an interaction's already
    /// committed reservation. The protected descriptor records host routing/input
    /// evidence, never server authority. A second claim cannot dispatch again.
    ///
    /// # Errors
    /// Returns stale ownership/session, expiry, cancellation, an existing claim,
    /// missing accounting or invalid bounded evidence.
    pub async fn claim_mcp_host_operation(
        &self,
        owner: &McpOwner,
        id: Digest,
        labels: SecurityLabels,
        descriptor: JsonDocument,
        now_ms: i64,
    ) -> Result<Digest, StoreError> {
        timestamp(now_ms)?;
        if descriptor.as_bytes().len() > self.config.limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            session_authority(&tx, &owner, old.session, true)?;
            if old.generation != owner.generation() { return Err(StoreError::Fenced); }
            if old.state != McpWorkState::Intent || !old.call_charged || !old.outstanding { return Err(StoreError::Terminal); }
            if now >= old.deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            checked_usage(&tx, &owner)?;
            let request = JsonDocument::new(&old.request, &limits).map_err(|_| StoreError::Corrupt)?;
            let Value::Text(method) = field(map(request.value())?, "method")? else { return Err(StoreError::Corrupt); };
            run::admit(&tx, &owner, old.session, old.origin.as_ref(), method, old.labels.clone(), old.deadline_ms, now, &limits)?;
            let claimed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='host_operation_claim')", params![owner.key().to_string(), id.to_string()], |row| row.get(0))?;
            if claimed { return Err(StoreError::Conflict); }
            let protection = old.labels.union(&labels, &limits).map_err(|_| StoreError::Invalid)?;
            let protection = cbor::encode(&protection.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            tx.execute("UPDATE mcp_interactions SET protection=?2,revision=revision+1 WHERE interaction_id=?1", params![id.to_string(), protection])?;
            let receipt = JsonDocument::from_value(&Value::Array(vec![Value::Text("htlk.mcp-host-work/1".into()), Value::Text(id.to_string()), Value::Text(old.request_digest.to_string())]), &limits).map_err(|_| StoreError::Invalid)?.digest();
            fact(&tx, &owner, Some(id), "host_operation_claim", now, &Value::Array(vec![Value::Text(receipt.to_string()), descriptor.value().clone()]), &limits)?;
            tx.commit()?;
            Ok(receipt)
        }).await.map_err(StoreError::from_worker)
    }

    pub(crate) async fn claim_mcp_native_operation(
        &self,
        owner: &McpOwner,
        id: Digest,
        claim: Digest,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            session_authority(&tx, &owner, old.session, true)?;
            if old.generation != owner.generation() || old.state != McpWorkState::Intent || !old.outstanding { return Err(StoreError::Fenced); }
            if now >= old.deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            let payload: Vec<u8> = tx.query_row("SELECT payload FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='host_operation_claim'", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?.ok_or(StoreError::Missing)?;
            let Value::Array(values) = cbor::decode(&payload, &limits)? else { return Err(StoreError::Corrupt); };
            if values.first() != Some(&Value::Text(claim.to_string())) { return Err(StoreError::Fenced); }
            let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='native_operation_intent')", params![owner.key().to_string(), id.to_string()], |row| row.get(0))?;
            if existing { return Err(StoreError::Conflict); }
            let request = JsonDocument::new(&old.request, &limits).map_err(|_| StoreError::Corrupt)?;
            let Value::Text(method) = field(map(request.value())?, "method")? else { return Err(StoreError::Corrupt); };
            run::admit(&tx, &owner, old.session, old.origin.as_ref(), method, old.labels.clone(), old.deadline_ms, now, &limits)?;
            fact(&tx, &owner, Some(id), "native_operation_intent", now, &Value::Text(claim.to_string()), &limits)?;
            tx.commit()?; Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    pub(crate) async fn retain_mcp_native_outcome(
        &self,
        owner: &McpOwner,
        id: Digest,
        payload: Value,
        labels: SecurityLabels,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        if cbor::encode(&payload, &limits)?.len() > limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            let claimed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='native_operation_intent')", params![owner.key().to_string(), id.to_string()], |row| row.get(0))?;
            if !claimed { return Err(StoreError::Missing); }
            let protection = record.labels.union(&labels, &limits).map_err(|_| StoreError::Invalid)?;
            let protection = cbor::encode(&protection.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            tx.execute("UPDATE mcp_interactions SET protection=?2,revision=revision+1 WHERE interaction_id=?1", params![id.to_string(), protection])?;
            let old: Option<Vec<u8>> = tx.query_row("SELECT payload FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='native_operation_outcome'", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?;
            if let Some(old) = old {
                if cbor::decode(&old, &limits)? != payload { return Err(StoreError::Conflict); }
            } else { fact(&tx, &owner, Some(id), "native_operation_outcome", now_ms, &payload, &limits)?; }
            tx.commit()?; Ok(())
        }).await.map_err(StoreError::from_worker)
    }

    pub(crate) async fn mcp_host_work_evidence(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<
        (
            McpStoredInteraction,
            Option<crate::mcp::client::McpHostWorkEvidence>,
        ),
        StoreError,
    > {
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction()?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            let claim: Option<Vec<u8>> = tx.query_row("SELECT payload FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='host_operation_claim'", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?;
            let Some(claim) = claim else { return Ok((record, None)); };
            let Value::Array(claim) = cbor::decode(&claim, &limits)? else { return Err(StoreError::Corrupt); };
            let [Value::Text(id_text), descriptor] = claim.as_slice() else { return Err(StoreError::Corrupt); };
            let outcome: Option<Vec<u8>> = tx.query_row("SELECT payload FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='native_operation_outcome'", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?;
            let evidence = crate::mcp::client::McpHostWorkEvidence { claim_id: id_text.parse().map_err(|_| StoreError::Corrupt)?, descriptor: descriptor.clone(), native_outcome: outcome.map(|bytes| cbor::decode(&bytes, &limits)).transpose()? };
            tx.commit()?;
            Ok((record, Some(evidence)))
        }).await.map_err(StoreError::from_worker)
    }

    pub(crate) async fn mcp_work_outcome(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<(McpStoredInteraction, Option<McpLateReply>), StoreError> {
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction()?;
            let record = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            let evidence = if let Some(digest) = record.settlement_digest {
                let mut statement = tx.prepare("SELECT digest,reply,protection,raw_digest,work_delivery,usage FROM mcp_late_results WHERE interaction_id=?1 AND digest=?2")?;
                let mut rows = statement.query(params![id.to_string(), digest.to_string()])?;
                Some(late_reply(rows.next()?.ok_or(StoreError::Corrupt)?, id, &record.request, &limits)?)
            } else { None };
            tx.commit()?;
            Ok((record, evidence))
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
        work_delivery: McpWorkDelivery,
        now_ms: i64,
    ) -> Result<McpStoredInteraction, StoreError> {
        self.complete_mcp_work_inner(
            owner,
            id,
            responder,
            idempotency_key,
            reply,
            labels,
            usage,
            work_delivery,
            now_ms,
            false,
        )
        .await
    }

    /// Accepts a human reply only while the original session remains live. Exact
    /// historical receipts remain retrievable without authorizing a new delivery.
    ///
    /// # Errors
    /// Returns stale session/ownership, expiry, conflicting key or malformed reply.
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_mcp_reply(
        &self,
        owner: &McpOwner,
        id: Digest,
        responder: &HostContext,
        idempotency_key: &str,
        reply: Vec<u8>,
        labels: SecurityLabels,
        now_ms: i64,
    ) -> Result<McpStoredInteraction, StoreError> {
        self.complete_mcp_work_inner(
            owner,
            id,
            responder,
            idempotency_key,
            reply,
            labels,
            None,
            McpWorkDelivery::Completed,
            now_ms,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn complete_mcp_work_inner(
        &self,
        owner: &McpOwner,
        id: Digest,
        responder: &HostContext,
        idempotency_key: &str,
        reply: Vec<u8>,
        labels: SecurityLabels,
        usage: Option<Value>,
        work_delivery: McpWorkDelivery,
        now_ms: i64,
        live_reply: bool,
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
            validate_work_reply(&reply, work_delivery, &limits)?;
            checked_usage(&tx, &owner)?;
            let digest = hash_bytes(&reply);
            let receipt: Option<(String, String, String)> = tx.query_row("SELECT interaction_id,reply_digest,work_delivery FROM mcp_receipts WHERE owner_key=?1 AND principal=?2 AND idempotency_key=?3",
                params![owner.key().to_string(), responder.principal(), idempotency_key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some(receipt) = receipt {
                if receipt != (id.to_string(), digest.to_string(), work_delivery.as_str().into()) { return Err(StoreError::IdempotencyConflict); }
                if old.reply_digest != Some(digest) { return Err(StoreError::Corrupt); }
                tx.commit()?;
                return Ok(old);
            }
            if old.state == McpWorkState::Result {
                let initial: String = tx.query_row("SELECT work_delivery FROM mcp_receipts WHERE owner_key=?1 AND interaction_id=?2 LIMIT 1", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?.ok_or(StoreError::Corrupt)?;
                if old.reply_digest != Some(digest) || initial != work_delivery.as_str() { return Err(StoreError::IdempotencyConflict); }
                tx.execute("INSERT INTO mcp_receipts VALUES(?1,?2,?3,?4,?5,?6)", params![owner.key().to_string(), responder.principal(), idempotency_key, id.to_string(), digest.to_string(), work_delivery.as_str()])?;
                fact(&tx, &owner, Some(id), "reply_receipt", now, &Value::Text(digest.to_string()), &limits)?;
                tx.commit()?;
                return Ok(old);
            }
            if live_reply { session_authority(&tx, &owner, old.session, true)?; }
            if old.state != McpWorkState::Intent && !(old.state == McpWorkState::Pending && work_delivery == McpWorkDelivery::NotStarted) { return Err(StoreError::Terminal); }
            if old.generation != owner.generation() { return Err(StoreError::Fenced); }
            if now >= old.deadline_ms { return Err(StoreError::Deadline(FailureCode::Timeout)); }
            let protection = old.labels.union(&labels, &limits).map_err(|_| StoreError::Invalid)?;
            let protection = cbor::encode(&protection.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            let outstanding = work_delivery == McpWorkDelivery::Uncertain;
            tx.execute("UPDATE mcp_interactions SET state='result',outstanding=?6,work_delivery=?7,reply=?2,reply_digest=?3,protection=?4,usage=?5,revision=revision+1 WHERE interaction_id=?1",
                params![id.to_string(), reply, digest.to_string(), protection, usage, outstanding, work_delivery.as_str()])?;
            if old.outstanding && !outstanding {
                run::release(&tx, &owner, &old, now, &limits)?;
                if tx.execute("UPDATE mcp_owners SET outstanding=outstanding-1 WHERE owner_key=?1 AND outstanding>0", [owner.key().to_string()])? != 1 { return Err(StoreError::Corrupt); }
            }
            tx.execute("INSERT INTO mcp_receipts VALUES(?1,?2,?3,?4,?5,?6)", params![owner.key().to_string(), responder.principal(), idempotency_key, id.to_string(), digest.to_string(), work_delivery.as_str()])?;
            fact(&tx, &owner, Some(id), "reply_recorded", now, &Value::Array(vec![Value::Text(digest.to_string()), Value::Text(work_delivery.as_str().into())]), &limits)?;
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

    /// Saves a protocol error for an already cancelled interaction without changing
    /// underlying work delivery, charges, or reservations. Deadlines do not prevent
    /// recording this terminal refusal; the owner and session must still be live.
    ///
    /// # Errors
    /// Returns stale authority, wrong state, conflicting reply or malformed error.
    pub async fn record_mcp_cancelled_reply(
        &self,
        owner: &McpOwner,
        id: Digest,
        reply: Vec<u8>,
        now_ms: i64,
    ) -> Result<McpStoredInteraction, StoreError> {
        timestamp(now_ms)?;
        if reply.len() > self.config.limits.max_document_bytes / 8 {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            if old.state != McpWorkState::Cancelled { return Err(StoreError::Terminal); }
            session_authority(&tx, &owner, old.session, true)?;
            validate_reply(&old.request, &reply, &limits)?;
            validate_work_reply(&reply, McpWorkDelivery::Uncertain, &limits)?;
            let digest = hash_bytes(&reply);
            if let Some(previous) = old.reply_digest {
                if previous != digest { return Err(StoreError::IdempotencyConflict); }
                tx.commit()?; return Ok(old);
            }
            tx.execute("UPDATE mcp_interactions SET reply=?2,reply_digest=?3,revision=revision+1 WHERE interaction_id=?1", params![id.to_string(), reply, digest.to_string()])?;
            fact(&tx, &owner, Some(id), "cancelled_reply_recorded", now, &Value::Text(digest.to_string()), &limits)?;
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
            if old.reply.is_none() || (accepted && old.send_attempts == 0) { return Err(StoreError::Invalid); }
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
        host_owner(owner)?;
        let owner = owner.clone();
        self.connection
            .call(move |db| -> Result<_, StoreError> { checked_usage(db, &owner) })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Preserves a late reply even after expiry/fencing. The exact known interaction
    /// and request ID are checked, labels are unioned, and no authoritative result,
    /// receipt, or work accounting changes. Retention is explicitly bounded.
    ///
    /// # Errors
    /// Returns missing identity, malformed reply, exhausted retention or storage failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn retain_mcp_late_reply(
        &self,
        owner: &McpOwner,
        id: Digest,
        reply: Vec<u8>,
        labels: SecurityLabels,
        work_delivery: McpWorkDelivery,
        usage: Option<Value>,
        maximum_replies: usize,
    ) -> Result<Digest, StoreError> {
        if reply.len() > self.config.limits.max_document_bytes / 8
            || maximum_replies == 0
            || maximum_replies > self.config.max_page_size
        {
            return Err(StoreError::Invalid);
        }
        let usage = usage
            .map(|value| cbor::encode(&value, &self.config.limits))
            .transpose()?;
        if usage
            .as_ref()
            .is_some_and(|bytes| bytes.len() > self.config.limits.max_document_bytes / 8)
        {
            return Err(StoreError::Invalid);
        }
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old = interaction(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
            validate_reply(&old.request, &reply, &limits)?;
            let raw_digest = hash_bytes(&reply);
            let digest = late_identity(id, raw_digest, work_delivery, usage.as_deref(), &limits)?;
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
            tx.execute("INSERT INTO mcp_late_results VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(interaction_id,digest) DO UPDATE SET protection=excluded.protection",
                params![id.to_string(), digest.to_string(), reply, protection, raw_digest.to_string(), work_delivery.as_str(), usage])?;
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
            let mut statement = db.prepare("SELECT digest,reply,protection,raw_digest,work_delivery,usage FROM mcp_late_results WHERE interaction_id=?1 AND digest>?2 ORDER BY digest LIMIT ?3")?;
            let mut rows = statement.query(params![id.to_string(), after.map_or_else(String::new, |id| id.to_string()), count])?;
            let mut replies = Vec::new(); let mut retained = 0usize;
            while let Some(row) = rows.next()? {
                let result = late_reply(row, id, &old.request, &limits)?;
                let size = result.reply.len() + cbor::encode(&result.labels.to_value(&limits).map_err(|_| StoreError::Corrupt)?, &limits)?.len()
                    + result.usage.as_ref().map(|value| cbor::encode(value, &limits)).transpose()?.map_or(0, |bytes| bytes.len()) + 512;
                retained = retained.checked_add(size).ok_or(StoreError::Invalid)?;
                if retained > limits.max_document_bytes { break; }
                replies.push(result);
            }
            Ok(replies)
        }).await.map_err(StoreError::from_worker)
    }

    /// Explicitly reconciles uncertain work against a retained known-completion or
    /// known-not-started receipt. Cancellation, saved RPC replies, and spent calls
    /// remain immutable. The caller must authenticate and authorize reconciliation.
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
            if !matches!(old.state, McpWorkState::Cancelled | McpWorkState::Result) || !old.outstanding { return Err(StoreError::Invalid); }
            checked_usage(&tx, &owner)?;
            let mut statement = tx.prepare("SELECT digest,reply,protection,raw_digest,work_delivery,usage FROM mcp_late_results WHERE interaction_id=?1 AND digest=?2")?;
            let mut rows = statement.query(params![id.to_string(), late_digest.to_string()])?;
            let row = rows.next()?.ok_or(StoreError::Missing)?;
            let evidence = late_reply(row, id, &old.request, &limits)?;
            if evidence.work_delivery == McpWorkDelivery::Uncertain { return Err(StoreError::Invalid); }
            let labels = old.labels.union(&evidence.labels, &limits).map_err(|_| StoreError::Invalid)?;
            let protection = cbor::encode(&labels.to_value(&limits).map_err(|_| StoreError::Invalid)?, &limits)?;
            if protection.len() > limits.max_document_bytes / 8 { return Err(StoreError::Invalid); }
            drop(rows); drop(statement);
            run::release(&tx, &owner, &old, now, &limits)?;
            tx.execute("UPDATE mcp_interactions SET outstanding=0,settlement_digest=?2,protection=?3,work_delivery=?4,revision=revision+1 WHERE interaction_id=?1", params![id.to_string(), late_digest.to_string(), protection, evidence.work_delivery.as_str()])?;
            if tx.execute("UPDATE mcp_owners SET outstanding=outstanding-1 WHERE owner_key=?1 AND outstanding>0", [owner.key().to_string()])? != 1 { return Err(StoreError::Corrupt); }
            fact(&tx, &owner, Some(id), "uncertain_work_settled", now, &Value::Text(late_digest.to_string()), &limits)?;
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
    let name = match owner.scope() {
        McpOwnerScope::Host(name) => name.as_str(),
        McpOwnerScope::Run(run) => run.as_str(),
    };
    let row: (String, String, i64, i64, i64) = db.query_row("SELECT principal,name,generation,expires_ms,last_time_ms FROM mcp_owners WHERE owner_key=?1", [owner.key().to_string()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(StoreError::Missing)?;
    let mut now = observed.max(row.4);
    let expiry = if let McpOwnerScope::Run(run) = owner.scope() {
        let header = load_header(db, run)?;
        now = now.max(header.last_time_ms);
        if header.fence != owner.generation() || header.status.is_terminal() {
            return Err(StoreError::Fenced);
        }
        header.expires_at_ms.ok_or(StoreError::Fenced)?
    } else {
        row.3
    };
    if row.0 != owner.host().principal()
        || row.1 != name
        || row.2 != owner.generation()
        || now >= expiry
    {
        return Err(StoreError::Fenced);
    }
    db.execute(
        "UPDATE mcp_owners SET last_time_ms=?2 WHERE owner_key=?1",
        params![owner.key().to_string(), now],
    )?;
    Ok(now)
}
fn host_owner(owner: &McpOwner) -> Result<(), StoreError> {
    if matches!(owner.scope(), McpOwnerScope::Host(_)) {
        Ok(())
    } else {
        Err(StoreError::Invalid)
    }
}
fn register_run_owner(
    db: &Db,
    owner: &McpOwner,
    observed: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    let McpOwnerScope::Run(run) = owner.scope() else {
        return Ok(());
    };
    let header = load_header(db, run)?;
    let now = observed.max(header.last_time_ms);
    if header.fence != owner.generation()
        || header.status.is_terminal()
        || header.expires_at_ms.is_none_or(|expiry| now >= expiry)
    {
        return Err(StoreError::Fenced);
    }
    let mut statement = db.prepare(
        "SELECT payload FROM records WHERE run_id=?1 AND family='scope' AND record_id=?2",
    )?;
    let mut rows = statement.query(params![run.as_str(), header.root_scope.to_string()])?;
    let row = rows.next()?.ok_or(StoreError::Missing)?;
    let scope = cbor::decode(
        row.get_ref(0)?.as_blob().map_err(|_| StoreError::Corrupt)?,
        limits,
    )?;
    if map(&scope)?.get("principal") != Some(&Value::Text(owner.host().principal().into())) {
        return Err(StoreError::Fenced);
    }
    drop(rows);
    drop(statement);
    let previous: Option<i64> = db
        .query_row(
            "SELECT generation FROM mcp_owners WHERE owner_key=?1",
            [owner.key().to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if previous.is_some_and(|generation| generation > owner.generation()) {
        return Err(StoreError::Fenced);
    }
    db.execute("INSERT INTO mcp_owners(owner_key,principal,name,generation,expires_ms,last_time_ms) VALUES(?1,?2,?3,?4,?5,?6)
        ON CONFLICT(owner_key) DO UPDATE SET generation=excluded.generation,expires_ms=excluded.expires_ms,last_time_ms=max(last_time_ms,excluded.last_time_ms)",
        params![owner.key().to_string(), owner.host().principal(), run.as_str(), owner.generation(), header.expires_at_ms, now])?;
    Ok(())
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
pub(super) fn run_metadata(
    db: &Db,
    run: &RunId,
    invocation: Option<crate::identity::InvocationId>,
    attempt: Option<crate::identity::AttemptId>,
    limits: &Limits,
) -> Result<(Vec<Digest>, SecurityLabels), StoreError> {
    let mut statement = db.prepare("SELECT interaction_id,protection FROM mcp_interactions WHERE origin_run=?1 AND (?2 IS NULL OR origin_invocation=?2) AND (?3 IS NULL OR origin_attempt=?3) ORDER BY interaction_id")?;
    let mut rows = statement.query(params![
        run.as_str(),
        invocation.map(|id| id.to_string()),
        attempt.map(|id| id.to_string())
    ])?;
    let mut ids = Vec::new();
    let mut labels = SecurityLabels::default();
    let mut retained = 0usize;
    while let Some(row) = rows.next()? {
        let bytes = row.get_ref(1)?.as_blob().map_err(|_| StoreError::Corrupt)?;
        retained = retained
            .checked_add(bytes.len() + 128)
            .ok_or(StoreError::Invalid)?;
        if retained > limits.max_document_bytes {
            return Err(StoreError::Invalid);
        }
        let protection = SecurityLabels::from_value(&cbor::decode(bytes, limits)?, limits)
            .map_err(|_| StoreError::Corrupt)?;
        labels = labels
            .union(&protection, limits)
            .map_err(|_| StoreError::Invalid)?;
        ids.push(
            row.get::<_, String>(0)?
                .parse()
                .map_err(|_| StoreError::Corrupt)?,
        );
    }
    Ok((ids, labels))
}

fn checked_usage(db: &Db, owner: &McpOwner) -> Result<McpOwnerUsage, StoreError> {
    let key = owner.key().to_string();
    let totals = db
        .query_row(
            "SELECT calls_spent,outstanding FROM mcp_owners WHERE owner_key=?1",
            [&key],
            |row| {
                Ok(McpOwnerUsage {
                    calls_spent: row.get(0)?,
                    outstanding: row.get(1)?,
                })
            },
        )
        .optional()?
        .ok_or(StoreError::Missing)?;
    let actual: (u64, u64) = db.query_row("SELECT coalesce(sum(call_charged),0),coalesce(sum(outstanding),0) FROM mcp_interactions WHERE owner_key=?1", [&key], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if (totals.calls_spent, totals.outstanding) != actual {
        return Err(StoreError::Corrupt);
    }
    Ok(totals)
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
fn validate_work_reply(
    reply: &[u8],
    delivery: McpWorkDelivery,
    limits: &Limits,
) -> Result<(), StoreError> {
    let reply = JsonDocument::new(reply, limits).map_err(|_| StoreError::Invalid)?;
    if delivery != McpWorkDelivery::Completed && map(reply.value())?.get("result").is_some() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
fn late_identity(
    id: Digest,
    raw: Digest,
    delivery: McpWorkDelivery,
    usage: Option<&[u8]>,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    identity(
        "late",
        vec![
            Value::Text(id.to_string()),
            Value::Text(raw.to_string()),
            Value::Text(delivery.as_str().into()),
            usage.map_or(Value::Null, |bytes| {
                Value::Text(hash_bytes(bytes).to_string())
            }),
        ],
        limits,
    )
}
fn late_reply(
    row: &rusqlite::Row<'_>,
    id: Digest,
    request: &[u8],
    limits: &Limits,
) -> Result<McpLateReply, StoreError> {
    let reply = bounded_blob(row, 1, limits)?.ok_or(StoreError::Corrupt)?;
    let protection = bounded_blob(row, 2, limits)?.ok_or(StoreError::Corrupt)?;
    let usage = bounded_blob(row, 5, limits)?;
    let raw_digest = row
        .get::<_, String>(3)?
        .parse()
        .map_err(|_| StoreError::Corrupt)?;
    let work_delivery =
        McpWorkDelivery::parse(&row.get::<_, String>(4)?).ok_or(StoreError::Corrupt)?;
    let digest = row
        .get::<_, String>(0)?
        .parse()
        .map_err(|_| StoreError::Corrupt)?;
    if hash_bytes(&reply) != raw_digest
        || late_identity(id, raw_digest, work_delivery, usage.as_deref(), limits)? != digest
    {
        return Err(StoreError::Corrupt);
    }
    validate_reply(request, &reply, limits).map_err(|_| StoreError::Corrupt)?;
    let labels = SecurityLabels::from_value(&cbor::decode(&protection, limits)?, limits)
        .map_err(|_| StoreError::Corrupt)?;
    Ok(McpLateReply {
        digest,
        raw_digest,
        work_delivery,
        reply,
        labels,
        usage: usage
            .map(|bytes| cbor::decode(&bytes, limits))
            .transpose()?,
    })
}
fn interaction(
    db: &Db,
    owner: &McpOwner,
    id: Digest,
    limits: &Limits,
) -> Result<Option<McpStoredInteraction>, StoreError> {
    let mut statement = db.prepare("SELECT session_id,request_digest,raw_digest,request,protection,generation,revision,state,deadline_ms,outstanding,reply,reply_digest,usage,send_attempts,transport_accepted,settlement_digest,work_delivery,call_charged,origin_invocation,origin_attempt,origin_run FROM mcp_interactions WHERE owner_key=?1 AND interaction_id=?2")?;
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
    let parent: Option<Vec<u8>> = db.query_row("SELECT payload FROM mcp_events WHERE owner_key=?1 AND interaction_id=?2 AND kind='interaction_parent'", params![owner.key().to_string(), id.to_string()], |row| row.get(0)).optional()?;
    let (parent_interaction, depth) = if let Some(parent) = parent {
        match cbor::decode(&parent, limits)? {
            Value::Array(values) => match values.as_slice() {
                [Value::Text(parent), Value::Integer(depth)] if *depth > 1 && *depth <= 1024 => (
                    Some(parent.parse().map_err(|_| StoreError::Corrupt)?),
                    *depth as usize,
                ),
                _ => return Err(StoreError::Corrupt),
            },
            _ => return Err(StoreError::Corrupt),
        }
    } else {
        (None, 1)
    };
    let mut record = McpStoredInteraction {
        parent_interaction,
        depth,
        origin: match (
            row.get::<_, Option<String>>(18)?,
            row.get::<_, Option<String>>(19)?,
            row.get::<_, Option<String>>(20)?,
            owner.scope(),
        ) {
            (Some(invocation), Some(attempt), Some(stored_run), McpOwnerScope::Run(run))
                if stored_run == run.as_str() =>
            {
                Some(McpOperationOrigin {
                    run_id: run.clone(),
                    invocation_id: invocation.parse().map_err(|_| StoreError::Corrupt)?,
                    attempt_id: attempt.parse().map_err(|_| StoreError::Corrupt)?,
                })
            }
            (None, None, None, _) => None,
            _ => return Err(StoreError::Corrupt),
        },
        id,
        session: row
            .get::<_, String>(0)?
            .parse()
            .map_err(|_| StoreError::Corrupt)?,
        request_digest: request_digest.parse().map_err(|_| StoreError::Corrupt)?,
        raw_digest: raw_digest.parse().map_err(|_| StoreError::Corrupt)?,
        request: request.to_vec(),
        labels: SecurityLabels::from_value(&protection, limits).map_err(|_| StoreError::Corrupt)?,
        generation: row.get(5)?,
        revision: row.get(6)?,
        state: McpWorkState::parse(&row.get::<_, String>(7)?)?,
        work_delivery: McpWorkDelivery::parse(&row.get::<_, String>(16)?)
            .ok_or(StoreError::Corrupt)?,
        deadline_ms: row.get(8)?,
        outstanding: row.get(9)?,
        call_charged: row.get(17)?,
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
        || (record.state == McpWorkState::Result && record.reply.is_none())
        || (matches!(record.state, McpWorkState::Pending | McpWorkState::Intent)
            && record.reply.is_some())
        || (record.outstanding != (record.work_delivery == McpWorkDelivery::Uncertain))
        || (record.outstanding && !record.call_charged)
        || (record.work_delivery == McpWorkDelivery::Completed && !record.call_charged)
        || (record.state == McpWorkState::Pending
            && (record.call_charged || record.work_delivery != McpWorkDelivery::NotStarted))
        || (record.state == McpWorkState::Intent && !record.outstanding)
        || record.generation <= 0
        || record.revision <= 0
        || record.deadline_ms <= 0
        || (record.transport_accepted && record.send_attempts == 0)
        || (record.settlement_digest.is_some()
            && (!matches!(record.state, McpWorkState::Cancelled | McpWorkState::Result)
                || record.outstanding))
    {
        return Err(StoreError::Corrupt);
    }
    if let Some(reply) = &record.reply {
        let delivery = if record.state == McpWorkState::Cancelled {
            McpWorkDelivery::Uncertain
        } else {
            record.work_delivery
        };
        validate_work_reply(reply, delivery, limits).map_err(|_| StoreError::Corrupt)?;
    }
    if let Some(digest) = record.settlement_digest {
        let mut statement = db.prepare("SELECT digest,reply,protection,raw_digest,work_delivery,usage FROM mcp_late_results WHERE interaction_id=?1 AND digest=?2")?;
        let mut rows = statement.query(params![record.id.to_string(), digest.to_string()])?;
        let evidence = late_reply(
            rows.next()?.ok_or(StoreError::Corrupt)?,
            record.id,
            &record.request,
            limits,
        )?;
        if evidence.work_delivery != record.work_delivery
            || evidence.work_delivery == McpWorkDelivery::Uncertain
        {
            return Err(StoreError::Corrupt);
        }
        record.labels = record
            .labels
            .union(&evidence.labels, limits)
            .map_err(|_| StoreError::Corrupt)?;
    }
    Ok(Some(record))
}
fn bounded_blob(
    row: &rusqlite::Row<'_>,
    index: usize,
    limits: &Limits,
) -> Result<Option<Vec<u8>>, StoreError> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Blob(bytes) if bytes.len() <= limits.max_document_bytes / 8 => {
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

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn recovery_pages_admitted_history_larger_than_one_document_and_checks_orphans() {
        actix::System::new().block_on(async {
            let limits = Limits { max_document_bytes: 16 * 1024, ..Limits::default() };
            let config = StoreConfig { limits: limits.clone(), max_page_size: 2, ..StoreConfig::default() };
            let path = std::env::temp_dir().join(format!("htlk-client-history-{}-{}.sqlite", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            let store = SqliteStore::open(&path, config.clone()).await.unwrap();
            let run_id = RunId::new("history".into(), &limits).unwrap();
            let root = ScopeInstanceId::root(&run_id, &limits).unwrap();
            store.create_run(RunSeed { run_id: run_id.clone(), fingerprint: hash_bytes(b"graph"), root_scope: root,
                actor: "alice".into(), causation: "test".into(), timestamp_ms: 1 }).await.unwrap();
            let lease = store.acquire(&run_id, "worker".into(), 1, 1000).await.unwrap();
            store.commit(&lease, Transition {
                context: TransitionContext { command_id: "scope".into(), actor: "worker".into(), causation: "test".into(), timestamp_ms: 2 },
                writes: vec![RecordWrite { key: RecordKey { family: RecordFamily::Scope, id: root.to_string() }, expected_revision: None, final_record: false,
                    payload: JsonDocument::new(br#"{"principal":"alice","deadline_ms":900}"#, &limits).unwrap().value().clone() }],
                next_status: None, root: None, expected_fingerprint: None,
            }).await.unwrap();
            let owner = McpOwner::new(HostContext::new("alice".into(), &limits).unwrap(), McpOwnerScope::Run(run_id.clone()), lease.fence(), &limits).unwrap();
            let session = store.open_mcp_epoch(&owner, hash_bytes(b"server"), 3).await.unwrap();
            let mut total = 0;
            for id in 0..20 {
                let request = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"roots/list","params":{{"padding":"{}"}}}}"#, "x".repeat(1024)).into_bytes();
                total += request.len();
                store.receive_run_mcp_interaction(&owner, session.id, None, request, SecurityLabels::default(), 800, 4, 32).await.unwrap();
            }
            assert!(total > limits.max_document_bytes);
            store.close().await.unwrap();
            let store = SqliteStore::open(&path, config).await.unwrap();
            store.validate_run_mcp_interactions(&run_id).await.unwrap();
            // An orphan reservation must still be detected after the last page.
            let run = run_id.clone();
            store.connection.call(move |db| db.execute("INSERT INTO records(run_id,family,record_id,revision,final,payload) VALUES(?1,'reservation','client:orphan',1,0,x'a0')", [run.as_str()])).await.unwrap();
            assert_eq!(store.validate_run_mcp_interactions(&run_id).await.unwrap_err(), StoreError::Corrupt);
            store.connection.call(|db| db.execute("DELETE FROM records WHERE record_id='client:orphan'", [])).await.unwrap();
            // Corruption in a later page cannot be skipped by bounded iteration.
            store.connection.call(|db| db.execute("UPDATE mcp_interactions SET raw_digest=?1 WHERE interaction_id=(SELECT max(interaction_id) FROM mcp_interactions)", [hash_bytes(b"wrong").to_string()])).await.unwrap();
            assert_eq!(store.validate_run_mcp_interactions(&run_id).await.unwrap_err(), StoreError::Corrupt);
            store.close().await.unwrap();
            for suffix in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", path.display())); }
        });
    }
}
