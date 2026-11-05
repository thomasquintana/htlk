//! Bounded MCP adapters and persistent bidirectional native client services.
//! JSON ingress is checked before normalization can erase duplicate keys or unsafe
//! mathematical numeric tokens. Runtime admission remains independent of adapters.
use crate::{
    failure::{FailureCode, RuntimeFailure},
    identity::{InvocationId, RunId},
    records::SecurityLabels,
};
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, McpTransport,
    cbor::{Limits, Map, Value},
};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

pub mod client;
mod native_sessions;
use native_sessions::NativeSessions;

/// An externally enforced, trusted-adapter-verified duplicate suppression contract.
#[derive(Clone, Debug, PartialEq)]
pub enum McpDeduplication {
    /// Endpoint enforces the invocation-stable protocol/header key.
    Protocol,
    /// Endpoint enforces this authored argument, whose scope and uniqueness the
    /// trusted adapter has verified. Runtime checks that its value is unchanged.
    Argument {
        /// Exact argument member name.
        name: String,
        /// Exact frozen value; never inserted into graph inputs by the runtime.
        value: Value,
    },
}

/// Trusted run ownership and lifetime, supplied by the runtime rather than inputs.
#[derive(Clone)]
pub struct McpRunContext {
    /// Durable run identity. Different runs never share native sessions.
    pub run_id: RunId,
    /// Authenticated principal owning this run.
    pub host: crate::HostContext,
    /// Current durable ownership generation.
    pub fence: i64,
    /// Stops session I/O when local ownership or execution ends.
    pub cancellation: Arc<McpCancellation>,
}
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
    /// Reads known audit spellings; unknown states grant no authority.
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

/// Cooperative cancellation, subordinate to durable acceptance fences.
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

/// Checked operation handle. Discovery finishes before dispatch authorization and
/// intent; invocation does not renegotiate the selected operation.
pub trait McpConnection: Send {
    /// Exact advertised selection on this connection.
    fn descriptor(&self) -> &JsonDocument;
    /// Sends the frozen request and reports observed delivery separately.
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a>;
    /// Takes the last invocation's protected correlated JSON-RPC error. Runtime
    /// records it separately from redacted graph failures and rechecks its bounds.
    /// A subsequent invocation must clear any previously retained error.
    fn take_protocol_error(&mut self) -> Option<client::McpRpcError> {
        None
    }
    /// Releases this operation handle; run-owned transport sessions survive.
    fn close(&mut self) -> McpFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
/// Pinned protocol request. Frozen authored arguments are never mutated.
#[derive(Clone)]
pub struct McpRequest {
    /// Complete raw-ingress representation limits before normalization.
    pub response_limits: Limits,
    /// Local cancellation hint, never a remote exactly-once guarantee.
    pub cancellation: Arc<McpCancellation>,
    /// Maximum cooperative completion wait after cancellation.
    pub cancellation_grace_ms: u64,
    /// Exact protocol method.
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
/// Trusted adapter contract. Runtime independently checks descriptors and replies;
/// tool annotations cannot establish replay safety.
pub trait McpAdapter: Send + Sync {
    /// Resolves and negotiates a checked operation handle. The default supports
    /// scripted adapters and conservatively treats invocation errors as possibly sent.
    fn connect<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, Box<dyn McpConnection + 'a>> {
        Box::pin(async move {
            let descriptor = self.descriptor(binding, context, limits).await?;
            Ok(Box::new(AdapterConnection {
                adapter: self,
                binding,
                context,
                descriptor,
            }) as Box<dyn McpConnection + 'a>)
        })
    }
    /// Verifies externally enforced deduplication. The default grants none.
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
        context: &'a McpRunContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument>;
    /// Sends one pinned request and returns raw result-object JSON before acceptance.
    fn invoke<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>>;
}
struct AdapterConnection<'a, A: McpAdapter + ?Sized> {
    adapter: &'a A,
    binding: &'a McpBinding,
    context: &'a McpRunContext,
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
            let result = self
                .adapter
                .invoke(self.binding, self.context, request)
                .await;
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
    /// Trusted interpretation, independently matched against the compiled policy.
    pub result_policy: htlk_executable::ToolResultPolicy,
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
    /// Launched local server; environment is trusted host configuration.
    Stdio {
        /// Program to launch.
        program: String,
        /// Exact arguments.
        arguments: Vec<String>,
        /// Additional explicit environment variables.
        environment: BTreeMap<String, String>,
    },
    /// Streamable HTTP with operator-supplied authentication headers.
    Http {
        /// Trusted deployment URL.
        url: String,
        /// Secrets and configuration supplied only by the host.
        headers: BTreeMap<String, String>,
    },
}

/// Native adapter backed by the persistent bidirectional client dispatcher. One
/// run-owned session per compound server preserves remote state between operations.
pub struct NativeMcpAdapter {
    sessions: NativeSessions,
    timeout_ms: u64,
    idempotent_bindings: std::collections::BTreeSet<htlk_executable::digest::Digest>,
}
impl NativeMcpAdapter {
    /// Constructs the graph adapter with explicit validated client limits and host
    /// event delivery. Effectful services require durable interaction admission.
    ///
    /// # Errors
    /// Returns unsupported service configuration or transport-owner startup failure.
    pub fn with_client_config(
        endpoints: BTreeMap<String, McpEndpoint>,
        config: client::McpClientConfig,
    ) -> Result<Self, RuntimeFailure> {
        let timeout_ms = config.limits().timeout_ms;
        Ok(Self {
            sessions: NativeSessions::configured(endpoints, config)?,
            timeout_ms,
            idempotent_bindings: Default::default(),
        })
    }
    /// Stops the transport owner and acknowledges bounded cleanup.
    ///
    /// # Errors
    /// Returns shutdown failure or a two-second timeout.
    pub async fn shutdown(&self) -> Result<(), RuntimeFailure> {
        self.sessions.shutdown().await
    }
    /// Constructs trusted deployment routing and a dedicated transport owner.
    ///
    /// # Errors
    /// Returns invalid finite discovery timeout or transport-owner startup failure.
    pub fn new(
        endpoints: BTreeMap<String, McpEndpoint>,
        timeout_ms: u64,
    ) -> Result<Self, RuntimeFailure> {
        valid_timeout(timeout_ms)?;
        Ok(Self {
            sessions: NativeSessions::launch(endpoints, timeout_ms)?,
            timeout_ms,
            idempotent_bindings: Default::default(),
        })
    }
    /// Configures exact operation digests whose HTTP endpoints enforce the
    /// invocation-stable Idempotency-Key. This is trusted deployment policy.
    pub fn with_protocol_idempotency(
        mut self,
        bindings: std::collections::BTreeSet<htlk_executable::digest::Digest>,
    ) -> Self {
        self.idempotent_bindings = bindings;
        self
    }
}
impl McpAdapter for NativeMcpAdapter {
    fn connect<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, Box<dyn McpConnection + 'a>> {
        Box::pin(async move {
            tokio::time::timeout(
                Duration::from_millis(self.timeout_ms),
                self.sessions.connect(binding, context, limits),
            )
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
            .map(|connection| Box::new(connection) as Box<dyn McpConnection + 'a>)
        })
    }
    fn deduplication(
        &self,
        binding: &McpBinding,
        _: InvocationId,
        _: &Value,
        limits: &Limits,
    ) -> Result<Option<McpDeduplication>, RuntimeFailure> {
        Ok(
            (binding.server().transport() == McpTransport::StreamableHttp
                && self.idempotent_bindings.contains(
                    &binding
                        .digest(limits)
                        .map_err(|_| fail(FailureCode::McpIdentity))?,
                ))
            .then_some(McpDeduplication::Protocol),
        )
    }
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            let mut connection = self.connect(binding, context, limits).await?;
            let descriptor = connection.descriptor().clone();
            connection.close().await?;
            Ok(descriptor)
        })
    }
    fn invoke<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(fail(FailureCode::Cancelled));
            }
            valid_timeout(request.timeout_ms)?;
            valid_timeout(request.cancellation_grace_ms)?;
            tokio::time::timeout(Duration::from_millis(request.timeout_ms), async {
                let mut limits = request.response_limits.clone();
                limits.max_document_bytes =
                    limits.max_document_bytes.min(request.max_response_bytes);
                let mut connection = self.sessions.connect(binding, context, &limits).await?;
                let result = if connection.descriptor().digest() != binding.descriptor() {
                    Err(fail(FailureCode::McpDescriptor))
                } else {
                    connection.invoke(request).await.result
                };
                connection.close().await?;
                result
            })
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
        })
    }
}

fn valid_timeout(ms: u64) -> Result<(), RuntimeFailure> {
    if ms == 0
        || ms > i64::MAX as u64
        || std::time::Instant::now()
            .checked_add(Duration::from_millis(ms))
            .is_none()
    {
        Err(fail(FailureCode::Profile))
    } else {
        Ok(())
    }
}
fn obj<const N: usize>(entries: [(&str, Value); N]) -> Result<Value, RuntimeFailure> {
    Ok(Value::Map(
        Map::try_from_entries(entries.into_iter().map(|(key, value)| (key.into(), value)))
            .map_err(|_| fail(FailureCode::McpProtocol))?,
    ))
}
fn as_map(value: &Value) -> Result<&Map, RuntimeFailure> {
    if let Value::Map(map) = value {
        Ok(map)
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn get<'a>(map: &'a Map, key: &str) -> Result<&'a Value, RuntimeFailure> {
    map.get(key).ok_or_else(|| fail(FailureCode::McpProtocol))
}
fn as_text(value: &Value) -> Result<&str, RuntimeFailure> {
    if let Value::Text(text) = value {
        Ok(text)
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn fail(code: FailureCode) -> RuntimeFailure {
    RuntimeFailure::new(code)
}
