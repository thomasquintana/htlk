//! Typed notification data and bounded progress. None of these observations grants
//! content-acquisition, catalog-adoption, or workflow-mutation authority.
use super::*;
use crate::mcp::{as_map, get};
use htlk_analyzer::{McpMessageKind, validate_mcp_message};
use htlk_executable::cbor::FiniteFloat;

/// Exact protocol progress identity. Integer and string tokens never alias.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct McpProgressToken(McpRequestId);
impl fmt::Debug for McpProgressToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            McpRequestId::Integer(value) => f.debug_tuple("Integer").field(value).finish(),
            McpRequestId::String(value) => f
                .debug_struct("String")
                .field("bytes", &value.len())
                .finish_non_exhaustive(),
        }
    }
}
impl McpProgressToken {
    /// Constructs an integer token; bounded protocol encoding independently checks
    /// the JSON safe-integer range before dispatch.
    pub fn integer(value: i64) -> Self {
        Self(McpRequestId::Integer(value))
    }
    /// Constructs an exact string token, including the valid empty string.
    pub fn string(value: String) -> Self {
        Self(McpRequestId::String(value))
    }
    /// Preserves the original wire type without coercion.
    pub fn to_value(&self) -> Value {
        self.0.to_value()
    }
    pub(crate) fn from_value(value: &Value) -> Result<Self, RuntimeFailure> {
        Ok(Self(McpRequestId::from_value(value)?))
    }
}

/// Immutable progress values with finite, nonnegative counters and a valid total.
/// A total can increase; omission preserves the last known total during admission.
#[derive(Clone)]
pub struct McpProgress {
    token: McpProgressToken,
    progress: f64,
    total: Option<f64>,
    message: Option<String>,
}
impl fmt::Debug for McpProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpProgress")
            .field("token", &self.token)
            .field("progress", &self.progress)
            .field("total", &self.total)
            .field("has_message", &self.message.is_some())
            .finish()
    }
}
impl McpProgress {
    /// Checks numeric and bounded pinned notification shape before construction.
    ///
    /// # Errors
    /// Returns invalid counters, total, token encoding, or representation limits.
    pub fn new(
        token: McpProgressToken,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
        limits: &Limits,
    ) -> Result<Self, RuntimeFailure> {
        if !progress.is_finite()
            || progress < 0.0
            || total.is_some_and(|n| !n.is_finite() || n < progress)
        {
            return Err(fail(FailureCode::Input));
        }
        let result = Self {
            token,
            progress,
            total,
            message,
        };
        let document = result.envelope(limits)?;
        validate_mcp_message(McpMessageKind::ProgressNotification, &document, limits)
            .map_err(|_| fail(FailureCode::Input))?;
        Ok(result)
    }
    /// Exact integer/string token.
    pub fn token(&self) -> &McpProgressToken {
        &self.token
    }
    /// Last reported completed work; it never proves operation completion.
    pub fn progress(&self) -> f64 {
        self.progress
    }
    /// Last known total, absent when unknown.
    pub fn total(&self) -> Option<f64> {
        self.total
    }
    /// Optional protected server/host message.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    pub(crate) fn params(&self) -> Result<Value, RuntimeFailure> {
        let mut fields = vec![
            ("progressToken".into(), self.token.to_value()),
            (
                "progress".into(),
                Value::Float(
                    FiniteFloat::new(self.progress).map_err(|_| fail(FailureCode::Input))?,
                ),
            ),
        ];
        if let Some(total) = self.total {
            fields.push((
                "total".into(),
                Value::Float(FiniteFloat::new(total).map_err(|_| fail(FailureCode::Input))?),
            ));
        }
        if let Some(message) = &self.message {
            fields.push(("message".into(), Value::Text(message.clone())));
        }
        Ok(Value::Map(
            Map::try_from_entries(fields).map_err(|_| fail(FailureCode::Input))?,
        ))
    }
    pub(crate) fn envelope(&self, limits: &Limits) -> Result<JsonDocument, RuntimeFailure> {
        envelope("notifications/progress", self.params()?, limits)
    }
    pub(crate) fn parse(document: &JsonDocument, limits: &Limits) -> Result<Self, RuntimeFailure> {
        validate_mcp_message(McpMessageKind::ProgressNotification, document, limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let params = as_map(get(as_map(document.value())?, "params")?)?;
        let number = |v: &Value| match v {
            Value::Integer(n) => Ok(*n as f64),
            Value::Float(n) => Ok(n.get()),
            _ => Err(fail(FailureCode::McpProtocol)),
        };
        let token = McpProgressToken::from_value(get(params, "progressToken")?)?;
        let progress = number(get(params, "progress")?)?;
        let total = params.get("total").map(number).transpose()?;
        if progress < 0.0 || total.is_some_and(|n| n < progress) {
            return Err(fail(FailureCode::McpProtocol));
        }
        let message = match params.get("message") {
            Some(Value::Text(s)) => Some(s.clone()),
            None => None,
            _ => return Err(fail(FailureCode::McpProtocol)),
        };
        Ok(Self {
            token,
            progress,
            total,
            message,
        })
    }
}

/// Remote subscription disposition at receipt time, not permission to read a URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpResourceUpdateState {
    /// No subscription for this URI was sent in this epoch; unsolicited update.
    Unknown,
    /// Subscribe was admitted and may be in flight; an early update can race ACK.
    Subscribing,
    /// A positive subscribe acknowledgement was observed.
    Subscribed,
    /// Last-consumer unsubscribe may be in flight.
    Unsubscribing,
    /// Unsubscribe was acknowledged; an already-in-flight update can still arrive.
    Unsubscribed,
    /// Failed or overlapping operations leave remote state uncertain.
    Uncertain,
}
/// Catalog category invalidated by the protocol notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpCatalogChanged {
    /// Tool inventory.
    Tools,
    /// Prompt inventory.
    Prompts,
    /// Resource and resource-template inventories share this invalidation.
    ResourcesAndTemplates,
}
/// Explicit non-authoritative disposition for a recognized but inadmissible event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpNotificationIgnored {
    /// Notification category/sub-capability was not advertised by this server.
    UnsupportedCapability,
    /// Initialization has not established server capabilities.
    BeforeInitialization,
    /// No live outgoing request or accepted task owns this exact progress token.
    UnknownProgressToken,
    /// A response stream belongs to a different outgoing request.
    WrongProgressRequest,
    /// Progress failed to increase, total decreased, or work exceeded known total.
    NonMonotonicProgress,
    /// The owning request was cancelled or its absolute deadline elapsed.
    ExpiredProgress,
}
/// Dedicated notification observation, accompanying the protected original params.
#[derive(Clone)]
pub enum McpNotification {
    /// Valid resource-update observation with explicit local subscription state.
    ResourceUpdated {
        /// Exact protected URI; never automatically read.
        uri: String,
        /// Session-local subscription/uncertainty disposition.
        subscription: McpResourceUpdateState,
    },
    /// Valid capability-gated catalog invalidation; no automatic refresh/adoption.
    CatalogChanged(McpCatalogChanged),
    /// Valid structured log with protected arbitrary JSON data.
    Logging {
        /// Closed pinned logging level.
        level: McpLoggingLevel,
        /// Optional protected logger identity.
        logger: Option<String>,
        /// Bounded protected JSON data, never flattened to a display string.
        data: JsonDocument,
    },
    /// Progress associated only through an actual registered outgoing token.
    Progress {
        /// Exact outgoing RPC whose token matched.
        request_id: McpRequestId,
        /// Independently established runtime origin, when present on that request.
        origin: Option<McpOperationOrigin>,
        /// Checked monotonic progress; no deadline extension or completion claim.
        update: McpProgress,
    },
    /// Recognized notification explicitly ignored without dispatch or acquisition.
    Ignored(McpNotificationIgnored),
}
impl fmt::Debug for McpNotification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceUpdated { subscription, .. } => f
                .debug_struct("ResourceUpdated")
                .field("subscription", subscription)
                .finish_non_exhaustive(),
            Self::CatalogChanged(category) => {
                f.debug_tuple("CatalogChanged").field(category).finish()
            }
            Self::Logging { level, .. } => f
                .debug_struct("Logging")
                .field("level", level)
                .finish_non_exhaustive(),
            Self::Progress { update, .. } => f
                .debug_struct("Progress")
                .field("update", update)
                .finish_non_exhaustive(),
            Self::Ignored(reason) => f.debug_tuple("Ignored").field(reason).finish(),
        }
    }
}

/// Transport observation for a notification. Success means local flush or HTTP
/// acceptance, never a correlated RPC reply or remote application consumption.
#[derive(Debug)]
pub struct McpNotificationReply {
    /// NotSent before dispatch; PossiblySent once transmission may have begun.
    pub delivery: crate::mcp::McpDelivery,
    /// Local send/transport-acceptance result.
    pub result: Result<(), RuntimeFailure>,
}

type ProgressSend = dyn Fn(McpProgress) -> McpFuture<'static, McpNotificationReply> + Send + Sync;
/// Opaque live callback progress capability. The dispatcher binds it to the exact
/// admitted incoming request, token, epoch, cancellation, and absolute deadline.
#[derive(Clone)]
pub struct McpProgressReporter {
    pub(crate) token: McpProgressToken,
    pub(crate) send: Arc<ProgressSend>,
}
impl McpProgressReporter {
    /// Original requesting peer's exact progress token.
    pub fn token(&self) -> &McpProgressToken {
        &self.token
    }
    /// Sends monotonic bounded progress on the I/O owner. Clones share the same
    /// admission cursor. Progress never extends callback or parent deadlines.
    ///
    /// # Errors
    /// Returns invalid token/counters, expired callback, capacity or owner failure.
    ///
    /// ```no_run
    /// use htlk_executable::cbor::Limits;
    /// use htlk_runtime::mcp::client::{McpInteractionContext, McpProgress};
    /// # async fn report(context: &McpInteractionContext)
    /// #     -> Result<(), htlk_runtime::failure::RuntimeFailure> {
    /// if let Some(sender) = &context.progress {
    ///     let update = McpProgress::new(
    ///         sender.token().clone(), 1.0, Some(2.0), None, &Limits::default(),
    ///     )?;
    ///     sender.report(update).await?.result?;
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn report(
        &self,
        update: McpProgress,
    ) -> Result<McpNotificationReply, RuntimeFailure> {
        if update.token() != &self.token {
            return Err(fail(FailureCode::Input));
        }
        (self.send)(update).await
    }
}

#[derive(Default)]
pub(crate) struct ProgressCursor {
    last: Option<f64>,
    total: Option<f64>,
}
impl ProgressCursor {
    pub(crate) fn admit(&mut self, update: &mut McpProgress) -> bool {
        if self.last.is_some_and(|last| update.progress <= last)
            || self.total.is_some_and(|total| {
                update.total.is_some_and(|next| next < total)
                    || update.progress > update.total.unwrap_or(total)
            })
        {
            return false;
        }
        self.last = Some(update.progress);
        self.total = update.total.or(self.total);
        update.total = self.total;
        true
    }
}
pub(crate) fn request_token(params: &Value) -> Result<Option<McpProgressToken>, RuntimeFailure> {
    let params = as_map(params)?;
    let Some(meta) = params.get("_meta") else {
        return Ok(None);
    };
    as_map(meta)?
        .get("progressToken")
        .map(McpProgressToken::from_value)
        .transpose()
}
pub(crate) fn envelope(
    method: &str,
    params: Value,
    limits: &Limits,
) -> Result<JsonDocument, RuntimeFailure> {
    JsonDocument::from_value(
        &obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("method", Value::Text(method.into())),
            ("params", params),
        ])?,
        limits,
    )
    .map_err(|_| fail(FailureCode::Input))
}
pub(crate) fn logging_level(value: &Value) -> Result<McpLoggingLevel, RuntimeFailure> {
    match value {
        Value::Text(value) => match value.as_str() {
            "debug" => Ok(McpLoggingLevel::Debug),
            "info" => Ok(McpLoggingLevel::Info),
            "notice" => Ok(McpLoggingLevel::Notice),
            "warning" => Ok(McpLoggingLevel::Warning),
            "error" => Ok(McpLoggingLevel::Error),
            "critical" => Ok(McpLoggingLevel::Critical),
            "alert" => Ok(McpLoggingLevel::Alert),
            "emergency" => Ok(McpLoggingLevel::Emergency),
            _ => Err(fail(FailureCode::McpProtocol)),
        },
        _ => Err(fail(FailureCode::McpProtocol)),
    }
}
