//! Durable client-direction protocol tasks. The existing interaction ledger owns
//! execution, accounting, work uncertainty and protected original replies. This
//! service owns deferred protocol acceptance and independently terminal task state.
use super::{
    McpClientAction, McpClientConfig, McpClientPersistence, McpClientService, McpInteractionHost,
    McpOwner, McpRequestId, McpRpcError, validate_client_request,
};
use crate::{
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, as_map, fail, obj},
    store::{McpReceivedTask, McpReceivedTaskLimits, StoreError},
};
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use std::{sync::Arc, time::Duration};

/// Request types for which the installed receiver accepts task augmentation.
/// These flags never install the ordinary sampling/elicitation handlers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpTaskReceiverMethods {
    /// sampling/createMessage, including its separately negotiated extensions.
    pub sampling: bool,
    /// elicitation/create, for the actually installed form/URL services.
    pub elicitation: bool,
}

/// Finite receiver task-control bounds, separate from execution work quotas.
#[derive(Clone, Copy, Debug)]
pub struct McpTaskReceiverWait {
    timeout_ms: u64,
    poll_ms: u64,
    page_size: usize,
    response_bytes: usize,
}
impl McpTaskReceiverWait {
    /// Checks positive bounded waits, page sizes and response capacity. Effective
    /// waits/pages are also capped by the client and store configuration.
    ///
    /// # Errors
    /// Returns invalid configuration before any task admission.
    pub fn new(
        timeout_ms: u64,
        poll_ms: u64,
        page_size: usize,
        response_bytes: usize,
    ) -> Result<Self, RuntimeFailure> {
        if timeout_ms == 0
            || timeout_ms > i64::MAX as u64
            || poll_ms == 0
            || poll_ms > timeout_ms
            || page_size == 0
            || page_size > 4096
            || response_bytes < 256
            || response_bytes > isize::MAX as usize
            || tokio::time::Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .is_none()
        {
            return Err(fail(FailureCode::Profile));
        }
        Ok(Self {
            timeout_ms,
            poll_ms,
            page_size,
            response_bytes,
        })
    }
}

/// Declarative immutable receiver installation. Parent configuration should hold
/// these settings, derive its tasks capability, and construct the service from the
/// same installed configuration; do not retain a self-referential config/service.
#[derive(Clone, Copy, Debug)]
pub struct McpTaskReceiverSettings {
    methods: McpTaskReceiverMethods,
    limits: McpReceivedTaskLimits,
    wait: McpTaskReceiverWait,
}
impl McpTaskReceiverSettings {
    /// Selects finite receiver services. At least one task request type is needed.
    /// Handler/persistence consistency is checked by McpTaskReceiver::new.
    ///
    /// # Errors
    /// Returns an empty service selection.
    pub fn new(
        methods: McpTaskReceiverMethods,
        limits: McpReceivedTaskLimits,
        wait: McpTaskReceiverWait,
    ) -> Result<Self, RuntimeFailure> {
        if !methods.sampling && !methods.elicitation {
            return Err(fail(FailureCode::Profile));
        }
        Ok(Self {
            methods,
            limits,
            wait,
        })
    }
    /// Frozen request-type selection, for capability/configuration validation.
    pub fn methods(&self) -> McpTaskReceiverMethods {
        self.methods
    }
    /// Exact tasks capability object. Advertise it only after new() validates the
    /// actual durable receiver plus its ordinary service handlers.
    ///
    /// # Errors
    /// Returns bounded representation failure.
    pub fn capabilities(&self, limits: &Limits) -> Result<JsonDocument, RuntimeFailure> {
        let mut requests = vec![];
        if self.methods.sampling {
            requests.push(("sampling".into(), obj([("createMessage", obj([])?)])?));
        }
        if self.methods.elicitation {
            requests.push(("elicitation".into(), obj([("create", obj([])?)])?));
        }
        JsonDocument::from_value(
            &obj([
                ("list", obj([])?),
                ("cancel", obj([])?),
                (
                    "requests",
                    Value::Map(
                        Map::try_from_entries(requests).map_err(|_| fail(FailureCode::Profile))?,
                    ),
                ),
            ])?,
            limits,
        )
        .map_err(|_| fail(FailureCode::Profile))
    }
}

/// Durable acceptance; sending its CreateTaskResult does not record or deliver the
/// underlying operation result. Redelivery returns the same receiver-generated ID.
#[derive(Debug)]
pub struct McpReceivedTaskAcceptance {
    /// Current durable task identity/state, for handler context association.
    pub task: McpReceivedTask,
    /// Stable original CreateTaskResult, always beginning in working status.
    pub result: JsonDocument,
}

/// Concrete task service backed by the existing SQLite interaction ledger. It
/// never invokes a handler or starts provider work; admission must precede the
/// existing begin_mcp_work/worker path, and controls need independent I/O capacity.
#[derive(Clone)]
pub struct McpTaskReceiver {
    config: Arc<McpClientConfig>,
    persistence: McpClientPersistence,
    host: McpInteractionHost,
    settings: McpTaskReceiverSettings,
    representation: Limits,
}
impl McpTaskReceiver {
    /// Checks real persistence, authorization, handler installation and finite
    /// receiver bounds. Merely setting capability JSON cannot install this service.
    ///
    /// # Errors
    /// Returns missing/inconsistent durable services or representation bounds.
    pub fn new(
        config: McpClientConfig,
        settings: McpTaskReceiverSettings,
    ) -> Result<Self, RuntimeFailure> {
        let persistence = config
            .persistence
            .clone()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        if config.authorizer.is_none()
            || config.limits.retained_bytes < 64 * 1024
            || (settings.methods.sampling && config.handler(McpClientService::Sampling).is_none())
            || (settings.methods.elicitation
                && config.handler(McpClientService::ElicitationForm).is_none()
                && config.handler(McpClientService::ElicitationUrl).is_none())
            || settings.limits.active() > config.limits.callbacks
            || settings.limits.records() > config.limits.requests_per_session
        {
            return Err(fail(FailureCode::Profile));
        }
        let representation = persistence.store.config().limits.clone();
        if settings.limits.result_bytes() < representation.max_document_bytes / 8 + 256
            || settings.limits.result_bytes() > representation.max_document_bytes / 4
            || settings.wait.response_bytes < settings.limits.result_bytes().saturating_add(256)
            || settings.wait.response_bytes > config.limits.frame_bytes
            || representation.max_depth < 4
        {
            return Err(fail(FailureCode::Profile));
        }
        let host = McpInteractionHost::new(config.clone())?;
        Ok(Self {
            config: Arc::new(config),
            persistence,
            host,
            settings,
            representation,
        })
    }
    /// Whether this exact method has receiver task augmentation installed.
    pub fn supports(&self, method: &str) -> bool {
        match method {
            "sampling/createMessage" => self.settings.methods.sampling,
            "elicitation/create" => self.settings.methods.elicitation,
            _ => false,
        }
    }
    /// Associates an admitted interaction before work intent or CreateTaskResult
    /// delivery. Unsupported methods return None without inspecting task metadata.
    /// No augmentation also returns None and retains ordinary callback behavior.
    ///
    /// # Errors
    /// Returns invalid request/metadata, denied authority, or durable admission failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn accept(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        interaction_id: Digest,
        request: &JsonDocument,
    ) -> Result<Option<McpReceivedTaskAcceptance>, RuntimeFailure> {
        let fields = as_map(request.value())?;
        let Some(Value::Text(method)) = fields.get("method") else {
            return Err(fail(FailureCode::McpProtocol));
        };
        if !self.supports(method) {
            return Ok(None);
        }
        let params = fields.get("params").map(as_map).transpose()?;
        if params.is_none_or(|params| params.get("task").is_none()) {
            return Ok(None);
        }
        let underlying = mcp_without_task_augmentation(request, &self.representation)?;
        validate_client_request(&underlying, &self.config, &self.representation)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let record = self.host.record(owner, interaction_id).await?;
        if record.request_digest != request.digest() || record.session != session {
            return Err(fail(FailureCode::McpIdentity));
        }
        self.host.authorize(
            owner.host(),
            owner,
            server,
            &record,
            McpClientAction::Dispatch,
            None,
        )?;
        let task = self
            .persistence
            .store
            .create_mcp_received_task(
                owner,
                session,
                server,
                interaction_id,
                self.persistence.clock.now_ms()?,
                self.settings.limits,
            )
            .await
            .map_err(|e| fail(e.code()))?;
        let result = task
            .acceptance(&self.representation)
            .map_err(|e| fail(e.code()))?;
        Ok(Some(McpReceivedTaskAcceptance { task, result }))
    }

    /// Serves tasks/get/list/result/cancel as complete correlated JSON-RPC replies.
    /// Unknown methods return None. tasks/result polls durable state under a finite
    /// deadline and cancellation independent of the underlying task's lifetime.
    /// Cancellation is terminal in SQLite before a successful cancel reply exists.
    ///
    /// # Errors
    /// Returns malformed envelope, denied disclosure or storage/clock failure that
    /// prevents a safe protocol reply. Task parameter/state errors are JSON-RPC errors.
    pub async fn handle_request(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        request: &JsonDocument,
        cancellation: Arc<McpCancellation>,
    ) -> Result<Option<JsonDocument>, RuntimeFailure> {
        let fields = as_map(request.value())?;
        let Some(Value::Text(method)) = fields.get("method") else {
            return Err(fail(FailureCode::McpProtocol));
        };
        if !matches!(
            method.as_str(),
            "tasks/get" | "tasks/list" | "tasks/result" | "tasks/cancel"
        ) {
            return Ok(None);
        }
        let id = fields
            .get("id")
            .ok_or_else(|| fail(FailureCode::McpProtocol))?;
        McpRequestId::from_value(id)?;
        if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
            || fields.get("result").is_some()
            || fields.get("error").is_some()
        {
            return Err(fail(FailureCode::McpProtocol));
        }
        let params = match fields.get("params") {
            None if method == "tasks/list" => None,
            Some(Value::Map(params)) => Some(params),
            _ => {
                return self
                    .error_reply(request, -32602, "Invalid task parameters.")
                    .map(Some);
            }
        };
        if method == "tasks/list" {
            return self
                .list(owner, server, session, request, params)
                .await
                .map(Some);
        }
        let Some(Value::Text(task_id)) = params.and_then(|params| params.get("taskId")) else {
            return self
                .error_reply(request, -32602, "Invalid task identity.")
                .map(Some);
        };
        if method == "tasks/cancel" {
            match self
                .persistence
                .store
                .mcp_received_task_cancel_receipt(
                    owner,
                    session,
                    server,
                    request,
                    self.persistence.clock.now_ms()?,
                )
                .await
            {
                Ok(Some(task)) => {
                    let record = self.host.record(owner, task.interaction_id()).await?;
                    self.host.authorize(
                        owner.host(),
                        owner,
                        server,
                        &record,
                        McpClientAction::Cancel,
                        None,
                    )?;
                    return self
                        .result_reply(
                            request,
                            task.protocol_task(&self.representation)
                                .map_err(|e| fail(e.code()))?,
                        )
                        .map(Some);
                }
                Ok(None) => {}
                Err(error) => return self.store_error_reply(request, error).map(Some),
            }
        }
        let until = tokio::time::Instant::now()
            + Duration::from_millis(
                self.settings
                    .wait
                    .timeout_ms
                    .min(self.config.limits.timeout_ms),
            );
        loop {
            let task = match self
                .persistence
                .store
                .mcp_received_task(
                    owner,
                    session,
                    server,
                    task_id,
                    self.persistence.clock.now_ms()?,
                )
                .await
            {
                Ok(task) => task,
                Err(error) => return self.store_error_reply(request, error).map(Some),
            };
            let record = self.host.record(owner, task.interaction_id()).await?;
            self.host.authorize(
                owner.host(),
                owner,
                server,
                &record,
                if method == "tasks/cancel" {
                    McpClientAction::Cancel
                } else {
                    McpClientAction::Inspect
                },
                None,
            )?;
            if method == "tasks/cancel" {
                return match self
                    .persistence
                    .store
                    .cancel_mcp_received_task_request(
                        owner,
                        session,
                        server,
                        request,
                        self.persistence.clock.now_ms()?,
                    )
                    .await
                {
                    Ok(task) => self
                        .result_reply(
                            request,
                            task.protocol_task(&self.representation)
                                .map_err(|e| fail(e.code()))?,
                        )
                        .map(Some),
                    Err(error) => self.store_error_reply(request, error).map(Some),
                };
            }
            if method == "tasks/get" {
                return self
                    .result_reply(
                        request,
                        task.protocol_task(&self.representation)
                            .map_err(|e| fail(e.code()))?,
                    )
                    .map(Some);
            }
            if let Some(result) = task.result() {
                self.host.authorize(
                    owner.host(),
                    owner,
                    server,
                    &record,
                    McpClientAction::Deliver,
                    Some(result.payload()),
                )?;
                let reply = if result.is_error() {
                    self.reply(request, Err(McpRpcError::new(result.payload().clone())?))
                } else {
                    self.result_reply(request, result.payload().clone())
                };
                return reply.map(Some);
            }
            if cancellation.is_cancelled() {
                return self
                    .error_reply(request, -32800, "Task result wait cancelled.")
                    .map(Some);
            }
            if tokio::time::Instant::now() >= until {
                return self
                    .error_reply(request, -32000, "Task result wait deadline exceeded.")
                    .map(Some);
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return self.error_reply(request, -32800, "Task result wait cancelled.").map(Some),
                _ = tokio::time::sleep_until((tokio::time::Instant::now() + Duration::from_millis(self.settings.wait.poll_ms)).min(until)) => {}
            }
        }
    }
    async fn list(
        &self,
        owner: &McpOwner,
        server: Digest,
        session: Digest,
        request: &JsonDocument,
        params: Option<&Map>,
    ) -> Result<JsonDocument, RuntimeFailure> {
        let cursor = match params.and_then(|params| params.get("cursor")) {
            None => None,
            Some(Value::Text(cursor)) => Some(cursor.as_str()),
            _ => return self.error_reply(request, -32602, "Invalid task cursor."),
        };
        let page = match self
            .persistence
            .store
            .mcp_received_tasks(
                owner,
                session,
                server,
                cursor,
                self.settings
                    .wait
                    .page_size
                    .min(self.persistence.store.config().max_page_size),
                self.persistence.clock.now_ms()?,
            )
            .await
        {
            Ok(page) => page,
            Err(error) => return self.store_error_reply(request, error),
        };
        let mut tasks = vec![];
        for task in page.tasks {
            let record = self.host.record(owner, task.interaction_id()).await?;
            if self
                .host
                .authorize(
                    owner.host(),
                    owner,
                    server,
                    &record,
                    McpClientAction::Inspect,
                    None,
                )
                .is_ok()
            {
                tasks.push(
                    task.protocol_task(&self.representation)
                        .map_err(|e| fail(e.code()))?
                        .value()
                        .clone(),
                );
            }
        }
        let mut entries = vec![("tasks".into(), Value::Array(tasks))];
        if let Some(cursor) = page.next_cursor {
            entries.push(("nextCursor".into(), Value::Text(cursor)));
        }
        let page = JsonDocument::from_value(
            &Value::Map(Map::try_from_entries(entries).map_err(|_| fail(FailureCode::Profile))?),
            &self.representation,
        )
        .map_err(|_| fail(FailureCode::Profile))?;
        self.result_reply(request, page)
    }
    fn store_error_reply(
        &self,
        request: &JsonDocument,
        error: StoreError,
    ) -> Result<JsonDocument, RuntimeFailure> {
        match error {
            StoreError::Missing
            | StoreError::Expired
            | StoreError::Terminal
            | StoreError::Invalid => {
                self.error_reply(request, -32602, "Invalid, expired or terminal task.")
            }
            StoreError::Budget => {
                self.error_reply(request, -32000, "Task service capacity exhausted.")
            }
            _ => Err(fail(error.code())),
        }
    }
    fn error_reply(
        &self,
        request: &JsonDocument,
        code: i64,
        message: &str,
    ) -> Result<JsonDocument, RuntimeFailure> {
        self.reply(
            request,
            Err(super::interactions::rpc_error(
                code,
                message,
                &self.representation,
            )?),
        )
    }
    fn result_reply(
        &self,
        request: &JsonDocument,
        result: JsonDocument,
    ) -> Result<JsonDocument, RuntimeFailure> {
        self.reply(request, Ok(result))
    }
    fn reply(
        &self,
        request: &JsonDocument,
        result: Result<JsonDocument, McpRpcError>,
    ) -> Result<JsonDocument, RuntimeFailure> {
        let bytes = super::interactions::response(request, result.as_ref(), &self.representation)?;
        if bytes.len() > self.settings.wait.response_bytes {
            return Err(fail(FailureCode::Budget));
        }
        JsonDocument::new(&bytes, &self.representation).map_err(|_| fail(FailureCode::McpProtocol))
    }
}

/// Removes task augmentation from a semantic request view while retaining all
/// other params, metadata and exact JSON-RPC identity. Preserve the original raw
/// bytes/digest in the interaction ledger. Unsupported receiver augmentation must
/// be ignored even when its metadata is malformed; installed acceptance validates
/// it separately before work begins.
///
/// # Errors
/// Returns malformed base envelope/params or representation failure.
pub fn mcp_without_task_augmentation(
    request: &JsonDocument,
    limits: &Limits,
) -> Result<JsonDocument, RuntimeFailure> {
    let fields = as_map(request.value())?;
    let mut entries: Vec<_> = fields
        .iter()
        .filter(|(key, _)| *key != "params")
        .map(|(key, value)| (key.into(), value.clone()))
        .collect();
    if let Some(params) = fields.get("params") {
        let params = as_map(params)?;
        let params = Map::try_from_entries(
            params
                .iter()
                .filter(|(key, _)| *key != "task")
                .map(|(key, value)| (key.into(), value.clone())),
        )
        .map_err(|_| fail(FailureCode::McpProtocol))?;
        entries.push(("params".into(), Value::Map(params)));
    }
    JsonDocument::from_value(
        &Value::Map(Map::try_from_entries(entries).map_err(|_| fail(FailureCode::McpProtocol))?),
        limits,
    )
    .map_err(|_| fail(FailureCode::McpProtocol))
}

/// Adds task association to nested request/notification params, preserving the
/// original progress token and other metadata. An existing conflicting task ID is
/// rejected. This pure representation helper grants no dispatch authority: the
/// caller must resolve the task in its owner/server/generation scope first.
///
/// # Errors
/// Returns malformed metadata, conflicting association or representation limits.
pub fn mcp_with_received_task_metadata(
    params: &JsonDocument,
    task_id: &str,
    limits: &Limits,
) -> Result<JsonDocument, RuntimeFailure> {
    let fields = as_map(params.value())?;
    let meta = fields.get("_meta").map(as_map).transpose()?;
    const KEY: &str = "io.modelcontextprotocol/related-task";
    if let Some(existing) = meta.and_then(|meta| meta.get(KEY))
        && as_map(existing)?.get("taskId") != Some(&Value::Text(task_id.into()))
    {
        return Err(fail(FailureCode::McpProtocol));
    }
    let mut meta: Vec<_> = meta
        .into_iter()
        .flat_map(Map::iter)
        .filter(|(key, _)| *key != KEY)
        .map(|(key, value)| (key.into(), value.clone()))
        .collect();
    meta.push((KEY.into(), obj([("taskId", Value::Text(task_id.into()))])?));
    let mut entries: Vec<_> = fields
        .iter()
        .filter(|(key, _)| *key != "_meta")
        .map(|(key, value)| (key.into(), value.clone()))
        .collect();
    entries.push((
        "_meta".into(),
        Value::Map(Map::try_from_entries(meta).map_err(|_| fail(FailureCode::McpProtocol))?),
    ));
    JsonDocument::from_value(
        &Value::Map(Map::try_from_entries(entries).map_err(|_| fail(FailureCode::McpProtocol))?),
        limits,
    )
    .map_err(|_| fail(FailureCode::McpProtocol))
}
