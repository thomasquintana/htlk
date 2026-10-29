//! Protected immutable normalization decisions and explicit bounded evidence reads.

use super::model::*;
use crate::{
    Authorization, HostContext, NormalizedToolResult, Runtime, RuntimeError,
    ToolNormalizationError,
    failure::FailureCode,
    identity::{AttemptId, InvocationId, RunId},
    mcp::McpDelivery,
    records::SecurityLabels,
    store::{RecordFamily, StoredRecord},
};
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, Operation,
    cbor::{self, Limits, Value},
    digest::{Digest, hash_bytes},
};

/// Authorized evidence, not a successful output artifact or permission to retry.
#[derive(Clone)]
pub struct McpEvidenceView {
    /// Original run, unchanged through extension.
    pub run_id: RunId,
    /// Original tool invocation.
    pub invocation_id: InvocationId,
    /// Exact one-based attempt selected by the caller.
    pub attempt_id: AttemptId,
    /// Unioned trusted evidence/outcome protection.
    pub labels: SecurityLabels,
    /// Frozen transport delivery evidence.
    pub delivery: Option<McpDelivery>,
    /// Whether a matching response was received, independent of data acceptance.
    pub response_received: bool,
    /// Conservative external-effect uncertainty, never inferred away from parsing.
    pub effect_outcome_uncertain: bool,
    /// Immutable persisted normalization metadata, if it fits the requested bound.
    pub recorded_normalization: Option<Value>,
    /// Exact original byte identity, including when only a prefix is returned.
    pub raw_digest: Option<Digest>,
    /// Authorized original result bytes or a clearly marked bounded prefix.
    pub raw_response: Option<Vec<u8>>,
    /// Original bytes were not returned completely.
    pub raw_truncated: bool,
    /// Reconstructed pinned evidence, including failed-tool JSON candidates.
    pub normalized: Option<NormalizedToolResult>,
    /// Complete reconstructed evidence did not fit the remaining response budget.
    pub normalized_truncated: bool,
    /// The original recorded attempt/normalization failure, if any.
    pub failure_code: Option<FailureCode>,
    /// A separate local reconstruction failure; cannot change the recorded outcome.
    pub reconstruction_failure: Option<FailureCode>,
}
impl std::fmt::Debug for McpEvidenceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEvidenceView")
            .field("attempt_id", &self.attempt_id)
            .field("raw_bytes", &self.raw_response.as_ref().map(Vec::len))
            .field("raw_truncated", &self.raw_truncated)
            .field("failure_code", &self.failure_code)
            .field("reconstruction_failure", &self.reconstruction_failure)
            .finish_non_exhaustive()
    }
}

pub(super) fn record_id(attempt: &str) -> String {
    format!("{attempt}:normalization")
}

pub(super) fn assess(
    runtime: &Runtime,
    executable: &crate::VerifiedExecutable<'_>,
    binding: &McpBinding,
    raw: &[u8],
) -> Result<NormalizedToolResult, ToolNormalizationError> {
    let document = JsonDocument::new(raw, &runtime.config.limits).map_err(|error| {
        ToolNormalizationError::Extraction(crate::ExtractionError::Protocol(
            htlk_analyzer::McpValidationError::Json(error),
        ))
    })?;
    crate::normalize_mcp_tool_result(
        &document,
        binding,
        executable.schemas(),
        &runtime.config.limits,
        &executable.policy().fields().evaluator_limits,
    )
}

pub(super) fn record_value(
    identity: (InvocationId, AttemptId),
    binding: &McpBinding,
    raw_digest: Digest,
    result: Result<&NormalizedToolResult, &ToolNormalizationError>,
    delivery: McpDelivery,
    protection: &SecurityLabels,
    limits: &Limits,
) -> Result<Value, RuntimeError> {
    let (invocation, attempt) = identity;
    let (
        failure,
        processing,
        envelope,
        extraction,
        selected,
        exhausted,
        count,
        validation,
        detail,
        truncated,
    ) = match result {
        Ok(result) => {
            let envelope = map(result.envelope().value())?;
            let extraction = map(field(result.envelope().value(), "extraction")?)?;
            let (detail, truncated) = details(
                result,
                binding
                    .result_policy()
                    .ok_or_else(err)?
                    .extraction
                    .limits
                    .diagnostic_bytes,
                limits,
            )?;
            (
                result.failure().map(|failure| failure.code()),
                false,
                Value::Text(result.envelope().document().digest().to_string()),
                extraction.get("status").ok_or_else(err)?.clone(),
                extraction
                    .get("selected_candidate_id")
                    .cloned()
                    .unwrap_or(Value::Null),
                extraction
                    .get("exhausted_limit")
                    .cloned()
                    .unwrap_or(Value::Null),
                integer(result.extraction().candidates().len())?,
                envelope.get("validation").ok_or_else(err)?.clone(),
                detail,
                truncated,
            )
        }
        Err(error) => (
            Some(error.code()),
            true,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Integer(0),
            Value::Null,
            Value::Array(vec![]),
            false,
        ),
    };
    let normalization = object([
        (
            "binding_digest",
            Value::Text(binding.digest(limits).map_err(|_| err())?.to_string()),
        ),
        ("raw_digest", Value::Text(raw_digest.to_string())),
        (
            "failure_code",
            failure.map_or(Value::Null, |code| Value::Text(code.to_string())),
        ),
        ("processing_failure", Value::Bool(processing)),
        ("envelope_digest", envelope),
        ("extraction_status", extraction),
        ("selected_candidate_id", selected),
        ("exhausted_limit", exhausted),
        ("candidate_count", count),
        ("validation", validation),
        ("detail", detail),
        ("detail_truncated", Value::Bool(truncated)),
        (
            "response_received",
            Value::Bool(delivery == McpDelivery::ResponseReceived),
        ),
        (
            "effect_outcome_uncertain",
            Value::Bool(
                delivery == McpDelivery::PossiblySent
                    || (failure.is_some() && delivery == McpDelivery::ResponseReceived),
            ),
        ),
    ])?;
    let normalization_digest = hash_bytes(&cbor::encode(&normalization, limits)?);
    object([
        ("category", Value::Text("mcp_normalization".into())),
        ("invocation_id", Value::Text(invocation.to_string())),
        ("attempt_id", Value::Text(attempt.to_string())),
        (
            "canonical_path",
            Value::Text(format!(
                "/bindings/{}",
                binding.digest(limits).map_err(|_| err())?
            )),
        ),
        (
            "failure_code",
            failure.map_or(Value::Null, |code| Value::Text(code.to_string())),
        ),
        (
            "normalization_digest",
            Value::Text(normalization_digest.to_string()),
        ),
        ("normalization", normalization),
        ("influence_labels", protection.to_value(limits)?),
    ])
}

pub(super) fn validate_record(
    record: &StoredRecord,
    invocation: InvocationId,
    attempt: AttemptId,
    binding: &McpBinding,
    raw: Digest,
    limits: &Limits,
) -> Result<Option<FailureCode>, RuntimeError> {
    if !record.final_record
        || record.key.id != record_id(&attempt.to_string())
        || string(&record.payload, "category")? != "mcp_normalization"
        || string(&record.payload, "invocation_id")? != invocation.to_string()
        || string(&record.payload, "attempt_id")? != attempt.to_string()
    {
        return Err(RuntimeError::failure(FailureCode::Digest));
    }
    let normalization = field(&record.payload, "normalization")?;
    if digest(&record.payload, "normalization_digest")?
        != hash_bytes(&cbor::encode(normalization, limits)?)
        || digest(normalization, "raw_digest")? != raw
        || digest(normalization, "binding_digest")? != binding.digest(limits).map_err(|_| err())?
        || field(&record.payload, "failure_code")? != field(normalization, "failure_code")?
        || string(&record.payload, "canonical_path")?
            != format!("/bindings/{}", binding.digest(limits).map_err(|_| err())?)
    {
        return Err(RuntimeError::failure(FailureCode::Digest));
    }
    match field(normalization, "processing_failure")? {
        Value::Bool(false) => Ok(None),
        Value::Bool(true) => Ok(Some(
            string(normalization, "failure_code")?
                .parse()
                .map_err(|_| err())?,
        )),
        _ => Err(err()),
    }
}

fn details(
    result: &NormalizedToolResult,
    maximum: usize,
    limits: &Limits,
) -> Result<(Value, bool), RuntimeError> {
    let mut remaining = maximum.saturating_sub(1);
    let mut output = Vec::new();
    let mut truncated = false;
    for (index, candidate) in result.extraction().candidates().iter().enumerate() {
        if remaining < 512 {
            truncated = true;
            break;
        }
        remaining -= 512;
        let mut sources = Vec::new();
        for source in candidate.sources() {
            if remaining < 256 {
                truncated = true;
                break;
            }
            let source = source_value(*source)?;
            let bytes = cbor::encode(&source, limits)?.len().saturating_add(9);
            if bytes > remaining {
                truncated = true;
                break;
            }
            remaining -= bytes;
            sources.try_reserve(1).map_err(|_| err())?;
            sources.push(source);
        }
        output.try_reserve(1).map_err(|_| err())?;
        output.push(object([
            ("kind", Value::Text("candidate".into())),
            ("id", Value::Text(format!("candidate_{index}"))),
            (
                "value_digest",
                Value::Text(candidate.document().digest().to_string()),
            ),
            ("source_count", integer(candidate.sources().len())?),
            ("sources", Value::Array(sources)),
        ])?);
        if truncated {
            break;
        }
    }
    if !truncated {
        for diagnostic in result.extraction().diagnostics() {
            if remaining < 256 {
                truncated = true;
                break;
            }
            remaining -= 256;
            output.try_reserve(1).map_err(|_| err())?;
            output.push(object([
                ("kind", Value::Text("parse".into())),
                ("category", Value::Text(diagnostic.category.into())),
                (
                    "content_block_index",
                    Value::Integer(i64::from(diagnostic.block)),
                ),
                ("byte_start", integer(diagnostic.start)?),
                ("byte_end", integer(diagnostic.end)?),
            ])?);
        }
    }
    let value = Value::Array(output);
    let effective = Limits {
        max_document_bytes: maximum.min(limits.max_document_bytes),
        max_depth: limits.max_depth,
    };
    cbor::encode(&value, &effective)?;
    Ok((value, truncated))
}
fn source_value(source: crate::ExtractionSource) -> Result<Value, RuntimeError> {
    Ok(match source {
        crate::ExtractionSource::StructuredContent => object([
            ("origin", Value::Text("structured_content".into())),
            ("method", Value::Text("structured_content".into())),
        ])?,
        crate::ExtractionSource::Text {
            block,
            start,
            end,
            method,
        } => object([
            ("origin", Value::Text("text".into())),
            (
                "method",
                Value::Text(
                    match method {
                        crate::TextExtractionMethod::WholeBlock => "whole_block",
                        crate::TextExtractionMethod::Fence => "fence",
                        crate::TextExtractionMethod::Embedded => "embedded",
                    }
                    .into(),
                ),
            ),
            ("content_block_index", Value::Integer(i64::from(block))),
            ("byte_start", integer(start)?),
            ("byte_end", integer(end)?),
        ])?,
    })
}
fn integer(value: usize) -> Result<Value, RuntimeError> {
    Ok(Value::Integer(i64::try_from(value).map_err(|_| err())?))
}

pub(super) fn binding_for_view<'a>(
    document: &'a htlk_executable::CanonicalDocument,
    view: &super::NodeView,
    limits: &Limits,
) -> Result<&'a McpBinding, RuntimeError> {
    let tokens = view.canonical_path.tokens();
    if tokens.len() != 4 || tokens[0] != "scopes" || tokens[2] != "nodes" {
        return Err(err());
    }
    let scope: Digest = tokens[1].parse().map_err(|_| err())?;
    let index: usize = tokens[3].parse().map_err(|_| err())?;
    let node = document
        .fields()
        .scopes
        .get(&scope)
        .and_then(|scope| scope.fields().nodes.get(index))
        .ok_or_else(err)?;
    if node.digest(limits).map_err(|_| err())? != view.definition_digest
        || node.id().as_str() != view.local_id
    {
        return Err(RuntimeError::failure(FailureCode::Digest));
    }
    let Operation::Mcp { binding, .. } = &node.fields().operation else {
        return Err(RuntimeError::failure(FailureCode::Input));
    };
    let binding = document.fields().bindings.get(binding).ok_or_else(err)?;
    if !matches!(binding.kind(), McpBindingKind::Tool { .. }) {
        return Err(RuntimeError::failure(FailureCode::Input));
    }
    Ok(binding)
}

impl Runtime {
    /// Reads one explicitly selected tool attempt's protected original/extracted evidence.
    /// Byte limits apply to returned raw/normalization representations, not exact heap
    /// usage. Reconstruction never dispatches, adopts schemas or changes the outcome.
    ///
    /// # Errors
    /// Returns current authorization denial, invalid selection/bounds, integrity,
    /// missing records or storage failure. Missing native implementations are reported
    /// separately while authorized original bytes remain inspectable.
    pub async fn inspect_mcp_evidence(
        &self,
        context: &HostContext,
        run: &RunId,
        invocation: InvocationId,
        attempt: AttemptId,
        maximum_bytes: usize,
    ) -> Result<McpEvidenceView, RuntimeError> {
        if maximum_bytes == 0 || maximum_bytes > self.config.limits.max_document_bytes {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let view = self.inspect_node(context, run, invocation).await?;
        let selected = view
            .attempts
            .iter()
            .find(|entry| entry.attempt_id == attempt)
            .ok_or_else(|| RuntimeError::failure(FailureCode::Input))?;
        if AttemptId::new(invocation, selected.number, &self.config.limits).map_err(|_| err())?
            != attempt
        {
            return Err(RuntimeError::failure(FailureCode::Digest));
        }
        let (_, document) = self.source_document(view.origin_executable).await?;
        let binding = binding_for_view(&document, &view, &self.config.limits)?;
        let mut result = McpEvidenceView {
            run_id: run.clone(),
            invocation_id: invocation,
            attempt_id: attempt,
            labels: view.labels.clone(),
            delivery: selected.delivery,
            response_received: selected.delivery == Some(McpDelivery::ResponseReceived),
            effect_outcome_uncertain: selected.dispatch_state == "possibly_sent"
                || selected.delivery == Some(McpDelivery::PossiblySent)
                || (selected.failure_code.is_some()
                    && selected.delivery == Some(McpDelivery::ResponseReceived)),
            recorded_normalization: None,
            raw_digest: None,
            raw_response: None,
            raw_truncated: false,
            normalized: None,
            normalized_truncated: false,
            failure_code: selected.failure_code,
            reconstruction_failure: None,
        };
        if selected.dispatch_state != "response_recorded" || selected.delivery.is_none() {
            return Ok(result);
        }
        let response = self
            .store
            .record(run, &key(RecordFamily::Response, attempt))
            .await?;
        result.labels = result.labels.union(
            &labels(&response.payload, &self.config.limits)?,
            &self.config.limits,
        )?;
        self.policy.authorize(
            context,
            Authorization::InspectRun {
                run_id: run,
                labels: &result.labels,
            },
        )?;
        let normalization = match self
            .store
            .record(
                run,
                &key(RecordFamily::Observation, record_id(&attempt.to_string())),
            )
            .await
        {
            Ok(record) => Some(record),
            Err(crate::store::StoreError::Missing) => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(record) = &normalization {
            result.labels = result.labels.union(
                &labels(&record.payload, &self.config.limits)?,
                &self.config.limits,
            )?;
            self.policy.authorize(
                context,
                Authorization::InspectRun {
                    run_id: run,
                    labels: &result.labels,
                },
            )?;
        }
        let mut remaining = maximum_bytes;
        let Some(Value::Bytes(raw)) = map(&response.payload)?.get("raw") else {
            return Ok(result);
        };
        let raw_digest = digest(&response.payload, "raw_digest")?;
        if hash_bytes(raw) != raw_digest {
            return Err(RuntimeError::failure(FailureCode::Digest));
        }
        result.raw_digest = Some(raw_digest);
        let take = raw.len().min(remaining);
        let mut returned = Vec::new();
        returned.try_reserve_exact(take).map_err(|_| err())?;
        returned.extend_from_slice(&raw[..take]);
        result.raw_response = Some(returned);
        result.raw_truncated = take != raw.len();
        remaining -= take;
        if let Some(record) = &normalization {
            validate_record(
                record,
                invocation,
                attempt,
                binding,
                raw_digest,
                &self.config.limits,
            )?;
            let value = field(&record.payload, "normalization")?;
            result.failure_code = match map(value)?.get("failure_code") {
                Some(Value::Text(code)) => Some(code.parse().map_err(|_| err())?),
                Some(Value::Null) => None,
                _ => return Err(err()),
            };
            result.effect_outcome_uncertain = match field(value, "effect_outcome_uncertain")? {
                Value::Bool(value) => *value,
                _ => return Err(err()),
            };
            let size = cbor::encode(value, &self.config.limits)?.len();
            if size <= remaining {
                result.recorded_normalization = Some(value.clone());
                remaining -= size;
            }
        }
        if result.raw_truncated {
            result.normalized_truncated = true;
            return Ok(result);
        }
        let executable = match self.admitted(view.origin_executable).await {
            Ok(executable) => executable,
            Err(error) => {
                result.reconstruction_failure = Some(error.code());
                return Ok(result);
            }
        };
        match assess(self, &executable, binding, raw) {
            Ok(normalized) => {
                result.effect_outcome_uncertain |=
                    normalized.failure().is_some() && result.response_received;
                if let Some(record) = &normalization {
                    let stored = field(&record.payload, "normalization")?;
                    if field(stored, "processing_failure")? == &Value::Bool(false)
                        && digest(stored, "envelope_digest")?
                            != normalized.envelope().document().digest()
                    {
                        return Err(RuntimeError::failure(FailureCode::Digest));
                    }
                }
                if normalized.envelope().document().as_bytes().len() <= remaining {
                    result.normalized = Some(normalized);
                } else {
                    result.normalized_truncated = true;
                }
            }
            Err(error) => result.reconstruction_failure = Some(error.code()),
        }
        Ok(result)
    }
}
