//! Transport I/O lives here, never on a short-lived execution worker reactor.
use super::*;
use htlk_executable::digest::Digest;
use std::sync::Mutex;
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};

type Key = (RunId, String, i64, Digest);
type Lease = OwnedMutexGuard<RunSession>;
type Slot = Arc<tokio::sync::Mutex<RunSession>>;
type Registry = Arc<Mutex<BTreeMap<Key, (Slot, Arc<McpCancellation>)>>>;

#[derive(Default)]
struct RunSession {
    connection: Option<NativeConnection>,
    // Set before any fallible I/O. Incomplete/ambiguous exchanges cannot be reused.
    broken: bool,
    lifetime: Arc<McpCancellation>,
}

enum Command {
    Connect {
        binding: McpBinding,
        context: McpRunContext,
        limits: Limits,
        reply: oneshot::Sender<Result<(Lease, JsonDocument), RuntimeFailure>>,
    },
    Invoke {
        lease: Lease,
        request: McpRequest,
        reply: oneshot::Sender<(Lease, McpOutcome)>,
    },
}

pub(super) struct NativeSessions {
    sender: mpsc::Sender<Command>,
}
impl NativeSessions {
    pub fn new(
        endpoints: BTreeMap<String, McpEndpoint>,
        timeout_ms: u64,
    ) -> Result<Self, RuntimeFailure> {
        let (sender, mut receiver) = mpsc::channel::<Command>(64);
        std::thread::Builder::new()
            .name("htlk-mcp-io".into())
            .spawn(move || {
                actix::System::new().block_on(async move {
                    let registry = Registry::default();
                    let stop = Arc::new(McpCancellation::new());
                    let endpoints = Arc::new(endpoints);
                    let mut jobs = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            command = receiver.recv() => {
                                let Some(command) = command else { break };
                                match command {
                                    Command::Connect { binding, context, limits, reply } => {
                                        let registry = registry.clone();
                                        let endpoints = endpoints.clone();
                                        let stop = stop.clone();
                                        jobs.spawn(async move {
                                            let result = tokio::time::timeout(
                                                Duration::from_millis(timeout_ms),
                                                connect(registry, endpoints, stop, binding, context, limits),
                                            ).await.unwrap_or_else(|_| Err(fail(FailureCode::McpTimeout)));
                                            let _ = reply.send(result);
                                        });
                                    }
                                    Command::Invoke { mut lease, request, reply } => {
                                        jobs.spawn(async move {
                                            lease.broken = true;
                                            let outcome = match lease.connection.as_mut() {
                                                Some(connection) => connection.invoke(request).await,
                                                None => McpOutcome {
                                                    delivery: McpDelivery::NotSent,
                                                    result: Err(fail(FailureCode::McpTransport)),
                                                },
                                            };
                                            // Correlated JSON-RPC errors end an exchange too.
                                            lease.broken = outcome.delivery != McpDelivery::ResponseReceived;
                                            let _ = reply.send((lease, outcome));
                                        });
                                    }
                                }
                            }
                            _ = jobs.join_next(), if !jobs.is_empty() => {}
                        }
                    }
                    stop.cancel();
                    let slots: Vec<_> = registry.lock().map(|entries|
                        entries.values().map(|(slot, _)| slot.clone()).collect()
                    ).unwrap_or_default();
                    for slot in slots {
                        jobs.spawn(async move { cleanup(slot).await });
                    }
                    // Session lifetime watchers finish bounded cleanup on this reactor.
                    while jobs.join_next().await.is_some() {}
                });
            })
            .map_err(|_| fail(FailureCode::McpTransport))?;
        Ok(Self { sender })
    }

    pub async fn connect(
        &self,
        binding: &McpBinding,
        context: &McpRunContext,
        limits: &Limits,
    ) -> Result<NativeLease, RuntimeFailure> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::Connect {
                binding: binding.clone(),
                context: context.clone(),
                limits: limits.clone(),
                reply,
            })
            .await
            .map_err(|_| fail(FailureCode::McpTransport))?;
        let (lease, descriptor) = receive
            .await
            .map_err(|_| fail(FailureCode::McpTransport))??;
        let lifetime = lease.lifetime.clone();
        Ok(NativeLease {
            sender: self.sender.clone(),
            lease: Some(lease),
            descriptor,
            lifetime,
        })
    }
}

async fn connect(
    registry: Registry,
    endpoints: Arc<BTreeMap<String, McpEndpoint>>,
    stop: Arc<McpCancellation>,
    binding: McpBinding,
    context: McpRunContext,
    limits: Limits,
) -> Result<(Lease, JsonDocument), RuntimeFailure> {
    if context.cancellation.is_cancelled() || context.fence <= 0 {
        return Err(fail(FailureCode::Cancelled));
    }
    limits.validate().map_err(|_| fail(FailureCode::Profile))?;
    let key = (
        context.run_id.clone(),
        context.host.principal().into(),
        context.fence,
        binding
            .server()
            .digest(&limits)
            .map_err(|_| fail(FailureCode::McpIdentity))?,
    );
    let (slot, created) = {
        let mut entries = registry.lock().map_err(|_| fail(FailureCode::Invariant))?;
        for ((run, principal, fence, _), (_, lifetime)) in entries.iter() {
            if run == &context.run_id && principal == context.host.principal() {
                if *fence > context.fence {
                    return Err(fail(FailureCode::Cancelled));
                }
                if *fence < context.fence {
                    lifetime.cancel();
                }
            }
        }
        match entries.get(&key) {
            Some((slot, _)) => (slot.clone(), false),
            None => {
                let mut lifetime = McpCancellation::linked(&context.cancellation);
                lifetime.parents.push(stop.state.clone());
                let slot = Arc::new(tokio::sync::Mutex::new(RunSession {
                    lifetime: Arc::new(lifetime),
                    ..Default::default()
                }));
                entries.insert(key.clone(), (slot.clone(), context.cancellation.clone()));
                (slot, true)
            }
        }
    };
    if created {
        let slot = slot.clone();
        let lifetime = context.cancellation.clone();
        tokio::spawn(async move {
            tokio::select! { _ = lifetime.cancelled() => {}, _ = stop.cancelled() => {} }
            if let Ok(mut entries) = registry.lock() {
                entries.remove(&key);
            }
            cleanup(slot).await;
        });
    }
    tokio::select! {
        biased;
        _ = context.cancellation.cancelled() => Err(fail(FailureCode::Cancelled)),
        result = async {
            let mut lease = slot.lock_owned().await;
            if lease.broken { return Err(fail(FailureCode::McpTransport)); }
            lease.broken = true;
            if lease.connection.is_none() {
                let endpoint = endpoints.get(binding.server().deployment_id())
                    .ok_or_else(|| fail(FailureCode::McpIdentity))?;
                let session = Session::new(endpoint, binding.server(), limits.clone()).await?;
                // Store transport before preparation so timeout still leaves a broken slot.
                lease.connection = Some(NativeConnection {
                    session, descriptor: JsonDocument::new(b"{}", &limits)
                        .map_err(|_| fail(FailureCode::Profile))?,
                });
            }
            let connection = lease.connection.as_mut().ok_or_else(|| fail(FailureCode::Invariant))?;
            connection.session.limits = limits;
            let descriptor = connection.session.prepare(&binding).await?;
            connection.descriptor = descriptor.clone();
            lease.broken = false;
            Ok((lease, descriptor))
        } => result,
    }
}

async fn cleanup(slot: Slot) {
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        let mut lease = slot.lock().await;
        lease.broken = true;
        if let Some(mut connection) = lease.connection.take() {
            let _ = connection.close().await;
        }
    })
    .await;
}

pub(super) struct NativeLease {
    sender: mpsc::Sender<Command>,
    lease: Option<Lease>,
    descriptor: JsonDocument,
    lifetime: Arc<McpCancellation>,
}
impl McpConnection for NativeLease {
    fn descriptor(&self) -> &JsonDocument {
        &self.descriptor
    }
    fn invoke<'a>(&'a mut self, mut request: McpRequest) -> McpAttemptFuture<'a> {
        Box::pin(async move {
            let unsent = |code| McpOutcome {
                delivery: McpDelivery::NotSent,
                result: Err(fail(code)),
            };
            if self.lifetime.is_cancelled() {
                return unsent(FailureCode::Cancelled);
            }
            let Some(lease) = self.lease.take() else {
                return unsent(FailureCode::McpTransport);
            };
            if lease.broken {
                return unsent(FailureCode::McpTransport);
            }
            let mut signal = McpCancellation::linked(&self.lifetime);
            signal.parents.push(request.cancellation.state.clone());
            signal
                .parents
                .extend(request.cancellation.parents.iter().cloned());
            request.cancellation = Arc::new(signal);
            let (reply, receive) = oneshot::channel();
            if self
                .sender
                .send(Command::Invoke {
                    lease,
                    request,
                    reply,
                })
                .await
                .is_err()
            {
                return unsent(FailureCode::McpTransport);
            }
            match receive.await {
                Ok((lease, outcome)) => {
                    self.lease = Some(lease);
                    outcome
                }
                Err(_) => McpOutcome {
                    delivery: McpDelivery::PossiblySent,
                    result: Err(fail(FailureCode::McpTransport)),
                },
            }
        })
    }
    fn close(&mut self) -> McpFuture<'_, ()> {
        self.lease.take();
        Box::pin(async { Ok(()) })
    }
}
