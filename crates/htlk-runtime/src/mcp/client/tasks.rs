//! Experimental MCP protocol tasks. These are remote operations, unrelated to
//! workflow graph task syntax. Creation, acceptance and operation results remain
//! distinct; no operation in this module retries a task-creating tools/call.
use super::{McpClientReply, McpClientSession};
use htlk_analyzer::{
    McpDescriptorKind, McpMessageKind, validate_mcp_descriptor, validate_mcp_message,
};
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use std::{collections::BTreeSet, fmt, sync::Arc, time::Duration};

use crate::mcp::{McpCancellation, McpRequest};

/// The selected protocol's task association metadata key.
pub const MCP_RELATED_TASK: &str = "io.modelcontextprotocol/related-task";

/// Redacted task admission or semantic-validation error. Wire errors and their
/// delivery evidence remain in the accompanying `McpClientReply`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTaskError {
    /// A durable intent/acceptance/update failed. The original wire reply, if any,
    /// remains available; uncertainty never authorizes another task-creating send.
    Persistence(crate::failure::FailureCode),
    /// The negotiated peer does not support this operation.
    Unsupported,
    /// Invalid local selection, descriptor, parameters or finite bounds.
    InvalidInput,
    /// Invalid task/result shape or task association.
    InvalidResponse,
    /// A task changed identity, creation time, TTL or immutable terminal status,
    /// or its update timestamp moved backwards.
    InvalidTransition,
    /// tasks/result failed as a retrieval RPC, without a proven underlying task
    /// error. Original wire evidence remains in reply; completion stays unknown.
    Retrieval,
    /// Local page, item, byte, request or elapsed-time budget was exhausted.
    Limit,
    /// The granted retention lifetime has elapsed; it is not safe to assume that
    /// the receiver still retains the operation. This does not cancel its work.
    Expired,
    /// Local waiting was cancelled. Explicit remote cancellation is separate.
    Cancelled,
}
impl fmt::Display for McpTaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP task: {self:?}")
    }
}
impl std::error::Error for McpTaskError {}

/// One observed RPC, preserving exact bytes, protected remote errors and delivery
/// even when typed admission fails. `Ok(None)` means no typed result was admitted;
/// inspect `reply.result` for the original outcome. For tasks/result, a validated
/// underlying execution error is `Ok(Some(()))`; an unproven retrieval RPC error
/// is `Err(McpTaskError::Retrieval)` and cannot establish operation completion.
#[derive(Debug)]
pub struct McpTaskReply<T> {
    /// Unmodified correlated wire evidence and response-capacity reservation.
    pub reply: McpClientReply,
    /// Typed interpretation; never substitutes task acceptance for a tool result.
    pub value: Result<Option<T>, McpTaskError>,
}

/// Exhaustive task services advertised by the initialized server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpTaskCapabilities {
    /// Task-augmented tools/call.
    pub tool_call: bool,
    /// Paginated tasks/list.
    pub list: bool,
    /// Explicit tasks/cancel.
    pub cancel: bool,
}

/// Explicit, frozen dispatch choice for a tool descriptor. Optional tools require
/// the host to choose; this API never silently switches between execution modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpToolTaskMode {
    /// Ordinary synchronous tools/call (forbidden for task-required descriptors).
    Inline,
    /// Task-augmented tools/call, with an optional requested retention duration.
    Task {
        /// Requested milliseconds from creation; the receiver may override this.
        ttl_ms: Option<u64>,
    },
}

/// Bounded per-RPC execution and whole-poll/list campaign limits. Effective limits
/// are capped by the initialized session, including bytes, pages and item counts.
#[derive(Clone, Copy, Debug)]
pub struct McpTaskLimits {
    response_bytes: usize,
    timeout_ms: u64,
    requests: usize,
    items: usize,
    total_bytes: usize,
    poll_interval_ms: u64,
}
impl McpTaskLimits {
    /// Checks finite positive resource bounds. `poll_interval_ms` is the fallback
    /// and local minimum; a larger server pollInterval is always respected.
    ///
    /// # Errors
    /// Returns invalid input for zero, overflowing or unrepresentable bounds.
    pub fn new(
        response_bytes: usize,
        timeout_ms: u64,
        requests: usize,
        items: usize,
        total_bytes: usize,
        poll_interval_ms: u64,
    ) -> Result<Self, McpTaskError> {
        if [response_bytes, requests, items, total_bytes]
            .into_iter()
            .any(|n| n == 0 || n > isize::MAX as usize)
            || [timeout_ms, poll_interval_ms].into_iter().any(|ms| {
                ms == 0
                    || ms > i64::MAX as u64
                    || tokio::time::Instant::now()
                        .checked_add(Duration::from_millis(ms))
                        .is_none()
            })
        {
            return Err(McpTaskError::InvalidInput);
        }
        Ok(Self {
            response_bytes,
            timeout_ms,
            requests,
            items,
            total_bytes,
            poll_interval_ms,
        })
    }
}

/// A protocol task's execution state, distinct from delivery of its RPC result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTaskStatus {
    /// Execution is active.
    Working,
    /// The requestor should open tasks/result to service task-related callbacks.
    InputRequired,
    /// Successful underlying operation.
    Completed,
    /// Underlying protocol error or tool result with isError=true.
    Failed,
    /// Cancellation was accepted; later underlying completion cannot change this.
    Cancelled,
}
impl McpTaskStatus {
    /// Whether further execution-state transitions are prohibited.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Immutable validated observation suitable for protected persistence. Debug
/// omits task IDs, status messages and other server-controlled private strings.
#[derive(Clone)]
pub struct McpTaskState {
    document: JsonDocument,
    id: String,
    status: McpTaskStatus,
    created: Timestamp,
    updated: Timestamp,
    ttl_ms: Option<u64>,
    poll_interval_ms: Option<u64>,
}
impl fmt::Debug for McpTaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpTaskState")
            .field("status", &self.status)
            .field("ttl_ms", &self.ttl_ms)
            .field("poll_interval_ms", &self.poll_interval_ms)
            .finish_non_exhaustive()
    }
}
impl McpTaskState {
    /// Validates the pinned Task schema, RFC3339 timestamps, nonnegative duration
    /// values and update ordering. Extension fields remain in the document.
    ///
    /// # Errors
    /// Returns invalid response shape or lifecycle timestamps.
    pub fn from_document(document: JsonDocument, limits: &Limits) -> Result<Self, McpTaskError> {
        validate_mcp_message(McpMessageKind::Task, &document, limits)
            .map_err(|_| McpTaskError::InvalidResponse)?;
        let fields = map(document.value())?;
        let id = text(fields, "taskId")?.to_owned();
        let status = match text(fields, "status")? {
            "working" => McpTaskStatus::Working,
            "input_required" => McpTaskStatus::InputRequired,
            "completed" => McpTaskStatus::Completed,
            "failed" => McpTaskStatus::Failed,
            "cancelled" => McpTaskStatus::Cancelled,
            _ => return Err(McpTaskError::InvalidResponse),
        };
        let created = timestamp(text(fields, "createdAt")?)?;
        let updated = timestamp(text(fields, "lastUpdatedAt")?)?;
        if updated < created {
            return Err(McpTaskError::InvalidResponse);
        }
        let ttl_ms = match fields.get("ttl") {
            Some(Value::Null) => None,
            Some(Value::Integer(n)) if *n >= 0 => Some(*n as u64),
            _ => return Err(McpTaskError::InvalidResponse),
        };
        let poll_interval_ms = match fields.get("pollInterval") {
            None => None,
            Some(Value::Integer(n)) if *n >= 0 => Some(*n as u64),
            _ => return Err(McpTaskError::InvalidResponse),
        };
        Ok(Self {
            document,
            id,
            status,
            created,
            updated,
            ttl_ms,
            poll_interval_ms,
        })
    }
    /// Exact opaque receiver-generated identity. Empty strings remain strings.
    pub fn task_id(&self) -> &str {
        &self.id
    }
    /// Current validated status.
    pub fn status(&self) -> McpTaskStatus {
        self.status
    }
    /// Granted retention duration from creation, or unlimited remote retention.
    pub fn ttl_ms(&self) -> Option<u64> {
        self.ttl_ms
    }
    pub(crate) fn remaining_ttl_ms(&self, now_ms: i64) -> Option<u64> {
        self.ttl_ms.map(|ttl| {
            ((self.created.nanos() / 1_000_000 + i128::from(ttl) - i128::from(now_ms))
                .max(0)
                .min(i128::from(ttl))) as u64
        })
    }
    /// Receiver's minimum suggested polling interval in milliseconds.
    pub fn poll_interval_ms(&self) -> Option<u64> {
        self.poll_interval_ms
    }
    /// Protected complete observation, including timestamps and statusMessage.
    pub fn document(&self) -> &JsonDocument {
        &self.document
    }
    /// Checks a later observation independently of transport and storage. A store
    /// must repeat this check inside its fenced task-update transaction.
    ///
    /// # Errors
    /// Returns changed identity/retention, backwards time or terminal regression.
    pub fn validate_successor(&self, next: &Self) -> Result<(), McpTaskError> {
        if self.id != next.id
            || self.created != next.created
            || self.ttl_ms != next.ttl_ms
            || next.updated < self.updated
            || (self.status.is_terminal() && self.status != next.status)
        {
            return Err(McpTaskError::InvalidTransition);
        }
        Ok(())
    }
}

/// A one-shot dispatch frozen to one descriptor, session epoch and execution mode.
/// Consuming it sends at most one tools/call. Lost acceptance is never recreated.
pub struct McpPreparedToolCall {
    session: McpClientSession,
    descriptor: JsonDocument,
    params: JsonDocument,
    mode: McpToolTaskMode,
    logical_key: Digest,
}
impl fmt::Debug for McpPreparedToolCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpPreparedToolCall")
            .field("epoch", &self.session.epoch())
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// Typed distinction between ordinary output and acceptance of deferred work.
#[derive(Debug)]
pub enum McpToolTaskAcceptance {
    /// The unmodified reply holds an admitted CallToolResult.
    Inline,
    /// A receiver-generated identity; the reply is only CreateTaskResult.
    Accepted(Box<McpRemoteTask>),
}

/// Live authority for one accepted task on one initialized owner/server/epoch.
/// It is deliberately not Clone: observations are serialized and terminal states
/// cannot be overwritten by racing local polling. Durable restoration must be an
/// explicit authorized action using a previously committed identity.
pub struct McpRemoteTask {
    session: McpClientSession,
    state: McpTaskState,
    observed: tokio::time::Instant,
    expires: Option<tokio::time::Instant>,
    result_identity: Option<Digest>,
    result_status: Option<McpTaskStatus>,
    intent_key: Option<Digest>,
    response_limits: Limits,
}
impl fmt::Debug for McpRemoteTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpRemoteTask")
            .field("epoch", &self.session.epoch())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl McpClientSession {
    /// Interprets only the negotiated server's task capabilities. Receiving
    /// sampling/elicitation tasks is a separate, installed durable client service.
    ///
    /// # Errors
    /// Returns invalid response if a task capability has an invalid shape.
    pub fn task_capabilities(&self) -> Result<McpTaskCapabilities, McpTaskError> {
        let caps = map(self.initialization().value())?
            .get("capabilities")
            .ok_or(McpTaskError::InvalidResponse)?;
        let Some(tasks) = map(caps)?.get("tasks") else {
            return Ok(McpTaskCapabilities::default());
        };
        let tasks = map(tasks)?;
        let leaf = |key| -> Result<bool, McpTaskError> {
            tasks.get(key).map(map).transpose().map(|v| v.is_some())
        };
        let tool_call = match tasks.get("requests") {
            None => false,
            Some(requests) => match map(requests)?.get("tools") {
                None => false,
                Some(tools) => map(tools)?.get("call").map(map).transpose()?.is_some(),
            },
        };
        Ok(McpTaskCapabilities {
            tool_call,
            list: leaf("list")?,
            cancel: leaf("cancel")?,
        })
    }

    /// Validates and freezes descriptor taskSupport plus server capability rules
    /// before any request. `params` must contain the matching name and optional
    /// argument/_meta maps, with no preexisting task augmentation.
    ///
    /// # Errors
    /// Returns unsupported/invalid selection or malformed descriptor/parameters.
    pub fn prepare_tool_task_call(
        &self,
        descriptor: &JsonDocument,
        params: JsonDocument,
        mode: McpToolTaskMode,
    ) -> Result<McpPreparedToolCall, McpTaskError> {
        validate_mcp_descriptor(
            McpDescriptorKind::Tool,
            descriptor,
            self.representation_limits(),
        )
        .map_err(|_| McpTaskError::InvalidInput)?;
        let frozen_descriptor = descriptor.clone();
        let descriptor = map(descriptor.value()).map_err(|_| McpTaskError::InvalidInput)?;
        let support = match descriptor.get("execution") {
            None => "forbidden",
            Some(execution) => match map(execution)?.get("taskSupport") {
                None => "forbidden",
                Some(Value::Text(value)) => value.as_str(),
                _ => return Err(McpTaskError::InvalidInput),
            },
        };
        let task = matches!(mode, McpToolTaskMode::Task { .. });
        if (task && (!self.task_capabilities()?.tool_call || support == "forbidden"))
            || (!task && support == "required")
        {
            return Err(McpTaskError::Unsupported);
        }
        if !matches!(support, "required" | "optional" | "forbidden") {
            return Err(McpTaskError::InvalidInput);
        }
        let fields = map(params.value()).map_err(|_| McpTaskError::InvalidInput)?;
        if fields.get("name") != descriptor.get("name")
            || fields.get("task").is_some()
            || fields
                .get("arguments")
                .is_some_and(|v| !matches!(v, Value::Map(_)))
            || fields
                .get("_meta")
                .is_some_and(|v| !matches!(v, Value::Map(_)))
        {
            return Err(McpTaskError::InvalidInput);
        }
        if let Some(Value::Map(meta)) = fields.get("_meta")
            && meta
                .get("progressToken")
                .is_some_and(|v| !matches!(v, Value::Integer(_) | Value::Text(_)))
        {
            return Err(McpTaskError::InvalidInput);
        }
        let mut entries: Vec<_> = fields
            .iter()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect();
        if let McpToolTaskMode::Task { ttl_ms } = mode {
            let task = match ttl_ms {
                None => object(vec![])?,
                Some(ms) => object(vec![(
                    "ttl".into(),
                    Value::Integer(i64::try_from(ms).map_err(|_| McpTaskError::InvalidInput)?),
                )])?,
            };
            entries.push(("task".into(), task));
        }
        let params = JsonDocument::from_value(&object(entries)?, self.representation_limits())
            .map_err(|_| McpTaskError::InvalidInput)?;
        Ok(McpPreparedToolCall {
            session: self.clone(),
            descriptor: frozen_descriptor,
            params,
            mode,
            logical_key: self.fresh_task_key(),
        })
    }

    /// Explicitly attaches an already-authorized, previously persisted task state
    /// to this epoch. Performs no creation or lookup. The embedding host must check
    /// principal, server identity and reconnect authority before calling this.
    ///
    /// `remaining_ttl_ms` must come from the original trusted local retention
    /// deadline, persisted at acceptance, rather than resetting TTL on reconnect.
    /// It is required for finite remote TTLs; it may impose a local limit on an
    /// unlimited remote task as well.
    ///
    /// # Errors
    /// Returns unsupported negotiation, missing finite retention or exhausted TTL.
    pub fn resume_remote_task(
        &self,
        state: McpTaskState,
        remaining_ttl_ms: Option<u64>,
    ) -> Result<McpRemoteTask, McpTaskError> {
        if !self.task_capabilities()?.tool_call {
            return Err(McpTaskError::Unsupported);
        }
        if state.ttl_ms.is_some() && remaining_ttl_ms.is_none() {
            return Err(McpTaskError::InvalidInput);
        }
        let mut task = McpRemoteTask::new(self.clone(), state)?;
        if let Some(remaining) = remaining_ttl_ms {
            let local = tokio::time::Instant::now()
                .checked_add(Duration::from_millis(remaining))
                .ok_or(McpTaskError::InvalidInput)?;
            task.expires = Some(task.expires.map_or(local, |remote| remote.min(local)));
        }
        task.check_retention()?;
        Ok(task)
    }

    /// Gets one bounded tasks/list page. A page is an observation, not authority
    /// to adopt other principals' work. Cursors remain opaque, including empty ones.
    pub async fn task_list_page(
        &self,
        cursor: Option<&str>,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskReply<McpTaskPage>, McpTaskError> {
        if !self.task_capabilities()?.list {
            return Err(McpTaskError::Unsupported);
        }
        let params = object(
            cursor
                .map(|s| vec![("cursor".into(), Value::Text(s.into()))])
                .unwrap_or_default(),
        )?;
        let reply = rpc(
            self,
            "tasks/list",
            params,
            limits,
            cancellation,
            self.representation_limits(),
        )
        .await?;
        let value = success(&reply)
            .map(|document| {
                validate_mcp_message(
                    McpMessageKind::TaskList,
                    document,
                    self.representation_limits(),
                )
                .map_err(|_| McpTaskError::InvalidResponse)?;
                let fields = map(document.value())?;
                let Some(Value::Array(values)) = fields.get("tasks") else {
                    return Err(McpTaskError::InvalidResponse);
                };
                if values.len() > limits.items.min(self.client_limits().catalog_items) {
                    return Err(McpTaskError::Limit);
                }
                let mut ids = BTreeSet::new();
                let mut tasks = Vec::with_capacity(values.len());
                for value in values {
                    let state = state(value, self.representation_limits())?;
                    if !ids.insert(state.id.clone()) {
                        return Err(McpTaskError::InvalidResponse);
                    }
                    tasks.push(state);
                }
                let next_cursor = match fields.get("nextCursor") {
                    None => None,
                    Some(Value::Text(value)) => Some(value.clone()),
                    _ => return Err(McpTaskError::InvalidResponse),
                };
                Ok(McpTaskPage { tasks, next_cursor })
            })
            .transpose();
        Ok(McpTaskReply { reply, value })
    }

    /// Enumerates tasks under cumulative page, item, byte and absolute-time bounds.
    /// Repeated opaque cursors and duplicate task identities stop enumeration with
    /// an explicit partial inventory. No task is adopted or executed by listing.
    pub async fn list_remote_tasks(
        &self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskInventory, McpTaskError> {
        let end = deadline(self, limits);
        let pages = limits.requests.min(self.client_limits().catalog_pages);
        let items = limits.items.min(self.client_limits().catalog_items);
        let mut inventory = McpTaskInventory {
            tasks: vec![],
            complete: false,
            stop: None,
            last_reply: None,
        };
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut bytes = 0usize;
        for _ in 0..pages {
            let next_limits = match remaining(limits, end) {
                Ok(limits) => limits,
                Err(error) => {
                    inventory.stop = Some(error);
                    return Ok(inventory);
                }
            };
            // Release the previous response's reservation before dispatch.
            inventory.last_reply = None;
            let page = self
                .task_list_page(cursor.as_deref(), next_limits, cancellation.clone())
                .await?;
            bytes = bytes
                .checked_add(reply_bytes(&page.reply))
                .ok_or(McpTaskError::Limit)?;
            let admitted = match &page.value {
                Ok(Some(page)) => Some(page),
                _ => None,
            };
            if bytes > limits.total_bytes.min(self.client_limits().retained_bytes) {
                inventory.stop = Some(McpTaskError::Limit);
            } else if let Some(admitted) = admitted {
                if inventory.tasks.len().saturating_add(admitted.tasks.len()) > items {
                    inventory.stop = Some(McpTaskError::Limit);
                } else if admitted
                    .tasks
                    .iter()
                    .any(|task| identities.contains(task.task_id()))
                {
                    inventory.stop = Some(McpTaskError::InvalidResponse);
                } else {
                    for task in &admitted.tasks {
                        identities.insert(task.id.clone());
                        inventory.tasks.push(task.clone());
                    }
                    match &admitted.next_cursor {
                        None => inventory.complete = true,
                        Some(next) if !cursors.insert(next.clone()) => {
                            inventory.stop = Some(McpTaskError::InvalidResponse)
                        }
                        Some(next) => cursor = Some(next.clone()),
                    }
                }
            }
            let failed = !matches!(page.value, Ok(Some(_)));
            inventory.last_reply = Some(page);
            if failed || inventory.complete || inventory.stop.is_some() {
                return Ok(inventory);
            }
        }
        inventory.stop = Some(McpTaskError::Limit);
        Ok(inventory)
    }
}

impl McpPreparedToolCall {
    /// Pins a host-selected stable work key across repeated preparation/restart.
    /// Once its durable intent exists it never grants another creating send.
    pub fn with_logical_key(mut self, key: Digest) -> Self {
        self.logical_key = key;
        self
    }
    /// Exact admitted descriptor, for an independently checked durable intent.
    pub fn descriptor(&self) -> &JsonDocument {
        &self.descriptor
    }
    /// Owning principal/server/epoch authority frozen with the dispatch choice.
    pub fn session(&self) -> &McpClientSession {
        &self.session
    }
    /// Frozen complete request parameters, for durable intent/digest recording
    /// before dispatch. Task acceptance must subsequently be persisted separately.
    pub fn params(&self) -> &JsonDocument {
        &self.params
    }
    /// Selected task/inline policy, frozen before effectful dispatch.
    pub fn mode(&self) -> McpToolTaskMode {
        self.mode
    }
    /// Executes exactly one request. In particular, PossiblySent after a lost
    /// create acknowledgement is returned unchanged and never reissued.
    pub async fn dispatch(
        self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskReply<McpToolTaskAcceptance>, McpTaskError> {
        self.dispatch_inner(limits, cancellation, None, None).await
    }
    pub(crate) async fn dispatch_operation(
        self,
        request: McpRequest,
        context: Option<crate::mcp::McpAttemptContext>,
    ) -> Result<McpTaskReply<McpToolTaskAcceptance>, McpTaskError> {
        let limits = McpTaskLimits::new(
            request.max_response_bytes,
            request.timeout_ms,
            1024,
            1024,
            self.session.client_limits().retained_bytes,
            10,
        )?;
        let cancellation = request.cancellation.clone();
        self.dispatch_inner(limits, cancellation, Some(request), context)
            .await
    }
    async fn dispatch_inner(
        self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
        request: Option<McpRequest>,
        context: Option<crate::mcp::McpAttemptContext>,
    ) -> Result<McpTaskReply<McpToolTaskAcceptance>, McpTaskError> {
        let response_limits = request
            .as_ref()
            .map(|r| r.response_limits.clone())
            .unwrap_or_else(|| self.session.representation_limits().clone());
        if matches!(self.mode, McpToolTaskMode::Task { .. })
            && let Some(persistence) = self.session.persistence()
        {
            let now = persistence
                .clock
                .now_ms()
                .map_err(|e| McpTaskError::Persistence(e.code()))?;
            let deadline = context
                .as_ref()
                .map_or(now.saturating_add(limits.timeout_ms as i64), |c| {
                    c.deadline_ms
                });
            let claimed = persistence
                .store
                .claim_mcp_remote_task(
                    self.session.owner(),
                    self.session
                        .durable_identity()
                        .ok_or(McpTaskError::InvalidInput)?,
                    self.session
                        .server()
                        .digest(self.session.representation_limits())
                        .map_err(|_| McpTaskError::InvalidInput)?,
                    self.logical_key,
                    self.descriptor.clone(),
                    self.params.clone(),
                    context.clone(),
                    now,
                    deadline,
                    self.session.client_limits().requests_per_session,
                )
                .await
                .map_err(|e| McpTaskError::Persistence(e.code()))?;
            if !claimed {
                return Err(McpTaskError::Persistence(
                    crate::failure::FailureCode::EffectUncertain,
                ));
            }
        }
        let mut request = request.unwrap_or_else(|| McpRequest {
            response_limits: self.session.representation_limits().clone(),
            cancellation,
            cancellation_grace_ms: self
                .session
                .client_limits()
                .cancellation_grace_ms
                .min(limits.timeout_ms),
            method: "tools/call".into(),
            params: self.params.value().clone(),
            timeout_ms: limits
                .timeout_ms
                .min(self.session.client_limits().timeout_ms),
            max_response_bytes: limits
                .response_bytes
                .min(self.session.client_limits().frame_bytes),
            idempotency_key: None,
        });
        request.params = self.params.value().clone();
        let task_key = if matches!(self.mode, McpToolTaskMode::Task { .. })
            && self.session.persistence().is_some()
        {
            Some(self.logical_key)
        } else {
            None
        };
        let reply = self
            .session
            .invoke_task_context(request, context, task_key)
            .await;
        let mut value = success(&reply)
            .map(|document| match self.mode {
                McpToolTaskMode::Inline => {
                    crate::validate_mcp_tool_result(document, self.session.representation_limits())
                        .map_err(|_| McpTaskError::InvalidResponse)?;
                    Ok(McpToolTaskAcceptance::Inline)
                }
                McpToolTaskMode::Task { .. } => {
                    validate_mcp_message(
                        McpMessageKind::CreateTaskResult,
                        document,
                        self.session.representation_limits(),
                    )
                    .map_err(|_| McpTaskError::InvalidResponse)?;
                    let fields = map(document.value())?;
                    if fields.get("content").is_some() || fields.get("isError").is_some() {
                        return Err(McpTaskError::InvalidResponse);
                    }
                    let state = state(
                        fields.get("task").ok_or(McpTaskError::InvalidResponse)?,
                        self.session.representation_limits(),
                    )?;
                    if state.status != McpTaskStatus::Working {
                        return Err(McpTaskError::InvalidTransition);
                    }
                    // Acceptance metadata may be omitted by peers; if supplied it
                    // must never bind the acceptance to a different task.
                    if let Some(id) = mcp_related_task_id(document.value())?
                        && id != state.id
                    {
                        return Err(McpTaskError::InvalidResponse);
                    }
                    Ok(McpToolTaskAcceptance::Accepted(Box::new(
                        McpRemoteTask::new(self.session.clone(), state)?,
                    )))
                }
            })
            .transpose();
        if let Ok(Some(McpToolTaskAcceptance::Accepted(task))) = &mut value {
            task.response_limits = response_limits;
            if let Some(persistence) = self.session.persistence() {
                if let Some(registered) = &reply.task_registration {
                    match registered {
                        Ok(()) => task.intent_key = Some(self.logical_key),
                        Err(error) => value = Err(McpTaskError::Persistence(error.code())),
                    }
                    return Ok(McpTaskReply { reply, value });
                }
                let outcome = async {
                    persistence
                        .store
                        .accept_mcp_remote_task(
                            self.session.owner(),
                            self.session
                                .durable_identity()
                                .ok_or(McpTaskError::InvalidInput)?,
                            self.session
                                .server()
                                .digest(self.session.representation_limits())
                                .map_err(|_| McpTaskError::InvalidInput)?,
                            self.logical_key,
                            task.state.clone(),
                            reply.raw_result.clone().unwrap_or_else(|| {
                                success(&reply)
                                    .expect("admitted acceptance")
                                    .as_bytes()
                                    .to_vec()
                            }),
                            persistence
                                .clock
                                .now_ms()
                                .map_err(|e| McpTaskError::Persistence(e.code()))?,
                        )
                        .await
                        .map_err(|e| McpTaskError::Persistence(e.code()))
                }
                .await;
                match outcome {
                    Ok(()) => task.intent_key = Some(self.logical_key),
                    Err(error) => value = Err(error),
                }
            }
        }
        Ok(McpTaskReply { reply, value })
    }
}

/// One admitted task inventory page. Debug exposes counts rather than cursors.
pub struct McpTaskPage {
    /// Validated task observations, not tool results.
    pub tasks: Vec<McpTaskState>,
    /// Exact opaque continuation, absent only when the peer reports completion.
    pub next_cursor: Option<String>,
}

/// Bounded inventory and explicit completion disposition. The last wire reply is
/// retained even on malformed pages, remote errors, or interrupted enumeration.
#[derive(Debug)]
pub struct McpTaskInventory {
    /// Independently admitted observations from complete accepted pages.
    pub tasks: Vec<McpTaskState>,
    /// Whether the peer returned a final page within all bounds.
    pub complete: bool,
    /// Local enumeration stop, if any. A wire error is in last_reply instead.
    pub stop: Option<McpTaskError>,
    /// Last unmodified page RPC and its typed admission.
    pub last_reply: Option<McpTaskReply<McpTaskPage>>,
}
impl fmt::Debug for McpTaskPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpTaskPage")
            .field("tasks", &self.tasks.len())
            .field("has_next", &self.next_cursor.is_some())
            .finish()
    }
}

/// Result of bounded polling, preserving the final status or operation RPC.
#[derive(Debug)]
pub enum McpTaskWait {
    /// Final underlying result/error, separate from task acceptance. This also
    /// opens the result stream when input_required is observed.
    Result(McpTaskReply<()>),
    /// Polling stopped on a local/remote/semantic status-RPC failure.
    Observation(McpTaskReply<McpTaskState>),
    /// Local cancellation sent an actual tasks/cancel RPC on an independent token.
    /// Inspect its reply to distinguish confirmed cancellation from uncertainty.
    Cancellation {
        /// Actual tasks/cancel request outcome, including cancellation races.
        reply: Result<McpTaskReply<McpTaskState>, McpTaskError>,
        /// In-flight status/result reply, if cancellation interrupted one. It is
        /// evidence in its own right and is never replaced by cancellation.
        interrupted: Option<Box<McpClientReply>>,
    },
}

impl McpRemoteTask {
    async fn persist(
        &self,
        state: Option<McpTaskState>,
        reply: Option<&McpClientReply>,
    ) -> Result<(), McpTaskError> {
        let (Some(key), Some(persistence)) = (self.intent_key, self.session.persistence()) else {
            return Ok(());
        };
        let final_payload = reply.and_then(|reply| match &reply.result {
            Ok(Ok(document)) => Some((
                false,
                reply
                    .raw_result
                    .clone()
                    .unwrap_or_else(|| document.as_bytes().to_vec()),
            )),
            Ok(Err(error)) => Some((true, error.as_bytes().to_vec())),
            Err(_) => None,
        });
        persistence
            .store
            .update_mcp_remote_task(
                self.session.owner(),
                self.session
                    .durable_identity()
                    .ok_or(McpTaskError::InvalidInput)?,
                self.session
                    .server()
                    .digest(self.session.representation_limits())
                    .map_err(|_| McpTaskError::InvalidInput)?,
                key,
                state,
                final_payload,
                persistence
                    .clock
                    .now_ms()
                    .map_err(|e| McpTaskError::Persistence(e.code()))?,
            )
            .await
            .map_err(|e| McpTaskError::Persistence(e.code()))
    }
    fn new(session: McpClientSession, state: McpTaskState) -> Result<Self, McpTaskError> {
        let observed = tokio::time::Instant::now();
        let expires = retention_deadline(&state, observed)?;
        let response_limits = session.representation_limits().clone();
        Ok(Self {
            session,
            state,
            observed,
            expires,
            result_identity: None,
            result_status: None,
            intent_key: None,
            response_limits,
        })
    }
    /// Current immutable observation for protected persistence or presentation.
    pub fn state(&self) -> &McpTaskState {
        &self.state
    }
    /// Exact session authority, including owner, compound server and epoch.
    pub fn session(&self) -> &McpClientSession {
        &self.session
    }
    fn check_retention(&self) -> Result<(), McpTaskError> {
        if self
            .expires
            .is_some_and(|at| at <= tokio::time::Instant::now())
        {
            return Err(McpTaskError::Expired);
        }
        Ok(())
    }
    fn operation_limits(&self, limits: McpTaskLimits) -> Result<McpTaskLimits, McpTaskError> {
        self.check_retention()?;
        match self.expires {
            None => Ok(limits),
            Some(end) => remaining(limits, end).map_err(|_| McpTaskError::Expired),
        }
    }
    fn params(&self) -> Result<Value, McpTaskError> {
        // taskId is the source of truth; never add related-task to these RPCs.
        object(vec![("taskId".into(), Value::Text(self.state.id.clone()))])
    }
    fn observe(
        &mut self,
        document: &JsonDocument,
        cancel: bool,
    ) -> Result<McpTaskState, McpTaskError> {
        let next =
            McpTaskState::from_document(document.clone(), self.session.representation_limits())?;
        self.state.validate_successor(&next)?;
        if self
            .result_status
            .is_some_and(|status| status != next.status)
        {
            return Err(McpTaskError::InvalidTransition);
        }
        if cancel && next.status != McpTaskStatus::Cancelled {
            return Err(McpTaskError::InvalidTransition);
        }
        self.observed = tokio::time::Instant::now();
        self.state = next.clone();
        Ok(next)
    }
    /// Makes one tasks/get request and independently checks lifecycle invariants.
    /// Manual get does not sleep; use poll/wait to respect pollInterval.
    pub async fn get(
        &mut self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskReply<McpTaskState>, McpTaskError> {
        let limits = self.operation_limits(limits)?;
        let reply = rpc(
            &self.session,
            "tasks/get",
            self.params()?,
            limits,
            cancellation,
            &self.response_limits,
        )
        .await?;
        let mut value = success(&reply)
            .map(|doc| self.observe(doc, false))
            .transpose();
        if let Ok(Some(state)) = &value
            && let Err(error) = self.persist(Some(state.clone()), None).await
        {
            value = Err(error)
        }
        Ok(McpTaskReply { reply, value })
    }
    /// Waits for the larger local/server poll interval, bounded by TTL and the
    /// absolute campaign timeout, then performs one status request. No optional
    /// notification can reset these bounds or supply completion authority.
    pub async fn poll(
        &mut self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskReply<McpTaskState>, McpTaskError> {
        self.check_retention()?;
        let end = deadline(&self.session, limits);
        let delay = limits
            .poll_interval_ms
            .max(self.state.poll_interval_ms.unwrap_or(0));
        let next = self
            .observed
            .checked_add(Duration::from_millis(delay))
            .ok_or(McpTaskError::Limit)?;
        let stop = self.expires.map_or(end, |expiry| expiry.min(end));
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(McpTaskError::Cancelled),
            _ = tokio::time::sleep_until(next.min(stop)) => {}
        }
        self.check_retention()?;
        self.get(remaining(limits, end)?, cancellation).await
    }
    /// Retrieves the underlying tool result, including while input_required so
    /// related callbacks can make progress. This RPC is independently bounded;
    /// cancelling its wait is not remote task cancellation.
    /// An error is an underlying execution result only with a previously observed
    /// failed/cancelled status and matching task metadata. Otherwise `Retrieval`
    /// preserves uncertainty; the host may explicitly inspect/poll the known task.
    pub async fn result(
        &mut self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskReply<()>, McpTaskError> {
        let limits = self.operation_limits(limits)?;
        let reply = rpc(
            &self.session,
            "tasks/result",
            self.params()?,
            limits,
            cancellation,
            &self.response_limits,
        )
        .await?;
        let mut value = success(&reply)
            .map(|document| {
                crate::validate_mcp_tool_result(document, self.session.representation_limits())
                    .map_err(|_| McpTaskError::InvalidResponse)?;
                if mcp_related_task_id(document.value())? != Some(self.state.id.as_str()) {
                    return Err(McpTaskError::InvalidResponse);
                }
                let is_error = matches!(
                    map(document.value())?.get("isError"),
                    Some(Value::Bool(true))
                );
                if (self.state.status == McpTaskStatus::Completed && is_error)
                    || (self.state.status == McpTaskStatus::Failed && !is_error)
                {
                    return Err(McpTaskError::InvalidTransition);
                }
                if self
                    .result_identity
                    .is_some_and(|identity| identity != document.digest())
                {
                    return Err(McpTaskError::InvalidTransition);
                }
                self.result_identity = Some(document.digest());
                self.result_status = Some(if self.state.status == McpTaskStatus::Cancelled {
                    McpTaskStatus::Cancelled
                } else if is_error {
                    McpTaskStatus::Failed
                } else {
                    McpTaskStatus::Completed
                });
                Ok(())
            })
            .transpose();
        if let Ok(Err(error)) = &reply.result {
            value = if matches!(
                self.state.status,
                McpTaskStatus::Failed | McpTaskStatus::Cancelled
            ) && mcp_related_task_id(error.document().value()).ok().flatten()
                == Some(self.state.id.as_str())
            {
                if self
                    .result_identity
                    .is_some_and(|identity| identity != error.document().digest())
                {
                    Err(McpTaskError::InvalidTransition)
                } else {
                    self.result_identity = Some(error.document().digest());
                    self.result_status = Some(self.state.status);
                    Ok(Some(()))
                }
            } else {
                Err(McpTaskError::Retrieval)
            };
        }
        if value == Err(McpTaskError::Retrieval)
            && let (Some(key), Some(persistence), Ok(Err(error))) =
                (self.intent_key, self.session.persistence(), &reply.result)
        {
            let retained = async {
                persistence
                    .store
                    .record_mcp_remote_task_retrieval_error(
                        self.session.owner(),
                        self.session
                            .durable_identity()
                            .ok_or(McpTaskError::InvalidInput)?,
                        self.session
                            .server()
                            .digest(self.session.representation_limits())
                            .map_err(|_| McpTaskError::InvalidInput)?,
                        key,
                        error.as_bytes().to_vec(),
                        persistence
                            .clock
                            .now_ms()
                            .map_err(|e| McpTaskError::Persistence(e.code()))?,
                    )
                    .await
                    .map_err(|e| McpTaskError::Persistence(e.code()))
            }
            .await;
            if let Err(error) = retained {
                value = Err(error);
            }
        }
        if matches!(value, Ok(Some(())))
            && let Err(error) = self.persist(None, Some(&reply)).await
        {
            value = Err(error)
        }
        Ok(McpTaskReply { reply, value })
    }
    /// Explicit remote cancellation, never just notifications/cancelled for the
    /// originating RPC. Terminal cancellation attempts are sent to the receiver,
    /// which must reject them with -32602; original errors remain inspectable.
    pub async fn cancel(
        &mut self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskReply<McpTaskState>, McpTaskError> {
        if !self.session.task_capabilities()?.cancel {
            return Err(McpTaskError::Unsupported);
        }
        let limits = self.operation_limits(limits)?;
        let reply = rpc(
            &self.session,
            "tasks/cancel",
            self.params()?,
            limits,
            cancellation,
            &self.response_limits,
        )
        .await?;
        let mut value = success(&reply)
            .map(|doc| self.observe(doc, true))
            .transpose();
        if let Ok(Some(state)) = &value
            && let Err(error) = self.persist(Some(state.clone()), None).await
        {
            value = Err(error)
        }
        Ok(McpTaskReply { reply, value })
    }
    /// Polls without status notifications. Stops at a final underlying reply, a
    /// status failure, or a finite request/byte/time bound. Local cancellation
    /// explicitly sends tasks/cancel using a fresh token, so an already-cancelled
    /// caller signal cannot suppress remote cancellation. No create RPC is retried.
    pub async fn wait(
        &mut self,
        limits: McpTaskLimits,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpTaskWait, McpTaskError> {
        let end = deadline(&self.session, limits);
        let mut bytes = 0usize;
        let requests = limits
            .requests
            .min(self.session.client_limits().requests_per_session);
        for _ in 0..requests {
            if cancellation.is_cancelled() {
                return self.cancel_after_wait(limits, None).await;
            }
            let mut step = remaining(limits, end)?;
            step.total_bytes = limits
                .total_bytes
                .checked_sub(bytes)
                .filter(|n| *n > 0)
                .ok_or(McpTaskError::Limit)?;
            if self.state.status.is_terminal() || self.state.status == McpTaskStatus::InputRequired
            {
                let result = self.result(step, cancellation.clone()).await?;
                if cancellation.is_cancelled() {
                    return self.cancel_after_wait(limits, Some(result.reply)).await;
                }
                return Ok(McpTaskWait::Result(result));
            }
            let mut observed = match self.poll(step, cancellation.clone()).await {
                Err(McpTaskError::Cancelled) => {
                    return self.cancel_after_wait(limits, None).await;
                }
                result => result?,
            };
            if cancellation.is_cancelled() {
                return self.cancel_after_wait(limits, Some(observed.reply)).await;
            }
            bytes = bytes.saturating_add(reply_bytes(&observed.reply));
            if bytes > limits.total_bytes {
                observed.value = Err(McpTaskError::Limit);
                return Ok(McpTaskWait::Observation(observed));
            }
            if !matches!(observed.value, Ok(Some(_))) {
                return Ok(McpTaskWait::Observation(observed));
            }
            // Release the wire response reservation before the next poll. The
            // durable journal integration must save each observation before here.
        }
        Err(McpTaskError::Limit)
    }
    async fn cancel_after_wait(
        &mut self,
        limits: McpTaskLimits,
        interrupted: Option<McpClientReply>,
    ) -> Result<McpTaskWait, McpTaskError> {
        let reply = self.cancel(limits, Arc::new(McpCancellation::new())).await;
        Ok(McpTaskWait::Cancellation {
            reply,
            interrupted: interrupted.map(Box::new),
        })
    }
}

/// Parses task association without granting authority to an arbitrary server ID.
/// Callback admission must resolve the result against its retained, fenced task
/// registry and preserve owner/server/epoch/origin; unknown IDs confer no origin.
///
/// # Errors
/// Returns malformed _meta/related-task objects or non-string task IDs.
pub fn mcp_related_task_id(value: &Value) -> Result<Option<&str>, McpTaskError> {
    let fields = map(value)?;
    let Some(meta) = fields.get("_meta") else {
        return Ok(None);
    };
    let Some(related) = map(meta)?.get(MCP_RELATED_TASK) else {
        return Ok(None);
    };
    Ok(Some(text(map(related)?, "taskId")?))
}

fn map(value: &Value) -> Result<&Map, McpTaskError> {
    match value {
        Value::Map(map) => Ok(map),
        _ => Err(McpTaskError::InvalidResponse),
    }
}
fn text<'a>(map: &'a Map, key: &str) -> Result<&'a str, McpTaskError> {
    match map.get(key) {
        Some(Value::Text(s)) => Ok(s),
        _ => Err(McpTaskError::InvalidResponse),
    }
}
fn object(entries: Vec<(String, Value)>) -> Result<Value, McpTaskError> {
    Map::try_from_entries(entries)
        .map(Value::Map)
        .map_err(|_| McpTaskError::InvalidInput)
}
fn state(value: &Value, limits: &Limits) -> Result<McpTaskState, McpTaskError> {
    McpTaskState::from_document(
        JsonDocument::from_value(value, limits).map_err(|_| McpTaskError::InvalidResponse)?,
        limits,
    )
}
fn success(reply: &McpClientReply) -> Option<&JsonDocument> {
    match &reply.result {
        Ok(Ok(document)) => Some(document),
        _ => None,
    }
}
fn reply_bytes(reply: &McpClientReply) -> usize {
    match &reply.result {
        Ok(Ok(doc)) => reply
            .raw_result
            .as_ref()
            .map_or(doc.as_bytes().len(), Vec::len),
        Ok(Err(error)) => error.as_bytes().len(),
        Err(_) => 0,
    }
}
fn deadline(session: &McpClientSession, limits: McpTaskLimits) -> tokio::time::Instant {
    tokio::time::Instant::now()
        + Duration::from_millis(limits.timeout_ms.min(session.client_limits().timeout_ms))
}
fn remaining(
    mut limits: McpTaskLimits,
    end: tokio::time::Instant,
) -> Result<McpTaskLimits, McpTaskError> {
    let left = end
        .saturating_duration_since(tokio::time::Instant::now())
        .as_millis();
    if left == 0 {
        return Err(McpTaskError::Limit);
    }
    limits.timeout_ms = limits.timeout_ms.min(left as u64);
    Ok(limits)
}
async fn rpc(
    session: &McpClientSession,
    method: &str,
    params: Value,
    limits: McpTaskLimits,
    cancellation: Arc<McpCancellation>,
    response_limits: &Limits,
) -> Result<McpClientReply, McpTaskError> {
    let params = JsonDocument::from_value(&params, session.representation_limits())
        .map_err(|_| McpTaskError::InvalidInput)?;
    let configured = session.client_limits();
    Ok(session
        .invoke(McpRequest {
            response_limits: response_limits.clone(),
            cancellation,
            cancellation_grace_ms: configured.cancellation_grace_ms.min(limits.timeout_ms),
            method: method.into(),
            params: params.value().clone(),
            timeout_ms: limits.timeout_ms.min(configured.timeout_ms),
            max_response_bytes: limits
                .response_bytes
                .min(limits.total_bytes)
                .min(configured.frame_bytes),
            idempotency_key: None,
        })
        .await)
}

fn retention_deadline(
    state: &McpTaskState,
    now: tokio::time::Instant,
) -> Result<Option<tokio::time::Instant>, McpTaskError> {
    let Some(ttl) = state.ttl_ms else {
        return Ok(None);
    };
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| McpTaskError::InvalidResponse)?
        .as_nanos() as i128;
    let remaining = (state.created.nanos() + i128::from(ttl) * 1_000_000 - wall).max(0);
    // A server clock in the future cannot extend the granted duration locally.
    let nanos = remaining.min(i128::from(ttl) * 1_000_000);
    let duration = Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    );
    Ok(Some(
        now.checked_add(duration)
            .ok_or(McpTaskError::InvalidResponse)?,
    ))
}

// Normalized full-precision fractional spelling preserves timestamp ordering even
// beyond clock nanosecond precision. Trailing zeros have no semantic significance.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Timestamp {
    seconds: i64,
    fraction: String,
}
impl Timestamp {
    fn nanos(&self) -> i128 {
        let mut nanos = 0i128;
        for at in 0..9 {
            nanos =
                nanos * 10 + i128::from(self.fraction.as_bytes().get(at).map_or(0, |b| b - b'0'));
        }
        i128::from(self.seconds) * 1_000_000_000 + nanos
    }
}
// RFC3339 with calendar validation and timezone offsets.
// No network date parser, locale dependence or unchecked server-time arithmetic.
fn timestamp(input: &str) -> Result<Timestamp, McpTaskError> {
    let bad = || McpTaskError::InvalidResponse;
    let b = input.as_bytes();
    if !input.is_ascii()
        || b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(bad());
    }
    let number = |start: usize, end: usize| -> Result<i64, McpTaskError> {
        let digits = b.get(start..end).ok_or_else(bad)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(bad());
        }
        input[start..end].parse().map_err(|_| bad())
    };
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return Err(bad()),
    };
    if !(1..=days).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return Err(bad());
    }
    let mut at = 19;
    let mut fraction = String::new();
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return Err(bad());
        }
        fraction = input[start..at].trim_end_matches('0').to_owned();
    }
    let offset = match b.get(at) {
        Some(b'Z' | b'z') if at + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if at + 6 == b.len() && b[at + 3] == b':' => {
            let h = number(at + 1, at + 3)?;
            let m = number(at + 4, at + 6)?;
            if h > 23 || m > 59 {
                return Err(bad());
            }
            (h * 60 + m) * 60 * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return Err(bad()),
    };
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(Timestamp {
        seconds: days * 86400 + hour * 3600 + minute * 60 + second - offset,
        fraction,
    })
}
