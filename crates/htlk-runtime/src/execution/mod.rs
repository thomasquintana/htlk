//! Actix supervision of fenced durable graph execution.
mod approval;
mod artifacts;
mod boundary;
mod checkpoint;
mod composite;
mod control;
mod extension;
pub use extension::{CloseWait, ExtensionReceipt, ExtensionRequest};
mod host;
mod inspection;
mod mcp;
mod mcp_evidence;
pub use mcp_evidence::{McpEvidenceKind, McpEvidencePayload, McpEvidenceView};
mod mcp_samples;
pub use mcp_samples::{McpInferenceSelection, ProtectedInferenceRevision};
mod recovery;
mod retry;
pub use inspection::{
    AttemptView, DecisionView, GraphView, HistoryEntry, HistoryPage, NodeView, RunView, ScopeView,
    WaitView,
};
mod waits;
pub use host::{CancelReceipt, ReplyReceipt};
pub use waits::WaitPolicy;
mod frame;
mod model;
mod step;
use crate::{
    Runtime, RuntimeError,
    failure::FailureCode,
    identity::RunId,
    store::{RunLease, StoreError},
};
use actix::WeakAddr;
use actix::prelude::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
    sync::Arc,
    time::Duration,
};

/// Host-selected coordination scope, independent of graph authorization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionScope {
    /// Coordinate every eligible nonterminal run in the store.
    #[default]
    All,
    /// Start empty and coordinate only runs explicitly attached by the host.
    /// Admission, inspection and host-command notifications never attach a run.
    Attached,
}

/// Finite worker, mailbox, ownership and polling settings.
#[derive(Clone, Debug)]
pub struct ExecutionConfig {
    /// Which runs may acquire ownership, recover, enforce deadlines or execute.
    pub scope: ExecutionScope,
    /// Finite lifetime attachment-set bound, separate from concurrent `max_runs`.
    pub max_attached_runs: usize,
    /// Concurrent calculation workers; network attempts never become pure retries.
    pub workers: usize,
    /// Maximum locally coordinated runs.
    pub max_runs: usize,
    /// Ownership lease duration in milliseconds.
    pub lease_ms: i64,
    /// Scheduling/recovery polling interval.
    pub poll_interval: Duration,
}
impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            scope: ExecutionScope::All,
            max_attached_runs: 4096,
            workers: 2,
            max_runs: 64,
            lease_ms: 30000,
            poll_interval: Duration::from_millis(20),
        }
    }
}
/// A running execution service. Dropping control Runtime data does not rewrite run
/// outcomes; shutdown stops workers and preserves durable state for recovery.
#[derive(Clone)]
pub struct ExecutionEngine {
    address: Addr<Supervisor>,
}
impl ExecutionEngine {
    /// Starts supervision inside the application's Actix system. Source admission
    /// can be used separately; this service consumes durable work and recovery hints.
    ///
    /// # Errors
    /// Returns missing Actix context, invalid finite configuration, or an already
    /// running engine attached to this runtime.
    pub fn launch(runtime: Arc<Runtime>, config: ExecutionConfig) -> Result<Self, RuntimeError> {
        if System::try_current().is_none()
            || config.workers == 0
            || config.workers > 64
            || config.max_runs == 0
            || config.max_runs > 4096
            || config.max_attached_runs == 0
            || config.max_attached_runs > 4096
            || config.lease_ms < 100
            || config.poll_interval.is_zero()
            || config.poll_interval > Duration::from_secs(10)
        {
            return Err(RuntimeError::failure(FailureCode::Profile));
        }
        // Reserve routing before starting any workers. Concurrent launches must
        // never leave two supervisors sharing a single host-command address.
        let mut execution = runtime
            .execution
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?;
        if execution
            .as_ref()
            .and_then(WeakAddr::upgrade)
            .is_some_and(|address| address.connected())
        {
            return Err(RuntimeError::failure(FailureCode::Profile));
        }
        let workers = SyncArbiter::start(config.workers, || Worker);
        let address = Supervisor {
            stop: Arc::new(crate::mcp::McpCancellation::new()),
            runtime: runtime.clone(),
            config,
            workers,
            active: BTreeMap::new(),
            attached: BTreeSet::new(),
            polling: false,
            cursor: None,
        }
        .start();
        *execution = Some(address.downgrade());
        Ok(Self { address })
    }

    /// Select an existing run for this engine's lifetime. Repetition is idempotent;
    /// attaching an already terminal run adds no entry. Prior attachments remain
    /// in the bounded set even after completion.
    /// An `Attached` engine does no background run work before this call. Selection
    /// does not grant policy authority, restart deadlines or create an admission.
    /// The set is additive: use shutdown/new engine to select a different scope.
    ///
    /// # Errors
    /// Returns an unknown run, exhausted attachment capacity or stopped engine.
    pub async fn attach_run(&self, run: &RunId) -> Result<(), RuntimeError> {
        self.address
            .send(AttachRun(run.clone()))
            .await
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?
    }
    /// Stops local coordination without changing terminal outcomes or redispatching
    /// effects. Restart reacquires ownership after existing leases expire.
    ///
    /// # Errors
    /// Returns unavailable supervisor.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        self.address
            .send(Stop)
            .await
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))
    }
}
struct Worker;
impl Actor for Worker {
    type Context = SyncContext<Self>;
}
struct Work {
    stop: Arc<crate::mcp::McpCancellation>,
    runtime: Arc<Runtime>,
    lease: RunLease,
    recovering: bool,
}
impl Message for Work {
    type Result = Result<step::Progress, RuntimeError>;
}
impl Handler<Work> for Worker {
    type Result = Result<step::Progress, RuntimeError>;
    fn handle(&mut self, msg: Work, _: &mut Self::Context) -> Self::Result {
        if msg.stop.is_cancelled() {
            return Ok(step::Progress::Blocked);
        }
        System::new().block_on(recovery::drive(msg.runtime, msg.lease, msg.recovering))
    }
}
pub(crate) struct Supervisor {
    stop: Arc<crate::mcp::McpCancellation>,
    runtime: Arc<Runtime>,
    config: ExecutionConfig,
    workers: Addr<Worker>,
    active: BTreeMap<RunId, Addr<Coordinator>>,
    attached: BTreeSet<RunId>,
    polling: bool,
    cursor: Option<RunId>,
}
impl Actor for Supervisor {
    type Context = Context<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.set_mailbox_capacity(64);
        ctx.notify(Poll);
        ctx.run_interval(self.config.poll_interval, |_, ctx| ctx.notify(Poll));
    }
    fn stopped(&mut self, ctx: &mut Self::Context) {
        self.stop.cancel();
        // A delayed stop from an earlier engine cannot detach its replacement.
        if let Ok(mut execution) = self.runtime.execution.lock()
            && execution
                .as_ref()
                .and_then(WeakAddr::upgrade)
                .is_some_and(|address| address == ctx.address())
        {
            *execution = None;
        }
    }
}
struct Poll;
impl Message for Poll {
    type Result = ();
}

struct AttachRun(RunId);
impl Message for AttachRun {
    type Result = Result<(), RuntimeError>;
}
impl Handler<AttachRun> for Supervisor {
    type Result = AtomicResponse<Self, Result<(), RuntimeError>>;
    fn handle(&mut self, msg: AttachRun, _: &mut Self::Context) -> Self::Result {
        let runtime = self.runtime.clone();
        AtomicResponse::new(Box::pin(
            async move {
                runtime
                    .store
                    .run_header(&msg.0)
                    .await
                    .map_err(RuntimeError::from)
            }
            .into_actor(self)
            .map(|result, actor, ctx| {
                let header = result?;
                if actor.config.scope == ExecutionScope::Attached
                    && !header.status.is_terminal()
                    && !actor.attached.contains(&header.run_id)
                {
                    if actor.attached.len() >= actor.config.max_attached_runs {
                        return Err(RuntimeError::failure(FailureCode::Budget));
                    }
                    actor.attached.insert(header.run_id);
                }
                ctx.notify(Poll);
                Ok(())
            }),
        ))
    }
}
struct Finished(RunId);
impl Message for Finished {
    type Result = ();
}
impl Handler<Finished> for Supervisor {
    type Result = ();
    fn handle(&mut self, msg: Finished, _: &mut Self::Context) {
        self.active.remove(&msg.0);
    }
}
impl Handler<Poll> for Supervisor {
    type Result = ();
    fn handle(&mut self, _: Poll, ctx: &mut Self::Context) {
        if self.polling {
            return;
        }
        self.polling = true;
        let runtime = self.runtime.clone();
        let cursor = self.cursor.clone();
        let active = self.active.keys().cloned().collect::<BTreeSet<_>>();
        let room = self
            .config
            .max_runs
            .saturating_sub(self.active.len())
            .max(1)
            .min(runtime.store.config().max_page_size);
        // Page the selected IDs themselves: unrelated store runs cannot starve
        // an attached run, acquire leases or trigger deadline/recovery mutations.
        let attached = (self.config.scope == ExecutionScope::Attached).then(|| {
            let after = cursor.as_ref().map_or(Bound::Unbounded, Bound::Excluded);
            self.attached
                .range((after, Bound::Unbounded))
                .take(room)
                .cloned()
                .collect::<Vec<_>>()
        });
        ctx.spawn(
            async move {
                let runs = match attached {
                    Some(runs) => runs,
                    None => runtime.store.running_runs(cursor.as_ref(), room).await?,
                };
                let mut ready = Vec::new();
                for run in &runs {
                    if active.contains(run) {
                        continue;
                    }
                    let header = runtime.store.run_header(run).await?;
                    if header.status.is_terminal() {
                        continue;
                    }
                    let now = runtime
                        .store
                        .coordinator_time(runtime.clock.now_ms()?)
                        .await?;
                    if header.owner.is_some() && header.expires_at_ms.is_some_and(|due| now < due) {
                        continue;
                    }
                    let owner = runtime.store.reserve_run_id().await?;
                    ready.push((run.clone(), owner.to_string(), now));
                }
                Ok::<_, RuntimeError>((runs, ready))
            }
            .into_actor(self)
            .map(move |result, actor, ctx| {
                actor.polling = false;
                if let Ok((runs, ready)) = result {
                    actor.cursor = runs.last().cloned();
                    if runs.len() < room {
                        actor.cursor = None
                    }
                    for (run, owner, now) in ready {
                        if actor.active.contains_key(&run)
                            || actor.active.len() >= actor.config.max_runs
                        {
                            continue;
                        }
                        let address = Coordinator {
                            stop: Arc::new(crate::mcp::McpCancellation::linked(&actor.stop)),
                            runtime: actor.runtime.clone(),
                            config: actor.config.clone(),
                            run: run.clone(),
                            owner,
                            state: CoordinationState::Acquiring,
                            busy: false,
                            renewing: false,
                            workers: actor.workers.clone(),
                            parent: ctx.address().downgrade(),
                            initial_time: now,
                            renew_after: now.saturating_add(actor.config.lease_ms / 3),
                            checking: false,
                        }
                        .start();
                        actor.active.insert(run, address);
                    }
                }
            }),
        );
    }
}
struct Stop;
impl Message for Stop {
    type Result = ();
}
impl Handler<Stop> for Supervisor {
    type Result = ();
    fn handle(&mut self, _: Stop, ctx: &mut Self::Context) {
        self.stop.cancel();
        for (_, actor) in std::mem::take(&mut self.active) {
            actor.do_send(Stop)
        }
        ctx.stop();
    }
}
struct Coordinator {
    stop: Arc<crate::mcp::McpCancellation>,
    runtime: Arc<Runtime>,
    config: ExecutionConfig,
    run: RunId,
    owner: String,
    state: CoordinationState,
    busy: bool,
    renewing: bool,
    workers: Addr<Worker>,
    parent: WeakAddr<Supervisor>,
    initial_time: i64,
    renew_after: i64,
    checking: bool,
}
enum CoordinationState {
    Acquiring,
    Recovering(RunLease),
    Ready(RunLease),
}
impl CoordinationState {
    fn lease(&self) -> Option<&RunLease> {
        match self {
            Self::Acquiring => None,
            Self::Recovering(lease) | Self::Ready(lease) => Some(lease),
        }
    }
    fn is_recovered(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
    fn mark_recovered(&mut self) {
        if let Self::Recovering(lease) = self {
            *self = Self::Ready(lease.clone());
        }
    }
}
impl Actor for Coordinator {
    type Context = Context<Self>;
    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.set_mailbox_capacity(32);
        let runtime = self.runtime.clone();
        let run = self.run.clone();
        let owner = self.owner.clone();
        let now = self.initial_time;
        let expiry = now.saturating_add(self.config.lease_ms);
        ctx.wait(
            async move { runtime.store.acquire(&run, owner, now, expiry).await }
                .into_actor(self)
                .map(|result, actor, ctx| match result {
                    Ok(lease) => {
                        if actor
                            .runtime
                            .mcp_control
                            .execution(&lease, &actor.stop)
                            .is_err()
                        {
                            ctx.stop();
                            return;
                        }
                        actor.state = CoordinationState::Recovering(lease);
                        ctx.notify(Pulse)
                    }
                    Err(_) => ctx.stop(),
                }),
        );
        ctx.run_interval(self.config.poll_interval, |_, ctx| ctx.notify(Pulse));
    }
    fn stopped(&mut self, _: &mut Self::Context) {
        self.stop.cancel();
        if let Some(parent) = self.parent.upgrade() {
            parent.do_send(Finished(self.run.clone()))
        }
    }
}
struct Pulse;
impl Message for Pulse {
    type Result = ();
}
impl Handler<Pulse> for Coordinator {
    type Result = ();
    fn handle(&mut self, _: Pulse, ctx: &mut Self::Context) {
        let Some(lease) = self.state.lease().cloned() else {
            return;
        };
        if !self.checking {
            self.checking = true;
            let runtime = self.runtime.clone();
            let check = lease.clone();
            ctx.spawn(
                control::enforce(runtime, check)
                    .into_actor(self)
                    .map(|_, actor, _| actor.checking = false),
            );
        }
        if !self.renewing
            && self
                .runtime
                .clock
                .now_ms()
                .is_ok_and(|now| now >= self.renew_after)
        {
            self.renewing = true;
            let runtime = self.runtime.clone();
            let duration = self.config.lease_ms;
            let lease = lease.clone();
            ctx.spawn(
                async move {
                    let now = runtime
                        .store
                        .coordinator_time(runtime.clock.now_ms()?)
                        .await?;
                    runtime
                        .store
                        .renew(&lease, now, now.saturating_add(duration))
                        .await
                        .map_err(RuntimeError::from)
                }
                .into_actor(self)
                .map(|result, actor, ctx| {
                    actor.renewing = false;
                    actor.renew_after = actor
                        .runtime
                        .clock
                        .now_ms()
                        .unwrap_or(actor.initial_time)
                        .saturating_add(actor.config.lease_ms / 3);
                    if result.is_err() {
                        ctx.stop()
                    }
                }),
            );
        }
        if self.busy {
            return;
        }
        self.busy = true;
        ctx.spawn(
            self.workers
                .send(Work {
                    stop: self.stop.clone(),
                    runtime: self.runtime.clone(),
                    lease,
                    recovering: !self.state.is_recovered(),
                })
                .into_actor(self)
                .map(|result, actor, ctx| {
                    actor.busy = false;
                    match result {
                        Ok(Ok(step::Progress::Changed)) => {
                            actor.state.mark_recovered();
                            ctx.notify(Pulse)
                        }
                        Ok(Ok(step::Progress::Blocked)) => {}
                        Ok(Ok(step::Progress::Terminal)) => ctx.stop(),
                        Ok(Err(RuntimeError::Storage(StoreError::Fenced))) => ctx.stop(),
                        Ok(Err(RuntimeError::Storage(
                            StoreError::Conflict | StoreError::Terminal,
                        ))) => ctx.notify(Pulse),
                        _ => ctx.stop(),
                    }
                }),
        );
    }
}
impl Handler<Stop> for Coordinator {
    type Result = ();
    fn handle(&mut self, _: Stop, ctx: &mut Self::Context) {
        self.stop.cancel();
        ctx.stop();
    }
}
