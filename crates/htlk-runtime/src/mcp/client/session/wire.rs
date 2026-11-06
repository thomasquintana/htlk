//! Continuously receiving native transports with bounded pending correlation.
use super::*;
use crate::mcp::{as_map, as_text, get};
use htlk_executable::McpTransport;
use std::{
    process::Stdio,
    sync::{OnceLock, atomic::AtomicBool},
};

mod callbacks;
mod http;
mod payload;
mod sse;
mod tasks;

enum Transport {
    Stdio {
        child: tokio::sync::Mutex<tokio::process::Child>,
        input: tokio::sync::Mutex<tokio::process::ChildStdin>,
    },
    Http(http::Http),
}

struct PendingRequest {
    task_key: Option<Digest>,
    reply: oneshot::Sender<McpClientReply>,
    limits: Limits,
    sent: Arc<AtomicBool>,
    cancellation: Arc<McpCancellation>,
    deadline: tokio::time::Instant,
    context: Option<McpAttemptContext>,
}
type Pending = BTreeMap<McpRequestId, PendingRequest>;
type QueuedEvent = (String, JsonDocument, tokio::sync::OwnedSemaphorePermit);

// Once a correlated outcome is admitted, cancellation of independent URL storage
// must not downgrade it to a transport failure or discard its protected bytes.
struct ObservedReply {
    sender: Option<oneshot::Sender<McpClientReply>>,
    reply: Option<McpClientReply>,
    registering_urls: bool,
    registering_tasks: bool,
}
impl Drop for ObservedReply {
    fn drop(&mut self) {
        if let (Some(sender), Some(mut reply)) = (self.sender.take(), self.reply.take()) {
            if self.registering_urls && reply.url_interactions.is_none() {
                reply.url_interactions = Some(Err(fail(FailureCode::Cancelled)));
            }
            if self.registering_tasks && reply.task_registration.is_none() {
                reply.task_registration = Some(Err(fail(FailureCode::Cancelled)));
            }
            let _ = sender.send(reply);
        }
    }
}

pub(super) struct Wire {
    pub owner: McpOwner,
    pub server: ServerIdentity,
    pub epoch: u64,
    pub initialization: OnceLock<JsonDocument>,
    pub stop: Arc<McpCancellation>,
    pub closed: tokio::sync::watch::Sender<bool>,
    pub ended: tokio::sync::watch::Sender<Option<McpSessionEnd>>,
    pub server_events: tokio::sync::watch::Sender<Option<bool>>,
    pub stored_session: Option<crate::store::McpStoredSession>,
    config: Arc<McpClientConfig>,
    limits: Limits,
    transport: Transport,
    pending: Mutex<Pending>,
    next: AtomicU64,
    task_sequence: AtomicU64,
    events: mpsc::Sender<QueuedEvent>,
    event_sequence: AtomicU64,
    received: AtomicU64,
    memory: Arc<tokio::sync::Semaphore>,
    cleanup: tokio::sync::Mutex<()>,
    executors: workers::Executors,
    incoming: Mutex<BTreeMap<McpRequestId, (Digest, Arc<McpCancellation>)>>,
    task_controls: Arc<tokio::sync::Semaphore>,
    task_cancel_controls: Arc<tokio::sync::Semaphore>,
    _task_cancel_memory: Option<tokio::sync::OwnedSemaphorePermit>,
    accepted_callbacks: Mutex<std::collections::BTreeSet<McpRequestId>>,
    task_waits: Mutex<BTreeMap<McpRequestId, (Digest, Arc<McpCancellation>)>>,
}

impl Wire {
    pub fn persistence(&self) -> Option<&McpClientPersistence> {
        self.config.persistence.as_ref()
    }
    pub fn fresh_task_key(&self) -> Digest {
        htlk_executable::digest::hash_bytes(
            format!(
                "htlk.remote-task/1:{}:{}:{}:{}",
                self.owner.key(),
                self.epoch,
                self.server
                    .digest(&self.limits)
                    .expect("initialized server identity"),
                self.task_sequence.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )
    }
    pub fn representation_limits(&self) -> &Limits {
        &self.limits
    }
    pub fn client_limits(&self) -> &McpClientLimits {
        &self.config.limits
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        owner: McpOwner,
        server: ServerIdentity,
        epoch: u64,
        endpoint: McpEndpoint,
        config: Arc<McpClientConfig>,
        mut limits: Limits,
        stop: Arc<McpCancellation>,
        stored_session: Option<crate::store::McpStoredSession>,
        executors: workers::Executors,
    ) -> Result<Arc<Self>, RuntimeFailure> {
        limits.max_document_bytes = limits.max_document_bytes.min(config.limits.frame_bytes);
        let (transport, output) = match endpoint {
            McpEndpoint::Stdio {
                program,
                arguments,
                environment,
            } if server.transport() == McpTransport::Stdio => {
                let mut child = tokio::process::Command::new(program)
                    .args(arguments)
                    .envs(environment)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                let input = child
                    .stdin
                    .take()
                    .ok_or_else(|| fail(FailureCode::McpTransport))?;
                let output = child
                    .stdout
                    .take()
                    .ok_or_else(|| fail(FailureCode::McpTransport))?;
                (
                    Transport::Stdio {
                        child: tokio::sync::Mutex::new(child),
                        input: tokio::sync::Mutex::new(input),
                    },
                    Some(output),
                )
            }
            McpEndpoint::Http { url, headers }
                if server.transport() == McpTransport::StreamableHttp =>
            {
                (
                    Transport::Http(http::Http::new(
                        url,
                        headers,
                        &owner,
                        &server,
                        &limits,
                        config.credentials.clone(),
                    )?),
                    None,
                )
            }
            _ => return Err(fail(FailureCode::McpIdentity)),
        };
        let (events, receiver) = mpsc::channel(config.limits.events);
        let memory = Arc::new(tokio::sync::Semaphore::new(config.limits.retained_bytes));
        // Reserve cancellation bytes before any application/control saturation.
        // Wire task-cancel requests have a separate 4 KiB ingress ceiling; their
        // generated Task/error replies fit 8 KiB, including the correlated ID.
        let task_cancel_memory = if config.task_receiver.is_some() {
            Some(
                memory
                    .clone()
                    .try_acquire_many_owned(64 * 1024)
                    .map_err(|_| fail(FailureCode::Budget))?,
            )
        } else {
            None
        };
        let wire = Arc::new(Self {
            owner,
            server,
            epoch,
            initialization: OnceLock::new(),
            stop,
            stored_session,
            closed: tokio::sync::watch::channel(false).0,
            ended: tokio::sync::watch::channel(None).0,
            server_events: tokio::sync::watch::channel(if output.is_some() {
                Some(true)
            } else {
                None
            })
            .0,
            memory,
            _task_cancel_memory: task_cancel_memory,
            task_controls: Arc::new(tokio::sync::Semaphore::new(config.limits.pending_requests)),
            task_cancel_controls: Arc::new(tokio::sync::Semaphore::new(2)),
            accepted_callbacks: Mutex::new(Default::default()),
            task_waits: Mutex::new(Default::default()),
            config,
            limits,
            transport,
            pending: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            task_sequence: AtomicU64::new(1),
            events,
            event_sequence: AtomicU64::new(1),
            received: AtomicU64::new(0),
            cleanup: tokio::sync::Mutex::new(()),
            executors,
            incoming: Mutex::new(BTreeMap::new()),
        });
        let reader = wire.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = async {
                    reader.stop.cancelled().await;
                    let active = reader.pending.lock().is_ok_and(|pending| !pending.is_empty());
                    if active { tokio::time::sleep(Duration::from_millis(reader.config.limits.cancellation_grace_ms)).await; }
                } => Err(fail(FailureCode::Cancelled)),
                result = async {
                    if let Some(output) = output { reader.read(output).await }
                    else { std::future::pending().await }
                } => result,
            };
            let code = result.err().map_or(FailureCode::McpTransport, |e| e.code());
            reader.end(match code {
                FailureCode::Cancelled => McpSessionEnd::Cancelled,
                FailureCode::McpProtocol => McpSessionEnd::Protocol,
                _ => McpSessionEnd::Transport,
            });
            reader.fail_pending(code);
            reader.stop.cancel();
            reader.close().await;
        });
        let consumer = wire.clone();
        tokio::spawn(async move {
            consumer.deliver_events(receiver).await;
        });
        if wire.config.persistence.is_some() {
            let watcher = wire.clone();
            tokio::spawn(async move {
                while !watcher.stop.is_cancelled() {
                    let checked = tokio::select! {
                        biased;
                        _ = watcher.stop.cancelled() => break,
                        result = watcher.check_owner() => result,
                    };
                    if checked.is_err() {
                        watcher.end(McpSessionEnd::Cancelled);
                        break;
                    }
                    tokio::select! {
                        _ = watcher.stop.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                    }
                }
            });
        }
        Ok(wire)
    }

    pub async fn initialize(self: &Arc<Self>) -> Result<(), RuntimeFailure> {
        let params = obj([
            ("protocolVersion", Value::Text(MCP_VERSION.into())),
            ("capabilities", self.config.capabilities.value().clone()),
            (
                "clientInfo",
                obj([
                    ("name", Value::Text("htlk".into())),
                    ("version", Value::Text(env!("CARGO_PKG_VERSION").into())),
                ])?,
            ),
        ])?;
        let request = self.default_request("initialize", params, self.stop.clone());
        let _reservation = self.reserve_request(&request)?;
        let reply = self.invoke(request).await;
        let result = reply.result?.map_err(|_| fail(FailureCode::McpProtocol))?;
        let fields = as_map(result.value())?;
        if as_text(get(fields, "protocolVersion")?)? != MCP_VERSION {
            return Err(fail(FailureCode::McpIdentity));
        }
        let info = as_map(get(fields, "serverInfo")?)?;
        if as_text(get(info, "name")?)? != self.server.implementation_name()
            || as_text(get(info, "version")?)? != self.server.implementation_version()
        {
            return Err(fail(FailureCode::McpIdentity));
        }
        as_map(get(fields, "capabilities")?)?;
        self.send(obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("method", Value::Text("notifications/initialized".into())),
        ])?)
        .await?;
        self.initialization
            .set(result)
            .map_err(|_| fail(FailureCode::Invariant))?;
        if matches!(self.transport, Transport::Http(_)) && self.config.listen_for_events {
            let wire = self.clone();
            tokio::spawn(async move {
                let Transport::Http(http) = &wire.transport else {
                    return;
                };
                let result = tokio::select! {
                    biased;
                    _ = wire.stop.cancelled() => return,
                    result = http.events(&wire) => result,
                };
                if let Err(error) = result {
                    wire.end(if error.code() == FailureCode::Policy {
                        McpSessionEnd::Authentication
                    } else if error.code() == FailureCode::McpProtocol {
                        McpSessionEnd::Protocol
                    } else {
                        McpSessionEnd::Transport
                    });
                }
            });
        }
        Ok(())
    }

    pub fn default_request(
        &self,
        method: &str,
        params: Value,
        cancellation: Arc<McpCancellation>,
    ) -> McpRequest {
        McpRequest {
            response_limits: self.limits.clone(),
            cancellation,
            cancellation_grace_ms: self.config.limits.cancellation_grace_ms,
            method: method.into(),
            params,
            timeout_ms: self.config.limits.timeout_ms,
            max_response_bytes: self.limits.max_document_bytes,
            idempotency_key: None,
        }
    }

    pub async fn invoke(self: &Arc<Self>, request: McpRequest) -> McpClientReply {
        self.invoke_context(request, None).await
    }
    pub async fn invoke_context(
        self: &Arc<Self>,
        request: McpRequest,
        context: Option<McpAttemptContext>,
    ) -> McpClientReply {
        self.invoke_scoped(request, context, None).await
    }
    pub async fn invoke_scoped(
        self: &Arc<Self>,
        request: McpRequest,
        context: Option<McpAttemptContext>,
        task_key: Option<Digest>,
    ) -> McpClientReply {
        if let Some(context) = &context
            && (self.server.digest(&self.limits).ok() != Some(context.server)
                || !matches!(self.owner.scope(), McpOwnerScope::Run(run) if run == &context.origin.run_id))
        {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpIdentity);
        }
        let cancellation = &request.cancellation;
        let method = request.method.as_str();
        if cancellation.is_cancelled() || self.stop.is_cancelled() {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Cancelled);
        }
        let mut limits = request.response_limits.clone();
        limits.max_document_bytes = limits.max_document_bytes.min(request.max_response_bytes);
        if request.response_limits.validate().is_err()
            || [request.timeout_ms, request.cancellation_grace_ms]
                .into_iter()
                .any(|ms| {
                    ms == 0
                        || ms > i64::MAX as u64
                        || std::time::Instant::now()
                            .checked_add(Duration::from_millis(ms))
                            .is_none()
                })
        {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Profile);
        }
        if method.is_empty() || !matches!(request.params, Value::Map(_)) {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Input);
        }
        if method == "tools/call"
            && self.config.persistence.is_some()
            && as_map(&request.params).is_ok_and(|params| params.get("task").is_some())
            && task_key.is_none()
        {
            // A raw request cannot bypass durable task intent/descriptor admission.
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Profile);
        }
        if self.initialization.get().is_some() && method == "initialize"
            || method.starts_with("notifications/")
        {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Input);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
        let checked = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(fail(FailureCode::Cancelled)),
            _ = self.stop.cancelled() => Err(fail(FailureCode::Cancelled)),
            result = tokio::time::timeout_at(deadline, self.check_owner()) => result.unwrap_or_else(|_| Err(fail(FailureCode::McpTimeout))),
        };
        if let Err(error) = checked {
            self.end(McpSessionEnd::Cancelled);
            return McpClientReply::error(McpDelivery::NotSent, error);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if id > self.config.limits.requests_per_session as u64 || id > i64::MAX as u64 {
            return McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpTransport);
        }
        let key = McpRequestId::Integer(id as i64);
        let message = obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("id", key.to_value()),
            ("method", Value::Text(method.into())),
            ("params", request.params.clone()),
        ]);
        let bytes = match message.and_then(|v| {
            JsonDocument::from_value(&v, &limits).map_err(|_| fail(FailureCode::Input))
        }) {
            Ok(doc) => doc.as_bytes().to_vec(),
            Err(error) => {
                return McpClientReply::error(McpDelivery::NotSent, error);
            }
        };
        let (reply, mut receive) = oneshot::channel();
        let sent_state = Arc::new(AtomicBool::new(false));
        {
            let Ok(mut pending) = self.pending.lock() else {
                return McpClientReply::failed(McpDelivery::NotSent, FailureCode::Invariant);
            };
            if pending.len() >= self.config.limits.pending_requests {
                return McpClientReply::failed(McpDelivery::NotSent, FailureCode::McpTransport);
            }
            pending.insert(
                key.clone(),
                PendingRequest {
                    task_key,
                    reply,
                    limits,
                    sent: sent_state.clone(),
                    cancellation: request.cancellation.clone(),
                    deadline,
                    context,
                },
            );
        }
        let mut observed = None;
        let result = tokio::time::timeout_at(deadline, async {
            let exchange = async {
                match &self.transport {
                    Transport::Stdio { .. } => self.send_bytes(&bytes, Some(&sent_state)).await,
                    Transport::Http(http) => http.exchange(&bytes, &key, self, request.idempotency_key.as_deref(), &sent_state).await,
                }
            };
            tokio::pin!(exchange);
            let completion = async {
                let mut sent = false;
                loop {
                    tokio::select! {
                        biased;
                        result = &mut receive, if observed.is_none() => {
                            observed = Some(result.map_err(|_| fail(FailureCode::McpTransport))?);
                            if sent { return Ok(observed.take().expect("observed reply")); }
                        },
                        result = exchange.as_mut(), if !sent => {
                            sent = true;
                            if let Err(error) = result {
                                if let Some(reply) = observed.take() {
                                    self.end(if error.code() == FailureCode::McpProtocol { McpSessionEnd::Protocol } else { McpSessionEnd::Transport });
                                    return Ok(reply);
                                }
                                // A reply sent by the exchange immediately before a
                                // trailing-frame error is still an observed outcome.
                                if let Ok(reply) = receive.try_recv() {
                                    self.end(McpSessionEnd::Protocol);
                                    return Ok(reply);
                                }
                                return Err(error);
                            }
                            if let Some(reply) = observed.take() { return Ok(reply); }
                        },
                    }
                }
            };
            tokio::pin!(completion);
            tokio::select! {
                biased;
                result = completion.as_mut() => result,
                _ = async {
                    tokio::select! { _ = self.stop.cancelled() => {}, _ = cancellation.cancelled() => {} }
                } => {
                    let cancel = obj([
                        ("jsonrpc", Value::Text("2.0".into())),
                        ("method", Value::Text("notifications/cancelled".into())),
                        ("params", obj([("requestId", key.to_value())])?),
                    ])?;
                    let grace = tokio::time::Instant::now() + Duration::from_millis(request.cancellation_grace_ms);
                    tokio::time::timeout_at(grace.min(deadline), async {
                        let notification = self.send(cancel);
                        tokio::pin!(notification);
                        // Keep reading the HTTP request stream while its cancellation POST runs.
                        tokio::select! {
                            result = completion.as_mut() => result,
                            result = notification.as_mut() => {
                                result?;
                                completion.as_mut().await
                            }
                        }
                    }).await.map_err(|_| fail(FailureCode::Cancelled))?
                }
            }
        }).await.unwrap_or_else(|_| Err(fail(FailureCode::McpTimeout)));
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&key);
        }
        match result {
            Ok(reply) => reply,
            Err(error) => {
                if let Some(reply) = observed.take().or_else(|| receive.try_recv().ok()) {
                    self.end(if error.code() == FailureCode::Cancelled {
                        McpSessionEnd::Cancelled
                    } else {
                        McpSessionEnd::Transport
                    });
                    return reply;
                }
                McpClientReply::error(
                    if sent_state.load(Ordering::Acquire) {
                        McpDelivery::PossiblySent
                    } else {
                        McpDelivery::NotSent
                    },
                    error,
                )
            }
        }
    }

    pub fn reserve_request(
        &self,
        request: &McpRequest,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, RuntimeFailure> {
        request
            .response_limits
            .validate()
            .map_err(|_| fail(FailureCode::Profile))?;
        let params = JsonDocument::from_value(&request.params, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?;
        let bytes = params
            .as_bytes()
            .len()
            .checked_add(request.method.len())
            .and_then(|n| n.checked_add(256))
            .filter(|n| *n <= self.limits.max_document_bytes)
            .ok_or_else(|| fail(FailureCode::Input))?;
        let response = request
            .max_response_bytes
            .min(request.response_limits.max_document_bytes)
            .min(self.limits.max_document_bytes);
        let reserved = response
            .checked_mul(2)
            .and_then(|n| n.checked_add(bytes))
            .ok_or_else(|| fail(FailureCode::McpTransport))?;
        self.reserve(reserved)
    }

    fn reserve(&self, bytes: usize) -> Result<tokio::sync::OwnedSemaphorePermit, RuntimeFailure> {
        let bytes = u32::try_from(bytes).map_err(|_| fail(FailureCode::McpTransport))?;
        self.memory
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| fail(FailureCode::McpTransport))
    }

    fn end(&self, reason: McpSessionEnd) {
        self.ended.send_if_modified(|end| {
            if end.is_none() {
                *end = Some(reason);
                true
            } else {
                false
            }
        });
        self.stop.cancel();
    }
    async fn check_owner(&self) -> Result<(), RuntimeFailure> {
        match &self.config.persistence {
            Some(persistence) => persistence.validate(&self.owner).await,
            None => Ok(()),
        }
    }

    async fn send(&self, message: Value) -> Result<(), RuntimeFailure> {
        let message = JsonDocument::from_value(&message, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        self.send_bytes(message.as_bytes(), None).await
    }
    async fn send_bytes(
        &self,
        bytes: &[u8],
        sent: Option<&AtomicBool>,
    ) -> Result<(), RuntimeFailure> {
        tokio::time::timeout(
            Duration::from_millis(self.config.limits.timeout_ms),
            async {
                let Transport::Stdio { input, .. } = &self.transport else {
                    let Transport::Http(http) = &self.transport else {
                        unreachable!()
                    };
                    return http.control(bytes, self).await;
                };
                let mut input = input.lock().await;
                if let Some(sent) = sent {
                    if self.stop.is_cancelled() {
                        return Err(fail(FailureCode::Cancelled));
                    }
                    sent.store(true, Ordering::Release);
                }
                input
                    .write_all(bytes)
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                input
                    .write_all(b"\n")
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                input
                    .flush()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))
            },
        )
        .await
        .map_err(|_| fail(FailureCode::McpTimeout))?
    }

    async fn read(
        self: &Arc<Self>,
        mut output: tokio::process::ChildStdout,
    ) -> Result<(), RuntimeFailure> {
        let mut chunk = [0u8; 4096];
        let mut frame = Vec::new();
        loop {
            let count = output
                .read(&mut chunk)
                .await
                .map_err(|_| fail(FailureCode::McpTransport))?;
            if count == 0 {
                return Err(fail(FailureCode::McpTransport));
            }
            for byte in &chunk[..count] {
                if *byte == b'\n' {
                    self.route(&frame, None, true).await?;
                    frame.clear();
                } else {
                    if frame.len() >= self.limits.max_document_bytes {
                        return Err(fail(FailureCode::McpProtocol));
                    }
                    frame.push(*byte);
                }
            }
        }
    }

    async fn route(
        self: &Arc<Self>,
        bytes: &[u8],
        associated: Option<&McpRequestId>,
        allow_response: bool,
    ) -> Result<(), RuntimeFailure> {
        let received = self.received.fetch_add(1, Ordering::Relaxed);
        if received >= self.config.limits.messages_per_session as u64
            || (self.initialization.get().is_none() && received >= 1024)
        {
            return Err(fail(FailureCode::McpProtocol));
        }
        let document =
            JsonDocument::new(bytes, &self.limits).map_err(|_| fail(FailureCode::McpProtocol))?;
        let fields = as_map(document.value())?;
        if fields.get("jsonrpc") != Some(&Value::Text("2.0".into())) {
            return Err(fail(FailureCode::McpProtocol));
        }
        if let Some(method) = fields.get("method") {
            let method = as_text(method)?;
            if fields.get("result").is_some() || fields.get("error").is_some() {
                return Err(fail(FailureCode::McpProtocol));
            }
            if let Some(id) = fields.get("id") {
                let id = McpRequestId::from_value(id)?;
                if self.initialization.get().is_some()
                    && self.config.task_receiver.is_some()
                    && matches!(
                        method,
                        "tasks/get" | "tasks/list" | "tasks/result" | "tasks/cancel"
                    )
                {
                    return self.start_task_control(document);
                }
                if self.initialization.get().is_some() && self.callback_supported(method) {
                    let related = if let Some(params) = fields.get("params") {
                        if let Some(task_id) = mcp_related_task_id(params)
                            .map_err(|_| fail(FailureCode::McpProtocol))?
                        {
                            let persistence = self
                                .config
                                .persistence
                                .as_ref()
                                .ok_or_else(|| fail(FailureCode::McpProtocol))?;
                            Some(
                                persistence
                                    .store
                                    .resolve_mcp_remote_task(
                                        &self.owner,
                                        self.stored_session
                                            .as_ref()
                                            .ok_or_else(|| fail(FailureCode::Profile))?
                                            .id,
                                        self.server
                                            .digest(&self.limits)
                                            .map_err(|_| fail(FailureCode::McpIdentity))?,
                                        task_id,
                                        persistence.clock.now_ms()?,
                                    )
                                    .await
                                    .map_err(|e| fail(e.code()))?,
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    return self
                        .start_callback(document, bytes.to_vec(), id, associated.cloned(), related)
                        .inspect_err(|_| self.end(McpSessionEnd::CallbackFailure));
                }
                let (kind, result) = if method == "ping"
                    && fields
                        .get("params")
                        .is_none_or(|v| matches!(v, Value::Map(_)))
                {
                    ("result", obj([])?)
                } else {
                    (
                        "error",
                        obj([
                            (
                                "code",
                                Value::Integer(if method == "ping" { -32602 } else { -32601 }),
                            ),
                            ("message", Value::Text("Unsupported client request.".into())),
                        ])?,
                    )
                };
                self.send(obj([
                    ("jsonrpc", Value::Text("2.0".into())),
                    ("id", id.to_value()),
                    (kind, result),
                ])?)
                .await?;
            } else {
                if method == "notifications/elicitation/complete" {
                    let elicitation = validate_url_completion(&document)?;
                    if self
                        .config
                        .handler(McpClientService::ElicitationUrl)
                        .is_none()
                    {
                        return Ok(());
                    }
                    let persistence = self
                        .config
                        .persistence
                        .as_ref()
                        .ok_or_else(|| fail(FailureCode::Profile))?;
                    let session = self
                        .stored_session
                        .as_ref()
                        .ok_or_else(|| fail(FailureCode::Profile))?;
                    let server = self
                        .server
                        .digest(&self.limits)
                        .map_err(|_| fail(FailureCode::McpIdentity))?;
                    let completion = persistence
                        .store
                        .complete_mcp_url_interaction(
                            &self.owner,
                            server,
                            session.id,
                            elicitation,
                            persistence.clock.now_ms()?,
                        )
                        .await
                        .map_err(|e| fail(e.code()))?;
                    if let McpUrlCompletion::Recorded { interaction_id } = completion {
                        self.url_event("htlk/url_completed", interaction_id)?;
                    }
                    return Ok(());
                }
                let params = fields.get("params").cloned().unwrap_or(obj([])?);
                if !matches!(params, Value::Map(_)) {
                    return Err(fail(FailureCode::McpProtocol));
                }
                if method == "notifications/cancelled"
                    && let Some(id) = as_map(&params)?.get("requestId")
                    && let Ok(id) = McpRequestId::from_value(id)
                    && let Some(signal) = self
                        .task_waits
                        .lock()
                        .ok()
                        .and_then(|waits| waits.get(&id).map(|(_, signal)| signal.clone()))
                {
                    signal.cancel();
                }
                if method == "notifications/cancelled"
                    && let Some(id) = as_map(&params)?.get("requestId")
                    && let Ok(id) = McpRequestId::from_value(id)
                    && !self
                        .accepted_callbacks
                        .lock()
                        .is_ok_and(|accepted| accepted.contains(&id))
                    && let Ok(incoming) = self.incoming.lock()
                    && let Some((_, signal)) = incoming.get(&id)
                {
                    signal.cancel();
                }
                let params = JsonDocument::from_value(&params, &self.limits)
                    .map_err(|_| fail(FailureCode::McpProtocol))?;
                if self.config.events.is_some() {
                    let reservation = self.reserve(bytes.len()).inspect_err(|_| {
                        self.end(McpSessionEnd::EventOverflow);
                    })?;
                    self.events
                        .try_send((method.into(), params, reservation))
                        .map_err(|_| {
                            self.end(McpSessionEnd::EventOverflow);
                            fail(FailureCode::McpTransport)
                        })?;
                }
            }
        } else {
            let id = McpRequestId::from_value(get(fields, "id")?)?;
            if !allow_response || associated.is_some_and(|expected| expected != &id) {
                return Err(fail(FailureCode::McpProtocol));
            }
            match (fields.get("result"), fields.get("error")) {
                (Some(Value::Map(_)), None) => {}
                (None, Some(Value::Map(error)))
                    if matches!(error.get("code"), Some(Value::Integer(_)))
                        && matches!(error.get("message"), Some(Value::Text(_))) => {}
                _ => return Err(fail(FailureCode::McpProtocol)),
            }
            {
                let pending = self
                    .pending
                    .lock()
                    .map_err(|_| fail(FailureCode::Invariant))?;
                if let Some(request) = pending.get(&id) {
                    JsonDocument::new(bytes, &request.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?;
                    if !request.sent.load(Ordering::Acquire) {
                        return Err(fail(FailureCode::McpProtocol));
                    }
                } else if !matches!(&id, McpRequestId::Integer(id) if *id > 0 && (*id as u64) < self.next.load(Ordering::Relaxed))
                {
                    return Err(fail(FailureCode::McpProtocol));
                } else {
                    // No waiting request can consume this late/duplicate payload.
                    return Ok(());
                }
            }
            let (result, raw_result) = if fields.get("result").is_some() {
                let raw = payload::member(bytes, "result", &self.limits)?;
                (
                    Ok(JsonDocument::new(raw, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?),
                    Some(raw.to_vec()),
                )
            } else {
                (
                    Err(McpRpcError::from_bytes(
                        payload::member(bytes, "error", &self.limits)?,
                        &self.limits,
                    )?),
                    None,
                )
            };
            let request = self
                .pending
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?
                .remove(&id);
            if let Some(request) = request {
                let mut observed = ObservedReply {
                    sender: Some(request.reply),
                    reply: Some(McpClientReply::correlated(result, raw_result)),
                    registering_urls: false,
                    registering_tasks: request.task_key.is_some(),
                };
                let reply = observed.reply.as_mut().expect("observed reply");
                reply.request_id = Some(id.clone());
                if let Some(key) = request.task_key {
                    let registration = async {
                        let persistence = self
                            .config
                            .persistence
                            .as_ref()
                            .ok_or_else(|| fail(FailureCode::Profile))?;
                        let session = self
                            .stored_session
                            .as_ref()
                            .ok_or_else(|| fail(FailureCode::Profile))?;
                        if let Ok(Ok(document)) = &reply.result {
                            let task = as_map(document.value())?
                                .get("task")
                                .ok_or_else(|| fail(FailureCode::McpProtocol))?;
                            let state = McpTaskState::from_document(
                                JsonDocument::from_value(task, &self.limits)
                                    .map_err(|_| fail(FailureCode::McpProtocol))?,
                                &self.limits,
                            )
                            .map_err(|_| fail(FailureCode::McpProtocol))?;
                            persistence
                                .store
                                .accept_mcp_remote_task(
                                    &self.owner,
                                    session.id,
                                    self.server
                                        .digest(&self.limits)
                                        .map_err(|_| fail(FailureCode::McpIdentity))?,
                                    key,
                                    state,
                                    reply
                                        .raw_result
                                        .clone()
                                        .ok_or_else(|| fail(FailureCode::McpProtocol))?,
                                    persistence.clock.now_ms()?,
                                )
                                .await
                                .map_err(|e| fail(e.code()))?;
                        } else if let Ok(Err(error)) = &reply.result {
                            persistence
                                .store
                                .record_mcp_remote_task_create_error(
                                    &self.owner,
                                    session.id,
                                    self.server
                                        .digest(&self.limits)
                                        .map_err(|_| fail(FailureCode::McpIdentity))?,
                                    key,
                                    error.as_bytes().to_vec(),
                                    persistence.clock.now_ms()?,
                                )
                                .await
                                .map_err(|e| fail(e.code()))?;
                        }
                        Ok(())
                    };
                    reply.task_registration = Some(
                        tokio::time::timeout_at(request.deadline, registration)
                            .await
                            .unwrap_or_else(|_| Err(fail(FailureCode::McpTimeout))),
                    );
                }
                if let Ok(Err(error)) = &reply.result
                    && error.code() == -32042
                    && self.initialization.get().is_some()
                {
                    observed.registering_urls = true;
                    let registration = async {
                        let session = self
                            .stored_session
                            .as_ref()
                            .ok_or_else(|| fail(FailureCode::Profile))?;
                        let server = self
                            .server
                            .digest(&self.limits)
                            .map_err(|_| fail(FailureCode::McpIdentity))?;
                        let host = McpInteractionHost::new((*self.config).clone())?;
                        let ids = host
                            .receive_url_required(
                                &self.owner,
                                server,
                                session.id,
                                id.clone(),
                                error,
                                request.context.as_ref(),
                            )
                            .await?;
                        for id in &ids {
                            self.url_event("htlk/url_interaction", *id)?;
                        }
                        Ok(ids)
                    };
                    let registered = tokio::time::timeout_at(request.deadline, registration)
                        .await
                        .unwrap_or_else(|_| Err(fail(FailureCode::Timeout)));
                    if let Err(error) = &registered {
                        // Correlated error/delivery remain intact even when policy,
                        // validation, storage or event admission independently fails.
                        let _ = self.local_event(
                            "htlk/url_registration_failed",
                            obj([
                                ("request_id", id.to_value()),
                                ("code", Value::Text(error.code().as_str().into())),
                            ])?,
                        );
                    }
                    reply.url_interactions = Some(registered);
                }
                drop(observed);
            }
            // A late response cannot resurrect a timed-out pending request.
        }
        Ok(())
    }

    fn url_event(&self, method: &str, id: Digest) -> Result<(), RuntimeFailure> {
        self.local_event(
            method,
            obj([("interaction_id", Value::Text(id.to_string()))])?,
        )
    }
    fn local_event(&self, method: &str, params: Value) -> Result<(), RuntimeFailure> {
        if self.config.events.is_none() {
            return Ok(());
        }
        let params = JsonDocument::from_value(&params, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let reservation = self.reserve(params.as_bytes().len())?;
        self.events
            .try_send((method.into(), params, reservation))
            .map_err(|_| {
                self.end(McpSessionEnd::EventOverflow);
                fail(FailureCode::Budget)
            })
    }

    async fn deliver_events(&self, mut receiver: mpsc::Receiver<QueuedEvent>) {
        loop {
            let event = tokio::select! {
                biased;
                _ = self.stop.cancelled() => break,
                event = receiver.recv() => match event { Some(event) => event, None => break },
            };
            let Some(handler) = &self.config.events else {
                continue;
            };
            if self.check_owner().await.is_err() {
                self.end(McpSessionEnd::Cancelled);
                break;
            }
            let _reservation = event.2;
            let sequence = self.event_sequence.fetch_add(1, Ordering::Relaxed);
            let identity = Value::Array(vec![
                Value::Text("htlk.mcp-event/1".into()),
                Value::Text(self.owner.key().to_string()),
                Value::Integer(self.owner.generation()),
                self.stored_session
                    .as_ref()
                    .map_or(Value::Null, |session| Value::Text(session.id.to_string())),
                Value::Integer(self.epoch as i64),
                Value::Integer(sequence as i64),
            ]);
            let Ok(identity) = JsonDocument::from_value(&identity, &self.limits) else {
                self.stop.cancel();
                break;
            };
            let event = McpClientEvent {
                owner: self.owner.clone(),
                server: self.server.clone(),
                epoch: self.epoch,
                session_id: self.stored_session.as_ref().map(|session| session.id),
                event_id: identity.digest(),
                method: event.0,
                params: event.1,
            };
            let handler = handler.clone();
            let Some(workers) = &self.executors.events else {
                self.end(McpSessionEnd::EventHandler);
                break;
            };
            let receive = match workers.submit(async move { handler.receive(event).await }) {
                Ok(receive) => receive,
                Err(_) => {
                    self.end(McpSessionEnd::EventOverflow);
                    break;
                }
            };
            let result = tokio::select! {
                biased;
                _ = self.stop.cancelled() => break,
                result = tokio::time::timeout(Duration::from_millis(self.config.limits.timeout_ms), receive) => result,
            };
            if !matches!(result, Ok(Ok(Ok(())))) {
                self.end(McpSessionEnd::EventHandler);
                break;
            }
        }
    }
    fn fail_pending(&self, code: FailureCode) {
        if let Ok(mut pending) = self.pending.lock() {
            for (_, request) in std::mem::take(&mut *pending) {
                let delivery = if request.sent.load(Ordering::Acquire) {
                    McpDelivery::PossiblySent
                } else {
                    McpDelivery::NotSent
                };
                let _ = request.reply.send(McpClientReply::failed(delivery, code));
            }
        }
    }
    pub async fn close(&self) {
        let _cleanup = self.cleanup.lock().await;
        if *self.closed.borrow() {
            return;
        }
        self.end(McpSessionEnd::Cancelled);
        self.fail_pending(FailureCode::McpTransport);
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            match &self.transport {
                Transport::Stdio { input, child } => {
                    let _ = tokio::time::timeout(Duration::from_millis(50), async {
                        let _ = input.lock().await.shutdown().await;
                    })
                    .await;
                    let _ = child.lock().await.kill().await;
                }
                Transport::Http(http) => http.close(self).await,
            }
        })
        .await;
        if let (Some(persistence), Some(session)) = (&self.config.persistence, &self.stored_session)
            && let Ok(now) = persistence.clock.now_ms()
        {
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                persistence
                    .store
                    .close_mcp_epoch(&self.owner, session.id, now),
            )
            .await;
        }
        self.closed.send_replace(true);
    }
}
