//! Lazy immutable artifact access on calculation workers only.
use super::{artifacts::WorkerArtifactResolver, checkpoint::InvocationCheckpoint, model::*};
use crate::{
    EvaluationContext, EvaluationError, EvaluationMeter, EvaluationOutcome, EvaluationValue,
    HostContext, RuntimePolicy,
    artifact_access::ArtifactReader,
    failure::RuntimeFailure,
    identity::{ArtifactId, RunId, ScopeInstanceId},
    influence::{
        InfluenceContext, InfluenceEnvironment, InfluencedError, InfluencedEvaluation,
        ReferenceInfluence,
    },
    records::{SecurityLabels, SourceState},
    store::StoredRecord,
};
use htlk_analyzer::{ExpressionSite, ScopeUse};
use htlk_executable::{
    Identifier, PathStep, PortTable, ValueReference,
    cbor::{Limits, Value},
};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub(super) enum Slot {
    Artifact(ArtifactId),
    Value(Arc<Value>),
    Absent,
    Pending,
    Unavailable,
}
pub(super) struct Frame {
    policy: Arc<dyn RuntimePolicy>,
    limits: Limits,
    context: HostContext,
    run: RunId,
    scope: ScopeInstanceId,
    values: BTreeMap<ValueReference, Slot>,
    outcomes: BTreeMap<Identifier, EvaluationOutcome>,
    pub influence: InfluenceEnvironment,
    artifacts: WorkerArtifactResolver,
    fault: RefCell<Option<RuntimeFailure>>,
}
impl Frame {
    pub fn new(
        reader: ArtifactReader,
        policy: Arc<dyn RuntimePolicy>,
        limits: Limits,
        context: HostContext,
        run: RunId,
        scope: ScopeInstanceId,
        base: SecurityLabels,
    ) -> Self {
        Self {
            artifacts: WorkerArtifactResolver::new(reader, context.clone()),
            policy,
            limits,
            context,
            run,
            scope,
            values: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            influence: InfluenceEnvironment {
                base,
                ..Default::default()
            },
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
        scope_definition: htlk_executable::digest::Digest,
    ) -> Result<(), crate::RuntimeError> {
        for (index, definition) in definitions.iter().enumerate() {
            let Some(record) = nodes.get(definition.id()) else {
                continue;
            };
            let checkpoint = InvocationCheckpoint::parse(record, &self.limits)?;
            let influence = checkpoint.influence().clone();
            let cause = checkpoint.cause(scope_definition, index, &self.limits)?;
            self.outcomes
                .insert(definition.id().clone(), checkpoint.outcome());
            self.influence.outcomes.insert(
                definition.id().clone(),
                ReferenceInfluence {
                    labels: influence.clone(),
                    cause: cause.clone(),
                },
            );
            for (port, _) in definition.fields().outputs.iter() {
                let slot = match checkpoint.output(port)? {
                    SourceState::Value(id) => Slot::Artifact(id),
                    SourceState::Absent => Slot::Absent,
                    SourceState::Unavailable(_) => Slot::Unavailable,
                    SourceState::Pending => Slot::Pending,
                };
                self.bind(
                    ValueReference::Output {
                        node: definition.id().clone(),
                        port: port.clone(),
                    },
                    slot,
                    influence.clone(),
                );
                if let Some(reference) =
                    self.influence.references.get_mut(&ValueReference::Output {
                        node: definition.id().clone(),
                        port: port.clone(),
                    })
                {
                    reference.cause = cause.clone();
                }
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
            self.policy.authorize(
                &self.context,
                crate::Authorization::ReadBoundary {
                    run_id: &self.run,
                    scope_instance_id: self.scope,
                    labels,
                },
            )
        };
        let guarded = InfluenceContext::new(self, &self.influence, &authorize, &self.limits)?;
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
                let value = match self.artifacts.read_on_worker(*id) {
                    Ok(value) => value,
                    Err(failure) => {
                        self.fault.replace(Some(failure));
                        return Err(EvaluationError::UnavailableSource);
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
