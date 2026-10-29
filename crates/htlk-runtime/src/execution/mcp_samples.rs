//! Explicit authorized sample selection from genuine recorded MCP invocations.

use super::{mcp_evidence::binding_for_view, model::*};
use crate::{
    HostContext, Runtime, RuntimeError,
    artifact::ArtifactProducer,
    failure::FailureCode,
    identity::{InvocationId, RunId},
    mcp::McpDelivery,
    records::SecurityLabels,
};
use htlk_analyzer::{
    InferenceContext, InferencePolicy, InferenceRevision, InferenceSample, InferenceSampleKind,
    SampleObservation,
};
use htlk_executable::{
    BuiltinType, JsonDocument, McpToolResult, ValueTypeKind,
    cbor::{Limits, Value},
};

/// An operator-selected invocation, not permission to call or replay its tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpInferenceSelection {
    /// Run retaining the source observation.
    pub run_id: RunId,
    /// Actual MCP invocation; generic graph JSON cannot impersonate its producer.
    pub invocation_id: InvocationId,
}
/// Proposed immutable revision retaining conservative source protection/provenance.
/// Adoption, publication and any later execution remain explicit host operations.
#[derive(Clone)]
pub struct ProtectedInferenceRevision {
    revision: InferenceRevision,
    labels: SecurityLabels,
    sources: Vec<McpInferenceSelection>,
    effect_outcome_uncertain: bool,
}
impl std::fmt::Debug for ProtectedInferenceRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedInferenceRevision")
            .field("schema", &self.revision.schema().digest())
            .field("evidence", &self.revision.evidence().digest())
            .field("source_count", &self.sources.len())
            .field("effect_outcome_uncertain", &self.effect_outcome_uncertain)
            .finish_non_exhaustive()
    }
}
impl ProtectedInferenceRevision {
    /// Proposed schema/evidence; this does not mutate any executable.
    pub fn revision(&self) -> &InferenceRevision {
        &self.revision
    }
    /// Preserve these labels when using existing artifact/publication mechanisms.
    pub fn labels(&self) -> &SecurityLabels {
        &self.labels
    }
    /// Explicit original selections, retaining run/invocation provenance.
    pub fn sources(&self) -> &[McpInferenceSelection] {
        &self.sources
    }
    /// External effect uncertainty is separate from a known complete JSON observation.
    pub fn effect_outcome_uncertain(&self) -> bool {
        self.effect_outcome_uncertain
    }
}

impl Runtime {
    /// Infers from explicitly selected genuine recorded invocations, without dispatch.
    /// Ordinary sampling requires accepted complete selected JSON; explicit error
    /// sampling requires isError:true evidence. Missing/ambiguous/incomplete data
    /// is rejected rather than invented or silently included. All current source
    /// inspection/artifact grants apply and output protection is conservatively unioned.
    ///
    /// # Errors
    /// Returns denial, forged/wrong producers, mixed capability contexts, missing
    /// selected data, unavailable pinned processing or inference/representation limits.
    pub async fn infer_mcp_results(
        &self,
        context: &HostContext,
        selections: &[McpInferenceSelection],
        policy: &InferencePolicy,
    ) -> Result<ProtectedInferenceRevision, RuntimeError> {
        policy.document(&self.config.limits)?;
        if selections.len() > policy.limits.samples {
            return Err(htlk_analyzer::InferenceError::Limit {
                resource: "samples",
                selected_samples: 0,
            }
            .into());
        }
        let selection_bytes = selections
            .iter()
            .try_fold(0usize, |sum, selection| {
                sum.checked_add(selection.run_id.as_str().len())
                    .and_then(|sum| sum.checked_add(71))
            })
            .filter(|bytes| *bytes <= policy.limits.sample_bytes);
        if selection_bytes.is_none() {
            return Err(htlk_analyzer::InferenceError::Limit {
                resource: "sample_bytes",
                selected_samples: 0,
            }
            .into());
        }
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(selections.len())
            .map_err(|_| RuntimeError::failure(FailureCode::Budget))?;
        let mut protection = SecurityLabels::default();
        let mut uncertain_effect = false;
        let mut bytes = 0usize;
        let mut expected = None;
        for selection in selections {
            let view = self
                .inspect_node(context, &selection.run_id, selection.invocation_id)
                .await?;
            let (_, document) = self.source_document(view.origin_executable).await?;
            let binding = binding_for_view(&document, &view, &self.config.limits)?;
            let selected_context = InferenceContext {
                server: binding
                    .server()
                    .digest(&self.config.limits)
                    .map_err(|_| err())?,
                descriptor: binding.descriptor(),
                extraction_policy: binding
                    .result_policy()
                    .ok_or_else(err)?
                    .extraction
                    .digest(&self.config.limits)
                    .map_err(|error| {
                        RuntimeError::from(crate::ToolNormalizationError::Model(error))
                    })?,
            };
            if expected.is_some_and(|expected| expected != selected_context) {
                return Err(htlk_analyzer::InferenceError::Context.into());
            }
            expected = Some(selected_context);
            protection = protection.union(&view.labels, &self.config.limits)?;
            uncertain_effect |= view.attempts.iter().any(|attempt| {
                attempt.dispatch_state == "possibly_sent"
                    || attempt.delivery == Some(McpDelivery::PossiblySent)
                    || (attempt.failure_code.is_some()
                        && attempt.delivery == Some(McpDelivery::ResponseReceived))
            });
            let (reference, data, successful) = match policy.sample_kind {
                InferenceSampleKind::Success => {
                    if view.status != "succeeded" {
                        return Err(RuntimeError::failure(FailureCode::Input));
                    }
                    let outputs = view.outputs.as_ref().ok_or_else(err)?;
                    let id = string(field(outputs, "value")?, "artifact_id")?
                        .parse()
                        .map_err(|_| err())?;
                    let artifact = self.read_artifact(context, id).await?;
                    if !matches!(artifact.metadata.producer(), ArtifactProducer::NodeOutput { invocation_id, port }
                        if *invocation_id == selection.invocation_id && port.as_str() == "value")
                        || artifact.metadata.validation_profile() != view.origin_executable
                        || !matches!(
                            artifact.metadata.value_type().kind(),
                            ValueTypeKind::Builtin(BuiltinType::McpToolResult)
                        )
                    {
                        return Err(RuntimeError::failure(FailureCode::Digest));
                    }
                    protection = protection
                        .union(artifact.metadata.security_labels(), &self.config.limits)?;
                    let envelope = McpToolResult::from_value(&artifact.value, &self.config.limits)
                        .map_err(crate::ToolNormalizationError::Model)?;
                    let data = selected_data(envelope.value())?;
                    let source = format!("artifact:{id}");
                    let value = bounded_sample(
                        data,
                        &source,
                        &mut bytes,
                        policy,
                        &self.config.limits,
                        samples.len(),
                    )?;
                    (source, value, true)
                }
                InferenceSampleKind::ToolError => {
                    let attempt = view
                        .attempts
                        .last()
                        .ok_or_else(|| RuntimeError::failure(FailureCode::Input))?;
                    let evidence = self
                        .inspect_mcp_evidence(
                            context,
                            &selection.run_id,
                            selection.invocation_id,
                            attempt.attempt_id,
                            self.config.limits.max_document_bytes,
                        )
                        .await?;
                    protection = protection.union(&evidence.labels, &self.config.limits)?;
                    uncertain_effect |= evidence.effect_outcome_uncertain;
                    let normalized = evidence.normalized.as_ref().ok_or_else(|| {
                        RuntimeError::failure(
                            evidence
                                .reconstruction_failure
                                .unwrap_or(FailureCode::OutputMissing),
                        )
                    })?;
                    if normalized.failure() != Some(crate::ToolResultFailure::ToolError) {
                        return Err(RuntimeError::failure(FailureCode::Input));
                    }
                    let data = selected_data(normalized.envelope().value())?;
                    let source = format!("response:{}", attempt.attempt_id);
                    let value = bounded_sample(
                        data,
                        &source,
                        &mut bytes,
                        policy,
                        &self.config.limits,
                        samples.len(),
                    )?;
                    (source, value, false)
                }
            };
            samples.push(InferenceSample {
                reference,
                value: data,
                context: selected_context,
                observation: SampleObservation {
                    successful,
                    complete: true,
                    truncated: false,
                    uncertain: false,
                },
                sanitized: false,
                excluded_fields: vec![],
            });
        }
        let selected_context = expected.ok_or(htlk_analyzer::InferenceError::NoSamples)?;
        let revision =
            htlk_analyzer::infer_schema(&samples, selected_context, policy, &self.config.limits)?;
        Ok(ProtectedInferenceRevision {
            revision,
            labels: protection,
            sources: selections.to_vec(),
            effect_outcome_uncertain: uncertain_effect,
        })
    }
}
fn selected_data(envelope: &Value) -> Result<&Value, RuntimeError> {
    let extraction = field(envelope, "extraction")?;
    if string(extraction, "status")? == "exhausted" {
        return Err(RuntimeError::failure(FailureCode::Budget));
    }
    if string(extraction, "status")? != "selected" {
        return Err(RuntimeError::failure(FailureCode::OutputMissing));
    }
    map(envelope)?
        .get("data")
        .ok_or_else(|| RuntimeError::failure(FailureCode::OutputMissing))
}
fn bounded_sample(
    value: &Value,
    reference: &str,
    used: &mut usize,
    policy: &InferencePolicy,
    limits: &Limits,
    selected: usize,
) -> Result<JsonDocument, RuntimeError> {
    let remaining = policy
        .limits
        .sample_bytes
        .saturating_sub(*used)
        .saturating_sub(reference.len());
    let effective = Limits {
        max_document_bytes: remaining.min(limits.max_document_bytes),
        max_depth: limits.max_depth.min(policy.limits.depth),
    };
    let document = JsonDocument::from_value(value, &effective).map_err(|error| match error {
        htlk_executable::JsonError::LimitExceeded {
            limit: htlk_executable::cbor::LimitKind::Depth,
            ..
        } => htlk_analyzer::InferenceError::Limit {
            resource: "depth",
            selected_samples: selected,
        },
        htlk_executable::JsonError::LimitExceeded { .. } => htlk_analyzer::InferenceError::Limit {
            resource: "sample_bytes",
            selected_samples: selected,
        },
        error => htlk_analyzer::InferenceError::Json(error),
    })?;
    *used = used
        .checked_add(document.as_bytes().len())
        .and_then(|used| used.checked_add(reference.len()))
        .filter(|used| *used <= policy.limits.sample_bytes)
        .ok_or(htlk_analyzer::InferenceError::Limit {
            resource: "sample_bytes",
            selected_samples: selected,
        })?;
    Ok(document)
}
