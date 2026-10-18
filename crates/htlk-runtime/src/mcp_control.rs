//! Local cooperative hints are bounded and subordinate to durable stop fences.
use crate::{
    RuntimeError,
    failure::FailureCode,
    identity::{AttemptId, InvocationId, RunId},
    mcp::McpCancellation,
    store::RunLease,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
};
struct ActiveAttempt {
    run: RunId,
    invocation: InvocationId,
    token: Weak<McpCancellation>,
}
#[derive(Default)]
pub(crate) struct McpControl {
    attempts: Mutex<BTreeMap<(AttemptId, i64), ActiveAttempt>>,
    executions: Mutex<BTreeMap<(RunId, String), Weak<McpCancellation>>>,
}
pub(crate) struct AttemptCancellation<'a> {
    control: &'a McpControl,
    key: (AttemptId, i64),
    pub token: Arc<McpCancellation>,
}
impl Drop for AttemptCancellation<'_> {
    fn drop(&mut self) {
        if let Ok(mut attempts) = self.control.attempts.lock() {
            attempts.remove(&self.key);
        }
    }
}
impl McpControl {
    pub fn execution(
        &self,
        lease: &RunLease,
        token: &Arc<McpCancellation>,
    ) -> Result<(), RuntimeError> {
        let mut executions = self
            .executions
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?;
        executions.retain(|_, v| v.strong_count() > 0);
        executions.insert(
            (lease.run_id().clone(), lease.owner().into()),
            Arc::downgrade(token),
        );
        Ok(())
    }
    pub fn attempt(
        &self,
        lease: &RunLease,
        invocation: InvocationId,
        attempt: AttemptId,
    ) -> Result<AttemptCancellation<'_>, RuntimeError> {
        let parent = self
            .executions
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?
            .get(&(lease.run_id().clone(), lease.owner().into()))
            .and_then(Weak::upgrade);
        let token = Arc::new(
            parent
                .as_ref()
                .map_or_else(McpCancellation::new, |p| McpCancellation::linked(p)),
        );
        let key = (attempt, lease.fence());
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?;
        if attempts.contains_key(&key) {
            return Err(crate::store::StoreError::Conflict.into());
        }
        attempts.insert(
            key,
            ActiveAttempt {
                run: lease.run_id().clone(),
                invocation,
                token: Arc::downgrade(&token),
            },
        );
        Ok(AttemptCancellation {
            control: self,
            key,
            token,
        })
    }
    pub fn cancel(&self, run: &RunId, invocations: Option<&BTreeSet<String>>) {
        if let Ok(attempts) = self.attempts.lock() {
            for active in attempts.values() {
                if &active.run == run
                    && invocations.is_none_or(|ids| ids.contains(&active.invocation.to_string()))
                    && let Some(token) = active.token.upgrade()
                {
                    token.cancel()
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::ScopeInstanceId,
        store::{RunSeed, SqliteStore, StoreConfig},
    };
    use htlk_executable::{cbor::Limits, digest::Digest};
    #[test]
    fn shutdown_before_tracking_is_sticky_and_retained_adapter_tokens_do_not_leak_registrations() {
        actix::System::new().block_on(async {
            let limits = Limits::default();
            let store = SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap();
            let run = RunId::new("stopped-execution".into(), &limits).unwrap();
            let scope = ScopeInstanceId::root(&run, &limits).unwrap();
            store
                .create_run(RunSeed {
                    run_id: run.clone(),
                    fingerprint: Digest::from_bytes([1; 32]),
                    root_scope: scope,
                    actor: "host".into(),
                    causation: "test".into(),
                    timestamp_ms: 0,
                })
                .await
                .unwrap();
            let lease = store.acquire(&run, "owner".into(), 0, 100).await.unwrap();
            let parent = McpCancellation::new();
            parent.cancel();
            let coordinator = Arc::new(McpCancellation::linked(&parent));
            let control = McpControl::default();
            control.execution(&lease, &coordinator).unwrap();
            let invocation = InvocationId::new(scope, &"lookup".parse().unwrap(), &limits).unwrap();
            let attempt = AttemptId::new(invocation, 1, &limits).unwrap();
            let tracked = control.attempt(&lease, invocation, attempt).unwrap();
            assert!(tracked.token.is_cancelled());
            tracked.token.cancelled().await;
            let retained = tracked.token.clone();
            drop(tracked);
            assert!(retained.is_cancelled());
            assert!(control.attempts.lock().unwrap().is_empty());
        })
    }
}
