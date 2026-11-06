//! One durable, ancestor-accounted host operation, separate from callback execution.
use super::*;
use crate::mcp::{as_map, get};
use crate::store::McpStoredInteraction;

/// Protected bounded evidence of the actual operation under a host-work claim.
/// A native RPC outcome is separate from the handler's mapped sampling result.
#[derive(Clone)]
pub struct McpHostWorkEvidence {
    /// Stable durable operation claim.
    pub claim_id: Digest,
    /// Host-selected routing and input/context identities.
    pub descriptor: Value,
    /// Native outcome envelope: delivery plus exact `raw` or `protocol_error`
    /// bytes/digests, or a redacted local failure. None is unknown, not no work.
    pub native_outcome: Option<Value>,
}

/// Receipt for exactly one accounted operation. This is an attribution/idempotency
/// address, not permission to launch an independent workflow or additional calls.
/// Hosts must pass this identity to provider deduplication/status APIs when supported.
#[derive(Clone)]
pub struct McpHostWorkReceipt {
    session: McpClientSession,
    pub(super) depth: usize,
    host: McpInteractionHost,
    owner: McpOwner,
    server: Digest,
    dispatched: Arc<std::sync::atomic::AtomicBool>,
    /// Stable interaction identity, independent of ownership takeover.
    pub(super) interaction_id: Digest,
    /// Stable durable claim identity for the one operation charged to all ancestors.
    claim_id: Digest,
    /// Real graph ancestry, absent for explicitly budgeted host-session work.
    origin: Option<McpOperationOrigin>,
    /// Maximum inherited absolute deadline.
    deadline_ms: i64,
    /// Parent-linked stop signal. Cancellation never proves provider rollback.
    cancellation: Arc<McpCancellation>,
    /// Minimum protection of operation inputs and evidence.
    labels: crate::records::SecurityLabels,
}
impl McpHostWorkReceipt {
    /// Stable parent interaction address.
    pub fn interaction_id(&self) -> Digest {
        self.interaction_id
    }
    /// Stable durable identity of the single accounted operation.
    pub fn claim_id(&self) -> Digest {
        self.claim_id
    }
    /// Real originating invocation and attempt, when established.
    pub fn origin(&self) -> Option<&McpOperationOrigin> {
        self.origin.as_ref()
    }
    /// Inherited absolute deadline; hosts cannot extend this authority.
    pub fn deadline_ms(&self) -> i64 {
        self.deadline_ms
    }
    /// Parent-linked cooperative cancellation.
    pub fn cancellation(&self) -> &Arc<McpCancellation> {
        &self.cancellation
    }
    /// Minimum operation protection.
    pub fn labels(&self) -> &crate::records::SecurityLabels {
        &self.labels
    }
    /// Dispatches the one native child RPC covered by this receipt. Its callbacks
    /// inherit real interaction ancestry, cancellation and all run reservations.
    /// This cannot create a second workflow with independent budgets. The host
    /// chooses the endpoint and maps the protected reply to a sampling result.
    pub async fn invoke(&self, mut request: crate::mcp::McpRequest) -> McpClientReply {
        let session = &self.session;
        use std::sync::atomic::Ordering;
        let rejected = |code| McpClientReply::failed(crate::mcp::McpDelivery::NotSent, code);
        if session.owner().key() != self.owner.key()
            || session.owner().generation() != self.owner.generation()
            || session
                .server()
                .digest(session.representation_limits())
                .ok()
                != Some(self.server)
        {
            return rejected(FailureCode::McpIdentity);
        }
        let record = match self.host.record(&self.owner, self.interaction_id).await {
            Ok(record) => record,
            Err(error) => return rejected(error.code()),
        };
        if record.state != crate::store::McpWorkState::Intent || self.cancellation.is_cancelled() {
            return rejected(FailureCode::Cancelled);
        }
        let now = match self.host.persistence.clock.now_ms() {
            Ok(now) => now,
            Err(error) => return rejected(error.code()),
        };
        let remaining = self.deadline_ms.saturating_sub(now);
        if remaining <= 0 {
            return rejected(FailureCode::McpTimeout);
        }
        if self.dispatched.swap(true, Ordering::AcqRel) {
            return rejected(FailureCode::EffectUncertain);
        }
        request.timeout_ms = request.timeout_ms.min(remaining as u64);
        // The exact native outcome has its own bounded durable evidence envelope.
        request.max_response_bytes = request.max_response_bytes.min(
            self.host
                .persistence
                .store
                .config()
                .limits
                .max_document_bytes
                / 16,
        );
        let mut cancellation = McpCancellation::linked(&self.cancellation);
        cancellation
            .parents
            .push(request.cancellation.state.clone());
        cancellation
            .parents
            .extend(request.cancellation.parents.iter().cloned());
        request.cancellation = Arc::new(cancellation);
        let context = self
            .origin
            .clone()
            .map(|origin| crate::mcp::McpAttemptContext {
                origin,
                server: self.server,
                deadline_ms: self.deadline_ms,
                labels: self.labels.clone(),
            });
        session
            .invoke_parent(request, context, None, Some(Arc::new(self.clone())))
            .await
    }
    pub(super) async fn claim_native_dispatch(&self) -> Result<(), RuntimeFailure> {
        self.host
            .persistence
            .store
            .claim_mcp_native_operation(
                &self.owner,
                self.interaction_id,
                self.claim_id,
                self.host.persistence.clock.now_ms()?,
            )
            .await
            .map_err(|error| fail(error.code()))
    }
    pub(super) async fn retain_native_outcome(
        &self,
        reply: &McpClientReply,
    ) -> Result<(), RuntimeFailure> {
        let payload = session::wire::outcome_value(reply)?;
        let labels = match &reply.result {
            Ok(Ok(result)) => {
                self.host
                    .authorizer
                    .result_labels(self.owner.host(), &self.owner, result)?
            }
            Ok(Err(error)) => self.host.authorizer.result_labels(
                self.owner.host(),
                &self.owner,
                error.document(),
            )?,
            Err(_) => self.labels.clone(),
        };
        self.host
            .persistence
            .store
            .retain_mcp_native_outcome(
                &self.owner,
                self.interaction_id,
                payload,
                labels,
                self.host.persistence.clock.now_ms()?,
            )
            .await
            .map_err(|error| fail(error.code()))
    }
}

/// Native single-operation wrapper minted only after durable callback admission.
/// Clones share one durable claim: they cannot spend the same reservation twice.
/// Use `run` for an actual host model operation, returning its protocol result and
/// trusted usage. Completion is persisted before control returns to the handler.
/// Arbitrary child `Runtime::start_run` calls do not inherit this accounting.
#[derive(Clone)]
pub struct McpHostWork {
    session: McpClientSession,
    host: McpInteractionHost,
    owner: McpOwner,
    record: McpStoredInteraction,
    cancellation: Arc<McpCancellation>,
}
impl McpHostWork {
    pub(super) fn new(
        host: McpInteractionHost,
        owner: McpOwner,
        record: McpStoredInteraction,
        cancellation: Arc<McpCancellation>,
        session: McpClientSession,
    ) -> Self {
        Self {
            session,
            host,
            owner,
            record,
            cancellation,
        }
    }
    /// Stable parent address, usable for protected evidence inspection.
    pub fn interaction_id(&self) -> Digest {
        self.record.id
    }

    /// Executes at most one host operation under the existing ancestor reservation.
    /// `descriptor` is bounded protected host evidence (for example selected model,
    /// context digest and provider request identity), reauthorized before dispatch.
    /// The closure must perform only the one operation represented by the receipt;
    /// additional operations need their own independently admitted reservations.
    /// A saved known result is returned without constructing/polling another operation.
    ///
    /// # Errors
    /// Returns denial, cancellation, deadline, stale authority, storage failure, or
    /// uncertainty when a previous claim has no known completion. Never blind retry.
    pub async fn run<F, Fut>(
        &self,
        descriptor: JsonDocument,
        operation: F,
    ) -> Result<McpServiceOutcome, RuntimeFailure>
    where
        F: FnOnce(McpHostWorkReceipt) -> Fut,
        Fut: Future<Output = McpServiceOutcome> + Send,
    {
        let mut old = self.host.record(&self.owner, self.record.id).await?;
        let descriptor_labels =
            self.host
                .authorizer
                .result_labels(self.owner.host(), &self.owner, &descriptor)?;
        old.labels = old
            .labels
            .union(&descriptor_labels, &self.host.limits)
            .map_err(|_| fail(FailureCode::Profile))?;
        self.host.authorize(
            self.owner.host(),
            &self.owner,
            self.host.server(&self.owner, &old).await?,
            &old,
            McpClientAction::Dispatch,
            Some(&descriptor),
        )?;
        if old.reply.is_some() && old.work_delivery != McpWorkDelivery::Uncertain {
            return decode(&old, &self.host.limits);
        }
        if self.cancellation.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        let claim_id = self
            .host
            .persistence
            .store
            .claim_mcp_host_operation(
                &self.owner,
                old.id,
                descriptor_labels,
                descriptor,
                self.host.persistence.clock.now_ms()?,
            )
            .await
            .map_err(|error| {
                fail(if matches!(error, crate::store::StoreError::Conflict) {
                    FailureCode::EffectUncertain
                } else {
                    error.code()
                })
            })?;
        let old = self.host.record(&self.owner, old.id).await?;
        let receipt = McpHostWorkReceipt {
            session: self.session.clone(),
            depth: old.depth,
            host: self.host.clone(),
            owner: self.owner.clone(),
            server: self.host.server(&self.owner, &old).await?,
            dispatched: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            interaction_id: old.id,
            claim_id,
            origin: old.origin.clone(),
            deadline_ms: old.deadline_ms,
            cancellation: self.cancellation.clone(),
            labels: old.labels.clone(),
        };
        // The independent I/O supervisor enforces the absolute deadline even if
        // this host future blocks. We keep polling cooperative late completion so
        // protected evidence survives without publishing an expired response.
        let outcome = operation(receipt).await;
        let saved = self.host.finish(&self.owner, &old, outcome).await?;
        decode(&saved, &self.host.limits)
    }
}
impl McpInteractionHost {
    /// Inspects actual claimed operation evidence under the interaction's current
    /// grant. Inspection performs no dispatch and cannot release uncertain work.
    ///
    /// # Errors
    /// Returns denial, missing interaction, corrupt evidence or storage failure.
    pub async fn inspect_work(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<Option<McpHostWorkEvidence>, RuntimeFailure> {
        let record = self.record(owner, id).await?;
        self.authorize(
            actor,
            owner,
            self.server(owner, &record).await?,
            &record,
            McpClientAction::Inspect,
            None,
        )?;
        self.persistence
            .store
            .mcp_host_work_evidence(owner, id)
            .await
            .map_err(|error| fail(error.code()))
    }
    /// Recovers a known host-work outcome by its original stable interaction ID.
    /// This never opens a connection or launches work. Unknown completion requires
    /// explicit provider reconciliation, including after callback interruption.
    ///
    /// # Errors
    /// Returns denial, missing evidence, unknown completion or storage failure.
    pub async fn recover_work(
        &self,
        actor: &HostContext,
        owner: &McpOwner,
        id: Digest,
    ) -> Result<McpServiceOutcome, RuntimeFailure> {
        let record = self.record(owner, id).await?;
        self.authorize(
            actor,
            owner,
            self.server(owner, &record).await?,
            &record,
            McpClientAction::Inspect,
            None,
        )?;
        if record.work_delivery == McpWorkDelivery::Uncertain {
            return Err(fail(FailureCode::EffectUncertain));
        }
        decode(&record, &self.limits)
    }
}
fn decode(
    record: &McpStoredInteraction,
    limits: &Limits,
) -> Result<McpServiceOutcome, RuntimeFailure> {
    let reply = JsonDocument::new(
        record
            .reply
            .as_deref()
            .ok_or_else(|| fail(FailureCode::EffectUncertain))?,
        limits,
    )
    .map_err(|_| fail(FailureCode::McpProtocol))?;
    let fields = as_map(reply.value())?;
    let result = if let Some(result) = fields.get("result") {
        Ok(JsonDocument::from_value(result, limits).map_err(|_| fail(FailureCode::McpProtocol))?)
    } else {
        Err(McpRpcError::new(
            JsonDocument::from_value(get(fields, "error")?, limits)
                .map_err(|_| fail(FailureCode::McpProtocol))?,
        )?)
    };
    Ok(McpServiceOutcome {
        result,
        delivery: record.work_delivery,
        labels: record.labels.clone(),
        usage: record.usage.as_ref().map(usage).transpose()?,
    })
}
fn usage(value: &Value) -> Result<McpUsage, RuntimeFailure> {
    let fields = as_map(value)?;
    let counter = |value: &Value| match value {
        Value::Null => Ok(None),
        Value::Integer(n) if *n >= 0 => Ok(Some(*n as u64)),
        _ => Err(fail(FailureCode::McpProtocol)),
    };
    Ok(McpUsage {
        input_tokens: counter(get(fields, "input_tokens")?)?,
        output_tokens: counter(get(fields, "output_tokens")?)?,
        cost: match get(fields, "cost")? {
            Value::Null => None,
            value => {
                let cost = as_map(value)?;
                Some(McpCost {
                    amount: counter(get(cost, "amount")?)?
                        .ok_or_else(|| fail(FailureCode::McpProtocol))?,
                    unit: crate::mcp::as_text(get(cost, "unit")?)?.into(),
                })
            }
        },
        provider_operation_id: match get(fields, "provider_operation_id")? {
            Value::Null => None,
            value => Some(crate::mcp::as_text(value)?.into()),
        },
    })
}
