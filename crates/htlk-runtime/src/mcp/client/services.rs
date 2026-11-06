//! Bounded host discovery and utility RPCs. Catalogs are observations, never
//! compiler adoption authority. Nothing in this module reacts to notifications or
//! reads resource content. Subscription acknowledgements describe one RPC in one
//! epoch; durable local subscriber ownership belongs to the host's intent ledger.
use super::{McpClientSession, McpRpcError};
use crate::{
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, McpDelivery, McpRequest, as_map, obj},
};
use htlk_analyzer::{
    McpDescriptorKind, McpMessageKind, validate_mcp_descriptor, validate_mcp_message,
};
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Map, Value},
};
use std::{collections::BTreeSet, fmt, sync::Arc, time::Duration};

/// Redacted service failure retaining protected correlated protocol errors and
/// conservative delivery evidence. An error does not imply remote work was undone.
pub enum McpServiceError {
    /// The initialized server did not advertise the required capability.
    Unsupported,
    /// Invalid local parameters or bounds; no request was sent.
    InvalidInput,
    /// A local I/O, cancellation, timeout, or validation failure.
    Local {
        /// Observed request delivery.
        delivery: McpDelivery,
        /// Redacted operational failure.
        failure: RuntimeFailure,
    },
    /// A complete correlated remote JSON-RPC error.
    Remote(McpRpcError),
    /// A correlated result violates the selected protocol or semantic constraints.
    InvalidResponse,
    /// A complete response exceeded a host service admission bound.
    ResponseLimit,
}
impl McpServiceError {
    /// Observed delivery, independent of service success.
    pub fn delivery(&self) -> McpDelivery {
        match self {
            Self::Unsupported | Self::InvalidInput => McpDelivery::NotSent,
            Self::Local { delivery, .. } => *delivery,
            Self::Remote(_) | Self::InvalidResponse | Self::ResponseLimit => {
                McpDelivery::ResponseReceived
            }
        }
    }
}
impl fmt::Debug for McpServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Unsupported"),
            Self::InvalidInput => f.write_str("InvalidInput"),
            Self::Local { delivery, failure } => f
                .debug_struct("Local")
                .field("delivery", delivery)
                .field("failure", failure)
                .finish(),
            Self::Remote(error) => f.debug_tuple("Remote").field(error).finish(),
            Self::InvalidResponse => f.write_str("InvalidResponse"),
            Self::ResponseLimit => f.write_str("ResponseLimit"),
        }
    }
}
impl fmt::Display for McpServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP service: {self:?}")
    }
}
impl std::error::Error for McpServiceError {}

/// Immutable response and absolute elapsed-time bounds for one utility RPC.
/// Effective bounds are also capped by the initialized session configuration.
#[derive(Clone, Copy, Debug)]
pub struct McpServiceLimits {
    response_bytes: usize,
    timeout_ms: u64,
}
impl McpServiceLimits {
    /// Checks positive representable response and time limits before dispatch.
    ///
    /// # Errors
    /// Returns invalid input for zero, overflowing, or unrepresentable bounds.
    pub fn new(response_bytes: usize, timeout_ms: u64) -> Result<Self, McpServiceError> {
        if response_bytes == 0
            || response_bytes > isize::MAX as usize
            || timeout_ms == 0
            || timeout_ms > i64::MAX as u64
            || std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .is_none()
        {
            return Err(McpServiceError::InvalidInput);
        }
        Ok(Self {
            response_bytes,
            timeout_ms,
        })
    }
    /// Maximum complete JSON-RPC response bytes, including its envelope.
    pub fn response_bytes(&self) -> usize {
        self.response_bytes
    }
    /// Absolute elapsed-time ceiling; progress does not renew it.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}

/// Supported catalog inventories. Enumeration never retrieves their content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpCatalogKind {
    /// Tool descriptors, without tools/call.
    Tools,
    /// Prompt descriptors, without prompts/get.
    Prompts,
    /// Resource descriptors, without resources/read.
    Resources,
    /// Resource-template descriptors, without expanding or reading a URI.
    ResourceTemplates,
}
impl McpCatalogKind {
    fn protocol(
        self,
    ) -> (
        &'static str,
        &'static str,
        &'static str,
        McpMessageKind,
        McpDescriptorKind,
    ) {
        match self {
            Self::Tools => (
                "tools",
                "tools/list",
                "tools",
                McpMessageKind::ToolList,
                McpDescriptorKind::Tool,
            ),
            Self::Prompts => (
                "prompts",
                "prompts/list",
                "prompts",
                McpMessageKind::PromptList,
                McpDescriptorKind::Prompt,
            ),
            Self::Resources => (
                "resources",
                "resources/list",
                "resources",
                McpMessageKind::ResourceList,
                McpDescriptorKind::Resource,
            ),
            Self::ResourceTemplates => (
                "resources",
                "resources/templates/list",
                "resourceTemplates",
                McpMessageKind::ResourceTemplateList,
                McpDescriptorKind::ResourceTemplate,
            ),
        }
    }
}

/// Exact opaque server cursor. Empty strings are valid and are never coerced to
/// the absence of a cursor. Debug does not disclose server-provided cursor text.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct McpCatalogCursor(String);
impl McpCatalogCursor {
    /// Checks only bounded representation, never parses or normalizes cursor text.
    ///
    /// # Errors
    /// Returns invalid input if the cursor exceeds representation limits.
    pub fn new(value: String, limits: &Limits) -> Result<Self, McpServiceError> {
        JsonDocument::from_value(&Value::Text(value.clone()), limits)
            .map_err(|_| McpServiceError::InvalidInput)?;
        Ok(Self(value))
    }
    /// Exact opaque text for explicitly persisting/resuming a page request.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for McpCatalogCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpCatalogCursor")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Immutable cumulative enumeration limits. Every accepted complete page counts
/// its original result bytes, including metadata and cursors, toward total bytes.
#[derive(Clone, Copy, Debug)]
pub struct McpCatalogLimits {
    pages: usize,
    items: usize,
    page: McpServiceLimits,
    total_bytes: usize,
}
impl McpCatalogLimits {
    /// Checks positive finite page, item, page-byte, total-byte, and time budgets.
    /// The time budget covers the entire enumeration, including local validation.
    ///
    /// # Errors
    /// Returns invalid input for zero or unrepresentable budgets.
    pub fn new(
        pages: usize,
        items: usize,
        page_bytes: usize,
        total_bytes: usize,
        timeout_ms: u64,
    ) -> Result<Self, McpServiceError> {
        if [pages, items, total_bytes]
            .into_iter()
            .any(|n| n == 0 || n > isize::MAX as usize)
        {
            return Err(McpServiceError::InvalidInput);
        }
        Ok(Self {
            pages,
            items,
            page: McpServiceLimits::new(page_bytes, timeout_ms)?,
            total_bytes,
        })
    }
    /// Maximum accepted pages, additionally capped by session catalog limits.
    pub fn pages(&self) -> usize {
        self.pages
    }
    /// Maximum retained descriptors, additionally capped by session catalog limits.
    pub fn items(&self) -> usize {
        self.items
    }
    /// Per-page complete response and overall enumeration time ceilings.
    pub fn page_limits(&self) -> McpServiceLimits {
        self.page
    }
    /// Maximum cumulative original result-object bytes admitted.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// One completely validated page. Descriptors preserve all extension fields.
/// Catalog mutation between pages is possible; this is not a snapshot guarantee.
pub struct McpCatalogPage {
    kind: McpCatalogKind,
    descriptors: Vec<JsonDocument>,
    next_cursor: Option<McpCatalogCursor>,
    response_bytes: usize,
}
impl McpCatalogPage {
    /// Catalog category validated for this page.
    pub fn kind(&self) -> McpCatalogKind {
        self.kind
    }
    /// Complete validated descriptors in server order.
    pub fn descriptors(&self) -> &[JsonDocument] {
        &self.descriptors
    }
    /// Exact next cursor; absence proves only the end of this traversal.
    pub fn next_cursor(&self) -> Option<&McpCatalogCursor> {
        self.next_cursor.as_ref()
    }
    /// Original result-object bytes charged, including unknown fields.
    pub fn response_bytes(&self) -> usize {
        self.response_bytes
    }
}

/// Why traversal stopped without observing a terminal page.
#[derive(Debug)]
pub enum McpCatalogStop {
    /// The next page would exceed the page budget.
    PageLimit,
    /// A page would exceed the remaining item budget. That page is not adopted.
    ItemLimit,
    /// A page would exceed the remaining cumulative byte budget.
    ByteLimit,
    /// Absolute enumeration deadline elapsed.
    Deadline,
    /// Caller cancellation was observed.
    Cancelled,
    /// A next cursor repeats a cursor already visited in this traversal.
    CursorCycle,
    /// A service/validation failure occurred after any retained prefix.
    Failed(McpServiceError),
}
/// Explicit terminal or incomplete catalog disposition.
#[derive(Debug)]
pub enum McpCatalogStatus {
    /// A validated page omitted nextCursor within all budgets.
    Complete,
    /// The retained validated prefix is incomplete for the specified reason.
    Incomplete(McpCatalogStop),
}
/// A bounded ordered prefix of complete validated pages. No descriptor is
/// automatically adopted, deduplicated, or interpreted as dispatch authority.
pub struct McpCatalog {
    kind: McpCatalogKind,
    descriptors: Vec<JsonDocument>,
    pages: usize,
    response_bytes: usize,
    next_cursor: Option<McpCatalogCursor>,
    status: McpCatalogStatus,
}
impl McpCatalog {
    /// Category of the retained descriptors.
    pub fn kind(&self) -> McpCatalogKind {
        self.kind
    }
    /// Validated descriptors, in page/server order, without silent truncation.
    pub fn descriptors(&self) -> &[JsonDocument] {
        &self.descriptors
    }
    /// Number of accepted complete pages, including accepted empty pages.
    pub fn pages(&self) -> usize {
        self.pages
    }
    /// Cumulative original accepted result bytes, including metadata/cursors.
    pub fn response_bytes(&self) -> usize {
        self.response_bytes
    }
    /// Next unadopted page's input cursor. None can mean an incomplete first page;
    /// always inspect status before treating None as traversal completion.
    pub fn next_cursor(&self) -> Option<&McpCatalogCursor> {
        self.next_cursor.as_ref()
    }
    /// Explicit completeness; partial data must never masquerade as a full catalog.
    pub fn status(&self) -> &McpCatalogStatus {
        &self.status
    }
}

/// Completion target, preserving a prompt name or exact resource-template URI.
#[derive(Clone)]
pub enum McpCompletionReference {
    /// A prompt argument.
    Prompt(String),
    /// A resource URI-template argument, never expanded or fetched here.
    ResourceTemplate(String),
}
/// Checked immutable completion parameters, including optional resolved arguments.
#[derive(Clone)]
pub struct McpCompletionRequest(JsonDocument);
impl McpCompletionRequest {
    /// Builds and validates completion parameters against the pinned request shape.
    /// Empty prefixes are valid. Context values are strings, not fetched content.
    ///
    /// # Errors
    /// Returns invalid input for representation or pinned-schema failures.
    pub fn new(
        reference: McpCompletionReference,
        argument_name: String,
        argument_value: String,
        context: Option<std::collections::BTreeMap<String, String>>,
        limits: &Limits,
    ) -> Result<Self, McpServiceError> {
        let reference = match reference {
            McpCompletionReference::Prompt(name) => obj([
                ("type", Value::Text("ref/prompt".into())),
                ("name", Value::Text(name)),
            ]),
            McpCompletionReference::ResourceTemplate(uri) => obj([
                ("type", Value::Text("ref/resource".into())),
                ("uri", Value::Text(uri)),
            ]),
        }
        .map_err(|_| McpServiceError::InvalidInput)?;
        let mut entries = vec![
            ("ref".into(), reference),
            (
                "argument".into(),
                obj([
                    ("name", Value::Text(argument_name)),
                    ("value", Value::Text(argument_value)),
                ])
                .map_err(|_| McpServiceError::InvalidInput)?,
            ),
        ];
        if let Some(context) = context {
            let arguments = Value::Map(
                Map::try_from_entries(context.into_iter().map(|(k, v)| (k, Value::Text(v))))
                    .map_err(|_| McpServiceError::InvalidInput)?,
            );
            entries.push((
                "context".into(),
                obj([("arguments", arguments)]).map_err(|_| McpServiceError::InvalidInput)?,
            ));
        }
        let params =
            Value::Map(Map::try_from_entries(entries).map_err(|_| McpServiceError::InvalidInput)?);
        let document =
            JsonDocument::from_value(&params, limits).map_err(|_| McpServiceError::InvalidInput)?;
        validate_completion_request(&document, limits)?;
        Ok(Self(document))
    }
    /// Checked protocol parameters without a synthetic JSON-RPC envelope.
    pub fn params(&self) -> &JsonDocument {
        &self.0
    }
}

/// Validated completion values and explicit remote continuation hints.
pub struct McpCompletion {
    document: JsonDocument,
    values: Vec<String>,
    total: Option<u64>,
    has_more: Option<bool>,
}
impl McpCompletion {
    /// At most 100 completion alternatives in server order.
    pub fn values(&self) -> &[String] {
        &self.values
    }
    /// Remote total when known; absent is unknown, not zero.
    pub fn total(&self) -> Option<u64> {
        self.total
    }
    /// Remote additional-values hint; absence remains unknown.
    pub fn has_more(&self) -> Option<bool> {
        self.has_more
    }
    /// Complete validated result, including extension fields.
    pub fn document(&self) -> &JsonDocument {
        &self.document
    }
}

/// MCP logging thresholds; exact protocol spellings are closed and validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpLoggingLevel {
    /// Debug messages and higher severity.
    Debug,
    /// Informational messages and higher severity.
    Info,
    /// Notice messages and higher severity.
    Notice,
    /// Warning messages and higher severity.
    Warning,
    /// Error messages and higher severity.
    Error,
    /// Critical messages and higher severity.
    Critical,
    /// Alert messages and higher severity.
    Alert,
    /// Emergency messages only.
    Emergency,
}
impl McpLoggingLevel {
    /// Exact wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Alert => "alert",
            Self::Emergency => "emergency",
        }
    }
}

/// A subscription RPC action. Local duplicate subscriber ownership is a separate
/// host intent-ledger concern; sending unsubscribe affects the remote URI globally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpResourceSubscriptionAction {
    /// Request resource-update notifications for this URI in this session.
    Subscribe,
    /// Request that notifications for this URI cease in this session.
    Unsubscribe,
}
/// Positive correlated subscription acknowledgement for one exact session epoch.
/// An acknowledgement promises neither replay nor uninterrupted delivery. Updates
/// already in flight may still arrive after an unsubscribe acknowledgement.
pub struct McpResourceSubscriptionAck {
    uri: String,
    epoch: u64,
    action: McpResourceSubscriptionAction,
    result: JsonDocument,
}
impl McpResourceSubscriptionAck {
    /// Exact opaque resource URI used in the request.
    pub fn uri(&self) -> &str {
        &self.uri
    }
    /// Source session epoch; acknowledgement must not be reused in a new epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Acknowledged protocol action.
    pub fn action(&self) -> McpResourceSubscriptionAction {
        self.action
    }
    /// Complete empty-result object, including metadata/extensions.
    pub fn result(&self) -> &JsonDocument {
        &self.result
    }
}

impl McpClientSession {
    /// Lists one fully validated page of any catalog category. No operation is
    /// invoked and no descriptor is adopted. The item ceiling also applies to this
    /// page; oversized pages fail rather than silently truncating their contents.
    ///
    /// # Errors
    /// Returns capability, request/response, item-budget, or protocol failures.
    pub async fn list_catalog(
        &self,
        kind: McpCatalogKind,
        cursor: Option<&McpCatalogCursor>,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalogPage, McpServiceError> {
        let (capability, method, field, page_kind, descriptor_kind) = kind.protocol();
        self.require_capability(capability, None)?;
        let params = match cursor {
            Some(cursor) => obj([("cursor", Value::Text(cursor.0.clone()))]),
            None => obj([]),
        }
        .map_err(|_| McpServiceError::InvalidInput)?;
        let (document, response_bytes) = self
            .service_request(method, params, limits, cancellation)
            .await?;
        let representation = self.representation_limits();
        validate_mcp_message(page_kind, &document, representation)
            .map_err(|_| McpServiceError::InvalidResponse)?;
        let fields = as_map(document.value()).map_err(|_| McpServiceError::InvalidResponse)?;
        let Some(Value::Array(items)) = fields.get(field) else {
            return Err(McpServiceError::InvalidResponse);
        };
        if items.len() > self.client_limits().catalog_items {
            return Err(McpServiceError::ResponseLimit);
        }
        let descriptors = items
            .iter()
            .map(|item| {
                let descriptor = JsonDocument::from_value(item, representation)
                    .map_err(|_| McpServiceError::InvalidResponse)?;
                validate_mcp_descriptor(descriptor_kind, &descriptor, representation)
                    .map_err(|_| McpServiceError::InvalidResponse)?;
                Ok(descriptor)
            })
            .collect::<Result<Vec<_>, McpServiceError>>()?;
        let next_cursor = match fields.get("nextCursor") {
            None => None,
            Some(Value::Text(cursor)) => Some(McpCatalogCursor(cursor.clone())),
            _ => return Err(McpServiceError::InvalidResponse),
        };
        Ok(McpCatalogPage {
            kind,
            descriptors,
            next_cursor,
            response_bytes,
        })
    }

    /// Enumerates a bounded prefix of complete pages, retaining explicit stop and
    /// resume state. Cursor history includes an initial resume cursor. A repeated
    /// cursor stops immediately before another request, even across empty pages.
    /// Catalogs can change between pages; duplicates are intentionally preserved.
    ///
    /// # Errors
    /// Returns missing capability before dispatch. Later failures are represented
    /// by an incomplete result so previously validated pages remain inspectable.
    pub async fn enumerate_catalog(
        &self,
        kind: McpCatalogKind,
        cursor: Option<&McpCatalogCursor>,
        limits: McpCatalogLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalog, McpServiceError> {
        self.require_capability(kind.protocol().0, None)?;
        let started = std::time::Instant::now();
        let deadline =
            Duration::from_millis(limits.page.timeout_ms.min(self.client_limits().timeout_ms));
        let mut seen = BTreeSet::new();
        let mut catalog = McpCatalog {
            kind,
            descriptors: Vec::new(),
            pages: 0,
            response_bytes: 0,
            next_cursor: cursor.cloned(),
            status: McpCatalogStatus::Complete,
        };
        if let Some(cursor) = cursor {
            seen.insert(cursor.clone());
        }
        let page_limit = limits.pages.min(self.client_limits().catalog_pages);
        let item_limit = limits.items.min(self.client_limits().catalog_items);
        let stopped = loop {
            if cancellation.is_cancelled() {
                break McpCatalogStop::Cancelled;
            }
            if started.elapsed() >= deadline {
                break McpCatalogStop::Deadline;
            }
            if catalog.pages >= page_limit {
                break McpCatalogStop::PageLimit;
            }
            if catalog.descriptors.len() >= item_limit {
                break McpCatalogStop::ItemLimit;
            }
            if catalog.response_bytes >= limits.total_bytes {
                break McpCatalogStop::ByteLimit;
            }
            let remaining_ms = (deadline.saturating_sub(started.elapsed()).as_millis())
                .min(u64::MAX as u128) as u64;
            if remaining_ms == 0 {
                break McpCatalogStop::Deadline;
            }
            let page_limits = McpServiceLimits {
                timeout_ms: remaining_ms,
                ..limits.page
            };
            let page = match self
                .list_catalog(
                    kind,
                    catalog.next_cursor.as_ref(),
                    page_limits,
                    cancellation.clone(),
                )
                .await
            {
                Ok(page) => page,
                Err(McpServiceError::Local { failure, .. })
                    if failure.code() == FailureCode::McpTimeout =>
                {
                    break McpCatalogStop::Deadline;
                }
                Err(McpServiceError::Local { failure, .. })
                    if failure.code() == FailureCode::Cancelled =>
                {
                    break McpCatalogStop::Cancelled;
                }
                Err(error) => break McpCatalogStop::Failed(error),
            };
            if cancellation.is_cancelled() {
                break McpCatalogStop::Cancelled;
            }
            if started.elapsed() >= deadline {
                break McpCatalogStop::Deadline;
            }
            if page.response_bytes > limits.total_bytes - catalog.response_bytes {
                break McpCatalogStop::ByteLimit;
            }
            if page.descriptors.len() > item_limit - catalog.descriptors.len() {
                break McpCatalogStop::ItemLimit;
            }
            catalog.pages += 1;
            catalog.response_bytes += page.response_bytes;
            catalog.descriptors.extend(page.descriptors);
            catalog.next_cursor = page.next_cursor;
            match &catalog.next_cursor {
                None => return Ok(catalog),
                Some(cursor) if !seen.insert(cursor.clone()) => break McpCatalogStop::CursorCycle,
                Some(_) => {}
            }
        };
        catalog.status = McpCatalogStatus::Incomplete(stopped);
        Ok(catalog)
    }

    /// Lists one tools page. See [`Self::list_catalog`] for bounds and validation.
    pub async fn list_tools(
        &self,
        cursor: Option<&McpCatalogCursor>,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalogPage, McpServiceError> {
        self.list_catalog(McpCatalogKind::Tools, cursor, limits, cancellation)
            .await
    }
    /// Lists one prompts page without retrieving prompts.
    pub async fn list_prompts(
        &self,
        cursor: Option<&McpCatalogCursor>,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalogPage, McpServiceError> {
        self.list_catalog(McpCatalogKind::Prompts, cursor, limits, cancellation)
            .await
    }
    /// Lists one resources page without reading resource content.
    pub async fn list_resources(
        &self,
        cursor: Option<&McpCatalogCursor>,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalogPage, McpServiceError> {
        self.list_catalog(McpCatalogKind::Resources, cursor, limits, cancellation)
            .await
    }
    /// Lists one resource-template page without expanding or fetching a URI.
    pub async fn list_resource_templates(
        &self,
        cursor: Option<&McpCatalogCursor>,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalogPage, McpServiceError> {
        self.list_catalog(
            McpCatalogKind::ResourceTemplates,
            cursor,
            limits,
            cancellation,
        )
        .await
    }
    /// Enumerates tools within cumulative page/item/byte/time budgets.
    pub async fn enumerate_tools(
        &self,
        limits: McpCatalogLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalog, McpServiceError> {
        self.enumerate_catalog(McpCatalogKind::Tools, None, limits, cancellation)
            .await
    }
    /// Enumerates prompts without retrieving prompt messages.
    pub async fn enumerate_prompts(
        &self,
        limits: McpCatalogLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalog, McpServiceError> {
        self.enumerate_catalog(McpCatalogKind::Prompts, None, limits, cancellation)
            .await
    }
    /// Enumerates resources without reading their content.
    pub async fn enumerate_resources(
        &self,
        limits: McpCatalogLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalog, McpServiceError> {
        self.enumerate_catalog(McpCatalogKind::Resources, None, limits, cancellation)
            .await
    }
    /// Enumerates templates without expanding/fetching resources.
    pub async fn enumerate_resource_templates(
        &self,
        limits: McpCatalogLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCatalog, McpServiceError> {
        self.enumerate_catalog(
            McpCatalogKind::ResourceTemplates,
            None,
            limits,
            cancellation,
        )
        .await
    }

    /// Completes a prompt or resource-template argument after capability and pinned
    /// shape validation. The protocol's 100-value ceiling is checked independently
    /// because the pinned generated schema omits this semantic constraint.
    ///
    /// # Errors
    /// Returns unsupported capability, transport/protocol errors, or invalid totals.
    pub async fn complete(
        &self,
        request: &McpCompletionRequest,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpCompletion, McpServiceError> {
        self.require_capability("completions", None)?;
        validate_completion_request(request.params(), self.representation_limits())?;
        let (document, _) = self
            .service_request(
                "completion/complete",
                request.0.value().clone(),
                limits,
                cancellation,
            )
            .await?;
        validate_mcp_message(
            McpMessageKind::CompletionResult,
            &document,
            self.representation_limits(),
        )
        .map_err(|_| McpServiceError::InvalidResponse)?;
        let fields = as_map(document.value()).map_err(|_| McpServiceError::InvalidResponse)?;
        let completion = fields
            .get("completion")
            .and_then(|v| as_map(v).ok())
            .ok_or(McpServiceError::InvalidResponse)?;
        let Some(Value::Array(values)) = completion.get("values") else {
            return Err(McpServiceError::InvalidResponse);
        };
        if values.len() > 100 {
            return Err(McpServiceError::InvalidResponse);
        }
        let values = values
            .iter()
            .map(|v| match v {
                Value::Text(text) => Ok(text.clone()),
                _ => Err(McpServiceError::InvalidResponse),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let total = match completion.get("total") {
            None => None,
            Some(Value::Integer(total)) if *total >= values.len() as i64 => Some(*total as u64),
            _ => return Err(McpServiceError::InvalidResponse),
        };
        let has_more = match completion.get("hasMore") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            _ => return Err(McpServiceError::InvalidResponse),
        };
        Ok(McpCompletion {
            document,
            values,
            total,
            has_more,
        })
    }

    /// Sets a capability-gated server logging threshold. Log notifications remain
    /// ordinary bounded host events and never authorize additional acquisition.
    ///
    /// # Errors
    /// Returns unsupported capability, transport, or invalid result shape.
    pub async fn set_logging_level(
        &self,
        level: McpLoggingLevel,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<JsonDocument, McpServiceError> {
        self.require_capability("logging", None)?;
        let params = obj([("level", Value::Text(level.as_str().into()))])
            .map_err(|_| McpServiceError::InvalidInput)?;
        let (document, _) = self
            .service_request("logging/setLevel", params, limits, cancellation)
            .await?;
        validate_empty_result(&document)?;
        Ok(document)
    }

    /// Sends resources/subscribe and returns an epoch-bound positive acknowledgement.
    /// The host must durably record intent before calling when recovery is needed.
    /// Notifications never trigger resource reads. This method does not promise
    /// replay, reconnect restoration, or local duplicate-subscriber coordination.
    ///
    /// # Errors
    /// Returns missing resources.subscribe, invalid URI representation, RPC failure,
    /// or invalid acknowledgement. Possibly-sent failures require reconciliation.
    pub async fn subscribe_resource(
        &self,
        uri: &str,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpResourceSubscriptionAck, McpServiceError> {
        self.resource_subscription(
            McpResourceSubscriptionAction::Subscribe,
            uri,
            limits,
            cancellation,
        )
        .await
    }
    /// Sends resources/unsubscribe. Already-queued updates can arrive after its
    /// acknowledgement and must remain observable rather than causing content reads.
    ///
    /// # Errors
    /// Returns the same capability/delivery-aware errors as subscribe_resource.
    pub async fn unsubscribe_resource(
        &self,
        uri: &str,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpResourceSubscriptionAck, McpServiceError> {
        self.resource_subscription(
            McpResourceSubscriptionAction::Unsubscribe,
            uri,
            limits,
            cancellation,
        )
        .await
    }

    async fn resource_subscription(
        &self,
        action: McpResourceSubscriptionAction,
        uri: &str,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpResourceSubscriptionAck, McpServiceError> {
        self.require_capability("resources", Some("subscribe"))?;
        let params =
            obj([("uri", Value::Text(uri.into()))]).map_err(|_| McpServiceError::InvalidInput)?;
        let method = match action {
            McpResourceSubscriptionAction::Subscribe => "resources/subscribe",
            McpResourceSubscriptionAction::Unsubscribe => "resources/unsubscribe",
        };
        let (result, _) = self
            .service_request(method, params, limits, cancellation)
            .await?;
        validate_empty_result(&result)?;
        Ok(McpResourceSubscriptionAck {
            uri: uri.into(),
            epoch: self.epoch(),
            action,
            result,
        })
    }

    fn require_capability(
        &self,
        capability: &str,
        extension: Option<&str>,
    ) -> Result<(), McpServiceError> {
        let supported = as_map(self.initialization().value())
            .ok()
            .and_then(|v| v.get("capabilities"))
            .and_then(|v| as_map(v).ok())
            .and_then(|v| v.get(capability))
            .and_then(|v| as_map(v).ok())
            .is_some_and(|v| extension.is_none_or(|key| v.get(key) == Some(&Value::Bool(true))));
        if supported {
            Ok(())
        } else {
            Err(McpServiceError::Unsupported)
        }
    }

    async fn service_request(
        &self,
        method: &str,
        params: Value,
        limits: McpServiceLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<(JsonDocument, usize), McpServiceError> {
        // Check outgoing parameters against the session's representation limits,
        // independently of the caller's possibly smaller response ceiling.
        JsonDocument::from_value(&params, self.representation_limits())
            .map_err(|_| McpServiceError::InvalidInput)?;
        let request_cancellation = Arc::new(McpCancellation::new());
        let _cancel_on_drop = CancelOnDrop(request_cancellation.clone());
        let client = self.client_limits();
        let mut response_limits = self.representation_limits().clone();
        let bytes = limits
            .response_bytes
            .min(response_limits.max_document_bytes)
            .min(client.frame_bytes);
        response_limits.max_document_bytes = bytes;
        let request = McpRequest {
            response_limits,
            cancellation: request_cancellation.clone(),
            cancellation_grace_ms: client.cancellation_grace_ms.min(limits.timeout_ms),
            method: method.into(),
            params,
            timeout_ms: limits.timeout_ms.min(client.timeout_ms),
            max_response_bytes: bytes,
            idempotency_key: None,
        };
        // The public dispatcher also bounds its transport wait. This outer bound
        // includes time spent queued before dispatch. Dropping it cancels the
        // linked signal while preserving conservative delivery evidence.
        let invoke = self.invoke(request);
        tokio::pin!(invoke);
        let completed = async {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    request_cancellation.cancel();
                    invoke.await
                },
                reply = &mut invoke => reply,
            }
        };
        let reply = tokio::time::timeout(
            Duration::from_millis(limits.timeout_ms.min(client.timeout_ms)),
            completed,
        )
        .await
        .map_err(|_| McpServiceError::Local {
            delivery: McpDelivery::PossiblySent,
            failure: crate::mcp::fail(FailureCode::McpTimeout),
        })?;
        let delivery = reply.delivery;
        let response_bytes = reply.raw_result.as_ref().map(Vec::len);
        let result = reply
            .result
            .map_err(|failure| McpServiceError::Local { delivery, failure })?
            .map_err(McpServiceError::Remote)?;
        let response_bytes = response_bytes.unwrap_or(result.as_bytes().len());
        if response_bytes > bytes || result.as_bytes().len() > bytes {
            return Err(McpServiceError::ResponseLimit);
        }
        Ok((result, response_bytes))
    }
}

struct CancelOnDrop(Arc<McpCancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn validate_completion_request(
    params: &JsonDocument,
    limits: &Limits,
) -> Result<(), McpServiceError> {
    let envelope = obj([
        ("jsonrpc", Value::Text("2.0".into())),
        ("id", Value::Integer(0)),
        ("method", Value::Text("completion/complete".into())),
        ("params", params.value().clone()),
    ])
    .map_err(|_| McpServiceError::InvalidInput)?;
    let envelope =
        JsonDocument::from_value(&envelope, limits).map_err(|_| McpServiceError::InvalidInput)?;
    validate_mcp_message(McpMessageKind::CompletionRequest, &envelope, limits)
        .map_err(|_| McpServiceError::InvalidInput)
}
fn validate_empty_result(document: &JsonDocument) -> Result<(), McpServiceError> {
    let fields = as_map(document.value()).map_err(|_| McpServiceError::InvalidResponse)?;
    if fields
        .get("_meta")
        .is_some_and(|v| !matches!(v, Value::Map(_)))
    {
        return Err(McpServiceError::InvalidResponse);
    }
    Ok(())
}
