//! Durable host callbacks execute away from the continuously receiving I/O owner.
use super::*;
use crate::mcp::client::interactions::{response, rpc_error};
use crate::store::{McpStoredInteraction, McpWorkClaim, McpWorkState};

impl Wire {
    pub(super) fn callback_supported(&self, method: &str) -> bool {
        let has = |service| self.config.handler(service).is_some();
        match method {
            "sampling/createMessage" => has(McpClientService::Sampling),
            "elicitation/create" => {
                has(McpClientService::ElicitationForm) || has(McpClientService::ElicitationUrl)
            }
            "roots/list" => has(McpClientService::Roots),
            _ => false,
        }
    }
    pub(super) fn start_callback(
        self: &Arc<Self>,
        request: JsonDocument,
        raw: Vec<u8>,
        request_id: McpRequestId,
        associated: Option<McpRequestId>,
    ) -> Result<(), RuntimeFailure> {
        let persistence = self
            .config
            .persistence
            .as_ref()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let session = self
            .stored_session
            .as_ref()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let id = persistence
            .store
            .mcp_interaction_id(session.id, &request_id)
            .map_err(|e| fail(e.code()))?;
        let mut token = McpCancellation::linked(&self.stop);
        let mut deadline =
            tokio::time::Instant::now() + Duration::from_millis(self.config.limits.timeout_ms);
        let mut origin_id = associated.clone();
        let mut attribution = if associated.is_some() {
            McpAttribution::ResponseStream
        } else {
            McpAttribution::Session
        };
        {
            let pending = self
                .pending
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?;
            if origin_id.is_none() && pending.len() == 1 {
                origin_id = pending.keys().next().cloned();
                attribution = McpAttribution::SerializedOperation;
            }
            if let Some(parent) = origin_id.as_ref().and_then(|id| pending.get(id)) {
                token.parents.push(parent.cancellation.state.clone());
                token
                    .parents
                    .extend(parent.cancellation.parents.iter().cloned());
                deadline = deadline.min(parent.deadline);
            }
        }
        let token = Arc::new(token);
        {
            let mut incoming = self
                .incoming
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?;
            if let Some((digest, _)) = incoming.get(&request_id) {
                if *digest != request.digest() {
                    return Err(fail(FailureCode::McpProtocol));
                }
                return Ok(());
            }
            if incoming.len() >= self.config.limits.callbacks {
                self.end(McpSessionEnd::CallbackOverflow);
                return Err(fail(FailureCode::Budget));
            }
            incoming.insert(request_id.clone(), (request.digest(), token.clone()));
        }
        let reservation = self.reserve(self.limits.max_document_bytes.saturating_mul(3))?;
        let host = McpInteractionHost::new((*self.config).clone())?;
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis();
        let deadline_ms = persistence
            .clock
            .now_ms()?
            .checked_add(i64::try_from(remaining).map_err(|_| fail(FailureCode::Profile))?)
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let context = McpInteractionContext {
            labels: Default::default(),
            owner: self.owner.clone(),
            server: self.server.clone(),
            epoch: self.epoch,
            interaction_id: id,
            request_id: request_id.clone(),
            originating_request_id: origin_id,
            depth: 1,
            origin: None,
            attribution,
            task_id: None,
            parent_interaction: None,
            deadline_ms,
            cancellation: token,
        };
        let work_context = context.clone();
        let work_request = request.clone();
        let work_host = host.clone();
        let wire = self.clone();
        let receive = self
            .executors
            .callbacks
            .as_ref()
            .ok_or_else(|| fail(FailureCode::Profile))?
            .submit(async move {
                wire.run_callback(work_host, work_context, work_request, raw)
                    .await
            })
            .inspect_err(|_| self.end(McpSessionEnd::CallbackOverflow))?;
        let wire = self.clone();
        tokio::spawn(async move {
            let _reservation = reservation;
            let result = wire
                .await_callback(&host, &context, &request, deadline, receive)
                .await;
            if result.is_err() && !wire.stop.is_cancelled() {
                wire.end(McpSessionEnd::CallbackFailure);
            }
            if let Ok(mut incoming) = wire.incoming.lock() {
                incoming.remove(&request_id);
            }
        });
        Ok(())
    }
    async fn run_callback(
        self: &Arc<Self>,
        host: McpInteractionHost,
        mut context: McpInteractionContext,
        request: JsonDocument,
        raw: Vec<u8>,
    ) -> Result<McpStoredInteraction, RuntimeFailure> {
        if context.cancellation.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        let kind = validate_client_request(&request, &self.config, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let session = self
            .stored_session
            .as_ref()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let server = self
            .server
            .digest(&self.limits)
            .map_err(|_| fail(FailureCode::McpIdentity))?;
        let record = host
            .receive(&self.owner, server, session.id, raw, context.deadline_ms)
            .await?;
        context.labels = record.labels.clone();
        if context.cancellation.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        let record = match host.begin(&self.owner, record.id).await {
            Ok(McpWorkClaim::Started(record)) => record,
            Ok(McpWorkClaim::Existing(record)) if record.reply.is_some() => return Ok(record),
            Ok(McpWorkClaim::Existing(_)) => return Err(fail(FailureCode::EffectUncertain)),
            Err(error) => {
                let outcome = McpServiceOutcome::failure(
                    McpWorkDelivery::NotStarted,
                    rpc_error(-32000, "Client work was not admitted.", &self.limits)?,
                );
                return host
                    .finish(&self.owner, &record, outcome)
                    .await
                    .map_err(|_| error);
            }
        };
        if context.cancellation.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        self.interaction_event(&record, kind)?;
        let service = match kind {
            McpIncomingKind::Sampling => {
                let params = as_map(get(as_map(request.value())?, "params")?)?;
                let tool_history = params.get("messages").is_some_and(|messages| matches!(messages, Value::Array(messages) if messages.iter().any(|message|
                    as_map(message).ok().and_then(|message| message.get("content")).is_some_and(|content| {
                        let blocks = match content { Value::Array(values) => values.as_slice(), value => std::slice::from_ref(value) };
                        blocks.iter().any(|block| as_map(block).is_ok_and(|block| matches!(block.get("type"), Some(Value::Text(kind)) if kind == "tool_use" || kind == "tool_result")))
                    }))));
                if params.get("tools").is_some()
                    || params.get("toolChoice").is_some()
                    || tool_history
                {
                    McpClientService::SamplingTools
                } else if params
                    .get("includeContext")
                    .is_some_and(|value| value != &Value::Text("none".into()))
                {
                    McpClientService::SamplingContext
                } else {
                    McpClientService::Sampling
                }
            }
            McpIncomingKind::Form => McpClientService::ElicitationForm,
            McpIncomingKind::Url => McpClientService::ElicitationUrl,
            McpIncomingKind::Roots => McpClientService::Roots,
        };
        let handler = self
            .config
            .handler(service)
            .ok_or_else(|| fail(FailureCode::Profile))?;
        let outcome = handler.handle(&context, &request).await;
        if context.cancellation.is_cancelled()
            && let Some(persistence) = &self.config.persistence
            && let Ok(now) = persistence.clock.now_ms()
        {
            let _ = persistence
                .store
                .cancel_mcp_work(&self.owner, record.id, now)
                .await;
        }
        host.finish(&self.owner, &record, outcome).await
    }
    fn interaction_event(
        &self,
        record: &McpStoredInteraction,
        kind: McpIncomingKind,
    ) -> Result<(), RuntimeFailure> {
        if self.config.events.is_none() {
            return Ok(());
        }
        let params = JsonDocument::from_value(
            &obj([
                ("interaction_id", Value::Text(record.id.to_string())),
                (
                    "request_digest",
                    Value::Text(record.request_digest.to_string()),
                ),
                (
                    "kind",
                    Value::Text(
                        match kind {
                            McpIncomingKind::Sampling => "sampling",
                            McpIncomingKind::Form => "form",
                            McpIncomingKind::Url => "url",
                            McpIncomingKind::Roots => "roots",
                        }
                        .into(),
                    ),
                ),
            ])?,
            &self.limits,
        )
        .map_err(|_| fail(FailureCode::McpProtocol))?;
        let reserved = self.reserve(params.as_bytes().len())?;
        self.events
            .try_send(("htlk/interaction".into(), params, reserved))
            .map_err(|_| fail(FailureCode::Budget))
    }
    async fn await_callback(
        &self,
        host: &McpInteractionHost,
        context: &McpInteractionContext,
        request: &JsonDocument,
        deadline: tokio::time::Instant,
        receive: oneshot::Receiver<Result<McpStoredInteraction, RuntimeFailure>>,
    ) -> Result<(), RuntimeFailure> {
        let persistence = self
            .config
            .persistence
            .as_ref()
            .ok_or_else(|| fail(FailureCode::Profile))?;
        tokio::pin!(receive);
        let result = loop {
            tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => break Err(fail(FailureCode::Cancelled)),
                _ = tokio::time::sleep_until(deadline) => { context.cancellation.cancel(); break Err(fail(FailureCode::McpTimeout)); }
                result = &mut receive => break result.unwrap_or_else(|_| Err(fail(FailureCode::EffectUncertain))),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    if matches!(persistence.store.mcp_interaction_state(&self.owner, context.interaction_id).await, Ok(Some(McpWorkState::Cancelled))) {
                        context.cancellation.cancel(); break Err(fail(FailureCode::Cancelled));
                    }
                }
            }
        };
        let mut record = result.ok();
        if record.is_none() {
            record = host.record(&self.owner, context.interaction_id).await.ok();
            if let Some(previous) = &record
                && previous.reply.is_none()
            {
                let _ = persistence
                    .store
                    .cancel_mcp_work(&self.owner, previous.id, persistence.clock.now_ms()?)
                    .await;
                let error = rpc_error(-32000, "Client interaction unavailable.", &self.limits)?;
                let raw = response(request, Err(&error), &self.limits)?;
                record = persistence
                    .store
                    .record_mcp_cancelled_reply(
                        &self.owner,
                        previous.id,
                        raw,
                        persistence.clock.now_ms()?,
                    )
                    .await
                    .ok();
            }
        }
        if self.stop.is_cancelled() {
            return Ok(());
        }
        self.check_owner().await?;
        if let Some(record) = record {
            let raw = record
                .reply
                .as_ref()
                .ok_or_else(|| fail(FailureCode::EffectUncertain))?;
            let reply =
                JsonDocument::new(raw, &self.limits).map_err(|_| fail(FailureCode::McpProtocol))?;
            host.authorize(
                self.owner.host(),
                &self.owner,
                self.server
                    .digest(&self.limits)
                    .map_err(|_| fail(FailureCode::McpIdentity))?,
                &record,
                McpClientAction::Deliver,
                Some(&reply),
            )?;
            for attempt in 0..2 {
                persistence
                    .store
                    .record_mcp_delivery(&self.owner, record.id, false, persistence.clock.now_ms()?)
                    .await
                    .map_err(|e| fail(e.code()))?;
                match self.send_bytes(raw, None).await {
                    Ok(()) => {
                        if matches!(self.transport, Transport::Http(_)) {
                            persistence
                                .store
                                .record_mcp_delivery(
                                    &self.owner,
                                    record.id,
                                    true,
                                    persistence.clock.now_ms()?,
                                )
                                .await
                                .map_err(|e| fail(e.code()))?;
                        }
                        return Ok(());
                    }
                    Err(error)
                        if attempt == 0
                            && matches!(self.transport, Transport::Http(_))
                            && error.code() == FailureCode::McpTransport
                            && !self.stop.is_cancelled()
                            && tokio::time::Instant::now() < deadline => {}
                    Err(_) => return Ok(()), // Saved uncertainty survives; never recall the handler.
                }
            }
        } else {
            let error = rpc_error(-32602, "Client request was not admitted.", &self.limits)?;
            self.send_bytes(&response(request, Err(&error), &self.limits)?, None)
                .await?;
        }
        Ok(())
    }
}
