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
        related: Option<crate::store::McpRemoteTaskRecord>,
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
        let mut operation = None;
        let mut parent_work = None;
        let mut attribution = if associated.is_some() {
            McpAttribution::ResponseStream
        } else {
            McpAttribution::Session
        };
        if related.is_none() {
            let pending = self
                .pending
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?;
            if origin_id.is_none()
                && matches!(self.transport, Transport::Stdio { .. })
                && pending.len() != 1
                && pending.values().any(|request| request.parent.is_some())
            {
                // A waiting ancestor and its native child are both outstanding.
                // Stdio carries no response-stream proof selecting either one.
                // Do not turn this into fresh standalone work with a reset parent.
                return Err(fail(FailureCode::McpIdentity));
            }
            if origin_id.is_none()
                && matches!(self.transport, Transport::Stdio { .. })
                && pending.len() == 1
            {
                origin_id = pending.keys().next().cloned();
                attribution = McpAttribution::SerializedOperation;
            }
            if let Some(parent) = origin_id.as_ref().and_then(|id| pending.get(id)) {
                parent_work = parent.parent.clone();
                operation = parent.context.clone();
                token.parents.push(parent.cancellation.state.clone());
                token
                    .parents
                    .extend(parent.cancellation.parents.iter().cloned());
                deadline = deadline.min(parent.deadline);
            }
        }
        if let Some(related) = &related {
            attribution = McpAttribution::RelatedTask;
            origin_id = None;
            let now = persistence.clock.now_ms()?;
            let remaining = related.deadline_ms.saturating_sub(now).max(0) as u64;
            deadline = deadline.min(tokio::time::Instant::now() + Duration::from_millis(remaining));
            operation = related.origin.clone().map(|origin| McpAttemptContext {
                origin,
                server: self
                    .server
                    .digest(&self.limits)
                    .expect("validated identity"),
                deadline_ms: related.deadline_ms,
                labels: related.labels.clone(),
            });
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
                if self
                    .accepted_callbacks
                    .lock()
                    .map_err(|_| fail(FailureCode::Invariant))?
                    .contains(&request_id)
                {
                    self.replay_task_acceptance(request.clone(), id)?;
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
        if let Some(operation) = &operation
            && (operation.server
                != self
                    .server
                    .digest(&self.limits)
                    .map_err(|_| fail(FailureCode::McpIdentity))?
                || !matches!(self.owner.scope(), McpOwnerScope::Run(run) if run == &operation.origin.run_id))
        {
            return Err(fail(FailureCode::McpIdentity));
        }
        let context = McpInteractionContext {
            work: None,
            progress: None,
            labels: operation
                .as_ref()
                .map_or_else(Default::default, |operation| operation.labels.clone()),
            owner: self.owner.clone(),
            server: self.server.clone(),
            epoch: self.epoch,
            interaction_id: id,
            request_id: request_id.clone(),
            originating_request_id: origin_id,
            depth: parent_work.as_ref().map_or(1, |parent| parent.depth + 1),
            origin: operation.as_ref().map(|operation| operation.origin.clone()),
            attribution,
            task_id: related
                .as_ref()
                .and_then(|record| record.task.as_ref())
                .map(|state| state.task_id().to_owned()),
            parent_interaction: parent_work.as_ref().map(|parent| parent.interaction_id),
            deadline_ms: operation.as_ref().map_or(deadline_ms, |operation| {
                deadline_ms.min(operation.deadline_ms)
            }),
            cancellation: token,
        };
        if context.depth > self.config.limits.callback_depth {
            return Err(fail(FailureCode::Budget));
        }
        if let Some(settings) = self.config.task_receiver {
            let receiver = McpTaskReceiver::new((*self.config).clone(), settings)?;
            let fields = as_map(request.value())?;
            let method = as_text(get(fields, "method")?)?;
            if receiver.supports(method)
                && fields
                    .get("params")
                    .and_then(|p| as_map(p).ok())
                    .is_some_and(|p| p.get("task").is_some())
            {
                self.start_task_callback(
                    host,
                    context,
                    request,
                    raw,
                    deadline,
                    reservation,
                    receiver,
                );
                return Ok(());
            }
        }
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
            wire.clear_progress(&request_id);
        });
        Ok(())
    }
    pub(super) async fn run_callback(
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
            .receive(
                &self.owner,
                server,
                session.id,
                context.origin.clone(),
                context.parent_interaction,
                context.labels.clone(),
                raw,
                context.deadline_ms,
            )
            .await?;
        let url_id = if kind == McpIncomingKind::Url {
            let persistence = self
                .config
                .persistence
                .as_ref()
                .ok_or_else(|| fail(FailureCode::Profile))?;
            Some(
                persistence
                    .store
                    .track_mcp_url_callback(
                        &self.owner,
                        server,
                        session.id,
                        record.id,
                        persistence.clock.now_ms()?,
                        self.config.limits.requests_per_session,
                    )
                    .await
                    .map_err(|e| fail(e.code()))?,
            )
        } else {
            None
        };
        context.labels = record.labels.clone();
        context.deadline_ms = record.deadline_ms;
        context.origin = record.origin.clone();
        context.parent_interaction = record.parent_interaction;
        context.depth = record.depth;
        host.authorize(
            self.owner.host(),
            &self.owner,
            server,
            &record,
            McpClientAction::Dispatch,
            None,
        )?;
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
        context.work = Some(McpHostWork::new(
            host.clone(),
            self.owner.clone(),
            record.clone(),
            context.cancellation.clone(),
            self.session(),
        ));
        if context.cancellation.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        self.interaction_event(&record, kind, url_id)?;
        let service = match kind {
            McpIncomingKind::Sampling => {
                protocol::sampling_service(as_map(get(as_map(request.value())?, "params")?)?)
            }
            McpIncomingKind::Form => McpClientService::ElicitationForm,
            McpIncomingKind::Url => McpClientService::ElicitationUrl,
            McpIncomingKind::Roots => McpClientService::Roots,
        };
        let handler = self
            .config
            .handler(service)
            .ok_or_else(|| fail(FailureCode::Profile))?;
        context.progress = self.progress_reporter(&request, &context)?;
        let outcome = handler.handle(&context, &request).await;
        self.clear_progress(&context.request_id);
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
        url_id: Option<Digest>,
    ) -> Result<(), RuntimeFailure> {
        if self.config.events.is_none() {
            return Ok(());
        }
        let mut fields = vec![
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
        ];
        if let Some(id) = url_id {
            fields.push(("url_interaction_id", Value::Text(id.to_string())));
        }
        let params = JsonDocument::from_value(
            &Value::Map(
                Map::try_from_entries(
                    fields
                        .into_iter()
                        .map(|(key, value)| (key.into(), value))
                        .collect::<Vec<_>>(),
                )
                .map_err(|_| fail(FailureCode::McpProtocol))?,
            ),
            &self.limits,
        )
        .map_err(|_| fail(FailureCode::McpProtocol))?;
        let reserved = self.reserve(params.as_bytes().len())?;
        self.events
            .try_send(("htlk/interaction".into(), params, None, reserved))
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
        if matches!(self.owner.scope(), McpOwnerScope::Run(_))
            && record
                .as_ref()
                .is_some_and(|record| record.origin != context.origin)
        {
            record = None;
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
