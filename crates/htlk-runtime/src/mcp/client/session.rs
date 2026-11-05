//! Long-lived transport ownership, independent of caller execution reactors.
use super::*;
use crate::mcp::{McpDelivery, McpEndpoint, McpRequest};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

mod wire;
use wire::Wire;

/// Correlated protocol result or protected protocol error, separate from local I/O
/// failure and conservative delivery evidence.
pub struct McpClientReply {
    /// Whether sending/completion was actually observed.
    pub delivery: McpDelivery,
    /// Local transport/validation failure or complete correlated result/error.
    pub result: Result<Result<JsonDocument, McpRpcError>, RuntimeFailure>,
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
    fn failed(delivery: McpDelivery, code: FailureCode) -> Self {
        Self {
            delivery,
            result: Err(fail(code)),
        }
    }
}

type Slot = Arc<tokio::sync::OnceCell<Result<Arc<Wire>, RuntimeFailure>>>;
type SessionKey = (Digest, i64, Digest);
type Registry =
    Arc<Mutex<BTreeMap<SessionKey, (Slot, Arc<McpCancellation>, Arc<McpCancellation>)>>>;

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
        wire: Arc<Wire>,
        request: McpRequest,
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
}
impl NativeMcpClient {
    /// Starts a bounded transport owner. Effectful service handlers require the
    /// durable interaction implementation and are rejected until it is installed.
    ///
    /// # Errors
    /// Returns unsupported service configuration or inability to launch the I/O owner.
    pub fn new(config: McpClientConfig) -> Result<Self, RuntimeFailure> {
        // A declaration alone must never advertise a nonfunctional service.
        if !config.handlers.is_empty() {
            return Err(fail(FailureCode::Profile));
        }
        let timeout_ms = config.limits.timeout_ms;
        let (sender, mut receiver) = mpsc::channel(config.limits.pending_requests);
        let stop = Arc::new(McpCancellation::new());
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
                                jobs.spawn(async move {
                                    let result = open(registry, config, stop, epochs, owner, server, endpoint, limits, lifetime).await;
                                    let _ = reply.send(result);
                                });
                            }
                            Some(Command::Request { wire, request, reply, reservation }) => {
                                if jobs.len() >= config.limits.pending_requests {
                                    let _ = reply.send(McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpTransport));
                                    continue;
                                }
                                jobs.spawn(async move {
                                    let _reservation = reservation;
                                    let _ = reply.send(wire.invoke(request).await);
                                });
                            }
                            None => break,
                        },
                    }
                }
                receiver.close();
                stop.cancel();
                let wires: Vec<_> = registry.lock().map(|entries| entries.values()
                    .filter_map(|(slot, _, _)| slot.get().and_then(|v| v.as_ref().ok()).cloned())
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
        let (reply, receive) = oneshot::channel();
        let cancellation = request.cancellation.clone();
        if cancellation.is_cancelled() || self.wire.stop.is_cancelled() {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Cancelled);
        }
        let reservation = match self.wire.reserve_request(&request) {
            Ok(reservation) => reservation,
            Err(error) => {
                return McpClientReply {
                    delivery: McpDelivery::NotSent,
                    result: Err(error),
                };
            }
        };
        let send = self.sender.send(Command::Request {
            wire: self.wire.clone(),
            request,
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
    stop: Arc<McpCancellation>,
    epochs: Arc<AtomicU64>,
    owner: McpOwner,
    server: ServerIdentity,
    endpoint: McpEndpoint,
    limits: Limits,
    lifetime: Arc<McpCancellation>,
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
    let (slot, token, _) = {
        let mut entries = registry.lock().map_err(|_| fail(FailureCode::Invariant))?;
        for ((id, generation, _), (_, token, lifetime)) in entries.iter() {
            if *id == owner.key() {
                if *generation > owner.generation() {
                    return Err(fail(FailureCode::Fenced));
                }
                if *generation < owner.generation() {
                    token.cancel();
                    if matches!(owner.scope(), McpOwnerScope::Run(_)) {
                        lifetime.cancel();
                    }
                }
            }
        }
        // Explicit host reopen creates a fresh epoch; old handles stay cancelled.
        // Run-owned handles retain sticky failure until the runtime reconciles them.
        if matches!(owner.scope(), McpOwnerScope::Host(_))
            && entries
                .get(&key)
                .is_some_and(|(_, token, _)| token.is_cancelled())
        {
            entries.remove(&key);
        }
        if !entries.contains_key(&key) && entries.len() >= config.limits.pending_requests {
            return Err(fail(FailureCode::McpTransport));
        }
        entries
            .entry(key)
            .or_insert_with(|| {
                let mut token = McpCancellation::linked(&stop);
                token.parents.push(lifetime.state.clone());
                token.parents.extend(lifetime.parents.iter().cloned());
                (
                    Arc::new(tokio::sync::OnceCell::new()),
                    Arc::new(token),
                    lifetime.clone(),
                )
            })
            .clone()
    };
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
