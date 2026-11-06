//! Host-facing configuration and identities for the bidirectional MCP client.
//!
//! Configuration describes installed services. It does not itself open a transport
//! or grant a callback authority to spend a run's budget.

use super::{McpCancellation, McpFuture, fail, obj};
use crate::{
    HostContext,
    failure::{FailureCode, RuntimeFailure},
    identity::{AttemptId, InvocationId, RunId},
};
use htlk_executable::{
    JsonDocument, ServerIdentity,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

/// The sole protocol version implemented by this client profile.
pub const MCP_VERSION: &str = "2025-11-25";

/// Frozen native graph-tool task selection, independent of authored arguments.
/// Enabling deferred execution requires the runtime's actual durable store/clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpGraphTaskPolicy {
    /// Ordinary calls only; required task tools fail before dispatch.
    #[default]
    Reject,
    /// Use tasks only when required by the exact pinned descriptor.
    Required {
        /// Requested finite remote retention in milliseconds.
        ttl_ms: u64,
    },
    /// Prefer tasks for optional descriptors too; forbidden descriptors stay inline.
    Prefer {
        /// Requested finite remote retention in milliseconds.
        ttl_ms: u64,
    },
}

pub mod auth;
mod session;
pub use session::{McpClientReply, McpClientSession, NativeMcpClient};
mod interactions;
mod notifications;
mod protocol;
mod task_receiver;
mod tasks;
pub use task_receiver::*;
pub use tasks::*;
mod services;
mod subscriptions;
mod url_interactions;
mod work;
mod workers;
pub use interactions::{
    McpAwaitReply, McpClientAction, McpClientAuthorization, McpClientAuthorizer,
    McpInteractionHost, McpInteractionPage, McpInteractionReceipt, McpInteractionView,
};
pub use notifications::{
    McpCatalogChanged, McpNotification, McpNotificationIgnored, McpNotificationReply, McpProgress,
    McpProgressReporter, McpProgressToken, McpResourceUpdateState,
};
pub use protocol::{
    McpClientValidationError, McpIncomingKind, validate_client_request, validate_client_result,
};
pub use services::{
    McpCatalog, McpCatalogCursor, McpCatalogKind, McpCatalogLimits, McpCatalogPage,
    McpCatalogStatus, McpCatalogStop, McpCompletion, McpCompletionReference, McpCompletionRequest,
    McpLoggingLevel, McpResourceSubscriptionAck, McpResourceSubscriptionAction, McpServiceError,
    McpServiceLimits,
};
pub use subscriptions::{McpSubscriptionConsumer, McpSubscriptionHost, McpSubscriptionReport};
pub use url_interactions::{
    McpStoredUrlInteraction, McpStoredUrlRequiredError, McpUrlAddress, McpUrlCompletion,
    McpUrlConsent, McpUrlPage, McpUrlRequest, McpUrlRequiredError, validate_url_completion,
};
pub use work::{McpHostWork, McpHostWorkEvidence, McpHostWorkReceipt};

/// Observable reason a session stopped receiving. This remains inspectable even
/// when the application event queue is full or its consumer is stalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpSessionEnd {
    /// Explicit owner close, shutdown, or lifetime cancellation.
    Cancelled,
    /// Transport EOF or I/O failure.
    Transport,
    /// Invalid or over-budget protocol ingress.
    Protocol,
    /// Event count or retained-byte budget exhausted.
    EventOverflow,
    /// Event consumer failed or exceeded its delivery deadline.
    EventHandler,
    /// Host callback capacity exhausted; no unbounded work is queued.
    CallbackOverflow,
    /// A callback could not be safely completed or disclosed.
    CallbackFailure,
    /// Server invalidated the HTTP session; reopening requires a new epoch.
    SessionExpired,
    /// Credentials or an HTTP authorization challenge require host attention.
    Authentication,
}

/// Namespace for an authenticated session owner. Equal names in different
/// namespaces never identify the same owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpOwnerScope {
    /// A workflow run; authority comes from its durable runtime lease.
    Run(RunId),
    /// A host-selected session group independent of any workflow run.
    Host(String),
}

/// Validated owner representation. Construction establishes shape, not authority;
/// the durable owner registry independently checks generation and principal.
#[derive(Clone)]
pub struct McpOwner {
    host: HostContext,
    scope: McpOwnerScope,
    generation: i64,
    key: Digest,
}
impl McpOwner {
    /// Creates an owner with a positive generation and bounded nonempty name.
    /// The stable key excludes generation so takeover cannot reset work identity.
    ///
    /// # Errors
    /// Returns invalid generation/name or representation limits.
    pub fn new(
        host: HostContext,
        scope: McpOwnerScope,
        generation: i64,
        limits: &Limits,
    ) -> Result<Self, RuntimeFailure> {
        let (kind, name) = match &scope {
            McpOwnerScope::Run(run) => ("run", run.as_str()),
            McpOwnerScope::Host(name) => ("host", name.as_str()),
        };
        if generation <= 0 || name.is_empty() {
            return Err(fail(FailureCode::Input));
        }
        let identity = Value::Array(vec![
            Value::Text("htlk.mcp-owner/1".into()),
            Value::Text(kind.into()),
            Value::Text(host.principal().into()),
            Value::Text(name.into()),
        ]);
        let key = JsonDocument::from_value(&identity, limits)
            .map_err(|_| fail(FailureCode::Input))?
            .digest();
        Ok(Self {
            host,
            scope,
            generation,
            key,
        })
    }
    /// Authenticated principal supplied by the embedding host.
    pub fn host(&self) -> &HostContext {
        &self.host
    }
    /// Run or host namespace and exact name.
    pub fn scope(&self) -> &McpOwnerScope {
        &self.scope
    }
    /// Current ownership generation, independently checked at commit.
    pub fn generation(&self) -> i64 {
        self.generation
    }
    /// Stable principal/namespace/name identity, excluding ownership generation.
    pub fn key(&self) -> Digest {
        self.key
    }
}
impl fmt::Debug for McpOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpOwner")
            .field("key", &self.key)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// How the dispatcher established a callback's operation association.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAttribution {
    /// No operation association; explicit host-session policy is required for work.
    Session,
    /// The request arrived on the HTTP response stream of an outgoing operation.
    ResponseStream,
    /// A serialized foreground operation provides the documented association.
    SerializedOperation,
    /// Validated protocol related-task metadata identifies a retained task.
    RelatedTask,
}

/// Immutable operation identity carried independently of server-provided metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOperationOrigin {
    /// Owning workflow run.
    pub run_id: RunId,
    /// Exact admitted invocation.
    pub invocation_id: InvocationId,
    /// Exact attempt; the runtime checks its relation to the invocation.
    pub attempt_id: AttemptId,
}

/// Context for one admitted host request. Authority and budget admission precede
/// handler dispatch; possession of a copied context grants no acceptance authority.
#[derive(Clone)]
pub struct McpInteractionContext {
    /// Single-operation durable work capability minted after ancestor admission.
    /// The native wrapper saves completion before returning to the handler.
    pub work: Option<McpHostWork>,
    /// Live request-bound progress sender, only when the peer requested progress.
    /// Progress never extends this interaction's deadline or implies completion.
    pub progress: Option<McpProgressReporter>,
    /// Frozen minimum protection of the request, parent work, and host policy.
    pub labels: crate::records::SecurityLabels,
    /// Authenticated durable owner and generation.
    pub owner: McpOwner,
    /// Exact compound server identity.
    pub server: ServerIdentity,
    /// Transport session epoch, distinct from owner generation.
    pub epoch: u64,
    /// Durable interaction identity, stable through explicitly authorized recovery.
    pub interaction_id: Digest,
    /// Incoming JSON-RPC ID, preserving integer versus string identity.
    pub request_id: McpRequestId,
    /// Outgoing request whose response stream carried this callback, when known.
    pub originating_request_id: Option<McpRequestId>,
    /// Trusted nesting depth, never copied from server metadata.
    pub depth: usize,
    /// Runtime-established originating work, when unambiguous.
    pub origin: Option<McpOperationOrigin>,
    /// Evidence used to associate the request with its origin.
    pub attribution: McpAttribution,
    /// Optional protocol task identity, validated against retained task authority.
    pub task_id: Option<String>,
    /// Parent client interaction for bounded nested work.
    pub parent_interaction: Option<Digest>,
    /// Trusted absolute deadline in milliseconds, not a server-provided timestamp.
    pub deadline_ms: i64,
    /// Cooperative cancellation subordinate to durable fencing.
    pub cancellation: Arc<McpCancellation>,
}

/// Non-null JSON-RPC request identity. Strings and integers are separate keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpRequestId {
    /// Integer identity.
    Integer(i64),
    /// Exact string identity (including the valid empty string).
    String(String),
}
impl McpRequestId {
    /// Validates a parsed ID without coercing its type.
    ///
    /// # Errors
    /// Returns a protocol failure for null, fractions, booleans, or other values.
    pub fn from_value(value: &Value) -> Result<Self, RuntimeFailure> {
        match value {
            Value::Integer(value) => Ok(Self::Integer(*value)),
            Value::Text(value) => Ok(Self::String(value.clone())),
            _ => Err(fail(FailureCode::McpProtocol)),
        }
    }
    /// Original protocol representation.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Integer(value) => Value::Integer(*value),
            Self::String(value) => Value::Text(value.clone()),
        }
    }
}

/// Protected JSON-RPC error. Debug deliberately excludes remote text and data.
#[derive(Clone)]
pub struct McpRpcError {
    code: i64,
    document: JsonDocument,
    original: Vec<u8>,
}
impl McpRpcError {
    /// Validates an exact error object while preserving its structured data.
    ///
    /// # Errors
    /// Returns a protocol failure for a missing integer code or string message.
    pub fn new(document: JsonDocument) -> Result<Self, RuntimeFailure> {
        let original = document.as_bytes().to_vec();
        Self::checked(document, original)
    }
    /// Validates and preserves exact original error-object bytes before JSON
    /// normalization. Remote text and data remain protected evidence.
    ///
    /// # Errors
    /// Returns bounded JSON ingress or invalid error shape.
    pub fn from_bytes(bytes: &[u8], limits: &Limits) -> Result<Self, RuntimeFailure> {
        let document =
            JsonDocument::new(bytes, limits).map_err(|_| fail(FailureCode::McpProtocol))?;
        Self::checked(document, bytes.to_vec())
    }
    fn checked(document: JsonDocument, original: Vec<u8>) -> Result<Self, RuntimeFailure> {
        let map = super::as_map(document.value())?;
        let Some(Value::Integer(code)) = map.get("code") else {
            return Err(fail(FailureCode::McpProtocol));
        };
        if !matches!(map.get("message"), Some(Value::Text(_))) {
            return Err(fail(FailureCode::McpProtocol));
        }
        Ok(Self {
            code: *code,
            document,
            original,
        })
    }
    /// Numeric JSON-RPC error code.
    pub fn code(&self) -> i64 {
        self.code
    }
    /// Exact protected error document, including optional data.
    pub fn document(&self) -> &JsonDocument {
        &self.document
    }
    /// Exact protected error-object bytes, distinct from canonical JSON identity.
    pub fn as_bytes(&self) -> &[u8] {
        &self.original
    }
}
impl fmt::Debug for McpRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpRpcError")
            .field("code", &self.code)
            .field("bytes", &self.original.len())
            .finish()
    }
}

/// Finite cumulative and instantaneous limits for one client session. Configuration
/// validates these before allocating channels or starting processes.
#[derive(Clone, Debug)]
pub struct McpClientLimits {
    /// Maximum distinct owner identities retained by one native client over its
    /// lifetime. Highest-generation fences are never evicted: new identities fail
    /// closed at this ceiling, while retired transports release session capacity.
    pub owner_identities: usize,
    /// Pending outgoing requests.
    pub pending_requests: usize,
    /// Simultaneously admitted host callbacks.
    pub callbacks: usize,
    /// Queued host events.
    pub events: usize,
    /// Maximum raw JSON message or incomplete SSE frame bytes.
    pub frame_bytes: usize,
    /// Maximum reserved request, original/canonical reply, and queued event bytes.
    /// Session-local subscription keys and single-use progress-token fingerprints
    /// also reserve this capacity until their observation/epoch is released.
    /// At least three frame ceilings are required to admit one full-size RPC.
    pub retained_bytes: usize,
    /// Cumulative admitted requests per epoch, bounding ID/replay retention.
    pub requests_per_session: usize,
    /// Maximum received frames per epoch, bounding even ignored notification work.
    /// Separately caps outgoing roots/progress notification attempts in that epoch.
    pub messages_per_session: usize,
    /// Maximum active callback ancestry. Without explicit parent attribution this
    /// conservatively bounds active callbacks globally, including parallel requests.
    pub callback_depth: usize,
    /// Maximum pages inspected in one catalog enumeration.
    pub catalog_pages: usize,
    /// Maximum descriptors retained in one catalog enumeration.
    pub catalog_items: usize,
    /// Absolute request timeout; progress never extends it.
    pub timeout_ms: u64,
    /// Maximum local cooperative cancellation grace.
    pub cancellation_grace_ms: u64,
}
impl Default for McpClientLimits {
    fn default() -> Self {
        Self {
            owner_identities: 4096,
            pending_requests: 64,
            callbacks: 8,
            events: 128,
            frame_bytes: 2 * 1024 * 1024,
            retained_bytes: 16 * 1024 * 1024,
            requests_per_session: 4096,
            messages_per_session: 65_536,
            callback_depth: 8,
            catalog_pages: 128,
            catalog_items: 4096,
            timeout_ms: 30_000,
            cancellation_grace_ms: 100,
        }
    }
}
impl McpClientLimits {
    /// Checks positive, representable bounds and consistent reservations.
    ///
    /// # Errors
    /// Returns a profile failure before any allocation or transport startup.
    pub fn validate(&self) -> Result<(), RuntimeFailure> {
        let counts = [
            self.owner_identities,
            self.pending_requests,
            self.callbacks,
            self.events,
            self.frame_bytes,
            self.retained_bytes,
            self.requests_per_session,
            self.messages_per_session,
            self.callback_depth,
            self.catalog_pages,
            self.catalog_items,
        ];
        if counts
            .into_iter()
            .any(|n| n == 0 || n > isize::MAX as usize)
            || self
                .frame_bytes
                .checked_mul(3)
                .is_none_or(|minimum| self.retained_bytes < minimum)
            || self.retained_bytes > u32::MAX as usize
            || self.requests_per_session < self.pending_requests
            || self.callbacks > self.requests_per_session
            || [
                self.owner_identities,
                self.pending_requests,
                self.callbacks,
                self.events,
            ]
            .into_iter()
            .any(|n| n > 65_536)
            || self.cancellation_grace_ms > self.timeout_ms
        {
            return Err(fail(FailureCode::Profile));
        }
        for ms in [self.timeout_ms, self.cancellation_grace_ms] {
            if ms == 0
                || ms > i64::MAX as u64
                || std::time::Instant::now()
                    .checked_add(Duration::from_millis(ms))
                    .is_none()
            {
                return Err(fail(FailureCode::Profile));
            }
        }
        Ok(())
    }
}

/// Protocol services whose implementation is supplied by the embedding host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpClientService {
    /// Basic sampling; tool/context extensions are independently installed.
    Sampling,
    /// Tool-enabled sampling, requiring basic sampling too.
    SamplingTools,
    /// Authorized context inclusion (soft-deprecated in the selected version).
    SamplingContext,
    /// Combined tool-enabled and authorized-context sampling. Required when both
    /// extensions are installed so dispatch cannot silently skip either service.
    SamplingToolsContext,
    /// Schema-driven form elicitation.
    ElicitationForm,
    /// External URL elicitation.
    ElicitationUrl,
    /// Host roots provider.
    Roots,
    /// Host roots-change service, requiring a roots provider too.
    RootsChanged,
}

/// Delivery of the underlying host/model operation, distinct from sending a
/// JSON-RPC callback response. Returning a timeout error does not prove work ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpWorkDelivery {
    /// No underlying operation was started.
    NotStarted,
    /// Work may have started; its completion is unknown.
    Uncertain,
    /// The underlying operation completed, successfully or with a known error.
    Completed,
}
impl McpWorkDelivery {
    /// Stable private journal spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Uncertain => "uncertain",
            Self::Completed => "completed",
        }
    }
    /// Reads a known spelling without granting meaning to unknown states.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "not_started" => Some(Self::NotStarted),
            "uncertain" => Some(Self::Uncertain),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// Observed host cost in an explicitly named integral unit, never an implicit
/// currency conversion or proof of hard budget enforcement.
#[derive(Clone)]
pub struct McpCost {
    /// Observed number of host-selected cost units.
    pub amount: u64,
    /// Exact unit, for example provider-defined microcredits.
    pub unit: String,
}

/// Optional trusted host usage observations. Unknown values remain absent rather
/// than becoming zero. Provider-operation identity supports explicit reconciliation.
#[derive(Clone, Default)]
pub struct McpUsage {
    /// Observed input token count, if actually metered.
    pub input_tokens: Option<u64>,
    /// Observed generated token count, if actually metered.
    pub output_tokens: Option<u64>,
    /// Observed cost in a named integral unit.
    pub cost: Option<McpCost>,
    /// Protected provider operation identifier, if known.
    pub provider_operation_id: Option<String>,
}
impl McpUsage {
    /// Checks and encodes a bounded private usage receipt. This does not advertise
    /// hard token/cost enforcement or turn provider hints into trusted metering.
    ///
    /// # Errors
    /// Returns out-of-range counters, an unnamed cost unit, or representation limits.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, RuntimeFailure> {
        let counter = |value: Option<u64>| {
            value
                .map(|n| {
                    i64::try_from(n)
                        .map(Value::Integer)
                        .map_err(|_| fail(FailureCode::Profile))
                })
                .transpose()
                .map(|v| v.unwrap_or(Value::Null))
        };
        let cost = if let Some(cost) = &self.cost {
            if cost.unit.is_empty() {
                return Err(fail(FailureCode::Profile));
            }
            obj([
                ("amount", counter(Some(cost.amount))?),
                ("unit", Value::Text(cost.unit.clone())),
            ])?
        } else {
            Value::Null
        };
        let value = obj([
            ("input_tokens", counter(self.input_tokens)?),
            ("output_tokens", counter(self.output_tokens)?),
            ("cost", cost),
            (
                "provider_operation_id",
                self.provider_operation_id
                    .as_ref()
                    .map_or(Value::Null, |id| Value::Text(id.clone())),
            ),
        ])?;
        htlk_executable::cbor::encode(&value, limits).map_err(|_| fail(FailureCode::Profile))?;
        Ok(value)
    }
}

/// Protected host work outcome. An uncertain operation can produce a JSON-RPC
/// error reply while its reservation remains outstanding. A successful result
/// requires completed work; protocol reply acceptance is a separate later fact.
pub struct McpServiceOutcome {
    /// Actual underlying work delivery/completion evidence.
    pub delivery: McpWorkDelivery,
    /// Complete protocol result or an error approved for disclosure to the server.
    pub result: Result<JsonDocument, McpRpcError>,
    /// Actual usage observations, independent of result success or failure.
    pub usage: Option<McpUsage>,
    /// Additional trusted protection from actual host inputs/results. The broker
    /// unions this with the original context; a handler cannot remove protection.
    pub labels: crate::records::SecurityLabels,
}
impl McpServiceOutcome {
    /// Reports known completed work with a protocol-success result.
    pub fn completed(result: JsonDocument) -> Self {
        Self {
            delivery: McpWorkDelivery::Completed,
            result: Ok(result),
            usage: None,
            labels: Default::default(),
        }
    }
    /// Reports an error and explicit underlying work delivery evidence.
    pub fn failure(delivery: McpWorkDelivery, error: McpRpcError) -> Self {
        Self {
            delivery,
            result: Err(error),
            usage: None,
            labels: Default::default(),
        }
    }
}
impl fmt::Debug for McpServiceOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServiceOutcome")
            .field("delivery", &self.delivery)
            .field("success", &self.result.is_ok())
            .field("has_usage", &self.usage.is_some())
            .finish_non_exhaustive()
    }
}

/// A delivery-aware host work future. Underlying work and RPC reply delivery are
/// intentionally represented separately.
pub type McpServiceFuture<'a> = Pin<Box<dyn Future<Output = McpServiceOutcome> + Send + 'a>>;

/// Installed host implementation. The client admits and validates a complete
/// protocol request before calling this handler, and independently validates its
/// result. Sampling receives the full structured request, not flattened prompt text.
pub trait McpClientHandler: Send + Sync {
    /// Executes one admitted interaction. Implementations must honor its cancellation,
    /// deadline and accounted work identity; persistence precedes reply transmission.
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        request: &'a JsonDocument,
    ) -> McpServiceFuture<'a>;
}

/// Bounded protected host event; `method` names protocol notifications or a local
/// lifecycle event. No event authorizes content acquisition or workflow mutation.
#[derive(Clone)]
pub struct McpClientEvent {
    /// Checked protocol notification observation. None denotes another protocol or
    /// local lifecycle event (including task and addressed URL events).
    pub notification: Option<McpNotification>,
    /// Durable owner context, without fabricated operation attribution.
    pub owner: McpOwner,
    /// Exact server identity.
    pub server: ServerIdentity,
    /// Source transport epoch.
    pub epoch: u64,
    /// Durable store-scoped session identity when persistence is configured.
    pub session_id: Option<Digest>,
    /// Locally stable event identity for consumer deduplication.
    pub event_id: Digest,
    /// Protocol method or local lifecycle event name.
    pub method: String,
    /// Protected bounded event parameters.
    pub params: JsonDocument,
}

/// Host event delivery boundary. Slow consumers are isolated from the protocol
/// reader by bounded queues; returning does not assert permanent archival.
pub trait McpEventHandler: Send + Sync {
    /// Receives one event under the session delivery deadline.
    fn receive<'a>(&'a self, event: McpClientEvent) -> McpFuture<'a, ()>;
}

/// Trusted persistence and clock for durable client ownership and session epochs.
/// Sharing a run-owned client with a runtime requires that runtime's actual store
/// and clock. Store access alone is not frontend authorization.
#[derive(Clone)]
pub struct McpClientPersistence {
    store: crate::store::SqliteStore,
    clock: Arc<dyn crate::clock::CoordinatorClock>,
}
impl McpClientPersistence {
    /// Links the existing trusted store and clock without opening any connection.
    pub fn new(
        store: crate::store::SqliteStore,
        clock: Arc<dyn crate::clock::CoordinatorClock>,
    ) -> Self {
        Self { store, clock }
    }
    /// Acquires an explicit host-session namespace under the authenticated host.
    /// Existing live ownership cannot be overwritten, including by the same caller.
    ///
    /// # Errors
    /// Returns active ownership, invalid finite lifetime, or storage/clock failure.
    pub async fn acquire_owner(
        &self,
        context: &HostContext,
        name: &str,
        lease_ms: u64,
    ) -> Result<McpOwner, RuntimeFailure> {
        let (now, expires) = self.lease_times(lease_ms)?;
        self.store
            .acquire_mcp_owner(context, name, now, expires)
            .await
            .map_err(|e| fail(e.code()))
    }
    /// Renews a live host owner; the generation and outstanding accounting persist.
    ///
    /// # Errors
    /// Returns stale ownership, shortening expiry, or storage/clock failure.
    pub async fn renew_owner(&self, owner: &McpOwner, lease_ms: u64) -> Result<(), RuntimeFailure> {
        let (now, expires) = self.lease_times(lease_ms)?;
        self.store
            .renew_mcp_owner(owner, now, expires)
            .await
            .map_err(|e| fail(e.code()))
    }
    /// Releases host ownership after its transports have stopped. Work evidence
    /// persists; a later acquisition gets a higher generation.
    ///
    /// # Errors
    /// Returns stale/non-host ownership or storage/clock failure.
    pub async fn release_owner(&self, owner: &McpOwner) -> Result<(), RuntimeFailure> {
        self.store
            .release_mcp_owner(owner, self.clock.now_ms()?)
            .await
            .map_err(|e| fail(e.code()))
    }
    fn lease_times(&self, lease_ms: u64) -> Result<(i64, i64), RuntimeFailure> {
        super::valid_timeout(lease_ms)?;
        let now = self.clock.now_ms()?;
        let expires = now
            .checked_add(lease_ms as i64)
            .ok_or_else(|| fail(FailureCode::Profile))?;
        Ok((now, expires))
    }
    async fn validate(&self, owner: &McpOwner) -> Result<(), RuntimeFailure> {
        self.store
            .validate_mcp_owner(owner, self.clock.now_ms()?)
            .await
            .map_err(|e| fail(e.code()))
    }
}

/// Immutable, checked client service configuration. Capabilities cannot be supplied
/// as arbitrary JSON. Installing services is distinct from enabling them on a live
/// native transport with its durable interaction journal.
#[derive(Clone)]
pub struct McpClientConfig {
    limits: McpClientLimits,
    handlers: BTreeMap<McpClientService, Arc<dyn McpClientHandler>>,
    events: Option<Arc<dyn McpEventHandler>>,
    capabilities: JsonDocument,
    listen_for_events: bool,
    persistence: Option<McpClientPersistence>,
    authorizer: Option<Arc<dyn McpClientAuthorizer>>,
    credentials: Option<Arc<dyn auth::McpCredentialProvider>>,
    authentication: auth::AuthenticationMonitor,
    task_receiver: Option<McpTaskReceiverSettings>,
    graph_tasks: McpGraphTaskPolicy,
}
impl McpClientConfig {
    /// Freezes a checked service registry and derives its protocol capabilities.
    ///
    /// # Errors
    /// Returns invalid limits or extension services missing their base provider.
    pub fn new(
        limits: McpClientLimits,
        handlers: BTreeMap<McpClientService, Arc<dyn McpClientHandler>>,
        events: Option<Arc<dyn McpEventHandler>>,
    ) -> Result<Self, RuntimeFailure> {
        limits.validate()?;
        use McpClientService::*;
        let has = |s| handlers.contains_key(&s);
        if ((has(SamplingTools) || has(SamplingContext)) && !has(Sampling))
            || ((has(SamplingTools) && has(SamplingContext)) != has(SamplingToolsContext))
            || (has(RootsChanged) && !has(Roots))
        {
            return Err(fail(FailureCode::Profile));
        }
        let mut capabilities = Vec::new();
        if has(Sampling) {
            let mut entries = Vec::new();
            if has(SamplingTools) {
                entries.push(("tools".into(), obj([])?));
            }
            if has(SamplingContext) {
                entries.push(("context".into(), obj([])?));
            }
            capabilities.push((
                "sampling".into(),
                Value::Map(Map::try_from_entries(entries).map_err(|_| fail(FailureCode::Profile))?),
            ));
        }
        if has(ElicitationForm) || has(ElicitationUrl) {
            let mut entries = Vec::new();
            if has(ElicitationForm) {
                entries.push(("form".into(), obj([])?));
            }
            if has(ElicitationUrl) {
                entries.push(("url".into(), obj([])?));
            }
            capabilities.push((
                "elicitation".into(),
                Value::Map(Map::try_from_entries(entries).map_err(|_| fail(FailureCode::Profile))?),
            ));
        }
        if has(Roots) {
            capabilities.push((
                "roots".into(),
                obj([("listChanged", Value::Bool(has(RootsChanged)))])?,
            ));
        }
        let capabilities = JsonDocument::from_value(
            &Value::Map(
                Map::try_from_entries(capabilities).map_err(|_| fail(FailureCode::Profile))?,
            ),
            &Limits::default(),
        )
        .map_err(|_| fail(FailureCode::Profile))?;
        Ok(Self {
            limits,
            handlers,
            events,
            capabilities,
            listen_for_events: true,
            persistence: None,
            authorizer: None,
            credentials: None,
            authentication: auth::AuthenticationMonitor::new(),
            task_receiver: None,
            graph_tasks: McpGraphTaskPolicy::Reject,
        })
    }
    /// Checked immutable resource bounds.
    pub fn limits(&self) -> &McpClientLimits {
        &self.limits
    }
    /// Exact client capability object derived from installed services.
    pub fn capabilities(&self) -> &JsonDocument {
        &self.capabilities
    }
    /// Borrows an explicitly installed service implementation.
    pub fn handler(&self, service: McpClientService) -> Option<&Arc<dyn McpClientHandler>> {
        self.handlers.get(&service)
    }
    /// Borrows the host event consumer, when installed.
    pub fn event_handler(&self) -> Option<&Arc<dyn McpEventHandler>> {
        self.events.as_ref()
    }
    /// Selects whether to open the optional idle HTTP GET stream. This does not
    /// disable reception on request-associated streams or on stdio. Hosts that
    /// subscribe to idle events must leave this enabled.
    pub fn with_server_event_stream(mut self, enabled: bool) -> Self {
        self.listen_for_events = enabled;
        self
    }
    /// Uses durable owner leases and store-scoped epoch identities. Host owners
    /// must first be acquired through this persistence service. Run owners are
    /// checked against the runtime's actual persisted ownership and principal.
    pub fn with_persistence(mut self, persistence: McpClientPersistence) -> Self {
        self.persistence = Some(persistence);
        self
    }
    /// Installs current host authorization/classification for client interactions.
    /// Effectful services require both this policy and durable persistence.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn McpClientAuthorizer>) -> Self {
        self.authorizer = Some(authorizer);
        self
    }
    /// Installs the durable experimental receiving task service and derives its
    /// capabilities from installed handlers. Persistence and authorization must
    /// already be configured. Configuration is rechecked when the client starts.
    ///
    /// # Errors
    /// Returns inconsistent handlers, persistence or finite receiver bounds.
    pub fn with_task_receiver(
        mut self,
        settings: McpTaskReceiverSettings,
    ) -> Result<Self, RuntimeFailure> {
        McpTaskReceiver::new(self.clone(), settings)?;
        let mut entries: Vec<_> = super::as_map(self.capabilities.value())?
            .iter()
            .filter(|(key, _)| *key != "tasks")
            .map(|(key, value)| (key.to_owned(), value.clone()))
            .collect();
        entries.push((
            "tasks".into(),
            settings.capabilities(&Limits::default())?.value().clone(),
        ));
        self.capabilities = JsonDocument::from_value(
            &Value::Map(Map::try_from_entries(entries).map_err(|_| fail(FailureCode::Profile))?),
            &Limits::default(),
        )
        .map_err(|_| fail(FailureCode::Profile))?;
        self.task_receiver = Some(settings);
        Ok(self)
    }
    /// Freezes deployment task selection for graph tools. Install persistence
    /// first, sharing the runtime's real store and clock; no syntax or authored
    /// argument is changed by this policy.
    ///
    /// # Errors
    /// Returns missing persistence or invalid finite requested TTL.
    pub fn with_graph_tasks(mut self, policy: McpGraphTaskPolicy) -> Result<Self, RuntimeFailure> {
        if let McpGraphTaskPolicy::Required { ttl_ms } | McpGraphTaskPolicy::Prefer { ttl_ms } =
            policy
            && (self.persistence.is_none() || ttl_ms == 0 || ttl_ms > i64::MAX as u64)
        {
            return Err(fail(FailureCode::Profile));
        }
        self.graph_tasks = policy;
        Ok(self)
    }
    /// Immutable native graph task selection used before operation admission.
    pub fn graph_task_policy(&self) -> McpGraphTaskPolicy {
        self.graph_tasks
    }
    /// Installs host-managed exact-server/resource/principal credentials for every
    /// HTTP request, including control POSTs, SSE GET/resumption and DELETE.
    /// Static Authorization headers cannot be combined with this provider. Stdio
    /// continues to use host environment configuration.
    pub fn with_credential_provider(
        mut self,
        provider: Arc<dyn auth::McpCredentialProvider>,
    ) -> Self {
        self.credentials = Some(provider);
        self
    }
    /// Watches the latest protected authentication concern across clients using
    /// this configuration. Sequence gaps expose overwritten concerns; this bounded
    /// monitor is not an audit archive. Subscribe before open to observe failures
    /// that prevent a session handle from being created. Host persistence and
    /// authorization interaction presentation are explicit host responsibilities.
    pub fn authentication_concerns(&self) -> auth::McpAuthenticationConcerns {
        self.authentication.subscribe()
    }
}
