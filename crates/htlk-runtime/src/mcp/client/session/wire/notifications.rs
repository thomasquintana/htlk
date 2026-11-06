//! Notification admission stays separate from URL/task control and never acquires
//! content. Outgoing controls run on the I/O reactor, not the callback worker.
use super::*;
use crate::mcp::client::notifications::{self as model, ProgressCursor};
use htlk_analyzer::{McpMessageKind, validate_mcp_message};

pub(super) struct SubscriptionObservation {
    state: McpResourceUpdateState,
    inflight: usize,
    conflicted: bool,
    _memory: tokio::sync::OwnedSemaphorePermit,
}
pub(super) struct SubscriptionRequest {
    uri: String,
    subscribing: bool,
    previous: McpResourceUpdateState,
}
pub(super) struct CallbackProgress {
    token: McpProgressToken,
    cursor: ProgressCursor,
    deadline_ms: i64,
    cancellation: Arc<McpCancellation>,
    context: McpInteractionContext,
    request: JsonDocument,
}
pub(super) struct ProgressLease {
    request_id: McpRequestId,
    token: McpProgressToken,
    cursor: ProgressCursor,
    origin: Option<McpOperationOrigin>,
    deadline: tokio::time::Instant,
    cancellation: Arc<McpCancellation>,
}
pub(super) struct TaskProgress {
    lease: ProgressLease,
    task_id: String,
    _memory: tokio::sync::OwnedSemaphorePermit,
}
pub(super) enum Control {
    RootsChanged,
    Progress {
        request: McpRequestId,
        digest: Digest,
        update: McpProgress,
    },
}
pub(super) struct ControlCommand {
    control: Control,
    document: JsonDocument,
    cancellation: Arc<McpCancellation>,
    deadline: tokio::time::Instant,
    reply: oneshot::Sender<McpNotificationReply>,
    _memory: tokio::sync::OwnedSemaphorePermit,
}
impl Wire {
    pub(super) fn install_task_progress(
        &self,
        id: &McpRequestId,
        lease: Option<ProgressLease>,
        state: &McpTaskState,
    ) {
        if let Err(error) = self.retain_task_progress(lease, state)
            && let Ok(params) = obj([
                ("request_id", id.to_value()),
                ("code", Value::Text(error.code().as_str().into())),
            ])
        {
            // Optional progress retention cannot downgrade a persisted task
            // acceptance or replace its correlated result/error evidence.
            let _ = self.local_event("htlk/progress_unavailable", params);
        }
    }
    pub(super) fn take_task_progress(
        &self,
        id: &McpRequestId,
        request: &mut PendingRequest,
    ) -> Option<ProgressLease> {
        request
            .progress
            .take()
            .map(|(token, cursor)| ProgressLease {
                request_id: id.clone(),
                token,
                cursor,
                origin: request.context.as_ref().map(|c| c.origin.clone()),
                deadline: request.deadline,
                cancellation: request.cancellation.clone(),
            })
    }
    pub(super) fn retain_task_progress(
        &self,
        lease: Option<ProgressLease>,
        state: &McpTaskState,
    ) -> Result<(), RuntimeFailure> {
        let Some(mut lease) = lease else {
            return Ok(());
        };
        if state.status().is_terminal() {
            return Ok(());
        }
        let bytes = state
            .task_id()
            .len()
            .saturating_add(
                JsonDocument::from_value(&lease.token.to_value(), &self.limits)
                    .map_err(|_| fail(FailureCode::McpProtocol))?
                    .as_bytes()
                    .len()
                    .saturating_mul(2),
            )
            .saturating_add(
                lease
                    .origin
                    .as_ref()
                    .map_or(0, |origin| origin.run_id.as_str().len()),
            )
            .saturating_add(512);
        let memory = self.reserve(bytes)?;
        let mut progress = self
            .task_progress
            .lock()
            .map_err(|_| fail(FailureCode::Invariant))?;
        progress.retain(|_, state| {
            !state.lease.cancellation.is_cancelled()
                && tokio::time::Instant::now() < state.lease.deadline
        });
        if progress.len() >= self.config.limits.requests_per_session {
            return Err(fail(FailureCode::Budget));
        }
        let now = match &self.config.persistence {
            Some(p) => p.clock.now_ms()?,
            None => crate::clock::CoordinatorClock::now_ms(&crate::clock::SystemClock)?,
        };
        if let Some(ttl) = state.remaining_ttl_ms(now) {
            let current = tokio::time::Instant::now();
            let remaining = lease.deadline.saturating_duration_since(current);
            let ttl = Duration::from_millis(ttl);
            // Clamp before adding an untrusted remote duration; neither huge TTL
            // values nor an already elapsed deadline can extend the request.
            if ttl < remaining {
                lease.deadline = current + ttl;
            }
        }
        progress.insert(
            lease.token.clone(),
            TaskProgress {
                lease,
                task_id: state.task_id().into(),
                _memory: memory,
            },
        );
        Ok(())
    }
    pub(super) fn finish_task_progress(&self, task: &str) {
        if let Ok(mut progress) = self.task_progress.lock() {
            progress.retain(|_, p| p.task_id != task);
        }
    }
    pub(super) fn start_controls(self: &Arc<Self>, mut receive: mpsc::Receiver<ControlCommand>) {
        let wire = self.clone();
        tokio::spawn(async move {
            let mut jobs = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _=wire.stop.cancelled()=>break,
                    _=jobs.join_next(),if !jobs.is_empty()=>{},
                    command=receive.recv()=>match command {
                        None=>break,
                        Some(command)=>{
                            if jobs.len()>=2 {
                                let _=command.reply.send(McpNotificationReply{delivery:McpDelivery::NotSent,result:Err(fail(FailureCode::Budget))});
                                continue;
                            }
                            let wire=wire.clone();jobs.spawn(async move {wire.send_control(command).await;});
                        }
                    }
                }
            }
            jobs.abort_all();
        });
    }
    pub async fn roots_changed(
        &self,
        cancellation: Arc<McpCancellation>,
    ) -> Result<McpNotificationReply, RuntimeFailure> {
        if self
            .config
            .handler(McpClientService::RootsChanged)
            .is_none()
        {
            return Err(fail(FailureCode::Profile));
        }
        let document = model::envelope("notifications/roots/list_changed", obj([])?, &self.limits)?;
        validate_mcp_message(McpMessageKind::RootsChanged, &document, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?;
        self.submit_control(Control::RootsChanged, document, cancellation)
            .await
    }
    async fn submit_control(
        &self,
        control: Control,
        document: JsonDocument,
        parent: Arc<McpCancellation>,
    ) -> Result<McpNotificationReply, RuntimeFailure> {
        if parent.is_cancelled() || self.stop.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        let cancellation = Arc::new(McpCancellation::linked(&parent));
        struct Cancel(Arc<McpCancellation>);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let _cancel = Cancel(cancellation.clone());
        let memory = self.reserve(document.as_bytes().len())?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(self.config.limits.timeout_ms);
        let (reply, receive) = oneshot::channel();
        let command = ControlCommand {
            control,
            document,
            cancellation: cancellation.clone(),
            deadline,
            reply,
            _memory: memory,
        };
        tokio::time::timeout_at(deadline,async {
            tokio::select!{biased;_=cancellation.cancelled()=>return Err(fail(FailureCode::Cancelled)),_=self.stop.cancelled()=>return Err(fail(FailureCode::Cancelled)),sent=self.controls.send(command)=>sent.map_err(|_|fail(FailureCode::McpTransport))?}
            receive.await.map_err(|_|fail(FailureCode::McpTransport))
        }).await.map_err(|_|fail(FailureCode::McpTimeout))?
    }
    async fn send_control(&self, command: ControlCommand) {
        let mut delivery = McpDelivery::NotSent;
        let result=tokio::time::timeout_at(command.deadline,async {
            if self.control_sequence.fetch_add(1,Ordering::Relaxed)>=self.config.limits.messages_per_session as u64 {return Err(fail(FailureCode::Budget));}
            self.check_owner().await?;
            if self.stop.is_cancelled() || command.cancellation.is_cancelled(){return Err(fail(FailureCode::Cancelled));}
            match &command.control {
                Control::RootsChanged=>{
                    if self.config.handler(McpClientService::RootsChanged).is_none(){return Err(fail(FailureCode::Profile));}
                },
                Control::Progress{request,digest,update}=>{
                    let incoming=self.incoming.lock().map_err(|_|fail(FailureCode::Invariant))?;
                    if !incoming.get(request).is_some_and(|(actual,_)|actual==digest){return Err(fail(FailureCode::Fenced));}
                    let mut progress=self.callback_progress.lock().map_err(|_|fail(FailureCode::Invariant))?;
                    let state=progress.get_mut(request).ok_or_else(||fail(FailureCode::Fenced))?;
                    let now=self.config.persistence.as_ref().ok_or_else(||fail(FailureCode::Profile))?.clock.now_ms()?;
                    if state.cancellation.is_cancelled() || now>=state.deadline_ms {return Err(fail(FailureCode::Cancelled));}
                    let mut update=update.clone();
                    if state.token!=*update.token() || !state.cursor.admit(&mut update){return Err(fail(FailureCode::Input));}
                    let context=state.context.clone();let request=state.request.clone();
                    drop(progress);drop(incoming);
                    let authorizer=self.config.authorizer.as_ref().ok_or_else(||fail(FailureCode::Profile))?;
                    let additional=authorizer.result_labels(self.owner.host(),&self.owner,&command.document)?;
                    let labels=context.labels.union(&additional,&self.limits).map_err(|_|fail(FailureCode::Profile))?;
                    authorizer.authorize(self.owner.host(),McpClientAuthorization{
                        origin:context.origin.as_ref(),action:McpClientAction::Deliver,owner:&self.owner,
                        server:Some(self.server.digest(&self.limits).map_err(|_|fail(FailureCode::McpIdentity))?),
                        interaction_id:Some(context.interaction_id),request_digest:Some(request.digest()),
                        labels:&labels,request:Some(&request),response:Some(&command.document),
                    })?;
                }
            }
            // Once a transport operation begins its result cannot prove no-send.
            delivery=McpDelivery::PossiblySent;
            tokio::select!{biased;_=command.cancellation.cancelled()=>Err(fail(FailureCode::Cancelled)),_=self.stop.cancelled()=>Err(fail(FailureCode::Cancelled)),result=self.send_bytes(command.document.as_bytes(),None)=>result}
        }).await.unwrap_or_else(|_|Err(fail(FailureCode::McpTimeout)));
        let _ = command
            .reply
            .send(McpNotificationReply { delivery, result });
    }
    pub(super) fn progress_reporter(
        self: &Arc<Self>,
        request: &JsonDocument,
        context: &McpInteractionContext,
    ) -> Result<Option<McpProgressReporter>, RuntimeFailure> {
        let Some(token) = as_map(request.value())?
            .get("params")
            .map(request_token)
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        let mut progress = self
            .callback_progress
            .lock()
            .map_err(|_| fail(FailureCode::Invariant))?;
        if progress.values().any(|state| state.token == token) {
            return Err(fail(FailureCode::McpProtocol));
        }
        if progress.len() >= self.config.limits.callbacks {
            return Err(fail(FailureCode::Budget));
        }
        progress.insert(
            context.request_id.clone(),
            CallbackProgress {
                token: token.clone(),
                cursor: ProgressCursor::default(),
                deadline_ms: context.deadline_ms,
                cancellation: context.cancellation.clone(),
                context: context.clone(),
                request: request.clone(),
            },
        );
        let wire = Arc::downgrade(self);
        let request_id = context.request_id.clone();
        let digest = request.digest();
        let cancellation = context.cancellation.clone();
        Ok(Some(McpProgressReporter {
            token,
            send: Arc::new(move |update| {
                let (wire, request, cancellation) =
                    (wire.clone(), request_id.clone(), cancellation.clone());
                Box::pin(async move {
                    let wire = wire.upgrade().ok_or_else(|| fail(FailureCode::Fenced))?;
                    let document = update.envelope(&wire.limits)?;
                    validate_mcp_message(
                        McpMessageKind::ProgressNotification,
                        &document,
                        &wire.limits,
                    )
                    .map_err(|_| fail(FailureCode::Input))?;
                    wire.submit_control(
                        Control::Progress {
                            request,
                            digest,
                            update,
                        },
                        document,
                        cancellation,
                    )
                    .await
                })
            }),
        }))
    }
    pub(super) fn clear_progress(&self, id: &McpRequestId) {
        if let Ok(mut progress) = self.callback_progress.lock() {
            progress.remove(id);
        }
    }

    pub(super) fn begin_subscription(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<Option<SubscriptionRequest>, RuntimeFailure> {
        if !matches!(method, "resources/subscribe" | "resources/unsubscribe") {
            return Ok(None);
        }
        let uri = as_text(get(as_map(params)?, "uri")?)?;
        let mut subscriptions = self
            .subscriptions
            .lock()
            .map_err(|_| fail(FailureCode::Invariant))?;
        if !subscriptions.contains_key(uri) {
            if subscriptions.len() >= self.config.limits.requests_per_session {
                return Err(fail(FailureCode::Budget));
            }
            let memory = self.reserve(
                uri.len()
                    .checked_add(128)
                    .ok_or_else(|| fail(FailureCode::Budget))?,
            )?;
            subscriptions.insert(
                uri.into(),
                SubscriptionObservation {
                    state: McpResourceUpdateState::Unknown,
                    inflight: 0,
                    conflicted: false,
                    _memory: memory,
                },
            );
        }
        let state = subscriptions.get_mut(uri).expect("inserted");
        let previous = state.state;
        if state.inflight > 0 {
            state.conflicted = true;
            state.state = McpResourceUpdateState::Uncertain;
        } else {
            state.conflicted = false;
            state.state = if method == "resources/subscribe" {
                McpResourceUpdateState::Subscribing
            } else {
                McpResourceUpdateState::Unsubscribing
            };
        }
        state.inflight += 1;
        Ok(Some(SubscriptionRequest {
            uri: uri.into(),
            subscribing: method == "resources/subscribe",
            previous,
        }))
    }
    pub(super) fn finish_subscription(
        &self,
        request: Option<SubscriptionRequest>,
        result: Option<&Result<JsonDocument, McpRpcError>>,
        sent: bool,
    ) {
        let Some(request) = request else { return };
        if let Ok(mut subscriptions) = self.subscriptions.lock()
            && let Some(state) = subscriptions.get_mut(&request.uri)
        {
            state.inflight = state.inflight.saturating_sub(1);
            let acknowledged = matches!(result,Some(Ok(document)) if as_map(document.value()).is_ok_and(|fields|fields.get("_meta").is_none_or(|v|matches!(v,Value::Map(_)))));
            state.state = if state.conflicted {
                McpResourceUpdateState::Uncertain
            } else if acknowledged {
                if request.subscribing {
                    McpResourceUpdateState::Subscribed
                } else {
                    McpResourceUpdateState::Unsubscribed
                }
            } else if !sent {
                request.previous
            } else {
                McpResourceUpdateState::Uncertain
            };
        }
    }
    fn capability(&self, name: &str, extension: Option<&str>) -> bool {
        self.initialization
            .get()
            .and_then(|d| as_map(d.value()).ok())
            .and_then(|p| p.get("capabilities"))
            .and_then(|p| as_map(p).ok())
            .and_then(|p| p.get(name))
            .and_then(|p| as_map(p).ok())
            .is_some_and(|p| extension.is_none_or(|key| p.get(key) == Some(&Value::Bool(true))))
    }
    pub(super) fn notification(
        &self,
        method: &str,
        document: &JsonDocument,
        associated: Option<&McpRequestId>,
    ) -> Result<Option<McpNotification>, RuntimeFailure> {
        use McpNotificationIgnored as Ignored;
        let (shape, capability, extension) = match method {
            "notifications/resources/updated" => (
                McpMessageKind::ResourceUpdated,
                Some("resources"),
                Some("subscribe"),
            ),
            "notifications/tools/list_changed" => (
                McpMessageKind::ToolsChanged,
                Some("tools"),
                Some("listChanged"),
            ),
            "notifications/prompts/list_changed" => (
                McpMessageKind::PromptsChanged,
                Some("prompts"),
                Some("listChanged"),
            ),
            "notifications/resources/list_changed" => (
                McpMessageKind::ResourcesChanged,
                Some("resources"),
                Some("listChanged"),
            ),
            "notifications/message" => (McpMessageKind::LoggingNotification, Some("logging"), None),
            "notifications/progress" => (McpMessageKind::ProgressNotification, None, None),
            _ => return Ok(None),
        };
        if self.initialization.get().is_none() {
            return Ok(Some(McpNotification::Ignored(
                Ignored::BeforeInitialization,
            )));
        }
        if capability.is_some_and(|name| !self.capability(name, extension)) {
            return Ok(Some(McpNotification::Ignored(
                Ignored::UnsupportedCapability,
            )));
        }
        validate_mcp_message(shape, document, &self.limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        let params = as_map(document.value())?
            .get("params")
            .and_then(|value| as_map(value).ok());
        Ok(Some(match method {
            "notifications/resources/updated" => {
                let uri = as_text(get(
                    params.ok_or_else(|| fail(FailureCode::McpProtocol))?,
                    "uri",
                )?)?
                .to_owned();
                let subscription = self
                    .subscriptions
                    .lock()
                    .map_err(|_| fail(FailureCode::Invariant))?
                    .get(&uri)
                    .map_or(McpResourceUpdateState::Unknown, |state| state.state);
                McpNotification::ResourceUpdated { uri, subscription }
            }
            "notifications/tools/list_changed" => {
                McpNotification::CatalogChanged(McpCatalogChanged::Tools)
            }
            "notifications/prompts/list_changed" => {
                McpNotification::CatalogChanged(McpCatalogChanged::Prompts)
            }
            "notifications/resources/list_changed" => {
                McpNotification::CatalogChanged(McpCatalogChanged::ResourcesAndTemplates)
            }
            "notifications/message" => {
                let params = params.ok_or_else(|| fail(FailureCode::McpProtocol))?;
                McpNotification::Logging {
                    level: model::logging_level(get(params, "level")?)?,
                    logger: params
                        .get("logger")
                        .and_then(|v| as_text(v).ok())
                        .map(str::to_owned),
                    data: JsonDocument::from_value(get(params, "data")?, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?,
                }
            }
            "notifications/progress" => {
                let mut update = McpProgress::parse(document, &self.limits)?;
                let mut pending = self
                    .pending
                    .lock()
                    .map_err(|_| fail(FailureCode::Invariant))?;
                let found = pending.iter_mut().find(|(_, p)| {
                    p.progress
                        .as_ref()
                        .is_some_and(|(token, _)| token == update.token())
                });
                let Some((id, request)) = found else {
                    let mut tasks = self
                        .task_progress
                        .lock()
                        .map_err(|_| fail(FailureCode::Invariant))?;
                    let Some(task) = tasks.get_mut(update.token()) else {
                        return Ok(Some(McpNotification::Ignored(
                            Ignored::UnknownProgressToken,
                        )));
                    };
                    let valid_stream = associated.is_none_or(|id| {
                        id == &task.lease.request_id
                            || pending.get(id).is_some_and(|p| {
                                p.task_target
                                    .as_ref()
                                    .is_some_and(|(id, _)| id == &task.task_id)
                            })
                    });
                    let lease = &mut task.lease;
                    return Ok(Some(if !valid_stream {
                        McpNotification::Ignored(Ignored::WrongProgressRequest)
                    } else if lease.cancellation.is_cancelled()
                        || tokio::time::Instant::now() >= lease.deadline
                    {
                        McpNotification::Ignored(Ignored::ExpiredProgress)
                    } else if !lease.cursor.admit(&mut update) {
                        McpNotification::Ignored(Ignored::NonMonotonicProgress)
                    } else {
                        McpNotification::Progress {
                            request_id: lease.request_id.clone(),
                            origin: lease.origin.clone(),
                            update,
                        }
                    }));
                };
                if associated.is_some_and(|expected| expected != id) {
                    McpNotification::Ignored(Ignored::WrongProgressRequest)
                } else if request.cancellation.is_cancelled()
                    || tokio::time::Instant::now() >= request.deadline
                {
                    McpNotification::Ignored(Ignored::ExpiredProgress)
                } else if !request.sent.load(Ordering::Acquire) {
                    McpNotification::Ignored(Ignored::UnknownProgressToken)
                } else if !request
                    .progress
                    .as_mut()
                    .expect("matched")
                    .1
                    .admit(&mut update)
                {
                    McpNotification::Ignored(Ignored::NonMonotonicProgress)
                } else {
                    McpNotification::Progress {
                        request_id: id.clone(),
                        origin: request.context.as_ref().map(|c| c.origin.clone()),
                        update,
                    }
                }
            }
            _ => unreachable!(),
        }))
    }
}
