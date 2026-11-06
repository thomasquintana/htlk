//! Long-lived transport ownership, independent of caller execution reactors.
use super::*;
use crate::mcp::{McpAttemptContext, McpDelivery, McpEndpoint, McpRequest};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

pub(super) mod wire;
use wire::Wire;

/// Correlated protocol result or protected protocol error, separate from local I/O
/// failure and conservative delivery evidence.
pub struct McpClientReply {
    /// Independent durable task acceptance registration, when this was a task
    /// creation. Failure never replaces correlated delivery or protected bytes.
    pub task_registration: Option<Result<(), RuntimeFailure>>,
    /// Exact correlated outgoing RPC identity, available for addressed error
    /// evidence inspection. Absent when no correlated response was admitted.
    pub request_id: Option<McpRequestId>,
    /// Whether sending/completion was actually observed.
    pub delivery: McpDelivery,
    /// Local transport/validation failure or complete correlated result/error.
    pub result: Result<Result<JsonDocument, McpRpcError>, RuntimeFailure>,
    /// Exact result-object bytes before JSON normalization; protocol errors retain
    /// their own original bytes in `McpRpcError`. Absent for local failures.
    pub raw_result: Option<Vec<u8>>,
    /// Independent URL-required admission outcome. A failure here never replaces
    /// the original correlated RPC error or changes its delivery classification.
    pub url_interactions: Option<Result<Vec<Digest>, RuntimeFailure>>,
    // Holds response capacity until the caller consumes or drops this reply.
    _reservation: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl fmt::Debug for McpClientReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpClientReply")
            .field("delivery", &self.delivery)
            .field("local_failure", &self.result.as_ref().err())
            .finish_non_exhaustive()
    }
}
impl McpClientReply {
    pub(super) fn failed(delivery: McpDelivery, code: FailureCode) -> Self {
        Self::error(delivery, fail(code))
    }
    fn error(delivery: McpDelivery, error: RuntimeFailure) -> Self {
        Self {
            task_registration: None,
            request_id: None,
            delivery,
            result: Err(error),
            raw_result: None,
            url_interactions: None,
            _reservation: None,
        }
    }
    fn correlated(result: Result<JsonDocument, McpRpcError>, raw_result: Option<Vec<u8>>) -> Self {
        Self {
            task_registration: None,
            request_id: None,
            delivery: McpDelivery::ResponseReceived,
            result: Ok(result),
            raw_result,
            url_interactions: None,
            _reservation: None,
        }
    }
    fn reserved(mut self, reservation: tokio::sync::OwnedSemaphorePermit) -> Self {
        self._reservation = Some(reservation);
        self
    }
}

type Slot = Arc<tokio::sync::OnceCell<Result<Arc<Wire>, RuntimeFailure>>>;
type SessionKey = (Digest, i64, Digest);
#[derive(Default)]
struct RegistryState {
    sessions: BTreeMap<SessionKey, SessionEntry>,
    // Bounded, non-evicting high-water marks outlive transport reclamation.
    generations: BTreeMap<Digest, i64>,
}
#[derive(Clone)]
struct SessionEntry {
    slot: Slot,
    stop: Arc<McpCancellation>,
    lifetime: Arc<McpCancellation>,
    run_owned: bool,
}
type Registry = Arc<Mutex<RegistryState>>;

enum Command {
    Open {
        owner: McpOwner,
        server: ServerIdentity,
        endpoint: McpEndpoint,
        limits: Limits,
        lifetime: Arc<McpCancellation>,
        reply: oneshot::Sender<Result<Arc<Wire>, RuntimeFailure>>,
    },
    Request {
        queued_at: std::time::Instant,
        parent: Option<Arc<McpHostWorkReceipt>>,
        task_key: Option<Digest>,
        wire: Arc<Wire>,
        request: McpRequest,
        context: Option<McpAttemptContext>,
        reply: oneshot::Sender<McpClientReply>,
        reservation: tokio::sync::OwnedSemaphorePermit,
    },
}

/// Explicit native client owner. A dedicated I/O reactor retains sessions across
/// short-lived caller reactors. Use `shutdown` to acknowledge bounded cleanup.
pub struct NativeMcpClient {
    sender: mpsc::Sender<Command>,
    timeout_ms: u64,
    stop: Arc<McpCancellation>,
    finished: tokio::sync::watch::Receiver<bool>,
    authentication: auth::AuthenticationMonitor,
}
impl NativeMcpClient {
    /// Starts a bounded transport owner. Host-owned service handlers require
    /// persistence and authorization, and execute on separate bounded workers.
    /// Prepares pinned offline protocol validators before transport startup and
    /// request deadlines, including clients without callback handlers.
    ///
    /// # Errors
    /// Returns unsupported service configuration or inability to launch the I/O owner.
    pub fn new(config: McpClientConfig) -> Result<Self, RuntimeFailure> {
        if let Some(settings) = config.task_receiver {
            McpTaskReceiver::new(config.clone(), settings)?;
        }
        // A declaration alone must never advertise a nonfunctional service.
        if !config.handlers.is_empty()
            && (config.persistence.is_none() || config.authorizer.is_none())
        {
            return Err(fail(FailureCode::Profile));
        }
        // Prepare pinned notification validators before any transport deadline or
        // idle reception begins, including catalog-only clients without handlers.
        htlk_analyzer::mcp_schema_validators().map_err(|_| fail(FailureCode::Profile))?;
        let timeout_ms = config.limits.timeout_ms;
        let authentication = config.authentication.clone();
        let (sender, mut receiver) = mpsc::channel(config.limits.pending_requests);
        let dispatcher = sender.clone();
        let stop = Arc::new(McpCancellation::new());
        let executors = workers::Executors::launch(&config, stop.clone())?;
        let owner_stop = stop.clone();
        let (finished, completion) = tokio::sync::watch::channel(false);
        std::thread::Builder::new().name("htlk-mcp-client".into()).spawn(move || {
            actix::System::new().block_on(async move {
                let config = Arc::new(config);
                let registry = Registry::default();
                let stop = owner_stop;
                let epochs = Arc::new(AtomicU64::new(1));
                let mut jobs = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        biased;
                        _ = stop.cancelled() => break,
                        _ = jobs.join_next(), if !jobs.is_empty() => {},
                        command = receiver.recv() => match command {
                            Some(Command::Open { owner, server, endpoint, limits, lifetime, reply }) => {
                                if jobs.len() >= config.limits.pending_requests {
                                    let _ = reply.send(Err(fail(FailureCode::McpTransport)));
                                    continue;
                                }
                                let registry = registry.clone();
                                let config = config.clone();
                                let stop = stop.clone();
                                let epochs = epochs.clone();
                                 let executors = executors.clone();
                                 let dispatcher = dispatcher.clone();
                                jobs.spawn(async move {
                                     let result = open(registry, config, executors, stop, epochs, owner, server, endpoint, limits, lifetime, dispatcher).await;
                                    let _ = reply.send(result);
                                });
                            }
                             Some(Command::Request { wire, mut request, context, task_key, reply, reservation, parent, queued_at }) => {
                                if jobs.len() >= config.limits.pending_requests {
                                    let _ = reply.send(McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpTransport));
                                    continue;
                                }
                                 jobs.spawn(async move {
                                     let elapsed = queued_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
                                     request.timeout_ms = request.timeout_ms.saturating_sub(elapsed);
                                     if request.timeout_ms == 0 {
                                         let _ = reply.send(McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpTimeout));
                                         return;
                                     }
                                     let outcome = wire.invoke_parent(request, context.clone(), task_key, parent.clone()).await.reserved(reservation);
                                     if let Some(parent) = parent
                                         && parent.retain_native_outcome(&outcome).await.is_err()
                                     {
                                         wire.stop.cancel();
                                     }
                                     if let Err(outcome) = reply.send(outcome) {
                                         // Persist observations even when the runtime's
                                         // bounded caller has already stopped awaiting us.
                                         wire.retain_abandoned_outcome(context.as_ref(), &outcome).await;
                                     }
                                });
                            }
                            None => break,
                        },
                    }
                }
                receiver.close();
                stop.cancel();
                    let wires: Vec<_> = registry.lock().map(|entries| entries.sessions.values()
                        .filter_map(|entry| entry.slot.get().and_then(|v| v.as_ref().ok()).cloned())
                    .collect()).unwrap_or_default();
                for wire in wires { jobs.spawn(async move { wire.close().await; }); }
                let _ = tokio::time::timeout(Duration::from_secs(1), async {
                    while jobs.join_next().await.is_some() {}
                }).await;
                jobs.abort_all();
                finished.send_replace(true);
            });
        }).map_err(|_| fail(FailureCode::McpTransport))?;
        Ok(Self {
            sender,
            timeout_ms,
            stop,
            finished: completion,
            authentication,
        })
    }

    /// Opens or borrows the exact owner/generation/server session. Higher owner
    /// generations cancel older local sessions; lower generations are rejected.
    /// Endpoints are trusted host data, never values read from a workflow.
    ///
    /// # Errors
    /// Returns configuration, cancellation, negotiation, identity, or transport failure.
    pub async fn open(
        &self,
        owner: McpOwner,
        server: ServerIdentity,
        endpoint: McpEndpoint,
        limits: Limits,
        lifetime: Arc<McpCancellation>,
    ) -> Result<McpClientSession, RuntimeFailure> {
        limits.validate().map_err(|_| fail(FailureCode::Profile))?;
        let (reply, receive) = oneshot::channel();
        tokio::time::timeout(Duration::from_millis(self.timeout_ms), async {
            self.sender
                .send(Command::Open {
                    owner,
                    server,
                    endpoint,
                    limits,
                    lifetime,
                    reply,
                })
                .await
                .map_err(|_| fail(FailureCode::McpTransport))?;
            let wire = receive
                .await
                .map_err(|_| fail(FailureCode::McpTransport))??;
            Ok(McpClientSession {
                sender: self.sender.clone(),
                wire,
            })
        })
        .await
        .map_err(|_| fail(FailureCode::McpTimeout))?
    }

    /// Permanently stops the owner and acknowledges bounded cleanup (two seconds).
    ///
    /// # Errors
    /// Returns an owner shutdown timeout or lost acknowledgement.
    pub async fn shutdown(&self) -> Result<(), RuntimeFailure> {
        self.stop.cancel();
        let mut finished = self.finished.clone();
        tokio::time::timeout(Duration::from_secs(2), finished.wait_for(|done| *done))
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
            .map(|_| ())
            .map_err(|_| fail(FailureCode::McpTransport))
    }
    /// Watches protected credential failures and HTTP challenges, including those
    /// preventing initialization. Only the latest concern is retained; sequence
    /// gaps expose loss. Acquiring credentials never automatically replays work.
    pub fn authentication_concerns(&self) -> auth::McpAuthenticationConcerns {
        self.authentication.subscribe()
    }
}
impl Drop for NativeMcpClient {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// A checked initialized session handle. Clones share this exact epoch; closing any
/// clone closes the session. No clone can initialize a replacement under this handle.
#[derive(Clone)]
pub struct McpClientSession {
    sender: mpsc::Sender<Command>,
    wire: Arc<Wire>,
}
impl McpClientSession {
    /// Announces changed host roots when a roots provider and its change service
    /// were installed before initialization. The notification performs no roots
    /// read and no filesystem access; the peer can explicitly request roots/list.
    ///
    /// # Errors
    /// Returns unsupported configuration, cancellation, capacity or owner failure.
    pub async fn roots_changed(
        &self,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpNotificationReply, RuntimeFailure> {
        self.wire.roots_changed(cancellation).await
    }
    pub(crate) fn persistence(&self) -> Option<&McpClientPersistence> {
        self.wire.persistence()
    }
    pub(crate) fn fresh_task_key(&self) -> Digest {
        self.wire.fresh_task_key()
    }
    /// Immutable representation limits applied to this initialized session.
    pub fn representation_limits(&self) -> &Limits {
        self.wire.representation_limits()
    }
    /// Immutable client resource limits governing this session.
    pub fn client_limits(&self) -> &McpClientLimits {
        self.wire.client_limits()
    }
    /// Immutable session owner and generation.
    pub fn owner(&self) -> &McpOwner {
        &self.wire.owner
    }
    /// Transport epoch, distinct from owner generation.
    pub fn epoch(&self) -> u64 {
        self.wire.epoch
    }
    /// Durable store-scoped session identity, absent for ephemeral clients.
    pub fn durable_identity(&self) -> Option<Digest> {
        self.wire.stored_session.as_ref().map(|session| session.id)
    }
    /// Session termination reason, observable independently of the event queue.
    pub fn ended(&self) -> Option<McpSessionEnd> {
        *self.wire.ended.borrow()
    }
    /// Whether idle server events are available. HTTP starts unknown until the GET
    /// result is observed; 405 sets false while ordinary POST operations remain usable.
    pub fn server_events_available(&self) -> Option<bool> {
        *self.wire.server_events.borrow()
    }
    /// Exact initialized compound server identity.
    pub fn server(&self) -> &ServerIdentity {
        &self.wire.server
    }
    /// Exact server initialization result, including negotiated capabilities.
    pub fn initialization(&self) -> &JsonDocument {
        self.wire
            .initialization
            .get()
            .expect("only initialized sessions are public")
    }
    /// Sends a bounded raw protocol operation on the persistent I/O owner. This is
    /// a trusted host API; runtime graph operations separately admit, journal and
    /// authorize dispatch. Caller cancellation never proves remote work was undone.
    pub async fn request(
        &self,
        method: &str,
        params: JsonDocument,
        cancellation: Arc<McpCancellation>,
    ) -> McpClientReply {
        self.invoke(
            self.wire
                .default_request(method, params.value().clone(), cancellation),
        )
        .await
    }

    /// Sends a request with explicit per-operation bounds and optional verified
    /// protocol deduplication key. Per-call limits never shrink later requests.
    pub async fn invoke(&self, request: McpRequest) -> McpClientReply {
        self.invoke_context(request, None).await
    }
    pub(crate) async fn invoke_context(
        &self,
        request: McpRequest,
        context: Option<McpAttemptContext>,
    ) -> McpClientReply {
        self.invoke_task_context(request, context, None).await
    }
    pub(crate) async fn invoke_task_context(
        &self,
        request: McpRequest,
        context: Option<McpAttemptContext>,
        task_key: Option<Digest>,
    ) -> McpClientReply {
        self.invoke_parent(request, context, task_key, None).await
    }
    pub(super) async fn invoke_parent(
        &self,
        request: McpRequest,
        context: Option<McpAttemptContext>,
        task_key: Option<Digest>,
        parent: Option<Arc<McpHostWorkReceipt>>,
    ) -> McpClientReply {
        if [request.timeout_ms, request.cancellation_grace_ms]
            .into_iter()
            .any(|ms| crate::mcp::valid_timeout(ms).is_err())
        {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Profile);
        }
        let (reply, receive) = oneshot::channel();
        let cancellation = request.cancellation.clone();
        if cancellation.is_cancelled() || self.wire.stop.is_cancelled() {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Cancelled);
        }
        let reservation = match self.wire.reserve_request(&request) {
            Ok(reservation) => reservation,
            Err(error) => {
                return McpClientReply::error(McpDelivery::NotSent, error);
            }
        };
        let send = self.sender.send(Command::Request {
            queued_at: std::time::Instant::now(),
            parent,
            task_key,
            wire: self.wire.clone(),
            request,
            context,
            reply,
            reservation,
        });
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Cancelled),
            sent = send => if sent.is_err() { return McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpTransport); }
        }
        receive.await.unwrap_or_else(|_| {
            McpClientReply::failed(McpDelivery::PossiblySent, FailureCode::McpTransport)
        })
    }
    /// Closes this exact epoch with bounded transport cleanup.
    ///
    /// # Errors
    /// Returns owner unavailability or cleanup timeout.
    pub async fn close(&self) -> Result<(), RuntimeFailure> {
        self.wire.stop.cancel();
        let mut closed = self.wire.closed.subscribe();
        tokio::time::timeout(Duration::from_secs(2), closed.wait_for(|done| *done))
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
            .map(|_| ())
            .map_err(|_| fail(FailureCode::McpTransport))
    }
}

#[allow(clippy::too_many_arguments)]
async fn open(
    registry: Registry,
    config: Arc<McpClientConfig>,
    executors: workers::Executors,
    stop: Arc<McpCancellation>,
    epochs: Arc<AtomicU64>,
    owner: McpOwner,
    server: ServerIdentity,
    endpoint: McpEndpoint,
    limits: Limits,
    lifetime: Arc<McpCancellation>,
    dispatcher: mpsc::Sender<Command>,
) -> Result<Arc<Wire>, RuntimeFailure> {
    if lifetime.is_cancelled() || stop.is_cancelled() {
        return Err(fail(FailureCode::Cancelled));
    }
    let key = (
        owner.key(),
        owner.generation(),
        server
            .digest(&limits)
            .map_err(|_| fail(FailureCode::McpIdentity))?,
    );
    let (entry, created) = {
        let mut registry = registry.lock().map_err(|_| fail(FailureCode::Invariant))?;
        if registry
            .generations
            .get(&owner.key())
            .is_some_and(|generation| *generation > owner.generation())
        {
            return Err(fail(FailureCode::Fenced));
        }
        if !registry.generations.contains_key(&owner.key())
            && registry.generations.len() >= config.limits.owner_identities
        {
            return Err(fail(FailureCode::Budget));
        }
        registry.generations.insert(owner.key(), owner.generation());
        for ((id, generation, _), entry) in &registry.sessions {
            if *id == owner.key() && *generation < owner.generation() {
                entry.stop.cancel();
                if entry.run_owned {
                    entry.lifetime.cancel();
                }
            }
        }
        // Active-run failure is sticky even after transport cleanup. Once its
        // original lifetime ends, only the small non-evicting fence remains.
        registry.sessions.retain(|_, entry| {
            !entry.lifetime.is_cancelled() && (entry.run_owned || !entry.stop.is_cancelled())
        });
        if !registry.sessions.contains_key(&key)
            && registry.sessions.len() >= config.limits.pending_requests
        {
            return Err(fail(FailureCode::McpTransport));
        }
        let created = !registry.sessions.contains_key(&key);
        let entry = registry
            .sessions
            .entry(key)
            .or_insert_with(|| {
                let mut token = McpCancellation::linked(&stop);
                token.parents.push(lifetime.state.clone());
                token.parents.extend(lifetime.parents.iter().cloned());
                SessionEntry {
                    slot: Arc::new(tokio::sync::OnceCell::new()),
                    stop: Arc::new(token),
                    lifetime: lifetime.clone(),
                    run_owned: matches!(owner.scope(), McpOwnerScope::Run(_)),
                }
            })
            .clone();
        (entry, created)
    };
    let SessionEntry {
        slot, stop: token, ..
    } = entry.clone();
    if created {
        let registry = registry.clone();
        let shutdown = stop.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown.cancelled() => {},
                _ = entry.lifetime.cancelled() => {},
                _ = entry.stop.cancelled(), if !entry.run_owned => {},
            }
            if let Ok(mut registry) = registry.lock()
                && registry
                    .sessions
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(&current.slot, &entry.slot))
            {
                registry.sessions.remove(&key);
            }
        });
    }
    let result = slot
        .get_or_init(|| async {
            let stored_session = if let Some(persistence) = &config.persistence {
                Some(
                    persistence
                        .store
                        .open_mcp_epoch(
                            &owner,
                            server
                                .digest(&limits)
                                .map_err(|_| fail(FailureCode::McpIdentity))?,
                            persistence.clock.now_ms()?,
                        )
                        .await
                        .map_err(|e| fail(e.code()))?,
                )
            } else {
                None
            };
            let epoch = stored_session.as_ref().map_or_else(
                || epochs.fetch_add(1, Ordering::Relaxed),
                |session| session.epoch,
            );
            let started = Wire::start(
                owner.clone(),
                server,
                epoch,
                endpoint,
                config.clone(),
                limits,
                token.clone(),
                stored_session.clone(),
                executors,
                dispatcher,
            )
            .await;
            let wire = match started {
                Ok(wire) => wire,
                Err(error) => {
                    if let (Some(persistence), Some(session)) =
                        (&config.persistence, stored_session)
                        && let Ok(now) = persistence.clock.now_ms()
                    {
                        let _ = persistence
                            .store
                            .close_mcp_epoch(&owner, session.id, now)
                            .await;
                    }
                    return Err(error);
                }
            };
            match wire.initialize().await {
                Ok(()) => Ok(wire),
                Err(error) => {
                    wire.close().await;
                    Err(error)
                }
            }
        })
        .await
        .clone();
    if token.is_cancelled() && result.is_ok() {
        return Err(fail(FailureCode::Cancelled));
    }
    result
}
