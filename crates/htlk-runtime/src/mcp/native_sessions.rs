//! Graph operation handles over the common bidirectional client; no reader lease
//! is retained across external work. Runtime authorization and intent remain separate.
use super::*;
use client::{
    McpClientConfig, McpClientLimits, McpClientSession, McpOwner, McpOwnerScope, NativeMcpClient,
};

pub(super) struct NativeSessions {
    client: NativeMcpClient,
    endpoints: BTreeMap<String, McpEndpoint>,
    timeout_ms: u64,
}
impl NativeSessions {
    pub fn launch(
        endpoints: BTreeMap<String, McpEndpoint>,
        timeout_ms: u64,
    ) -> Result<Self, RuntimeFailure> {
        let config = McpClientConfig::new(
            McpClientLimits {
                timeout_ms,
                cancellation_grace_ms: 100.min(timeout_ms),
                messages_per_session: 1024 * 1024,
                ..Default::default()
            },
            BTreeMap::new(),
            None,
        )?
        .with_server_event_stream(false);
        Self::configured(endpoints, config)
    }
    pub fn configured(
        endpoints: BTreeMap<String, McpEndpoint>,
        config: McpClientConfig,
    ) -> Result<Self, RuntimeFailure> {
        let timeout_ms = config.limits().timeout_ms;
        Ok(Self {
            client: NativeMcpClient::new(config)?,
            endpoints,
            timeout_ms,
        })
    }
    pub async fn shutdown(&self) -> Result<(), RuntimeFailure> {
        self.client.shutdown().await
    }
    pub async fn connect(
        &self,
        binding: &McpBinding,
        context: &McpRunContext,
        limits: &Limits,
    ) -> Result<NativeLease, RuntimeFailure> {
        if context.cancellation.is_cancelled() || context.fence <= 0 {
            return Err(fail(FailureCode::Cancelled));
        }
        limits.validate().map_err(|_| fail(FailureCode::Profile))?;
        let endpoint = self
            .endpoints
            .get(binding.server().deployment_id())
            .ok_or_else(|| fail(FailureCode::McpIdentity))?;
        let owner = McpOwner::new(
            context.host.clone(),
            McpOwnerScope::Run(context.run_id.clone()),
            context.fence,
            limits,
        )?;
        let session = self
            .client
            .open(
                owner,
                binding.server().clone(),
                endpoint.clone(),
                Limits::default(),
                context.cancellation.clone(),
            )
            .await
            .map_err(|error| match error.code() {
                FailureCode::Cancelled if !context.cancellation.is_cancelled() => {
                    fail(FailureCode::McpTransport)
                }
                FailureCode::Fenced => fail(FailureCode::Cancelled),
                _ => error,
            })?;
        let prepared = self.prepare(&session, binding, context, limits).await;
        match prepared {
            Ok(descriptor) => Ok(NativeLease {
                session,
                descriptor,
                lifetime: context.cancellation.clone(),
                closed: false,
                protocol_error: None,
                context: None,
            }),
            Err(error) => {
                let _ = session.close().await;
                Err(error)
            }
        }
    }
    async fn prepare(
        &self,
        session: &McpClientSession,
        binding: &McpBinding,
        context: &McpRunContext,
        limits: &Limits,
    ) -> Result<JsonDocument, RuntimeFailure> {
        let capabilities = get(as_map(session.initialization().value())?, "capabilities")?;
        let category = match binding.kind() {
            McpBindingKind::Tool { .. } => "tools",
            McpBindingKind::Prompt { .. } => "prompts",
            _ => "resources",
        };
        if !matches!(as_map(capabilities)?.get(category), Some(Value::Map(_))) {
            return Err(fail(FailureCode::McpDescriptor));
        }
        let (method, collection, key, name) = match binding.kind() {
            McpBindingKind::Tool { name, .. } => ("tools/list", "tools", "name", name),
            McpBindingKind::Prompt { name } => ("prompts/list", "prompts", "name", name),
            McpBindingKind::Resource { uri } => ("resources/list", "resources", "uri", uri),
            McpBindingKind::Template { uri_template } => (
                "resources/templates/list",
                "resourceTemplates",
                "uriTemplate",
                uri_template,
            ),
        };
        let mut cursor = None;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1024 {
            let params = cursor.as_ref().map_or_else(
                || obj([]),
                |cursor: &String| obj([("cursor", Value::Text(cursor.clone()))]),
            )?;
            let reply = session
                .invoke(McpRequest {
                    response_limits: limits.clone(),
                    cancellation: context.cancellation.clone(),
                    cancellation_grace_ms: 100,
                    method: method.into(),
                    params,
                    timeout_ms: self.timeout_ms,
                    max_response_bytes: limits.max_document_bytes,
                    idempotency_key: None,
                })
                .await;
            let result = reply.result?.map_err(|_| fail(FailureCode::McpProtocol))?;
            let fields = as_map(result.value())?;
            let Value::Array(entries) = get(fields, collection)? else {
                return Err(fail(FailureCode::McpProtocol));
            };
            for descriptor in entries {
                if as_map(descriptor)?.get(key) == Some(&Value::Text(name.clone())) {
                    let descriptor = JsonDocument::from_value(descriptor, limits)
                        .map_err(|_| fail(FailureCode::McpDescriptor))?;
                    let task_calls = as_map(capabilities)?
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
                    return Ok(descriptor);
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
}

pub(super) struct NativeLease {
    session: McpClientSession,
    descriptor: JsonDocument,
    lifetime: Arc<McpCancellation>,
    closed: bool,
    protocol_error: Option<client::McpRpcError>,
    context: Option<McpAttemptContext>,
}
impl McpConnection for NativeLease {
    fn invoke_with_context<'a>(
        &'a mut self,
        request: McpRequest,
        context: McpAttemptContext,
    ) -> McpAttemptFuture<'a> {
        self.context = Some(context);
        self.invoke(request)
    }
    fn descriptor(&self) -> &JsonDocument {
        &self.descriptor
    }
    fn invoke<'a>(&'a mut self, mut request: McpRequest) -> McpAttemptFuture<'a> {
        Box::pin(async move {
            self.protocol_error = None;
            if self.lifetime.is_cancelled() {
                return McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(fail(FailureCode::Cancelled)),
                };
            }
            if self.closed || self.session.ended().is_some() {
                return McpOutcome {
                    delivery: McpDelivery::NotSent,
                    result: Err(fail(FailureCode::McpTransport)),
                };
            }
            let mut signal = McpCancellation::linked(&self.lifetime);
            signal.parents.push(request.cancellation.state.clone());
            signal
                .parents
                .extend(request.cancellation.parents.iter().cloned());
            request.cancellation = Arc::new(signal);
            let mut reply = self
                .session
                .invoke_context(request, self.context.take())
                .await;
            if let Ok(Err(error)) = &reply.result {
                self.protocol_error = Some(error.clone());
            }
            if reply.delivery != McpDelivery::ResponseReceived {
                self.closed = true;
                let _ = self.session.close().await;
            }
            let raw_result = reply.raw_result.take();
            McpOutcome {
                delivery: reply.delivery,
                result: reply.result.and_then(|result| {
                    result
                        .map(|result| raw_result.unwrap_or_else(|| result.as_bytes().to_vec()))
                        .map_err(|_| fail(FailureCode::McpProtocol))
                }),
            }
        })
    }
    fn close(&mut self) -> McpFuture<'_, ()> {
        self.closed = true;
        Box::pin(async { Ok(()) })
    }
    fn take_protocol_error(&mut self) -> Option<client::McpRpcError> {
        self.protocol_error.take()
    }
}
