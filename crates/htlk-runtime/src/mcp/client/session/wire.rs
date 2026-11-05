//! Continuously receiving native transports with bounded pending correlation.
use super::*;
use crate::mcp::{as_map, as_text, get};
use htlk_executable::McpTransport;
use std::{
    process::Stdio,
    sync::{OnceLock, atomic::AtomicBool},
};

mod http;
mod sse;

enum Transport {
    Stdio {
        child: tokio::sync::Mutex<tokio::process::Child>,
        input: tokio::sync::Mutex<tokio::process::ChildStdin>,
    },
    Http(http::Http),
}

struct PendingRequest {
    reply: oneshot::Sender<McpClientReply>,
    limits: Limits,
    sent: Arc<AtomicBool>,
}
type Pending = BTreeMap<McpRequestId, PendingRequest>;
type QueuedEvent = (String, JsonDocument, tokio::sync::OwnedSemaphorePermit);

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
    events: mpsc::Sender<QueuedEvent>,
    event_sequence: AtomicU64,
    received: AtomicU64,
    memory: Arc<tokio::sync::Semaphore>,
    cleanup: tokio::sync::Mutex<()>,
}

impl Wire {
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
                (Transport::Http(http::Http::new(url, headers)?), None)
            }
            _ => return Err(fail(FailureCode::McpIdentity)),
        };
        let (events, receiver) = mpsc::channel(config.limits.events);
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
            memory: Arc::new(tokio::sync::Semaphore::new(config.limits.retained_bytes)),
            config,
            limits,
            transport,
            pending: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            events,
            event_sequence: AtomicU64::new(1),
            received: AtomicU64::new(0),
            cleanup: tokio::sync::Mutex::new(()),
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
        let reply = self
            .invoke(self.default_request("initialize", params, self.stop.clone()))
            .await;
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
                    wire.end(if error.code() == FailureCode::McpProtocol {
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
            return McpClientReply {
                delivery: McpDelivery::NotSent,
                result: Err(error),
            };
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
                return McpClientReply {
                    delivery: McpDelivery::NotSent,
                    result: Err(error),
                };
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
                    reply,
                    limits,
                    sent: sent_state.clone(),
                },
            );
        }
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
                        result = &mut receive => return result.map_err(|_| fail(FailureCode::McpTransport)),
                        result = exchange.as_mut(), if !sent => { result?; sent = true; },
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
            Err(error) => McpClientReply {
                delivery: if sent_state.load(Ordering::Acquire) {
                    McpDelivery::PossiblySent
                } else {
                    McpDelivery::NotSent
                },
                result: Err(error),
            },
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
        self.reserve(bytes)
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

    async fn read(&self, mut output: tokio::process::ChildStdout) -> Result<(), RuntimeFailure> {
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
        &self,
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
                let params = fields.get("params").cloned().unwrap_or(obj([])?);
                if !matches!(params, Value::Map(_)) {
                    return Err(fail(FailureCode::McpProtocol));
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
            let result = match (fields.get("result"), fields.get("error")) {
                (Some(Value::Map(_)), None) => Ok(JsonDocument::from_value(
                    get(fields, "result")?,
                    &self.limits,
                )
                .map_err(|_| fail(FailureCode::McpProtocol))?),
                (None, Some(Value::Map(_))) => Err(McpRpcError::new(
                    JsonDocument::from_value(get(fields, "error")?, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?,
                )?),
                _ => return Err(fail(FailureCode::McpProtocol)),
            };
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
            }
            drop(pending);
            if let Some(request) = self
                .pending
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?
                .remove(&id)
            {
                let _ = request.reply.send(McpClientReply {
                    delivery: McpDelivery::ResponseReceived,
                    result: Ok(result),
                });
            }
            // A late response cannot resurrect a timed-out pending request.
        }
        Ok(())
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
            let result = tokio::select! {
                biased;
                _ = self.stop.cancelled() => break,
                result = tokio::time::timeout(Duration::from_millis(self.config.limits.timeout_ms), handler.receive(event)) => result,
            };
            if !matches!(result, Ok(Ok(()))) {
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
                Transport::Http(http) => http.close().await,
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
