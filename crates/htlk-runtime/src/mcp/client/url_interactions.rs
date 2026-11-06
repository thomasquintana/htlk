//! Protected URL-elicitation facts for MCP 2025-11-25.
//!
//! Consent and server completion are independent facts. None of these APIs opens
//! a URL, fetches metadata, changes connection credentials, or retries an RPC.
use crate::{
    failure::{FailureCode, RuntimeFailure},
    mcp::client::{McpOperationOrigin, McpRequestId, McpRpcError},
    records::SecurityLabels,
};
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use std::{collections::BTreeSet, fmt};

fn invalid() -> RuntimeFailure {
    RuntimeFailure::new(FailureCode::McpProtocol)
}
fn map(value: &Value) -> Result<&Map, RuntimeFailure> {
    match value {
        Value::Map(fields) => Ok(fields),
        _ => Err(invalid()),
    }
}
fn text<'a>(fields: &'a Map, name: &str) -> Result<&'a str, RuntimeFailure> {
    match fields.get(name) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(invalid()),
    }
}

/// Validated URL parameters. Debug omits URLs, messages, IDs, and metadata.
#[derive(Clone)]
pub struct McpUrlRequest {
    params: JsonDocument,
}
impl McpUrlRequest {
    /// Validates bounded URL parameters without navigation or metadata acquisition.
    /// The exact supplied URL remains available for authorized presentation.
    ///
    /// # Errors
    /// Returns a redacted protocol failure for invalid URL-mode parameters.
    pub fn from_params(params: &Value, limits: &Limits) -> Result<Self, RuntimeFailure> {
        let params = JsonDocument::from_value(params, limits).map_err(|_| invalid())?;
        let fields = map(params.value())?;
        if text(fields, "mode")? != "url"
            || fields
                .get("_meta")
                .is_some_and(|value| !matches!(value, Value::Map(_)))
        {
            return Err(invalid());
        }
        text(fields, "elicitationId")?;
        text(fields, "message")?;
        reqwest::Url::parse(text(fields, "url")?).map_err(|_| invalid())?;
        Ok(Self { params })
    }
    /// Protected server-selected completion ID, scoped by owner/server/session.
    pub fn elicitation_id(&self) -> &str {
        text(
            map(self.params.value()).expect("validated map"),
            "elicitationId",
        )
        .expect("validated ID")
    }
    /// Full original URL spelling, including path and query, for consent review.
    pub fn url(&self) -> &str {
        text(map(self.params.value()).expect("validated map"), "url").expect("validated URL")
    }
    /// Protected server explanation; untrusted presentation data, never authority.
    pub fn message(&self) -> &str {
        text(map(self.params.value()).expect("validated map"), "message")
            .expect("validated message")
    }
    /// Complete protected parameters, including original metadata.
    pub fn params(&self) -> &JsonDocument {
        &self.params
    }
}
impl fmt::Debug for McpUrlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpUrlRequest")
            .field("digest", &self.params.digest())
            .finish_non_exhaustive()
    }
}

/// Validated URL-required error. The original outcome is retained byte-for-byte;
/// discovering required interactions does not turn this error into a success.
#[derive(Clone)]
pub struct McpUrlRequiredError {
    error: McpRpcError,
    interactions: Vec<McpUrlRequest>,
}
impl McpUrlRequiredError {
    /// Checks the complete required-interaction list before any durable admission.
    /// Other error codes return `None` without inspecting irrelevant error data.
    ///
    /// # Errors
    /// Returns a redacted failure for a malformed, duplicate-ID, or oversized
    /// required list. The caller retains its original operation error on failure.
    pub fn parse(
        error: &McpRpcError,
        maximum_interactions: usize,
        limits: &Limits,
    ) -> Result<Option<Self>, RuntimeFailure> {
        if error.code() != -32042 {
            return Ok(None);
        }
        if error.as_bytes().len() > limits.max_document_bytes || maximum_interactions == 0 {
            return Err(invalid());
        }
        let fields = map(error.document().value())?;
        let data = map(fields.get("data").ok_or_else(invalid)?)?;
        let Some(Value::Array(values)) = data.get("elicitations") else {
            return Err(invalid());
        };
        if values.len() > maximum_interactions {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        let mut interactions = Vec::with_capacity(values.len());
        for value in values {
            let request = McpUrlRequest::from_params(value, limits)?;
            if !ids.insert(request.elicitation_id().to_string()) {
                return Err(invalid());
            }
            interactions.push(request);
        }
        Ok(Some(Self {
            error: error.clone(),
            interactions,
        }))
    }
    /// Exact protected original JSON-RPC error, independent of completion state.
    pub fn error(&self) -> &McpRpcError {
        &self.error
    }
    /// Fully validated required URL interactions in the server's original order.
    pub fn interactions(&self) -> &[McpUrlRequest] {
        &self.interactions
    }
}
impl fmt::Debug for McpUrlRequiredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpUrlRequiredError")
            .field("count", &self.interactions.len())
            .finish_non_exhaustive()
    }
}

/// A human decision, distinct from the optional server completion observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpUrlConsent {
    /// No accepted human response has been recorded.
    Pending,
    /// Explicit consent to the interaction; not proof of navigation or completion.
    Accepted,
    /// Explicit refusal.
    Declined,
    /// Dismissal without consent.
    Cancelled,
}
impl McpUrlConsent {
    /// Validates a URL result, including the prohibition on in-band credentials.
    ///
    /// # Errors
    /// Returns a protocol failure for content or an invalid action/metadata shape.
    pub fn from_result(result: &JsonDocument) -> Result<Self, RuntimeFailure> {
        let fields = map(result.value())?;
        if fields.get("content").is_some()
            || fields
                .get("_meta")
                .is_some_and(|value| !matches!(value, Value::Map(_)))
        {
            return Err(invalid());
        }
        match text(fields, "action")? {
            "accept" => Ok(Self::Accepted),
            "decline" => Ok(Self::Declined),
            "cancel" => Ok(Self::Cancelled),
            _ => Err(invalid()),
        }
    }
    /// Private durable spelling, with no implicit state ordering.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accept",
            Self::Declined => "decline",
            Self::Cancelled => "cancel",
        }
    }
    /// Parses a private durable spelling; unknown states are never reinterpreted.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "accept" => Some(Self::Accepted),
            "decline" => Some(Self::Declined),
            "cancel" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Protected durable URL facts. Authorization must precede disclosure of `request`
/// or `original_error`; ordinary events need only carry the opaque `id`.
#[derive(Clone)]
pub struct McpStoredUrlInteraction {
    /// Runtime-established operation origin, absent for independent host work.
    pub origin: Option<McpOperationOrigin>,
    /// Stable owner/server/session/elicitation identity.
    pub id: Digest,
    /// Exact compound server identity.
    pub server: Digest,
    /// Store-scoped transport epoch; never restored implicitly.
    pub session: Digest,
    /// Original callback identity, absent for error-originated interactions.
    pub callback_id: Option<Digest>,
    /// Original protected URL parameters, including full URL and metadata.
    pub request: McpUrlRequest,
    /// Conservative request, original error, and reply protection.
    pub labels: SecurityLabels,
    /// Explicit human response; acceptance remains separate from completion.
    pub consent: McpUrlConsent,
    /// Server completion observation, which grants no retry or navigation authority.
    pub completed_ms: Option<i64>,
    /// Absolute consent deadline. Expiry does not erase completion evidence.
    pub deadline_ms: i64,
    /// Original transport epoch closed; delivery may have a gap.
    pub session_closed: bool,
}
impl McpStoredUrlInteraction {
    /// Whether the consent window elapsed. Manual continuation must be a separately
    /// authorized action; it cannot resurrect this deadline or retry an RPC here.
    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms >= self.deadline_ms
    }
}
impl fmt::Debug for McpStoredUrlInteraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpStoredUrlInteraction")
            .field("id", &self.id)
            .field("consent", &self.consent)
            .field("completed_ms", &self.completed_ms)
            .field("session_closed", &self.session_closed)
            .finish_non_exhaustive()
    }
}

/// Outcome of a completion notification. Ignored notifications emit no event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpUrlCompletion {
    /// Unknown or already-completed ID within the exact owner/server/session scope.
    Ignored,
    /// The first server completion observation was committed.
    Recorded {
        /// Addressed local identity for a protected host event.
        interaction_id: Digest,
    },
}

/// Exact protected error evidence associated with one original outgoing request.
#[derive(Clone)]
pub struct McpStoredUrlRequiredError {
    /// Exact operation origin established before the original RPC was dispatched.
    pub origin: Option<McpOperationOrigin>,
    /// Original outgoing RPC identity, preserving integer/string distinction.
    pub request_id: McpRequestId,
    /// Original immutable error bytes, including all remote metadata.
    pub error: McpRpcError,
    /// Unioned protection of the original operation and server error.
    pub labels: SecurityLabels,
    /// Required local URL identities in the original list order.
    pub interactions: Vec<Digest>,
}

/// Opaque address for URL inspection and consent, without remote URLs or IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpUrlAddress {
    /// Durable local URL identity.
    pub interaction_id: Digest,
    /// Exact compound server identity.
    pub server: Digest,
    /// Store-scoped transport epoch.
    pub session: Digest,
}

/// Authorized bounded URL address page for interactive and polling consumers.
#[derive(Clone, Debug)]
pub struct McpUrlPage {
    /// Individually authorized addresses; values require explicit inspection.
    pub interactions: Vec<McpUrlAddress>,
    /// Last scanned identity, including records filtered by inspection policy.
    pub next: Option<Digest>,
}
impl fmt::Debug for McpStoredUrlRequiredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpStoredUrlRequiredError")
            .field("count", &self.interactions.len())
            .finish_non_exhaustive()
    }
}

/// Validates a complete completion notification without interpreting its ID as a
/// global key. Unknown IDs must subsequently be ignored by the scoped store.
///
/// # Errors
/// Returns a protocol failure for malformed notifications, including request IDs.
pub fn validate_url_completion(notification: &JsonDocument) -> Result<&str, RuntimeFailure> {
    let fields = map(notification.value())?;
    if text(fields, "jsonrpc")? != "2.0"
        || text(fields, "method")? != "notifications/elicitation/complete"
        || fields.get("id").is_some()
        || fields.get("result").is_some()
        || fields.get("error").is_some()
    {
        return Err(invalid());
    }
    let params = map(fields.get("params").ok_or_else(invalid)?)?;
    if params
        .get("_meta")
        .is_some_and(|value| !matches!(value, Value::Map(_)))
    {
        return Err(invalid());
    }
    text(params, "elicitationId")
}
