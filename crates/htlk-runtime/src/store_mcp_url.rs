//! Fenced URL-elicitation store facade. Declare as a child of `store_mcp` so
//! transactions reuse its actual host/run authority and session checks.
use super::*;
use crate::mcp::client::{
    McpRpcError, McpStoredUrlInteraction, McpStoredUrlRequiredError, McpUrlCompletion,
    McpUrlConsent, McpUrlRequest, McpUrlRequiredError,
};
use crate::store_db as state_db;
#[path = "store_mcp_url_state.rs"]
mod state;

fn checked_scope<'a>(
    db: &Db,
    owner: &'a McpOwner,
    server: Digest,
    session: Digest,
    now_ms: i64,
) -> Result<(state::Scope<'a>, i64), StoreError> {
    let now = authority(db, owner, now_ms)?;
    session_authority(db, owner, session, true)?;
    let actual: String = db.query_row(
        "SELECT server_digest FROM mcp_epochs WHERE session_id=?1",
        [session.to_string()],
        |row| row.get(0),
    )?;
    if actual != server.to_string() {
        return Err(StoreError::Fenced);
    }
    Ok((
        state::Scope {
            owner,
            server,
            session,
        },
        now,
    ))
}

impl SqliteStore {
    /// Enumerates bounded URL addresses without reading URL/error values. Each
    /// tuple is `(interaction_id, server_digest, session_id)`. The authenticated
    /// host service applies its list grant and per-record inspection grant before
    /// returning these identities to an interactive or headless frontend.
    ///
    /// # Errors
    /// Returns invalid bounds, corrupt identities, or storage failure.
    pub async fn mcp_url_addresses(
        &self,
        owner: &McpOwner,
        after: Option<Digest>,
        maximum: usize,
    ) -> Result<Vec<(Digest, Digest, Digest)>, StoreError> {
        let count = self.page_size(maximum)?;
        let key = owner.key().to_string();
        self.connection.call(move |db| -> Result<_, StoreError> {
            let mut statement = db.prepare("SELECT url_id,server_digest,session_id FROM mcp_url_interactions WHERE owner_key=?1 AND url_id>?2 ORDER BY url_id LIMIT ?3")?;
            let mut rows = statement.query(params![key, after.map_or_else(String::new, |id| id.to_string()), count])?;
            let mut addresses = Vec::new();
            while let Some(row) = rows.next()? {
                let parse = |column| row.get::<_, String>(column)?.parse().map_err(|_| StoreError::Corrupt);
                addresses.push((parse(0)?, parse(1)?, parse(2)?));
            }
            Ok(addresses)
        }).await.map_err(StoreError::from_worker)
    }

    /// Registers an already admitted URL callback before publishing its event or
    /// dispatching its handler. Its existing protected request/reply remains the
    /// sole authority for consent; no second response ledger is introduced.
    ///
    /// # Errors
    /// Returns stale owner/session, wrong server/method/mode, conflict, or limits.
    #[allow(clippy::too_many_arguments)]
    pub async fn track_mcp_url_callback(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        callback: Digest,
        now_ms: i64,
        maximum_records: usize,
    ) -> Result<Digest, StoreError> {
        timestamp(now_ms)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (scope, now) = checked_scope(&tx, &owner, server, session, now_ms)?;
                let record =
                    interaction(&tx, &owner, callback, &limits)?.ok_or(StoreError::Missing)?;
                if record.session != session || record.generation != owner.generation() {
                    return Err(StoreError::Fenced);
                }
                let request =
                    JsonDocument::new(&record.request, &limits).map_err(|_| StoreError::Corrupt)?;
                let fields = map(request.value())?;
                if fields.get("method") != Some(&Value::Text("elicitation/create".into())) {
                    return Err(StoreError::Invalid);
                }
                let request = McpUrlRequest::from_params(field(fields, "params")?, &limits)
                    .map_err(|_| StoreError::Invalid)?;
                let id = state::register(
                    &tx,
                    scope,
                    &request,
                    Some(callback),
                    &record.labels,
                    record.deadline_ms,
                    maximum_records,
                    &limits,
                )?;
                state::set_origin(&tx, id, record.origin.as_ref(), false, &limits)?;
                // Opaque identities only: URL, elicitationId and metadata stay protected.
                fact(
                    &tx,
                    &owner,
                    Some(callback),
                    "url_interaction_known",
                    now,
                    &Value::Text(id.to_string()),
                    &limits,
                )?;
                tx.commit()?;
                Ok(id)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Atomically records an original URL-required error and its entire validated
    /// list. An invalid list or capacity/conflict failure commits no partial IDs.
    /// The original RPC outcome is never replaced, retried, or marked successful.
    ///
    /// # Errors
    /// Returns malformed error/list, stale authority, expired deadline, conflict,
    /// bounded retention exhaustion, or storage failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_mcp_url_required_error(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        request_id: McpRequestId,
        error: McpRpcError,
        origin: Option<McpOperationOrigin>,
        labels: SecurityLabels,
        deadline_ms: i64,
        now_ms: i64,
        maximum_records: usize,
    ) -> Result<Vec<Digest>, StoreError> {
        check_lease_times(now_ms, deadline_ms)?;
        let limits = self.config.limits.clone();
        let required = McpUrlRequiredError::parse(&error, maximum_records, &limits)
            .map_err(|_| StoreError::Invalid)?
            .ok_or(StoreError::Invalid)?;
        let owner = owner.clone();
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (scope, now) = checked_scope(&tx, &owner, server, session, now_ms)?;
                let (labels, deadline_ms) = run::admit(
                    &tx,
                    &owner,
                    session,
                    origin.as_ref(),
                    "elicitation/create",
                    labels,
                    deadline_ms,
                    now,
                    &limits,
                )?;
                if now >= deadline_ms {
                    return Err(StoreError::Deadline(FailureCode::Timeout));
                }
                let ids = state::required(
                    &tx,
                    scope,
                    &request_id,
                    &required,
                    &labels,
                    deadline_ms,
                    maximum_records,
                    &limits,
                )?;
                state::required_origin(&tx, scope, &request_id, origin.as_ref(), &limits)?;
                for id in &ids {
                    state::set_origin(&tx, *id, origin.as_ref(), false, &limits)?;
                }
                fact(
                    &tx,
                    &owner,
                    None,
                    "url_required_error",
                    now,
                    &Value::Array(ids.iter().map(|id| Value::Text(id.to_string())).collect()),
                    &limits,
                )?;
                tx.commit()?;
                Ok(ids)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Records the first completion notification for an exact scoped known ID.
    /// Unknown/already-completed IDs are ignored without loading any URL values or
    /// producing an event. Completion is evidence even if the consent window ended.
    ///
    /// # Errors
    /// Returns stale owner/session, wrong server, invalid bounds, or store failure.
    pub async fn complete_mcp_url_interaction(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        elicitation_id: &str,
        now_ms: i64,
    ) -> Result<McpUrlCompletion, StoreError> {
        timestamp(now_ms)?;
        self.check_text(elicitation_id)?;
        let (owner, elicitation_id, limits) = (
            owner.clone(),
            elicitation_id.to_string(),
            self.config.limits.clone(),
        );
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (scope, now) = checked_scope(&tx, &owner, server, session, now_ms)?;
                let completion = state::complete(&tx, scope, &elicitation_id, now, &limits)?;
                if let McpUrlCompletion::Recorded { interaction_id } = completion {
                    fact(
                        &tx,
                        &owner,
                        None,
                        "url_interaction_completed",
                        now,
                        &Value::Text(interaction_id.to_string()),
                        &limits,
                    )?;
                }
                tx.commit()?;
                Ok(completion)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Reads exact protected URL facts, including original callback consent and
    /// session closure. The host API must authorize the unioned labels before
    /// disclosing these values. Historical inspection grants no live authority.
    ///
    /// # Errors
    /// Returns absent/cross-scope identity, corruption, or store failure.
    pub async fn mcp_url_interaction(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        id: Digest,
    ) -> Result<McpStoredUrlInteraction, StoreError> {
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| {
                state::inspect(
                    db,
                    state::Scope {
                        owner: &owner,
                        server,
                        session,
                    },
                    id,
                    &limits,
                )
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Submits explicit consent for an error-originated URL interaction. Callback
    /// responses must use the existing addressed callback API instead. Stable
    /// receipts fence racing responders; completion never supplies consent. An
    /// exact saved receipt remains retrievable after deadline/session closure,
    /// under current ownership and host authorization, without new delivery.
    ///
    /// # Errors
    /// Returns stale authority/session, wrong request identity, conflict, expiry,
    /// invalid URL result, or store failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_mcp_url_consent(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        id: Digest,
        request_digest: Digest,
        responder: &HostContext,
        idempotency_key: &str,
        result: JsonDocument,
        labels: SecurityLabels,
        now_ms: i64,
    ) -> Result<Digest, StoreError> {
        timestamp(now_ms)?;
        self.check_text(responder.principal())?;
        self.check_text(idempotency_key)?;
        if idempotency_key.is_empty() {
            return Err(StoreError::Invalid);
        }
        McpUrlConsent::from_result(&result).map_err(|_| StoreError::Invalid)?;
        let (owner, responder, key, limits) = (
            owner.clone(),
            responder.clone(),
            idempotency_key.to_string(),
            self.config.limits.clone(),
        );
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                authority(&tx, &owner, now_ms)?;
                if let Some(receipt) = state::receipt(
                    &tx,
                    state::Scope {
                        owner: &owner,
                        server,
                        session,
                    },
                    id,
                    request_digest,
                    responder.principal(),
                    &key,
                    &result,
                    &limits,
                )? {
                    tx.commit()?;
                    return Ok(receipt);
                }
                let (scope, now) = checked_scope(&tx, &owner, server, session, now_ms)?;
                let receipt = state::respond(
                    &tx,
                    scope,
                    id,
                    request_digest,
                    responder.principal(),
                    &key,
                    &result,
                    &labels,
                    now,
                    &limits,
                )?;
                fact(
                    &tx,
                    &owner,
                    None,
                    "url_consent_recorded",
                    now,
                    &Value::Text(id.to_string()),
                    &limits,
                )?;
                tx.commit()?;
                Ok(receipt)
            })
            .await
            .map_err(StoreError::from_worker)
    }

    /// Reads the exact original protected URL-required error independently of later
    /// consent/completion. No original operation, reply, or delivery fact is changed.
    ///
    /// # Errors
    /// Returns missing/cross-scope evidence, corruption, or storage failure.
    pub async fn mcp_url_required_error(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        request_id: McpRequestId,
    ) -> Result<McpStoredUrlRequiredError, StoreError> {
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| {
                state::original_error(
                    db,
                    state::Scope {
                        owner: &owner,
                        server,
                        session,
                    },
                    &request_id,
                    &limits,
                )
            })
            .await
            .map_err(StoreError::from_worker)
    }
}
