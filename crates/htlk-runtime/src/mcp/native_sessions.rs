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
    task_policy: client::McpGraphTaskPolicy,
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
        let task_policy = config.graph_task_policy();
        Ok(Self {
            client: NativeMcpClient::new(config)?,
            endpoints,
            timeout_ms,
            task_policy,
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
                task_mode: self.select_task_mode(&session, &descriptor, binding)?,
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
    fn select_task_mode(
        &self,
        session: &McpClientSession,
        descriptor: &JsonDocument,
        binding: &McpBinding,
    ) -> Result<client::McpToolTaskMode, RuntimeFailure> {
        if !matches!(binding.kind(), McpBindingKind::Tool { .. }) {
            return Ok(client::McpToolTaskMode::Inline);
        }
        let support = as_map(descriptor.value())?
            .get("execution")
            .and_then(|e| as_map(e).ok()?.get("taskSupport"));
        let required = support == Some(&Value::Text("required".into()));
        let optional = support == Some(&Value::Text("optional".into()));
        let mode = match self.task_policy {
            client::McpGraphTaskPolicy::Required { ttl_ms } if required => {
                client::McpToolTaskMode::Task {
                    ttl_ms: Some(ttl_ms),
                }
            }
            client::McpGraphTaskPolicy::Prefer { ttl_ms } if required || optional => {
                client::McpToolTaskMode::Task {
                    ttl_ms: Some(ttl_ms),
                }
            }
            _ if required => return Err(fail(FailureCode::McpDescriptor)),
            _ => client::McpToolTaskMode::Inline,
        };
        if matches!(mode, client::McpToolTaskMode::Task { .. })
            && !session
                .task_capabilities()
                .map_err(|_| fail(FailureCode::McpDescriptor))?
                .tool_call
        {
            return if required {
                Err(fail(FailureCode::McpDescriptor))
            } else {
                Ok(client::McpToolTaskMode::Inline)
            };
        }
        Ok(mode)
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
                    self.select_task_mode(session, &descriptor, binding)?;
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
    task_mode: client::McpToolTaskMode,
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
            let mut reply = if matches!(self.task_mode, client::McpToolTaskMode::Task { .. }) {
                match self.invoke_task(request).await {
                    Ok(reply) => reply,
                    Err((delivery, error)) => {
                        if delivery != McpDelivery::ResponseReceived {
                            self.closed = true;
                            let _ = self.session.close().await;
                        }
                        return McpOutcome {
                            delivery,
                            result: Err(error),
                        };
                    }
                }
            } else {
                self.session
                    .invoke_context(request, self.context.take())
                    .await
            };
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
impl NativeLease {
    async fn invoke_task(
        &mut self,
        request: McpRequest,
    ) -> Result<client::McpClientReply, (McpDelivery, RuntimeFailure)> {
        let context = self
            .context
            .take()
            .ok_or_else(|| (McpDelivery::NotSent, fail(FailureCode::Profile)))?;
        let key = htlk_executable::digest::hash_bytes(
            format!(
                "htlk.graph-remote-task/1:{}:{}",
                context.origin.run_id, context.origin.invocation_id
            )
            .as_bytes(),
        );
        let limits = client::McpTaskLimits::new(
            request.max_response_bytes,
            request.timeout_ms,
            1024,
            1024,
            self.session.client_limits().retained_bytes,
            10,
        )
        .map_err(|_| (McpDelivery::NotSent, fail(FailureCode::Profile)))?;
        let cancel = request.cancellation.clone();
        let max_response_bytes = request.max_response_bytes;
        let until = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
        let params = JsonDocument::from_value(&request.params, &request.response_limits)
            .map_err(|_| (McpDelivery::NotSent, fail(FailureCode::Input)))?;
        let prepared = self
            .session
            .prepare_tool_task_call(&self.descriptor, params, self.task_mode)
            .map_err(|_| (McpDelivery::NotSent, fail(FailureCode::McpDescriptor)))?
            .with_logical_key(key);
        let accepted = prepared
            .dispatch_operation(request, Some(context))
            .await
            .map_err(task_failure)?;
        let mut task = match accepted.value {
            Ok(Some(client::McpToolTaskAcceptance::Accepted(task))) => task,
            Ok(None) => return Ok(accepted.reply),
            Err(error) => return Err((McpDelivery::PossiblySent, task_failure(error).1)),
            _ => return Err((McpDelivery::PossiblySent, fail(FailureCode::McpProtocol))),
        };
        drop(accepted.reply);
        let left = until
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis() as u64;
        if left == 0 {
            let _ = task.cancel(limits, Arc::new(McpCancellation::new())).await;
            return Err((McpDelivery::PossiblySent, fail(FailureCode::McpTimeout)));
        }
        let limits = client::McpTaskLimits::new(
            max_response_bytes,
            left,
            1024,
            1024,
            self.session.client_limits().retained_bytes,
            10,
        )
        .map_err(|_| (McpDelivery::PossiblySent, fail(FailureCode::McpTimeout)))?;
        match task
            .wait(limits, cancel)
            .await
            .map_err(|e| (McpDelivery::PossiblySent, task_failure(e).1))?
        {
            client::McpTaskWait::Result(result) => match result.value {
                Ok(Some(())) => {
                    let mut reply = result.reply;
                    if reply.delivery != McpDelivery::ResponseReceived {
                        reply.delivery = McpDelivery::PossiblySent;
                    }
                    Ok(reply)
                }
                Ok(None) => {
                    // A failed result-retrieval transport is not the original
                    // operation's completion, even if this RPC was never sent.
                    let mut reply = result.reply;
                    reply.delivery = McpDelivery::PossiblySent;
                    Ok(reply)
                }
                Err(error) => Err((McpDelivery::PossiblySent, task_failure(error).1)),
            },
            client::McpTaskWait::Cancellation { .. } => {
                Err((McpDelivery::PossiblySent, fail(FailureCode::Cancelled)))
            }
            client::McpTaskWait::Observation(_) => {
                Err((McpDelivery::PossiblySent, fail(FailureCode::McpProtocol)))
            }
        }
    }
}
fn task_failure(error: client::McpTaskError) -> (McpDelivery, RuntimeFailure) {
    let code = match error {
        client::McpTaskError::Persistence(code) => code,
        client::McpTaskError::Cancelled => FailureCode::Cancelled,
        client::McpTaskError::Expired => FailureCode::RetentionExpired,
        client::McpTaskError::Limit => FailureCode::McpTimeout,
        _ => FailureCode::McpProtocol,
    };
    (
        if code == FailureCode::EffectUncertain {
            McpDelivery::PossiblySent
        } else {
            McpDelivery::NotSent
        },
        fail(code),
    )
}
