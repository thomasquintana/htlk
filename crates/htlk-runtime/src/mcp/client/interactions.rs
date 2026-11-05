//! Addressed host commands and admission over durable client work records.
use super::*;
use crate::mcp::{as_map, get};
use crate::{
    records::SecurityLabels,
    store::{McpStoredInteraction, McpWorkClaim, McpWorkState},
};

/// A distinct authority purpose; replying to a form is not approval of a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpClientAction {
    /// Enumerate an owner's interaction identities, with per-record inspection grants.
    List,
    /// Admit a server request to a host service.
    Dispatch,
    /// Inspect protected interaction values and history.
    Inspect,
    /// Submit an addressed human response.
    Reply,
    /// Cancel an interaction without undoing work already dispatched.
    Cancel,
    /// Disclose the recorded result/error to the requesting MCP server.
    Deliver,
    /// Reconcile uncertain work against explicit retained evidence.
    Reconcile,
}

/// Exact protected action presented to the current host authorization policy.
pub struct McpClientAuthorization<'a> {
    /// Authority purpose selected by the client, never by request data.
    pub action: McpClientAction,
    /// Actual session owner and ownership generation.
    pub owner: &'a McpOwner,
    /// Exact compound server identity, absent for owner-wide enumeration.
    pub server: Option<Digest>,
    /// Durable interaction identity, absent before initial admission.
    pub interaction_id: Option<Digest>,
    /// Exact canonical incoming request identity, absent for owner-wide enumeration.
    pub request_digest: Option<Digest>,
    /// Conservative protection of all relevant inputs/results.
    pub labels: &'a SecurityLabels,
    /// Admitted request for dispatch decisions; absent for metadata-only grants.
    pub request: Option<&'a JsonDocument>,
    /// Proposed response/disclosure value, when relevant to authorization.
    pub response: Option<&'a JsonDocument>,
}

/// Trusted embedding-host authorization and classification. These hooks receive
/// untrusted MCP values as data and must not treat their metadata as authority.
pub trait McpClientAuthorizer: Send + Sync {
    /// Authorizes the actual authenticated actor's exact action. This is a
    /// nonblocking decision hook; asynchronous human input uses addressed replies.
    ///
    /// # Errors
    /// Returns a redacted denial before the corresponding action is performed.
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure>;
    /// Classifies a server request before it is persisted or dispatched.
    ///
    /// # Errors
    /// Returns a redacted classification failure.
    fn request_labels(
        &self,
        _owner: &McpOwner,
        _server: Digest,
        _request: &JsonDocument,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
    /// Adds trusted classification to a human reply or host result.
    ///
    /// # Errors
    /// Returns a redacted classification failure before acceptance/disclosure.
    fn result_labels(
        &self,
        _actor: &HostContext,
        _owner: &McpOwner,
        _result: &JsonDocument,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
}

/// Authorized bounded interaction evidence. Truncated bytes are not complete
/// protocol documents and must not be parsed as complete requests or replies.
#[derive(Clone, Debug)]
pub struct McpInteractionView {
    /// Protected facts with bounded request/reply/usage representations.
    pub record: McpStoredInteraction,
    /// Exact server identity, retained independently of live connections.
    pub server: Digest,
    /// Whether the original transport epoch has closed. Stored work remains evidence.
    pub session_closed: bool,
    /// Original request was not returned completely.
    pub request_truncated: bool,
    /// Original reply was not returned completely.
    pub reply_truncated: bool,
    /// Usage did not fit the remaining representation allowance.
    pub usage_truncated: bool,
}

/// Stable receipt for a submitted response. Acceptance of a host reply does not
/// assert server consumption or completion of an external URL interaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpInteractionReceipt {
    /// Exact addressed interaction.
    pub interaction_id: Digest,
    /// Immutable full reply identity.
    pub reply_digest: Digest,
}

/// A bounded authorized identity page. Continue with `next`, including when all
/// scanned records were filtered by their individual protection grants.
#[derive(Clone, Debug)]
pub struct McpInteractionPage {
    /// Authorized selected identities; values require explicit inspection.
    pub interactions: Vec<Digest>,
    /// Opaque continuation from the last scanned record, absent at the end.
    pub next: Option<Digest>,
}

/// Common interaction API for interactive and headless hosts. It uses the same
/// current authorizer, immutable request digest, generation checks, and receipts.
#[derive(Clone)]
pub struct McpInteractionHost {
    config: Arc<McpClientConfig>,
    persistence: McpClientPersistence,
    authorizer: Arc<dyn McpClientAuthorizer>,
    limits: Limits,
}
impl McpInteractionHost {
    /// Links a checked configuration's persistence and authorization services.
    ///
    /// # Errors
    /// Returns a profile failure if either required service is missing.
    pub fn new(config: McpClientConfig) -> Result<Self, RuntimeFailure> {
        let persistence = config
            .persistence
            .clone()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let authorizer = config
            .authorizer
            .clone()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let mut limits = persistence.store.config().limits.clone();
        limits.max_document_bytes = limits.max_document_bytes.min(config.limits.frame_bytes);
        Ok(Self {
            config: Arc::new(config),
            persistence,
            authorizer,
            limits,
        })
    }
    /// Enumerates durable interactions for a headless/polling frontend, using the
    /// same identity and authorization contract as live interaction events.
    ///
    /// # Errors
    /// Returns denial, invalid paging bounds, corruption, or storage failure.
    pub async fn list(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        after: Option<Digest>,
        maximum: usize,
    ) -> Result<McpInteractionPage, RuntimeFailure> {
        let labels = SecurityLabels::default();
        self.authorizer.authorize(
            actor,
            McpClientAuthorization {
                action: McpClientAction::List,
                owner,
                server: None,
                interaction_id: None,
                request_digest: None,
                labels: &labels,
                request: None,
                response: None,
            },
        )?;
        let ids = self
            .persistence
            .store
            .mcp_interaction_ids(owner, after, maximum)
            .await
            .map_err(|e| fail(e.code()))?;
        let next = (ids.len() == maximum)
            .then(|| ids.last().copied())
            .flatten();
        let mut interactions = Vec::new();
        for id in ids {
            let record = self.record(owner, id).await?;
            let server = self.server(owner, &record).await?;
            match self.authorize(
                actor,
                owner,
                server,
                &record,
                McpClientAction::Inspect,
                None,
            ) {
                Ok(()) => interactions.push(id),
                Err(error) if error.code() == FailureCode::Policy => {}
                Err(error) => return Err(error),
            }
        }
        Ok(McpInteractionPage { interactions, next })
    }
    /// Reads one explicitly selected interaction after current authorization.
    /// The byte allowance covers returned request/reply/usage representations;
    /// metadata remains independently bounded by the store codec.
    ///
    /// # Errors
    /// Returns denial, invalid bounds/selection, corruption, or storage failure.
    pub async fn inspect(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        id: Digest,
        maximum_bytes: usize,
    ) -> Result<McpInteractionView, RuntimeFailure> {
        if maximum_bytes == 0 || maximum_bytes > self.limits.max_document_bytes {
            return Err(fail(FailureCode::Input));
        }
        let mut record = self.record(owner, id).await?;
        let session = self
            .persistence
            .store
            .mcp_session(owner, record.session)
            .await
            .map_err(|e| fail(e.code()))?;
        let server = session.server;
        self.authorize(
            actor,
            owner,
            server,
            &record,
            McpClientAction::Inspect,
            None,
        )?;
        let mut remaining = maximum_bytes;
        let request_truncated = record.request.len() > remaining;
        record.request.truncate(remaining);
        remaining -= record.request.len();
        let mut reply_truncated = false;
        if let Some(reply) = &mut record.reply {
            reply_truncated = reply.len() > remaining;
            reply.truncate(remaining);
            remaining -= reply.len();
        }
        let usage_truncated = record.usage.as_ref().is_some_and(|usage| {
            htlk_executable::cbor::encode(usage, &self.limits)
                .map_or(true, |bytes| bytes.len() > remaining)
        });
        if usage_truncated {
            record.usage = None;
        }
        Ok(McpInteractionView {
            record,
            server,
            session_closed: session.closed,
            request_truncated,
            reply_truncated,
            usage_truncated,
        })
    }
    /// Submits a schema-validated form/URL response using the exact original request
    /// digest. Sampling/model results use the host-handler path, not human input.
    ///
    /// # Errors
    /// Returns purpose/schema/authority/deadline/key conflicts or storage failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn respond(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        id: Digest,
        request_digest: Digest,
        result: JsonDocument,
        idempotency_key: &str,
    ) -> Result<McpInteractionReceipt, RuntimeFailure> {
        let record = self.record(owner, id).await?;
        if record.request_digest != request_digest {
            return Err(fail(FailureCode::Digest));
        }
        let server = self.server(owner, &record).await?;
        self.authorize(
            actor,
            owner,
            server,
            &record,
            McpClientAction::Reply,
            Some(&result),
        )?;
        let request = JsonDocument::new(&record.request, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let kind = validate_client_request(&request, &self.config, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?;
        if !matches!(kind, McpIncomingKind::Form | McpIncomingKind::Url) {
            return Err(fail(FailureCode::Policy));
        }
        validate_client_result(&request, &result, &self.config, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?;
        let reply = response(&request, Ok(&result), &self.limits)?;
        let labels = self.authorizer.result_labels(actor, owner, &result)?;
        let saved = self
            .persistence
            .store
            .submit_mcp_reply(
                owner,
                id,
                actor,
                idempotency_key,
                reply,
                labels,
                self.persistence.clock.now_ms()?,
            )
            .await
            .map_err(|e| fail(e.code()))?;
        Ok(McpInteractionReceipt {
            interaction_id: id,
            reply_digest: saved
                .reply_digest
                .ok_or_else(|| fail(FailureCode::Invariant))?,
        })
    }
    /// Commits cancellation under current authority. The callback executor observes
    /// the same durable disposition; no unknown external work is refunded.
    ///
    /// # Errors
    /// Returns denial, stale ownership, or storage failure.
    pub async fn cancel(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<McpWorkState, RuntimeFailure> {
        let record = self.record(owner, id).await?;
        self.authorize(
            actor,
            owner,
            self.server(owner, &record).await?,
            &record,
            McpClientAction::Cancel,
            None,
        )?;
        self.persistence
            .store
            .cancel_mcp_work(owner, id, self.persistence.clock.now_ms()?)
            .await
            .map(|record| record.state)
            .map_err(|e| fail(e.code()))
    }
    /// Reconciles an exact retained late observation without changing the saved RPC
    /// reply or reviving cancelled work.
    ///
    /// # Errors
    /// Returns denial, uncertain/missing evidence, stale ownership, or storage failure.
    pub async fn reconcile(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        id: Digest,
        evidence: Digest,
    ) -> Result<(), RuntimeFailure> {
        let record = self.record(owner, id).await?;
        self.authorize(
            actor,
            owner,
            self.server(owner, &record).await?,
            &record,
            McpClientAction::Reconcile,
            None,
        )?;
        self.persistence
            .store
            .settle_mcp_work(owner, id, evidence, self.persistence.clock.now_ms()?)
            .await
            .map(|_| ())
            .map_err(|e| fail(e.code()))
    }
    pub(super) async fn receive(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        raw: Vec<u8>,
        deadline: i64,
    ) -> Result<McpStoredInteraction, RuntimeFailure> {
        let request =
            JsonDocument::new(&raw, &self.limits).map_err(|_| fail(FailureCode::McpProtocol))?;
        validate_client_request(&request, &self.config, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let labels = self.authorizer.request_labels(owner, server, &request)?;
        self.authorizer.authorize(
            owner.host(),
            McpClientAuthorization {
                action: McpClientAction::Dispatch,
                owner,
                server: Some(server),
                interaction_id: None,
                request_digest: Some(request.digest()),
                labels: &labels,
                request: Some(&request),
                response: None,
            },
        )?;
        self.persistence
            .store
            .receive_mcp_interaction(
                owner,
                session,
                raw,
                labels,
                deadline,
                self.persistence.clock.now_ms()?,
                self.config.limits.requests_per_session,
            )
            .await
            .map_err(|e| fail(e.code()))
    }
    pub(super) async fn begin(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<McpWorkClaim, RuntimeFailure> {
        self.persistence
            .store
            .begin_mcp_work(
                owner,
                id,
                self.persistence.clock.now_ms()?,
                self.config.limits.requests_per_session as u64,
                self.config.limits.callbacks as u64,
            )
            .await
            .map_err(|e| fail(e.code()))
    }
    pub(super) async fn finish(
        &self,
        owner: &McpOwner,
        record: &McpStoredInteraction,
        mut outcome: McpServiceOutcome,
    ) -> Result<McpStoredInteraction, RuntimeFailure> {
        let request = JsonDocument::new(&record.request, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let result_document = match &outcome.result {
            Ok(result) => result,
            Err(error) => error.document(),
        };
        outcome.labels = outcome
            .labels
            .union(
                &self
                    .authorizer
                    .result_labels(owner.host(), owner, result_document)?,
                &self.limits,
            )
            .map_err(|_| fail(FailureCode::Profile))?;
        let rejected = if let Ok(result) = &outcome.result
            && (outcome.delivery != McpWorkDelivery::Completed
                || validate_client_result(&request, result, &self.config, &self.limits).is_err())
        {
            let rejected = response(&request, Ok(result), &self.limits).ok();
            outcome.result = Err(rpc_error(
                -32603,
                "Invalid host-service result.",
                &self.limits,
            )?);
            rejected
        } else {
            None
        };
        let reply = response(&request, outcome.result.as_ref(), &self.limits)?;
        let usage = outcome
            .usage
            .map(|usage| usage.to_value(&self.limits))
            .transpose()?;
        let retention = 16.min(self.persistence.store.config().max_page_size);
        if let Some(rejected) = rejected {
            let _ = self
                .persistence
                .store
                .retain_mcp_late_reply(
                    owner,
                    record.id,
                    rejected,
                    outcome.labels.clone(),
                    outcome.delivery,
                    usage.clone(),
                    retention,
                )
                .await;
        }
        let saved = self
            .persistence
            .store
            .complete_mcp_work(
                owner,
                record.id,
                owner.host(),
                &format!("host-handler:{}", record.id),
                reply.clone(),
                outcome.labels.clone(),
                usage.clone(),
                outcome.delivery,
                self.persistence.clock.now_ms()?,
            )
            .await;
        match saved {
            Ok(record) => Ok(record),
            Err(error) => {
                let _ = self
                    .persistence
                    .store
                    .retain_mcp_late_reply(
                        owner,
                        record.id,
                        reply,
                        outcome.labels,
                        outcome.delivery,
                        usage,
                        retention,
                    )
                    .await;
                Err(fail(error.code()))
            }
        }
    }
    pub(super) async fn record(
        &self,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<McpStoredInteraction, RuntimeFailure> {
        self.persistence
            .store
            .mcp_interaction(owner, id)
            .await
            .map_err(|e| fail(e.code()))
    }
    pub(super) async fn server(
        &self,
        owner: &McpOwner,
        record: &McpStoredInteraction,
    ) -> Result<Digest, RuntimeFailure> {
        self.persistence
            .store
            .mcp_session(owner, record.session)
            .await
            .map(|session| session.server)
            .map_err(|e| fail(e.code()))
    }
    pub(super) fn authorize(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        server: Digest,
        record: &McpStoredInteraction,
        action: McpClientAction,
        result: Option<&JsonDocument>,
    ) -> Result<(), RuntimeFailure> {
        self.authorizer.authorize(
            actor,
            McpClientAuthorization {
                action,
                owner,
                server: Some(server),
                interaction_id: Some(record.id),
                request_digest: Some(record.request_digest),
                labels: &record.labels,
                request: None,
                response: result,
            },
        )
    }
}

/// Elicitation handler that waits for an addressed `McpInteractionHost::respond`
/// command. Both interactive and headless consumers use that same public API.
pub struct McpAwaitReply {
    persistence: McpClientPersistence,
}
impl McpAwaitReply {
    /// Uses the same persistence service as the client configuration.
    pub fn new(persistence: McpClientPersistence) -> Self {
        Self { persistence }
    }
}
impl McpClientHandler for McpAwaitReply {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            let limits = self.persistence.store.config().limits.clone();
            let rejected = || {
                McpServiceOutcome::failure(
                    McpWorkDelivery::NotStarted,
                    rpc_error(-1, "Client interaction cancelled.", &Limits::default())
                        .expect("fixed bounded error"),
                )
            };
            loop {
                if context.cancellation.is_cancelled() {
                    return rejected();
                }
                let record = match self
                    .persistence
                    .store
                    .mcp_interaction(&context.owner, context.interaction_id)
                    .await
                {
                    Ok(record) => record,
                    Err(_) => return rejected(),
                };
                if record.state == McpWorkState::Cancelled {
                    return rejected();
                }
                if let Some(reply) = record.reply {
                    let parsed = JsonDocument::new(&reply, &limits).ok();
                    if let Some(parsed) = parsed
                        && let Some(result) = as_map(parsed.value())
                            .ok()
                            .and_then(|fields| fields.get("result"))
                        && let Ok(result) = JsonDocument::from_value(result, &limits)
                    {
                        return McpServiceOutcome::completed(result);
                    }
                    return rejected();
                }
                tokio::select! {
                    _ = context.cancellation.cancelled() => return rejected(),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        })
    }
}

pub(super) fn rpc_error(
    code: i64,
    message: &str,
    limits: &Limits,
) -> Result<McpRpcError, RuntimeFailure> {
    McpRpcError::new(
        JsonDocument::from_value(
            &obj([
                ("code", Value::Integer(code)),
                ("message", Value::Text(message.into())),
            ])?,
            limits,
        )
        .map_err(|_| fail(FailureCode::McpProtocol))?,
    )
}
pub(super) fn response(
    request: &JsonDocument,
    result: Result<&JsonDocument, &McpRpcError>,
    limits: &Limits,
) -> Result<Vec<u8>, RuntimeFailure> {
    let id = get(as_map(request.value())?, "id")?.clone();
    let (key, value) = match result {
        Ok(result) => ("result", result.value()),
        Err(error) => ("error", error.document().value()),
    };
    JsonDocument::from_value(
        &obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("id", id),
            (key, value.clone()),
        ])?,
        limits,
    )
    .map(|document| document.as_bytes().to_vec())
    .map_err(|_| fail(FailureCode::McpProtocol))
}
