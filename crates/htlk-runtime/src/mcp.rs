//! Bounded raw MCP wire adapters. JSON ingress is checked before an SDK/Serde map
//! could erase duplicate keys or unsafe mathematical numeric tokens.
use crate::{
    failure::{FailureCode, RuntimeFailure},
    identity::InvocationId,
    records::SecurityLabels,
};
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, McpTransport, ServerIdentity,
    cbor::{Limits, Map, Value},
};
use std::{
    collections::BTreeMap, future::Future, pin::Pin, process::Stdio, sync::Arc, time::Duration,
};
/// An externally enforced, trusted-adapter-verified duplicate suppression contract.
#[derive(Clone, Debug, PartialEq)]
pub enum McpDeduplication {
    /// Endpoint enforces the invocation-stable protocol/header key.
    Protocol,
    /// Endpoint enforces this already-authored argument, whose scope and uniqueness
    /// the trusted adapter has verified. Runtime checks that its value is unchanged.
    Argument {
        /// Exact argument member name.
        name: String,
        /// Exact frozen value; never inserted into graph inputs by the runtime.
        value: Value,
    },
}
use tokio::io::{AsyncReadExt, AsyncWriteExt};
/// Sendable bounded external operation future.
pub type McpFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RuntimeFailure>> + Send + 'a>>;
/// Observed delivery, independent of whether a graph accepts a proposed result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpDelivery {
    /// No operation request was sent.
    NotSent,
    /// The request may have reached the remote operation; completion is unknown.
    PossiblySent,
    /// A valid matching protocol response ended this RPC, including remote errors.
    ResponseReceived,
}
impl McpDelivery {
    /// Stable private attempt-audit spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::PossiblySent => "possibly_sent",
            Self::ResponseReceived => "response_received",
        }
    }
    /// Reads a known private audit spelling; unknown states grant no authority.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "not_sent" => Some(Self::NotSent),
            "possibly_sent" => Some(Self::PossiblySent),
            "response_received" => Some(Self::ResponseReceived),
            _ => None,
        }
    }
}
/// Raw proposal plus trusted adapter delivery evidence. Debug omits value bytes.
pub struct McpOutcome {
    /// Delivery evidence; an error code alone does not prove completion.
    pub delivery: McpDelivery,
    /// Raw bounded result-object JSON or a redacted operational failure.
    pub result: Result<Vec<u8>, RuntimeFailure>,
}
impl std::fmt::Debug for McpOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOutcome")
            .field("delivery", &self.delivery)
            .field("response_bytes", &self.result.as_ref().ok().map(Vec::len))
            .field("failure", &self.result.as_ref().err())
            .finish()
    }
}
/// A sendable delivery-aware attempt future.
pub type McpAttemptFuture<'a> = Pin<Box<dyn Future<Output = McpOutcome> + Send + 'a>>;
/// Cancellation hint, linked to execution shutdown when created by the runtime.
/// Durable fences, rather than this hint, decide whether a result can be accepted.
pub struct McpCancellation {
    state: tokio::sync::watch::Sender<bool>,
    parents: Vec<tokio::sync::watch::Sender<bool>>,
}
impl Default for McpCancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl McpCancellation {
    /// Creates an uncancelled host/adapter signal.
    pub fn new() -> Self {
        Self {
            state: tokio::sync::watch::channel(false).0,
            parents: vec![],
        }
    }
    pub(crate) fn linked(parent: &Self) -> Self {
        Self {
            state: tokio::sync::watch::channel(false).0,
            parents: std::iter::once(parent.state.clone())
                .chain(parent.parents.iter().cloned())
                .collect(),
        }
    }
    /// Requests cooperative cancellation; the signal cannot be reset.
    pub fn cancel(&self) {
        self.state.send_replace(true);
    }
    /// Observes cancellation, including parent execution shutdown.
    pub fn is_cancelled(&self) -> bool {
        *self.state.borrow() || self.parents.iter().any(|p| *p.borrow())
    }
    /// Waits without lost wakeups, including cancellation before subscription.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut signals = std::iter::once(&self.state)
            .chain(self.parents.iter())
            .map(|state| {
                let mut receiver = state.subscribe();
                Box::pin(async move {
                    let _ = receiver.wait_for(|v| *v).await;
                })
            })
            .collect::<Vec<_>>();
        std::future::poll_fn(move |cx| {
            for signal in &mut signals {
                if signal.as_mut().poll(cx).is_ready() {
                    return std::task::Poll::Ready(());
                }
            }
            std::task::Poll::Pending
        })
        .await;
    }
}
/// A live validated connection. Connection/discovery completes before dispatch
/// authorization and intent; invoking it must not renegotiate the operation.
pub trait McpConnection: Send {
    /// Exact advertised selection on this connection.
    fn descriptor(&self) -> &JsonDocument;
    /// Sends the frozen request and reports observed delivery separately.
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a>;
    /// Best-effort finite caller-bounded cleanup of process/session resources.
    fn close(&mut self) -> McpFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
/// A pinned protocol request; frozen authored arguments are never mutated.
#[derive(Clone)]
pub struct McpRequest {
    /// Complete raw-ingress representation limits, before JSON normalization.
    pub response_limits: Limits,
    /// Local cooperative cancellation hint; never a remote exactly-once guarantee.
    pub cancellation: Arc<McpCancellation>,
    /// Maximum wait for cooperative completion after cancellation.
    pub cancellation_grace_ms: u64,
    /// Exact MCP method.
    pub method: String,
    /// Exact protocol parameters, separately audited.
    pub params: Value,
    /// Finite total attempt timeout.
    pub timeout_ms: u64,
    /// Raw response byte ceiling before parsing.
    pub max_response_bytes: usize,
    /// Verified invocation-stable protocol key if supported.
    pub idempotency_key: Option<String>,
}
/// Trusted adapter contract. Descriptors and raw replies remain independently
/// checked by runtime; adapters cannot claim replay safety from tool annotations.
pub trait McpAdapter: Send + Sync {
    /// Resolves/negotiates an authorized live connection. The runtime supplies the
    /// actual run principal, never an ordinary input field. The default wrapper
    /// supports scripted adapters and conservatively treats invoke errors as
    /// possibly sent; live adapters override this to finish discovery before send.
    fn connect<'a>(
        &'a self,
        binding: &'a McpBinding,
        _context: &'a crate::HostContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, Box<dyn McpConnection + 'a>> {
        Box::pin(async move {
            let descriptor = self.descriptor(binding, limits).await?;
            Ok(Box::new(AdapterConnection {
                adapter: self,
                binding,
                descriptor,
            }) as Box<dyn McpConnection + 'a>)
        })
    }
    /// Verifies actual endpoint deduplication, including authored-key scope and
    /// uniqueness. The default grants none; MCP annotations do not grant it.
    fn deduplication(
        &self,
        _binding: &McpBinding,
        _invocation: InvocationId,
        _params: &Value,
        _limits: &Limits,
    ) -> Result<Option<McpDeduplication>, RuntimeFailure> {
        Ok(None)
    }
    /// Retrieves the selected exact advertised descriptor under trusted deployment.
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument>;
    /// Sends one pinned request; returns raw result-object JSON before acceptance.
    fn invoke<'a>(&'a self, binding: &'a McpBinding, request: McpRequest)
    -> McpFuture<'a, Vec<u8>>;
}
struct AdapterConnection<'a, A: McpAdapter + ?Sized> {
    adapter: &'a A,
    binding: &'a McpBinding,
    descriptor: JsonDocument,
}
impl<A: McpAdapter + ?Sized> McpConnection for AdapterConnection<'_, A> {
    fn descriptor(&self) -> &JsonDocument {
        &self.descriptor
    }
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(fail(FailureCode::Cancelled)),
                };
            }
            let result = self.adapter.invoke(self.binding, request).await;
            let delivery = if result.is_ok() {
                McpDelivery::ResponseReceived
            } else {
                McpDelivery::PossiblySent
            };
            McpOutcome { delivery, result }
        })
    }
}
/// Trusted replay/approval/result-protection facts, separate from executable pins.
#[derive(Clone, Debug, Default)]
pub struct McpPolicy {
    /// Host vouches replay safe for this compound operation.
    pub replay_safe: bool,
    /// Trusted external-result protection.
    pub result_labels: SecurityLabels,
    /// Exact-action approval authority, if required.
    pub approval_authority: Option<String>,
    /// Deployment security policy version.
    pub policy_version: String,
}
/// Host connection configuration, never part of executable semantic identity.
#[derive(Clone)]
pub enum McpEndpoint {
    /// Launched local server; environment is explicit trusted host configuration.
    Stdio {
        /// Program to launch.
        program: String,
        /// Exact arguments.
        arguments: Vec<String>,
        /// Additional explicit environment variables.
        environment: BTreeMap<String, String>,
    },
    /// Streamable HTTP server with operator-supplied authentication headers.
    Http {
        /// Trusted deployment URL.
        url: String,
        /// Secrets/configuration supplied only by the host.
        headers: BTreeMap<String, String>,
    },
}
/// Native bounded stdio/Streamable-HTTP implementation. Each operation negotiates
/// a fresh finite session and rechecks the exact descriptor before invoking it.
pub struct NativeMcpAdapter {
    endpoints: BTreeMap<String, McpEndpoint>,
    timeout_ms: u64,
    idempotent_bindings: std::collections::BTreeSet<htlk_executable::digest::Digest>,
}
impl NativeMcpAdapter {
    /// Constructs trusted deployment routing with a finite discovery deadline.
    ///
    /// # Errors
    /// Returns zero/unsupported timeout configuration.
    pub fn new(
        endpoints: BTreeMap<String, McpEndpoint>,
        timeout_ms: u64,
    ) -> Result<Self, RuntimeFailure> {
        if timeout_ms == 0
            || timeout_ms > i64::MAX as u64
            || std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .is_none()
        {
            return Err(fail(FailureCode::Profile));
        }
        Ok(Self {
            endpoints,
            timeout_ms,
            idempotent_bindings: Default::default(),
        })
    }
    /// Configures exact compound operation digests whose HTTP endpoint is known by
    /// the operator to enforce `Idempotency-Key`. This is trusted deployment policy,
    /// never a promise inferred from tool annotations or graph input data.
    pub fn with_protocol_idempotency(
        mut self,
        bindings: std::collections::BTreeSet<htlk_executable::digest::Digest>,
    ) -> Self {
        self.idempotent_bindings = bindings;
        self
    }
    async fn ready(
        &self,
        binding: &McpBinding,
        limits: &Limits,
    ) -> Result<NativeConnection, RuntimeFailure> {
        let endpoint = self
            .endpoints
            .get(binding.server().deployment_id())
            .ok_or_else(|| fail(FailureCode::McpIdentity))?;
        let mut session = Session::new(endpoint, binding.server(), limits.clone()).await?;
        let result = session.prepare(binding).await;
        match result {
            Ok(descriptor) => Ok(NativeConnection {
                session,
                descriptor,
                binding: binding.clone(),
            }),
            // Preserve the primary identity/protocol failure. Transport drop
            // closes/kills the failed session; cleanup cannot turn it into timeout.
            Err(error) => Err(error),
        }
    }
}
impl McpAdapter for NativeMcpAdapter {
    fn connect<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a crate::HostContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, Box<dyn McpConnection + 'a>> {
        Box::pin(async move {
            tokio::time::timeout(
                Duration::from_millis(self.timeout_ms),
                self.ready(binding, limits),
            )
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
            .map(|c| Box::new(c) as Box<dyn McpConnection + 'a>)
        })
    }
    fn deduplication(
        &self,
        binding: &McpBinding,
        _: InvocationId,
        _: &Value,
        limits: &Limits,
    ) -> Result<Option<McpDeduplication>, RuntimeFailure> {
        if binding.server().transport() == McpTransport::StreamableHttp
            && self.idempotent_bindings.contains(
                &binding
                    .digest(limits)
                    .map_err(|_| fail(FailureCode::McpIdentity))?,
            )
        {
            Ok(Some(McpDeduplication::Protocol))
        } else {
            Ok(None)
        }
    }
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_millis(self.timeout_ms), async {
                let mut connection = self.ready(binding, limits).await?;
                let descriptor = connection.descriptor.clone();
                let _ = tokio::time::timeout(
                    Duration::from_millis(self.timeout_ms.min(1000)),
                    connection.close(),
                )
                .await;
                Ok(descriptor)
            })
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
        })
    }
    fn invoke<'a>(
        &'a self,
        binding: &'a McpBinding,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(fail(FailureCode::Cancelled));
            }
            if request.timeout_ms == 0
                || request.cancellation_grace_ms == 0
                || [request.timeout_ms, request.cancellation_grace_ms]
                    .into_iter()
                    .any(|ms| {
                        ms > i64::MAX as u64
                            || std::time::Instant::now()
                                .checked_add(Duration::from_millis(ms))
                                .is_none()
                    })
            {
                return Err(fail(FailureCode::Profile));
            }
            let grace = request.cancellation_grace_ms;
            tokio::time::timeout(Duration::from_millis(request.timeout_ms), async {
                let mut limits = request.response_limits.clone();
                limits.max_document_bytes =
                    limits.max_document_bytes.min(request.max_response_bytes);
                let mut connection = self.ready(binding, &limits).await?;
                let result = if connection.descriptor.digest() != binding.descriptor() {
                    Err(fail(FailureCode::McpDescriptor))
                } else {
                    connection.invoke(request).await.result
                };
                let _ =
                    tokio::time::timeout(Duration::from_millis(grace), connection.close()).await;
                result
            })
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
        })
    }
}
struct NativeConnection {
    session: Session,
    descriptor: JsonDocument,
    binding: McpBinding,
}
impl McpConnection for NativeConnection {
    fn descriptor(&self) -> &JsonDocument {
        &self.descriptor
    }
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a> {
        Box::pin(async move {
            self.session.delivery = McpDelivery::NotSent;
            if request.cancellation.is_cancelled() {
                return McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(fail(FailureCode::Cancelled)),
                };
            }
            if request.timeout_ms == 0
                || request.cancellation_grace_ms == 0
                || request.response_limits.validate().is_err()
            {
                return McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(fail(FailureCode::Profile)),
                };
            }
            self.session.limits.max_document_bytes = self
                .session
                .limits
                .max_document_bytes
                .min(request.response_limits.max_document_bytes)
                .min(request.max_response_bytes);
            self.session.limits.max_depth = self
                .session
                .limits
                .max_depth
                .min(request.response_limits.max_depth);
            let notifier = match self.session.cancellation_notifier(self.session.next + 1) {
                Ok(n) => n,
                Err(e) => {
                    return McpOutcome {
                        delivery: McpDelivery::NotSent,
                        result: Err(e),
                    };
                }
            };
            let Some(deadline) =
                tokio::time::Instant::now().checked_add(Duration::from_millis(request.timeout_ms))
            else {
                return McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(fail(FailureCode::Profile)),
                };
            };
            let result = {
                let rpc = self.session.rpc(
                    &request.method,
                    request.params,
                    request.idempotency_key.as_deref(),
                );
                tokio::pin!(rpc);
                tokio::time::timeout_at(deadline,async{
                tokio::select!{
                    biased;
                    _=request.cancellation.cancelled()=>{
                        let grace=tokio::time::Instant::now().checked_add(Duration::from_millis(request.cancellation_grace_ms)).unwrap_or(deadline).min(deadline);
                            let notification=notifier.send();tokio::pin!(notification);
                            tokio::time::timeout_at(grace,async{tokio::select!{result=rpc.as_mut()=>result,_=notification.as_mut()=>rpc.as_mut().await}}).await.unwrap_or_else(|_|Err(fail(FailureCode::McpTimeout)))
                    },
                    result=rpc.as_mut()=>result,
                }
            }).await.unwrap_or_else(|_|Err(fail(FailureCode::McpTimeout)))
            };
            let delivery = self.session.delivery;
            if self.session.expired {
                self.session.reset_expired();
                // Renew protocol state, never replay the operation here. The
                // coordinator retains the original delivery evidence and alone
                // decides whether another authored attempt is authorized.
                let renewal =
                    tokio::time::timeout_at(deadline, self.session.prepare(&self.binding)).await;
                if let Ok(Ok(descriptor)) = renewal
                    && descriptor.digest() == self.binding.descriptor()
                {
                    self.descriptor = descriptor;
                }
            }
            let result = result.and_then(|value| {
                JsonDocument::from_value(&value, &self.session.limits)
                    .map(|d| d.as_bytes().to_vec())
                    .map_err(|_| fail(FailureCode::McpProtocol))
            });
            McpOutcome { delivery, result }
        })
    }
    fn close(&mut self) -> McpFuture<'_, ()> {
        Box::pin(self.session.close())
    }
}
enum CancellationNotifier {
    Stdio {
        input: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
        bytes: Vec<u8>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        headers: BTreeMap<String, String>,
        session: Option<String>,
        bytes: Vec<u8>,
    },
}
impl CancellationNotifier {
    async fn send(self) -> Result<(), RuntimeFailure> {
        match self {
            Self::Stdio { input, bytes } => {
                let mut writer = input.lock().await;
                writer
                    .write_all(&bytes)
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                writer
                    .flush()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                Ok(())
            }
            Self::Http {
                client,
                url,
                headers,
                session,
                bytes,
            } => {
                let mut request = client
                    .post(url)
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .header("mcp-protocol-version", "2025-11-25")
                    .body(bytes);
                for (k, v) in headers {
                    request = request.header(k, v)
                }
                if let Some(session) = session {
                    request = request.header("mcp-session-id", session)
                }
                request
                    .send()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                Ok(())
            }
        }
    }
}
enum Transport {
    Stdio {
        child: tokio::process::Child,
        input: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
        output: tokio::process::ChildStdout,
    },
    Http {
        client: reqwest::Client,
        url: String,
        headers: BTreeMap<String, String>,
        session: Option<String>,
    },
}
struct Session {
    transport: Transport,
    next: i64,
    limits: Limits,
    delivery: McpDelivery,
    expired: bool,
}
impl Session {
    fn reset_expired(&mut self) {
        if let Transport::Http { session, .. } = &mut self.transport {
            *session = None;
        }
        self.expired = false;
    }
    async fn prepare(&mut self, binding: &McpBinding) -> Result<JsonDocument, RuntimeFailure> {
        for renewal in 0..2 {
            let result = async {
                let capabilities = self.initialize(binding.server()).await?;
                let capability = match binding.kind() {
                    McpBindingKind::Tool { .. } => "tools",
                    McpBindingKind::Prompt { .. } => "prompts",
                    _ => "resources",
                };
                if !matches!(as_map(&capabilities)?.get(capability), Some(Value::Map(_))) {
                    return Err(fail(FailureCode::McpDescriptor));
                }
                let descriptor = self.descriptor(binding).await?;
                let task_calls = as_map(&capabilities)?
                    .get("tasks")
                    .and_then(|v| as_map(v).ok()?.get("requests"))
                    .and_then(|v| as_map(v).ok()?.get("tools"))
                    .and_then(|v| as_map(v).ok()?.get("call"))
                    .is_some();
                let task_required = as_map(descriptor.value())?
                    .get("execution")
                    .and_then(|v| as_map(v).ok()?.get("taskSupport"))
                    == Some(&Value::Text("required".into()));
                if matches!(binding.kind(), McpBindingKind::Tool { .. })
                    && task_calls
                    && task_required
                {
                    return Err(fail(FailureCode::McpDescriptor));
                }
                Ok(descriptor)
            }
            .await;
            if self.expired && renewal == 0 {
                self.reset_expired();
            } else {
                return result;
            }
        }
        Err(fail(FailureCode::McpTransport))
    }
    fn cancellation_notifier(&self, id: i64) -> Result<CancellationNotifier, RuntimeFailure> {
        let message = obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("method", Value::Text("notifications/cancelled".into())),
            (
                "params",
                obj([
                    ("requestId", Value::Integer(id)),
                    ("reason", Value::Text("runtime cancellation".into())),
                ])?,
            ),
        ])?;
        let bytes = JsonDocument::from_value(&message, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?
            .as_bytes()
            .to_vec();
        Ok(match &self.transport {
            Transport::Stdio { input, .. } => CancellationNotifier::Stdio {
                input: input.clone(),
                bytes,
            },
            Transport::Http {
                client,
                url,
                headers,
                session,
            } => CancellationNotifier::Http {
                client: client.clone(),
                url: url.clone(),
                headers: headers.clone(),
                session: session.clone(),
                bytes,
            },
        })
    }
    async fn close(&mut self) -> Result<(), RuntimeFailure> {
        match &mut self.transport {
            Transport::Stdio { child, .. } => child
                .kill()
                .await
                .map_err(|_| fail(FailureCode::McpTransport)),
            Transport::Http {
                client,
                url,
                headers,
                session,
            } => {
                if let Some(session) = session.take() {
                    let mut request = client
                        .delete(url.as_str())
                        .header("mcp-session-id", session)
                        .header("mcp-protocol-version", "2025-11-25");
                    for (k, v) in headers.iter() {
                        request = request.header(k, v)
                    }
                    let _ = request
                        .send()
                        .await
                        .map_err(|_| fail(FailureCode::McpTransport))?;
                }
                Ok(())
            }
        }
    }
    async fn new(
        endpoint: &McpEndpoint,
        server: &ServerIdentity,
        limits: Limits,
    ) -> Result<Self, RuntimeFailure> {
        limits.validate().map_err(|_| fail(FailureCode::Profile))?;
        let transport = match endpoint {
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
                Transport::Stdio {
                    child,
                    input: Arc::new(tokio::sync::Mutex::new(input)),
                    output,
                }
            }
            McpEndpoint::Http { url, headers }
                if server.transport() == McpTransport::StreamableHttp =>
            {
                Transport::Http {
                    client: reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .no_gzip()
                        .no_brotli()
                        .no_deflate()
                        .no_zstd()
                        .build()
                        .map_err(|_| fail(FailureCode::McpTransport))?,
                    url: url.clone(),
                    headers: headers.clone(),
                    session: None,
                }
            }
            _ => return Err(fail(FailureCode::McpIdentity)),
        };
        Ok(Self {
            transport,
            next: 0,
            limits,
            delivery: McpDelivery::NotSent,
            expired: false,
        })
    }
    async fn initialize(&mut self, server: &ServerIdentity) -> Result<Value, RuntimeFailure> {
        let params = obj([
            ("protocolVersion", Value::Text("2025-11-25".into())),
            ("capabilities", obj([])?),
            (
                "clientInfo",
                obj([
                    ("name", Value::Text("htlk".into())),
                    ("version", Value::Text("0.1".into())),
                ])?,
            ),
        ])?;
        let result = self.rpc("initialize", params, None).await?;
        let map = as_map(&result)?;
        if as_text(get(map, "protocolVersion")?)? != "2025-11-25" {
            return Err(fail(FailureCode::McpIdentity));
        }
        let info = as_map(get(map, "serverInfo")?)?;
        if as_text(get(info, "name")?)? != server.implementation_name()
            || as_text(get(info, "version")?)? != server.implementation_version()
        {
            return Err(fail(FailureCode::McpIdentity));
        }
        let capabilities = get(map, "capabilities")?.clone();
        as_map(&capabilities)?;
        self.notify("notifications/initialized").await?;
        Ok(capabilities)
    }
    async fn descriptor(&mut self, binding: &McpBinding) -> Result<JsonDocument, RuntimeFailure> {
        let (method, collection, key, name) = match binding.kind() {
            McpBindingKind::Tool { name, .. } => ("tools/list", "tools", "name", name),
            McpBindingKind::Resource { uri } => ("resources/list", "resources", "uri", uri),
            McpBindingKind::Template { uri_template } => (
                "resources/templates/list",
                "resourceTemplates",
                "uriTemplate",
                uri_template,
            ),
            McpBindingKind::Prompt { name } => ("prompts/list", "prompts", "name", name),
        };
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1024 {
            let params = if let Some(cursor) = &cursor {
                obj([("cursor", Value::Text(cursor.clone()))])?
            } else {
                obj([])?
            };
            let result = self.rpc(method, params, None).await?;
            let fields = as_map(&result)?;
            let Value::Array(entries) = get(fields, collection)? else {
                return Err(fail(FailureCode::McpProtocol));
            };
            for descriptor in entries {
                if as_map(descriptor)?
                    .get(key)
                    .is_some_and(|v| matches!(v,Value::Text(s)if s==name))
                {
                    return JsonDocument::from_value(descriptor, &self.limits)
                        .map_err(|_| fail(FailureCode::McpDescriptor));
                }
            }
            cursor = match fields.get("nextCursor") {
                None => return Err(fail(FailureCode::McpDescriptor)),
                Some(Value::Text(cursor))
                    if seen.insert(htlk_executable::digest::hash_bytes(cursor.as_bytes())) =>
                {
                    Some(cursor.clone())
                }
                _ => return Err(fail(FailureCode::McpProtocol)),
            };
        }
        Err(fail(FailureCode::McpProtocol))
    }
    async fn notify(&mut self, method: &str) -> Result<(), RuntimeFailure> {
        let message = obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("method", Value::Text(method.into())),
        ])?;
        self.exchange(message, None, None).await.map(|_| ())
    }
    async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        key: Option<&str>,
    ) -> Result<Value, RuntimeFailure> {
        self.delivery = McpDelivery::NotSent;
        self.next += 1;
        let id = self.next;
        let message = obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("id", Value::Integer(id)),
            ("method", Value::Text(method.into())),
            ("params", params),
        ])?;
        let reply = self
            .exchange(message, Some(id), key)
            .await?
            .ok_or_else(|| fail(FailureCode::McpProtocol))?;
        let fields = as_map(&reply)?;
        if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
            || fields.get("id") != Some(&Value::Integer(id))
        {
            return Err(fail(FailureCode::McpProtocol));
        }
        match (fields.get("result"), fields.get("error")) {
            (Some(_), None) => self.delivery = McpDelivery::ResponseReceived,
            (None, Some(Value::Map(error)))
                if matches!(error.get("code"), Some(Value::Integer(_)))
                    && matches!(error.get("message"), Some(Value::Text(_))) =>
            {
                self.delivery = McpDelivery::ResponseReceived;
                return Err(fail(FailureCode::McpProtocol));
            }
            _ => return Err(fail(FailureCode::McpProtocol)),
        }
        Ok(get(fields, "result")?.clone())
    }
    async fn exchange(
        &mut self,
        message: Value,
        id: Option<i64>,
        key: Option<&str>,
    ) -> Result<Option<Value>, RuntimeFailure> {
        let bytes = JsonDocument::from_value(&message, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?
            .as_bytes()
            .to_vec();
        if id.is_some() {
            self.delivery = McpDelivery::PossiblySent;
        }
        match &mut self.transport {
            Transport::Stdio {
                child,
                input,
                output,
            } => {
                let _ = child.id();
                let mut writer = input.lock().await;
                writer
                    .write_all(&bytes)
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                writer
                    .flush()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                drop(writer);
                if id.is_none() {
                    return Ok(None);
                }
                for _ in 0..1024 {
                    let mut line = Vec::new();
                    loop {
                        let mut byte = [0];
                        if output
                            .read(&mut byte)
                            .await
                            .map_err(|_| fail(FailureCode::McpTransport))?
                            == 0
                        {
                            return Err(fail(FailureCode::McpTransport));
                        }
                        if byte[0] == b'\n' {
                            break;
                        }
                        if line.len() >= self.limits.max_document_bytes {
                            return Err(fail(FailureCode::McpProtocol));
                        }
                        line.push(byte[0]);
                    }
                    let document = JsonDocument::new(&line, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?;
                    let fields = as_map(document.value())?;
                    if let Some(bytes) = server_reply(document.value(), &self.limits)? {
                        CancellationNotifier::Stdio {
                            input: input.clone(),
                            bytes,
                        }
                        .send()
                        .await?;
                        continue;
                    }
                    if fields.get("method").is_some() {
                        continue;
                    }
                    if fields.get("id") == id.map(Value::Integer).as_ref() {
                        return Ok(Some(document.value().clone()));
                    }
                }
                Err(fail(FailureCode::McpProtocol))
            }
            Transport::Http {
                client,
                url,
                headers,
                session,
            } => {
                let mut request = client
                    .post(url.as_str())
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .header("mcp-protocol-version", "2025-11-25")
                    .body(bytes);
                for (k, v) in headers.iter() {
                    request = request.header(k, v);
                }
                if let Some(session) = session.as_ref() {
                    request = request.header("mcp-session-id", session)
                }
                if let Some(key) = key {
                    request = request.header("idempotency-key", key)
                }
                let mut response = request
                    .send()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                if !response.status().is_success() {
                    if response.status() == reqwest::StatusCode::NOT_FOUND && session.is_some() {
                        self.expired = true;
                    }
                    return Err(fail(FailureCode::McpTransport));
                }
                if let Some(value) = response.headers().get("mcp-session-id") {
                    let value = value.to_str().map_err(|_| fail(FailureCode::McpProtocol))?;
                    if session.as_ref().is_some_and(|old| old != value) {
                        return Err(fail(FailureCode::McpProtocol));
                    }
                    *session = Some(value.into());
                }
                if id.is_none() {
                    return Ok(None);
                }
                let mime = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.split(';').next())
                    .map(str::trim)
                    .ok_or_else(|| fail(FailureCode::McpProtocol))?;
                let sse = mime.eq_ignore_ascii_case("text/event-stream");
                if !sse && !mime.eq_ignore_ascii_case("application/json") {
                    return Err(fail(FailureCode::McpProtocol));
                }
                let mut body = Vec::new();
                let mut seen = 0;
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?
                {
                    if body
                        .len()
                        .checked_add(chunk.len())
                        .is_none_or(|n| n > self.limits.max_document_bytes)
                    {
                        return Err(fail(FailureCode::McpProtocol));
                    }
                    body.extend_from_slice(&chunk);
                    if sse {
                        for value in sse_messages(&body, &mut seen, &self.limits)? {
                            if let Some(bytes) = server_reply(&value, &self.limits)? {
                                CancellationNotifier::Http {
                                    client: client.clone(),
                                    url: url.clone(),
                                    headers: headers.clone(),
                                    session: session.clone(),
                                    bytes,
                                }
                                .send()
                                .await?;
                                continue;
                            }
                            let fields = as_map(&value)?;
                            if fields.get("method").is_none()
                                && fields.get("id") == id.map(Value::Integer).as_ref()
                            {
                                return Ok(Some(value));
                            }
                        }
                    }
                }
                if sse {
                    return Err(fail(FailureCode::McpProtocol));
                }
                Ok(Some(
                    JsonDocument::new(&body, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?
                        .value()
                        .clone(),
                ))
            }
        }
    }
}
fn server_reply(value: &Value, limits: &Limits) -> Result<Option<Vec<u8>>, RuntimeFailure> {
    let fields = as_map(value)?;
    let Some(method) = fields.get("method") else {
        return Ok(None);
    };
    if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
        || fields.get("result").is_some()
        || fields.get("error").is_some()
    {
        return Err(fail(FailureCode::McpProtocol));
    }
    let method = as_text(method)?;
    let Some(id) = fields.get("id") else {
        return Ok(None);
    };
    if !matches!(id, Value::Integer(_) | Value::Text(_)) {
        return Err(fail(FailureCode::McpProtocol));
    }
    let (key, result) = if method == "ping"
        && fields
            .get("params")
            .is_none_or(|p| matches!(p, Value::Map(_)))
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
    let message = obj([
        ("jsonrpc", Value::Text("2.0".into())),
        ("id", id.clone()),
        (key, result),
    ])?;
    Ok(Some(
        JsonDocument::from_value(&message, limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?
            .as_bytes()
            .to_vec(),
    ))
}
fn sse_messages(
    bytes: &[u8],
    seen: &mut usize,
    limits: &Limits,
) -> Result<Vec<Value>, RuntimeFailure> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => return Ok(vec![]),
        Err(_) => return Err(fail(FailureCode::McpProtocol)),
    };
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let complete = normalized.matches("\n\n").count();
    if complete > 1024 {
        return Err(fail(FailureCode::McpProtocol));
    }
    let mut messages = Vec::new();
    for event in normalized.split("\n\n").take(complete).skip(*seen) {
        let data = event
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let document = JsonDocument::new(data.as_bytes(), limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        messages.push(document.value().clone());
    }
    *seen = complete;
    Ok(messages)
}
fn obj<const N: usize>(entries: [(&str, Value); N]) -> Result<Value, RuntimeFailure> {
    Ok(Value::Map(
        Map::try_from_entries(entries.into_iter().map(|(k, v)| (k.into(), v)))
            .map_err(|_| fail(FailureCode::McpProtocol))?,
    ))
}
fn as_map(value: &Value) -> Result<&Map, RuntimeFailure> {
    if let Value::Map(m) = value {
        Ok(m)
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn get<'a>(m: &'a Map, key: &str) -> Result<&'a Value, RuntimeFailure> {
    m.get(key).ok_or_else(|| fail(FailureCode::McpProtocol))
}
fn as_text(value: &Value) -> Result<&str, RuntimeFailure> {
    if let Value::Text(s) = value {
        Ok(s)
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn fail(code: FailureCode) -> RuntimeFailure {
    RuntimeFailure::new(code)
}
