//! Authorized value acquisition followed by pure selected-boundary validation.
use super::model::err;
use crate::{
    HostContext, Runtime, RuntimeError, VerifiedExecutable,
    artifact::{ArtifactMetadata, AuthorizedValue},
    binding::BindingDecision,
    failure::{FailureBoundary, FailureCode, RuntimeFailure},
    records::{InputManifest, ManifestEntry, SecurityLabels},
};
use htlk_executable::{Identifier, PortTable, ValueType, cbor::Limits};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) enum Boundary {
    Input,
    Output,
}
impl Boundary {
    fn failure_boundary(self) -> FailureBoundary {
        match self {
            Self::Input => FailureBoundary::Input,
            Self::Output => FailureBoundary::Output,
        }
    }
}
pub(super) struct ValidationFailure {
    pub primary: RuntimeFailure,
    pub diagnostics: Vec<(Identifier, RuntimeFailure)>,
}
pub(super) type SelectionValidation = Result<InputManifest, ValidationFailure>;
struct ValidatedArtifact {
    metadata: ArtifactMetadata,
    validation: Result<(), RuntimeFailure>,
}

pub(super) async fn validate_selected(
    runtime: &Runtime,
    executable: &VerifiedExecutable<'_>,
    context: &HostContext,
    ports: &PortTable,
    selected: &BTreeMap<Identifier, BindingDecision>,
    influence: &SecurityLabels,
    boundary: Boundary,
) -> Result<SelectionValidation, RuntimeError> {
    let mut values = BTreeMap::new();
    for (name, port) in ports.iter() {
        if let BindingDecision::Value { artifact_id, .. } = selected.get(name).ok_or_else(err)? {
            let value = runtime
                .read_artifact(context, *artifact_id)
                .await
                .map(|value| {
                    validate_artifact(
                        executable,
                        value,
                        port.value_type(),
                        boundary,
                        &runtime.config.limits,
                    )
                })
                .map_err(|error| RuntimeFailure::new(error.code()));
            values.insert(name.clone(), value);
        }
    }
    freeze_selected(
        ports,
        selected,
        influence,
        boundary,
        &values,
        &runtime.config.limits,
    )
}

// Validate each authorized value immediately and retain metadata only. Raw
// artifact values do not accumulate across a large selected-input inventory.
fn validate_artifact(
    executable: &VerifiedExecutable<'_>,
    value: AuthorizedValue,
    ty: &ValueType,
    boundary: Boundary,
    limits: &Limits,
) -> ValidatedArtifact {
    let validation = crate::validate_typed_value(
        &value.value,
        ty,
        Some(executable.schemas()),
        limits,
        &executable.policy().fields().evaluator_limits,
    )
    .map(|_| ())
    .map_err(|error| {
        RuntimeFailure::new(FailureCode::for_evaluation(
            &error,
            boundary.failure_boundary(),
        ))
    });
    ValidatedArtifact {
        metadata: value.metadata,
        validation,
    }
}

// All reads and authorization have completed. This function has no storage,
// clock, policy callback or transport capability and returns only a proposal.
fn freeze_selected(
    ports: &PortTable,
    selected: &BTreeMap<Identifier, BindingDecision>,
    influence: &SecurityLabels,
    boundary: Boundary,
    values: &BTreeMap<Identifier, Result<ValidatedArtifact, RuntimeFailure>>,
    limits: &Limits,
) -> Result<SelectionValidation, RuntimeError> {
    let mut entries = Vec::new();
    let mut first = None;
    let mut extra = Vec::new();
    let mut diagnostics = Vec::new();
    for (name, _) in ports.iter() {
        match selected.get(name).ok_or_else(err)? {
            BindingDecision::Value {
                artifact_id,
                influence: edge_labels,
                ..
            } => match values.get(name).ok_or_else(err)? {
                Ok(value) => {
                    extra.push(value.metadata.security_labels());
                    if let Err(failure) = &value.validation {
                        first.get_or_insert_with(|| failure.clone());
                        diagnostics.push((name.clone(), failure.clone()));
                    }
                    entries.push((
                        name.clone(),
                        ManifestEntry::Value {
                            artifact_id: *artifact_id,
                            content_digest: value.metadata.content_digest(),
                            security_labels: value.metadata.security_labels().clone(),
                        },
                    ));
                    extra.push(edge_labels);
                }
                Err(failure) => {
                    first.get_or_insert_with(|| failure.clone());
                    diagnostics.push((name.clone(), failure.clone()));
                }
            },
            BindingDecision::Absent { .. } => entries.push((name.clone(), ManifestEntry::Absent)),
            BindingDecision::Failed { failures, .. } => {
                let failure = &failures.first().ok_or_else(err)?.error;
                first.get_or_insert_with(|| failure.clone());
            }
            BindingDecision::Pending { .. } => return Err(err()),
        }
    }
    if let Some(primary) = first {
        return Ok(Err(ValidationFailure {
            primary,
            diagnostics,
        }));
    }
    if matches!(boundary, Boundary::Output)
        && entries.iter().any(|(name, entry)| {
            ports.get(name.as_str()).is_some_and(|port| port.required())
                && matches!(entry, ManifestEntry::Absent)
        })
    {
        return Ok(Err(ValidationFailure {
            primary: RuntimeFailure::new(FailureCode::OutputMissing),
            diagnostics,
        }));
    }
    let labels = SecurityLabels::combine(std::iter::once(influence).chain(extra), limits)?;
    Ok(Ok(InputManifest::freeze(ports, entries, labels, limits)?))
}
