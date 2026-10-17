//! Lazy immutable artifact access on calculation workers only.
use super::model::*;
use crate::{
    EvaluationContext, EvaluationError, EvaluationMeter, EvaluationOutcome, EvaluationValue,
    HostContext, Runtime,
    failure::{FailureCode, RuntimeFailure},
    identity::{ArtifactId, RunId, ScopeInstanceId},
    influence::{
        InfluenceContext, InfluenceEnvironment, InfluencedError, InfluencedEvaluation,
        ReferenceInfluence,
    },
    records::SecurityLabels,
    store::StoredRecord,
};
use htlk_analyzer::{ExpressionSite, ScopeUse};
use htlk_executable::{Identifier, PathStep, PortTable, ValueReference, cbor::Value};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

#[derive(Clone)]
pub(super) enum Slot {
    Artifact(ArtifactId),
    Value(Arc<Value>),
    Absent,
    Pending,
    Unavailable,
}
pub(super) struct Frame {
    pub runtime: Arc<Runtime>,
    pub context: HostContext,
    pub run: RunId,
    pub scope: ScopeInstanceId,
    pub values: BTreeMap<ValueReference, Slot>,
    pub outcomes: BTreeMap<Identifier, EvaluationOutcome>,
    pub influence: InfluenceEnvironment,
    cache: RefCell<BTreeMap<ArtifactId, Arc<Value>>>,
    fault: RefCell<Option<RuntimeFailure>>,
}
impl Frame {
    pub fn new(
        runtime: Arc<Runtime>,
        context: HostContext,
        run: RunId,
        scope: ScopeInstanceId,
        base: SecurityLabels,
    ) -> Self {
        Self {
            runtime,
            context,
            run,
            scope,
            values: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            influence: InfluenceEnvironment {
                base,
                ..Default::default()
            },
            cache: RefCell::new(BTreeMap::new()),
            fault: RefCell::new(None),
        }
    }
    pub fn bind_manifest(
        &mut self,
        manifest: &Value,
        ports: &PortTable,
        outputs: bool,
    ) -> Result<(), crate::RuntimeError> {
        for (name, _) in ports.iter() {
            let slot = match map(manifest)?.get(name.as_str()) {
                Some(value) => entry(value)?.map_or(Slot::Absent, Slot::Artifact),
                None => return Err(err()),
            };
            let reference = if outputs {
                ValueReference::ScopeOutput(name.clone())
            } else {
                ValueReference::Input(name.clone())
            };
            self.bind(reference, slot, SecurityLabels::default());
        }
        Ok(())
    }
    pub fn bind(&mut self, reference: ValueReference, slot: Slot, labels: SecurityLabels) {
        self.values.insert(reference.clone(), slot);
        self.influence.references.insert(
            reference,
            ReferenceInfluence {
                labels,
                cause: None,
            },
        );
    }
    pub fn siblings(
        &mut self,
        nodes: &BTreeMap<Identifier, StoredRecord>,
        definitions: &[htlk_executable::Node],
    ) -> Result<(), crate::RuntimeError> {
        for definition in definitions {
            let Some(record) = nodes.get(definition.id()) else {
                continue;
            };
            let status = string(&record.payload, "status")?;
            let influence = labels(&record.payload, &self.runtime.config.limits)?;
            let outcome = match status.as_str() {
                "succeeded" => EvaluationOutcome::Succeeded,
                "skipped" => EvaluationOutcome::Skipped,
                "cancelled" => EvaluationOutcome::Cancelled,
                "failed" => {
                    let f = failure(&record.payload, &self.runtime.config.limits)?;
                    EvaluationOutcome::Failed {
                        code: f.code().to_string(),
                        message: f.message().into(),
                    }
                }
                _ => EvaluationOutcome::Pending,
            };
            self.outcomes.insert(definition.id().clone(), outcome);
            self.influence.outcomes.insert(
                definition.id().clone(),
                ReferenceInfluence {
                    labels: influence.clone(),
                    cause: None,
                },
            );
            for (port, _) in definition.fields().outputs.iter() {
                let slot = match status.as_str() {
                    "succeeded" => entry(
                        map(field(&record.payload, "outputs")?)?
                            .get(port.as_str())
                            .ok_or_else(err)?,
                    )?
                    .map_or(Slot::Absent, Slot::Artifact),
                    "skipped" => Slot::Absent,
                    "failed" | "cancelled" => Slot::Unavailable,
                    _ => Slot::Pending,
                };
                self.bind(
                    ValueReference::Output {
                        node: definition.id().clone(),
                        port: port.clone(),
                    },
                    slot,
                    influence.clone(),
                );
            }
        }
        Ok(())
    }
    pub fn evaluate(
        &self,
        executable: &crate::VerifiedExecutable<'_>,
        site: &ScopeUse,
        expression: &ExpressionSite,
    ) -> Result<InfluencedEvaluation, crate::RuntimeError> {
        let authorize = |labels: &SecurityLabels| {
            self.runtime.policy.authorize(
                &self.context,
                crate::Authorization::ReadBoundary {
                    run_id: &self.run,
                    scope_instance_id: self.scope,
                    labels,
                },
            )
        };
        let guarded = InfluenceContext::new(
            self,
            &self.influence,
            &authorize,
            &self.runtime.config.limits,
        )?;
        let mut result = guarded.evaluate(executable, site, expression)?;
        if let Some(fault) = self.fault.borrow().clone() {
            result.result = Err(InfluencedError::Failure(fault));
        }
        Ok(result)
    }
}
impl EvaluationContext for Frame {
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        meter.visit(1)?;
        if !path.is_empty() {
            return Err(EvaluationError::InvalidProjection);
        }
        match self
            .values
            .get(reference)
            .ok_or(EvaluationError::UnknownReference)?
        {
            Slot::Absent => Ok(EvaluationValue::Absent),
            Slot::Pending => Ok(EvaluationValue::Pending),
            Slot::Unavailable => Err(EvaluationError::UnavailableSource),
            Slot::Value(value) => Ok(EvaluationValue::Present(meter.copy_value(value)?)),
            Slot::Artifact(id) => {
                let cached = self.cache.borrow().get(id).cloned();
                let value = if let Some(value) = cached {
                    value
                } else {
                    match block_on(self.runtime.read_artifact(&self.context, *id)) {
                        Ok(value) => {
                            let value = Arc::new(value.value);
                            self.cache.borrow_mut().insert(*id, value.clone());
                            value
                        }
                        Err(error) => {
                            let code = if error.code() == FailureCode::Input {
                                FailureCode::Digest
                            } else {
                                error.code()
                            };
                            self.fault.replace(Some(RuntimeFailure::new(code)));
                            return Err(EvaluationError::UnavailableSource);
                        }
                    }
                };
                Ok(EvaluationValue::Present(meter.copy_value(&value)?))
            }
        }
    }
    fn outcome(&self, node: &Identifier) -> Option<&EvaluationOutcome> {
        self.outcomes.get(node)
    }
    fn template(
        &self,
        _: &htlk_executable::digest::Digest,
    ) -> Option<&htlk_executable::PromptTemplate> {
        None
    }
}
// SQLite's worker/oneshot futures are safe to poll here; never use this bridge for
// network/timer work. It runs only inside an execution calculation worker.
pub(super) fn block_on<F: Future>(future: F) -> F::Output {
    struct Thread(std::thread::Thread);
    impl Wake for Thread {
        fn wake(self: Arc<Self>) {
            self.0.unpark()
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark()
        }
    }
    let waker = Waker::from(Arc::new(Thread(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}
