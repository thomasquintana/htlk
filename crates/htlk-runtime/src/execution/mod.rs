//! Actix supervision of fenced durable graph execution.
mod approval;
mod composite;
mod control;
mod extension;
pub use extension::{CloseWait, ExtensionReceipt, ExtensionRequest};
mod host;
mod inspection;
mod mcp;
mod recovery;
pub use inspection::{GraphView, NodeView, RunView, WaitView};
mod waits;
pub use host::{CancelReceipt, ReplyReceipt};
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
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// Finite worker, mailbox, ownership and polling settings.
#[derive(Clone, Debug)]
pub struct ExecutionConfig {
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
    /// Returns missing Actix context or invalid finite configuration.
    pub fn launch(runtime: Arc<Runtime>, config: ExecutionConfig) -> Result<Self, RuntimeError> {
        if System::try_current().is_none()
            || config.workers == 0
            || config.workers > 64
            || config.max_runs == 0
            || config.max_runs > 4096
            || config.lease_ms < 100
            || config.poll_interval.is_zero()
            || config.poll_interval > Duration::from_secs(10)
        {
            return Err(RuntimeError::failure(FailureCode::Profile));
        }
        let workers = SyncArbiter::start(config.workers, || Worker);
        let address = Supervisor {
            runtime: runtime.clone(),
            config,
            workers,
            active: BTreeMap::new(),
            polling: false,
            cursor: None,
        }
        .start();
        *runtime
            .execution
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))? =
            Some(address.downgrade());
        Ok(Self { address })
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
        System::new().block_on(recovery::drive(msg.runtime, msg.lease, msg.recovering))
    }
}
pub(crate) struct Supervisor {
    runtime: Arc<Runtime>,
    config: ExecutionConfig,
    workers: Addr<Worker>,
    active: BTreeMap<RunId, Addr<Coordinator>>,
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
}
struct Poll;
impl Message for Poll {
    type Result = ();
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
        let room = self
            .config
            .max_runs
            .saturating_sub(self.active.len())
            .max(1)
            .min(runtime.store.config().max_page_size);
        ctx.spawn(
            async move {
                let runs = runtime.store.running_runs(cursor.as_ref(), room).await?;
                let mut ready = Vec::new();
                for run in &runs {
                    let owner = runtime.store.reserve_run_id().await?;
                    let now = runtime
                        .store
                        .coordinator_time(runtime.clock.now_ms()?)
                        .await?;
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
                            runtime: actor.runtime.clone(),
                            config: actor.config.clone(),
                            run: run.clone(),
                            owner,
                            lease: None,
                            busy: false,
                            renewing: false,
                            workers: actor.workers.clone(),
                            parent: ctx.address().downgrade(),
                            initial_time: now,
                            renew_after: now + actor.config.lease_ms / 3,
                            checking: false,
                            recovered: false,
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
        for (_, actor) in std::mem::take(&mut self.active) {
            actor.do_send(Stop)
        }
        ctx.stop();
    }
}
struct Coordinator {
    recovered: bool,
    runtime: Arc<Runtime>,
    config: ExecutionConfig,
    run: RunId,
    owner: String,
    lease: Option<RunLease>,
    busy: bool,
    renewing: bool,
    workers: Addr<Worker>,
    parent: WeakAddr<Supervisor>,
    initial_time: i64,
    renew_after: i64,
    checking: bool,
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
                        actor.lease = Some(lease);
                        ctx.notify(Pulse)
                    }
                    Err(_) => ctx.stop(),
                }),
        );
        ctx.run_interval(self.config.poll_interval, |_, ctx| ctx.notify(Pulse));
    }
    fn stopped(&mut self, _: &mut Self::Context) {
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
        let Some(lease) = self.lease.clone() else {
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
                    actor.renew_after = actor.runtime.clock.now_ms().unwrap_or(actor.initial_time)
                        + actor.config.lease_ms / 3;
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
                    runtime: self.runtime.clone(),
                    lease,
                    recovering: !self.recovered,
                })
                .into_actor(self)
                .map(|result, actor, ctx| {
                    actor.busy = false;
                    match result {
                        Ok(Ok(step::Progress::Changed)) => {
                            actor.recovered = true;
                            ctx.notify(Pulse)
                        }
                        Ok(Ok(step::Progress::Blocked)) => {}
                        Ok(Ok(step::Progress::Terminal)) => ctx.stop(),
                        Ok(Err(RuntimeError::Storage(
                            StoreError::Fenced | StoreError::Terminal,
                        ))) => ctx.stop(),
                        Ok(Err(RuntimeError::Storage(StoreError::Conflict))) => ctx.notify(Pulse),
                        _ => ctx.stop(),
                    }
                }),
        );
    }
}
impl Handler<Stop> for Coordinator {
    type Result = ();
    fn handle(&mut self, _: Stop, ctx: &mut Self::Context) {
        ctx.stop();
    }
}
