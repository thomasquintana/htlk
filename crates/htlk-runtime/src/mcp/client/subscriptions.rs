//! Durable host subscription commands. This trusted embedding-host surface uses
//! real owner leases and explicit restoration grants, never notification-triggered
//! reads or automatic reconnect replay. Frontends must authenticate their callers
//! before granting access to a host-owned session or its protected URI inventory.
use super::{
    McpClientPersistence, McpClientSession, McpOwner, McpOwnerScope, McpServiceError,
    McpServiceLimits,
};
use crate::{
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, McpDelivery},
    store::{
        McpStoredSubscription, McpSubscriptionAction, McpSubscriptionClaim, McpSubscriptionIntent,
        McpSubscriptionLimits, McpSubscriptionOutcome,
    },
};
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Validated exact resource URI and local consumer key. Equal keys identify the
/// same desired consumer; cloning a handle never adds a reference or performs I/O.
#[derive(Clone)]
pub struct McpSubscriptionConsumer {
    uri: String,
    subscriber: String,
}
impl McpSubscriptionConsumer {
    /// Validates nonempty bounded URI/subscriber text without reading its content.
    ///
    /// # Errors
    /// Returns invalid local input for empty or oversized representation.
    pub fn new(uri: String, subscriber: String, limits: &Limits) -> Result<Self, RuntimeFailure> {
        if uri.is_empty()
            || subscriber.is_empty()
            || uri.len() > limits.max_document_bytes / 8
            || subscriber.len() > limits.max_document_bytes / 8
        {
            return Err(RuntimeFailure::new(FailureCode::Input));
        }
        JsonDocument::from_value(
            &Value::Array(vec![
                Value::Text(uri.clone()),
                Value::Text(subscriber.clone()),
            ]),
            limits,
        )
        .map_err(|_| RuntimeFailure::new(FailureCode::Input))?;
        Ok(Self { uri, subscriber })
    }
    /// Protected exact resource URI.
    pub fn uri(&self) -> &str {
        &self.uri
    }
    /// Exact host-selected subscriber key; it is not a server request identity.
    pub fn subscriber(&self) -> &str {
        &self.subscriber
    }
}

/// Result of a durable desired-state command. Desired state survives every RPC
/// outcome. A non-converged record explicitly reports uncertainty, gaps, or a
/// bounded follow-up deferred by the transition/deadline ceiling.
pub struct McpSubscriptionReport {
    record: McpStoredSubscription,
    attempted_rpcs: usize,
    failure: Option<McpServiceError>,
}
impl McpSubscriptionReport {
    /// Durable observation after the command; values remain owner-protected.
    pub fn record(&self) -> &McpStoredSubscription {
        &self.record
    }
    /// Number of RPC attempts made by this command, including known not-sent calls.
    pub fn attempted_rpcs(&self) -> usize {
        self.attempted_rpcs
    }
    /// Delivery-aware RPC failure, absent for successful ACKs and local-only desire
    /// changes. Absence does not prove convergence; always inspect the record.
    pub fn failure(&self) -> Option<&McpServiceError> {
        self.failure.as_ref()
    }
}

/// Durable subscription manager shared by interactive and headless trusted hosts.
/// Separate instances coordinate through SQLite, not a process-local mutex. Only
/// host ownership is supported; these methods cannot escape run-accounted work.
///
/// Use the persistence service that opened the supplied durable host session:
///
/// ```no_run
/// use htlk_executable::cbor::Limits;
/// use htlk_runtime::{
///     mcp::{McpCancellation, client::{
///         McpClientPersistence, McpClientSession, McpServiceLimits,
///         McpSubscriptionConsumer, McpSubscriptionHost,
///     }},
///     store::McpSubscriptionLimits,
/// };
/// use std::sync::Arc;
/// # async fn monitor(session: &McpClientSession, persistence: McpClientPersistence)
/// #     -> Result<(), Box<dyn std::error::Error>> {
/// let host = McpSubscriptionHost::new(
///     persistence,
///     McpSubscriptionLimits::new(128, 512, 4096, 4 * 1024 * 1024, 4)?,
/// );
/// let consumer = McpSubscriptionConsumer::new(
///     "context://status".into(), "status-panel".into(), &Limits::default(),
/// )?;
/// let bounds = McpServiceLimits::new(4096, 2000)?;
/// let cancellation = Arc::new(McpCancellation::new());
/// let subscribed = host.subscribe(session, &consumer, bounds, cancellation.clone()).await?;
/// println!("{:?}", subscribed.record().state());
/// // Release this consumer explicitly when monitoring ends. Notifications alone
/// // never read the resource or modify the workflow.
/// let released = host.unsubscribe(session, &consumer, bounds, cancellation).await?;
/// println!("converged: {}", released.record().converged());
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct McpSubscriptionHost {
    persistence: McpClientPersistence,
    limits: McpSubscriptionLimits,
}
impl McpSubscriptionHost {
    /// Links the same trusted store/clock used to open native durable sessions.
    /// Construction performs no reads, subscription restoration, or remote RPCs.
    pub fn new(persistence: McpClientPersistence, limits: McpSubscriptionLimits) -> Self {
        Self {
            persistence,
            limits,
        }
    }
    /// Persists this consumer's desired presence before sending any remote RPC.
    /// The first consumer subscribes; duplicate/additional consumers share its
    /// acknowledged state or in-flight uncertainty without duplicate requests.
    ///
    /// # Errors
    /// Returns missing durable/host ownership, stale authority, quotas or storage
    /// failure. RPC errors are retained in the returned report, not discarded.
    pub async fn subscribe(
        &self,
        session: &McpClientSession,
        consumer: &McpSubscriptionConsumer,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpSubscriptionReport, RuntimeFailure> {
        self.desire(session, consumer, true, limits, cancellation)
            .await
    }
    /// Persists release of one exact consumer. Only releasing the last desired
    /// consumer sends unsubscribe. Updates already in flight remain ordinary host
    /// events even after the unsubscribe acknowledgement; no content is fetched.
    ///
    /// # Errors
    /// Returns the same durable admission errors as subscribe.
    pub async fn unsubscribe(
        &self,
        session: &McpClientSession,
        consumer: &McpSubscriptionConsumer,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpSubscriptionReport, RuntimeFailure> {
        self.desire(session, consumer, false, limits, cancellation)
            .await
    }
    /// Explicit trusted restoration authorization, bound to an exact previous
    /// epoch and selected subscription identity. It exposes a durable gap and
    /// creates a new subscribe only when consumers still desire monitoring.
    /// A live same-generation old epoch cannot be silently replaced. Uncertainty
    /// in the old epoch remains historical evidence, never a claimed successful ACK.
    ///
    /// # Errors
    /// Returns stale restoration grant/owner/epoch, quotas or storage failure.
    pub async fn restore(
        &self,
        session: &McpClientSession,
        id: Digest,
        previous_session: Digest,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpSubscriptionReport, RuntimeFailure> {
        let started = Instant::now();
        let durable = self.session(session)?;
        let claim = self
            .persistence
            .store
            .restore_mcp_subscription(
                session.owner(),
                id,
                previous_session,
                durable,
                self.limits,
                self.now()?,
            )
            .await
            .map_err(store_failure)?;
        self.drive(session, claim, limits, cancellation, started)
            .await
    }
    /// Explicitly continues a known active/inactive state toward current desire.
    /// This never retries an uncertain claim and never restores another epoch.
    /// It is useful after the bounded transition allowance was exhausted by churn.
    ///
    /// # Errors
    /// Returns stale/foreign epoch, missing record, quotas or storage failure.
    pub async fn reconcile(
        &self,
        session: &McpClientSession,
        id: Digest,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpSubscriptionReport, RuntimeFailure> {
        let started = Instant::now();
        let durable = self.session(session)?;
        let claim = self
            .persistence
            .store
            .claim_mcp_subscription(session.owner(), durable, id, self.limits, self.now()?)
            .await
            .map_err(store_failure)?;
        self.drive(session, claim, limits, cancellation, started)
            .await
    }
    /// Inspects a selected protected owner/server/URI observation. Closed epochs
    /// and ownership takeovers are explicitly reported as gaps. No RPC is sent.
    ///
    /// # Errors
    /// Returns stale ownership, invalid selection or storage failure.
    pub async fn inspect(
        &self,
        owner: &McpOwner,
        server: Digest,
        uri: &str,
    ) -> Result<Option<McpStoredSubscription>, RuntimeFailure> {
        self.persistence
            .store
            .mcp_subscription(owner, server, uri, self.now()?)
            .await
            .map_err(store_failure)
    }
    /// Lists a bounded protected page, continuing after the final returned ID.
    /// Listing is passive: it never restores subscriptions or refreshes catalogs.
    ///
    /// # Errors
    /// Returns invalid paging, stale owner or storage failure.
    pub async fn list(
        &self,
        owner: &McpOwner,
        after: Option<Digest>,
        maximum: usize,
    ) -> Result<Vec<McpStoredSubscription>, RuntimeFailure> {
        self.persistence
            .store
            .mcp_subscriptions(owner, after, maximum, self.now()?)
            .await
            .map_err(store_failure)
    }
    /// Durably closes this exact epoch before bounded local transport cleanup.
    /// Desired consumers remain persisted; later inspection exposes a delivery gap.
    /// Reopening a transport alone never restores monitoring.
    ///
    /// # Errors
    /// Returns stale ownership/epoch, storage failure or transport cleanup timeout.
    pub async fn disconnect(&self, session: &McpClientSession) -> Result<(), RuntimeFailure> {
        let durable = self.session(session)?;
        self.persistence
            .store
            .close_mcp_epoch(session.owner(), durable, self.now()?)
            .await
            .map_err(store_failure)?;
        session.close().await
    }

    async fn desire(
        &self,
        session: &McpClientSession,
        consumer: &McpSubscriptionConsumer,
        desired: bool,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpSubscriptionReport, RuntimeFailure> {
        let started = Instant::now();
        let durable = self.session(session)?;
        let server = session
            .server()
            .digest(&self.persistence.store.config().limits)
            .map_err(|_| RuntimeFailure::new(FailureCode::McpIdentity))?;
        let intent = McpSubscriptionIntent::new(
            durable,
            server,
            consumer.uri.clone(),
            consumer.subscriber.clone(),
            desired,
            &self.persistence.store.config().limits,
        )
        .map_err(store_failure)?;
        let claim = self
            .persistence
            .store
            .set_mcp_subscription_intent(session.owner(), intent, self.limits, self.now()?)
            .await
            .map_err(store_failure)?;
        self.drive(session, claim, limits, cancellation, started)
            .await
    }
    fn now(&self) -> Result<i64, RuntimeFailure> {
        self.persistence.clock.now_ms()
    }
    fn session(&self, session: &McpClientSession) -> Result<Digest, RuntimeFailure> {
        if !matches!(session.owner().scope(), McpOwnerScope::Host(_)) {
            return Err(RuntimeFailure::new(FailureCode::Input));
        }
        session
            .durable_identity()
            .ok_or_else(|| RuntimeFailure::new(FailureCode::Profile))
    }
    async fn drive(
        &self,
        session: &McpClientSession,
        mut claim: McpSubscriptionClaim,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
        started: Instant,
    ) -> Result<McpSubscriptionReport, RuntimeFailure> {
        let mut attempted_rpcs = 0;
        let timeout =
            Duration::from_millis(limits.timeout_ms().min(session.client_limits().timeout_ms));
        loop {
            let (record, operation) = match claim {
                McpSubscriptionClaim::Retained(record) => {
                    return Ok(McpSubscriptionReport {
                        record,
                        attempted_rpcs,
                        failure: None,
                    });
                }
                McpSubscriptionClaim::Dispatch { record, operation } => (record, operation),
            };
            let remaining = timeout
                .saturating_sub(started.elapsed())
                .as_millis()
                .min(u64::MAX as u128) as u64;
            let result = if cancellation.is_cancelled() || remaining == 0 {
                Err(McpServiceError::Local {
                    delivery: McpDelivery::NotSent,
                    failure: RuntimeFailure::new(if cancellation.is_cancelled() {
                        FailureCode::Cancelled
                    } else {
                        FailureCode::McpTimeout
                    }),
                })
            } else {
                attempted_rpcs += 1;
                let bounded = McpServiceLimits::new(limits.response_bytes(), remaining)
                    .map_err(|_| RuntimeFailure::new(FailureCode::Input))?;
                match operation.action() {
                    McpSubscriptionAction::Subscribe => {
                        session
                            .subscribe_resource(record.uri(), bounded, cancellation.clone())
                            .await
                    }
                    McpSubscriptionAction::Unsubscribe => {
                        session
                            .unsubscribe_resource(record.uri(), bounded, cancellation.clone())
                            .await
                    }
                }
            };
            let outcome = match &result {
                Ok(_) => McpSubscriptionOutcome::Acknowledged,
                Err(error) if error.delivery() == McpDelivery::NotSent => {
                    McpSubscriptionOutcome::NotSent
                }
                Err(_) => McpSubscriptionOutcome::Uncertain,
            };
            let record = self
                .persistence
                .store
                .settle_mcp_subscription(session.owner(), &operation, outcome, self.now()?)
                .await
                .map_err(store_failure)?;
            let failure = result.err();
            if failure.is_some()
                || record.converged()
                || attempted_rpcs >= self.limits.transitions()
                || started.elapsed() >= timeout
            {
                return Ok(McpSubscriptionReport {
                    record,
                    attempted_rpcs,
                    failure,
                });
            }
            claim = self
                .persistence
                .store
                .claim_mcp_subscription(
                    session.owner(),
                    operation.session(),
                    record.id(),
                    self.limits,
                    self.now()?,
                )
                .await
                .map_err(store_failure)?;
        }
    }
}
fn store_failure(error: crate::store::StoreError) -> RuntimeFailure {
    RuntimeFailure::new(error.code())
}
