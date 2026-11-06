//! Task admission/control never occupies the sole reader or a waiting handler's
//! execution worker. SQLite is the source of truth for cancellation and replay.
use super::*;
use crate::mcp::client::interactions::{response, rpc_error};

impl Wire {
    pub(super) fn start_task_control(
        self: &Arc<Self>,
        request: JsonDocument,
    ) -> Result<(), RuntimeFailure> {
        let method = as_text(get(as_map(request.value())?, "method")?)?;
        let cancelling = method == "tasks/cancel";
        if cancelling && request.as_bytes().len() > 4096 {
            return Err(fail(FailureCode::Budget));
        }
        let capacity = if method == "tasks/cancel" {
            &self.task_cancel_controls
        } else {
            &self.task_controls
        };
        let permit = capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(FailureCode::Budget))?;
        let memory = if cancelling {
            None
        } else {
            Some(
                self.reserve(
                    request
                        .as_bytes()
                        .len()
                        .saturating_add(self.limits.max_document_bytes.saturating_mul(2)),
                )?,
            )
        };
        let request_id = McpRequestId::from_value(get(as_map(request.value())?, "id")?)?;
        let token = Arc::new(McpCancellation::linked(&self.stop));
        let waiting = method == "tasks/result";
        if waiting {
            let mut waits = self
                .task_waits
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?;
            if let Some((digest, _)) = waits.get(&request_id) {
                return if *digest == request.digest() {
                    Ok(())
                } else {
                    Err(fail(FailureCode::McpProtocol))
                };
            }
            waits.insert(request_id.clone(), (request.digest(), token.clone()));
        }
        let wire = self.clone();
        tokio::spawn(async move {
            let (_permit, _memory) = (permit, memory);
            let result = async {
                let receiver = McpTaskReceiver::new(
                    (*wire.config).clone(),
                    wire.config
                        .task_receiver
                        .ok_or_else(|| fail(FailureCode::Profile))?,
                )?;
                let session = wire
                    .stored_session
                    .as_ref()
                    .ok_or_else(|| fail(FailureCode::Profile))?;
                if let Some(reply) = receiver
                    .handle_request(
                        &wire.owner,
                        wire.server
                            .digest(&wire.limits)
                            .map_err(|_| fail(FailureCode::McpIdentity))?,
                        session.id,
                        &request,
                        token,
                    )
                    .await?
                {
                    if cancelling && reply.as_bytes().len() > 8192 {
                        return Err(fail(FailureCode::Budget));
                    }
                    wire.check_owner().await?;
                    wire.send_bytes(reply.as_bytes(), None).await?;
                }
                Ok::<_, RuntimeFailure>(())
            }
            .await;
            if waiting && let Ok(mut waits) = wire.task_waits.lock() {
                waits.remove(&request_id);
            }
            if result.is_err() && !wire.stop.is_cancelled() {
                wire.end(McpSessionEnd::CallbackFailure);
            }
        });
        Ok(())
    }

    pub(super) fn replay_task_acceptance(
        self: &Arc<Self>,
        request: JsonDocument,
        interaction: Digest,
    ) -> Result<(), RuntimeFailure> {
        let memory = self.reserve(
            request
                .as_bytes()
                .len()
                .saturating_add(self.limits.max_document_bytes.saturating_mul(2)),
        )?;
        let permit = self
            .task_controls
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(FailureCode::Budget))?;
        let wire = self.clone();
        tokio::spawn(async move {
            let (_permit, _memory) = (permit, memory);
            let _ = async {
                let receiver = McpTaskReceiver::new(
                    (*wire.config).clone(),
                    wire.config
                        .task_receiver
                        .ok_or_else(|| fail(FailureCode::Profile))?,
                )?;
                let session = wire
                    .stored_session
                    .as_ref()
                    .ok_or_else(|| fail(FailureCode::Profile))?;
                let server = wire
                    .server
                    .digest(&wire.limits)
                    .map_err(|_| fail(FailureCode::McpIdentity))?;
                if let Some(acceptance) = receiver
                    .accept(&wire.owner, server, session.id, interaction, &request)
                    .await?
                {
                    wire.send_task_acceptance(&request, &acceptance).await?;
                }
                Ok::<_, RuntimeFailure>(())
            }
            .await;
        });
        Ok(())
    }

    async fn send_task_acceptance(
        &self,
        request: &JsonDocument,
        acceptance: &McpReceivedTaskAcceptance,
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
        let server = self
            .server
            .digest(&self.limits)
            .map_err(|_| fail(FailureCode::McpIdentity))?;
        let host = McpInteractionHost::new((*self.config).clone())?;
        let record = host
            .record(&self.owner, acceptance.task.interaction_id())
            .await?;
        host.authorize(
            self.owner.host(),
            &self.owner,
            server,
            &record,
            McpClientAction::Deliver,
            Some(&acceptance.result),
        )?;
        let raw = response(request, Ok(&acceptance.result), &self.limits)?;
        for attempt in 0..2 {
            self.check_owner().await?;
            persistence
                .store
                .record_mcp_received_task_acceptance_delivery(
                    &self.owner,
                    session.id,
                    server,
                    acceptance.task.task_id(),
                    false,
                    persistence.clock.now_ms()?,
                )
                .await
                .map_err(|e| fail(e.code()))?;
            match self.send_bytes(&raw, None).await {
                Ok(()) => {
                    if matches!(self.transport, Transport::Http(_)) {
                        persistence
                            .store
                            .record_mcp_received_task_acceptance_delivery(
                                &self.owner,
                                session.id,
                                server,
                                acceptance.task.task_id(),
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
                        && error.code() == FailureCode::McpTransport => {}
                Err(error) => return Err(error),
            }
        }
        Err(fail(FailureCode::McpTransport))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_task_callback(
        self: &Arc<Self>,
        host: McpInteractionHost,
        mut context: McpInteractionContext,
        request: JsonDocument,
        raw: Vec<u8>,
        deadline: tokio::time::Instant,
        reservation: tokio::sync::OwnedSemaphorePermit,
        receiver: McpTaskReceiver,
    ) {
        let wire = self.clone();
        tokio::spawn(async move {
            let _reservation = reservation;
            let result = async {
                validate_client_request(&request, &wire.config, &wire.limits).map_err(|_| fail(FailureCode::McpProtocol))?;
                let session = wire.stored_session.as_ref().ok_or_else(|| fail(FailureCode::Profile))?;
                let persistence = wire.config.persistence.as_ref().ok_or_else(|| fail(FailureCode::Profile))?;
                let server = wire.server.digest(&wire.limits).map_err(|_| fail(FailureCode::McpIdentity))?;
                let record = host.receive(&wire.owner, server, session.id, context.origin.clone(), context.parent_interaction, context.labels.clone(), raw.clone(), context.deadline_ms).await?;
                let acceptance = receiver.accept(&wire.owner, server, session.id, record.id, &request).await?.ok_or_else(|| fail(FailureCode::McpProtocol))?;
                context.task_id = Some(acceptance.task.task_id().to_owned());
                wire.accepted_callbacks.lock().map_err(|_| fail(FailureCode::Invariant))?.insert(context.request_id.clone());
                wire.send_task_acceptance(&request, &acceptance).await?;
                // A saved intent can only be reconciled, never dispatched again.
                if record.state != crate::store::McpWorkState::Pending { return Ok::<(),RuntimeFailure>(()); }
                let worker_wire = wire.clone();
                let worker_host = host.clone();
                let worker_context = context.clone();
                let worker_request = request.clone();
                let submitted = wire.executors.callbacks.as_ref().ok_or_else(|| fail(FailureCode::Profile))?
                    .submit(async move { worker_wire.run_callback(worker_host, worker_context, worker_request, raw).await });
                let receive = match submitted {
                    Ok(receive) => receive,
                    Err(_) => {
                        host.finish(&wire.owner, &record, McpServiceOutcome::failure(McpWorkDelivery::NotStarted, rpc_error(-32000, "Client task execution capacity exhausted.", &wire.limits)?)).await?;
                        return Ok(());
                    }
                };
                tokio::pin!(receive);
                loop {
                    tokio::select! {
                        biased;
                        _ = wire.stop.cancelled() => { context.cancellation.cancel(); return Ok(()); }
                        _ = tokio::time::sleep_until(deadline) => {
                            // Synchronize deadline failure before cancelling the
                            // worker; its unknown work reservation is retained.
                            let _ = persistence.store.mcp_received_task(&wire.owner, session.id, server, acceptance.task.task_id(), persistence.clock.now_ms()?).await;
                            context.cancellation.cancel();
                            let _ = persistence.store.cancel_mcp_work(&wire.owner, record.id, persistence.clock.now_ms()?).await;
                            return Ok(());
                        }
                        _ = &mut receive => return Ok(()),
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {
                            if matches!(persistence.store.mcp_interaction_state(&wire.owner, record.id).await, Ok(Some(crate::store::McpWorkState::Cancelled))) {
                                context.cancellation.cancel(); return Ok(());
                            }
                        }
                    }
                }
            }.await;
            if result.is_err() && !wire.stop.is_cancelled() {
                wire.end(McpSessionEnd::CallbackFailure);
            }
            if let Ok(mut incoming) = wire.incoming.lock() {
                incoming.remove(&context.request_id);
            }
            wire.clear_progress(&context.request_id);
        });
    }
}
