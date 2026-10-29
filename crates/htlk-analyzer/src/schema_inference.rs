//! Passive, deterministic structural inference from explicitly identified samples.

mod merge;
mod wire;

use htlk_executable::{
    AdoptedInference, InferredSchemaPolicy, JsonDocument, JsonError, JsonPointer,
    cbor::{self, Limits, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, fmt};

/// Pinned generalization algorithm; observations are not upstream guarantees.
pub const SCHEMA_INFERENCE_PROFILE: &str = "htlk.schema-inference/1";

/// Inference admission/operation failure, never containing sample values or secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InferenceError {
    /// A fixed policy/evidence/sample schema site is malformed.
    Shape(&'static str),
    /// Samples are associated with another pinned capability/extraction policy.
    Context,
    /// One immutable source reference names inconsistent observations.
    ConflictingSample,
    /// No complete observation matches the selected sample class.
    NoSamples,
    /// No revision is produced after cumulative resource exhaustion.
    Limit {
        /// Stable exhausted allowance.
        resource: &'static str,
        /// Eligible observations selected before the failure.
        selected_samples: usize,
    },
    /// Strict bounded JSON failure.
    Json(JsonError),
    /// Native representation failure.
    Codec(cbor::Error),
    /// A fallible allocation failed independently of configured limits.
    AllocationFailed,
}
impl fmt::Display for InferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(site) => write!(f, "invalid inference record: {site}"),
            Self::Context => {
                f.write_str("inference sample context differs from the selected capability")
            }
            Self::ConflictingSample => {
                f.write_str("inference sample reference has conflicting observations")
            }
            Self::NoSamples => f.write_str("no complete eligible inference samples"),
            Self::Limit {
                resource,
                selected_samples,
            } => write!(
                f,
                "inference {resource} exhausted after selecting {selected_samples} samples; no revision produced"
            ),
            Self::Json(error) => write!(f, "inference JSON: {error}"),
            Self::Codec(error) => write!(f, "inference codec: {error}"),
            Self::AllocationFailed => f.write_str("inference allocation failed"),
        }
    }
}
impl std::error::Error for InferenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}
impl From<JsonError> for InferenceError {
    fn from(value: JsonError) -> Self {
        Self::Json(value)
    }
}
impl From<cbor::Error> for InferenceError {
    fn from(value: cbor::Error) -> Self {
        Self::Codec(value)
    }
}

/// Exact selected upstream association; inference never replaces its descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InferenceContext {
    /// Compound server-identity digest, not endpoint credentials.
    pub server: Digest,
    /// Original selected tool descriptor digest.
    pub descriptor: Digest,
    /// Full extraction-policy identity used for sample selection.
    pub extraction_policy: Digest,
}
/// Explicit source status; callers obtain it from authorized immutable evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleObservation {
    /// The tool observation was successful rather than isError:true.
    pub successful: bool,
    /// Extraction and recording completed.
    pub complete: bool,
    /// The observation was truncated.
    pub truncated: bool,
    /// The source JSON observation itself is uncertain. External effect outcome
    /// uncertainty is retained separately in source evidence and grants no replay.
    pub uncertain: bool,
}
/// Bounded operator/host-selected sample. This record grants no artifact access.
#[derive(Clone, Debug, PartialEq)]
pub struct InferenceSample {
    /// Exact immutable artifact/sample reference; must not contain credentials.
    pub reference: String,
    /// Explicit selected JSON value, not an automatically captured private response.
    pub value: JsonDocument,
    /// Original selected capability and extraction policy.
    pub context: InferenceContext,
    /// Source eligibility facts, independently retained in evidence.
    pub observation: SampleObservation,
    /// Whether the operator explicitly supplied a sanitized sample.
    pub sanitized: bool,
    /// Explicit excluded original fields; only valid for identified sanitization.
    pub excluded_fields: Vec<JsonPointer>,
}

/// Separate ordinary-result and tool-error inference; never silently mix them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InferenceSampleKind {
    /// Learn from complete successful observations only.
    #[default]
    Success,
    /// Explicit separate inference of complete tool-error observations.
    ToolError,
}
/// Cumulative inference ceilings; none creates a promise about future responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InferenceLimits {
    /// Maximum explicitly supplied observations, including exclusions/duplicates.
    pub samples: usize,
    /// Maximum input value nesting depth.
    pub depth: usize,
    /// Total canonical sample and source-reference bytes.
    pub sample_bytes: usize,
    /// Structural/metadata visits and deterministic ordering work.
    pub nodes: usize,
    /// Conservative cumulative derived-allocation charges.
    pub allocation_bytes: usize,
    /// Combined canonical schema/evidence output bytes.
    pub output_bytes: usize,
}
impl Default for InferenceLimits {
    fn default() -> Self {
        Self {
            samples: 256,
            depth: 32,
            sample_bytes: 1024 * 1024,
            nodes: 65_536,
            allocation_bytes: 4 * 1024 * 1024,
            output_bytes: 1024 * 1024,
        }
    }
}
/// Explicit pinned inference policy; runtime Observe/Enforce is a separate adoption.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InferencePolicy {
    /// Selected observation class.
    pub sample_kind: InferenceSampleKind,
    /// Cumulative bounds.
    pub limits: InferenceLimits,
}
impl InferencePolicy {
    /// Encodes every policy field as canonical JSON, with no implicit wire defaults.
    ///
    /// # Errors
    /// Returns invalid limits or codec/JSON failures.
    pub fn document(&self, limits: &Limits) -> Result<JsonDocument, InferenceError> {
        wire::policy_document(self, limits)
    }
    /// Independently checks an exact complete policy document.
    ///
    /// # Errors
    /// Returns unsupported profiles, malformed fields, or resource failures.
    pub fn from_document(document: &JsonDocument, limits: &Limits) -> Result<Self, InferenceError> {
        wire::parse_policy(document, limits)
    }
}

/// Immutable candidate schema and separate sample provenance, not a server guarantee.
#[derive(Clone, Debug, PartialEq)]
pub struct InferenceRevision {
    schema: JsonDocument,
    evidence: JsonDocument,
    context: InferenceContext,
}
impl InferenceRevision {
    /// Independently checks schema/evidence identities, policies and sample metadata.
    /// Does not authenticate sources or claim that observed requirements are universal.
    ///
    /// # Errors
    /// Rejects inconsistent schema/policy identities, malformed evidence, or bounds.
    /// Binding admission separately checks the returned context against its pins.
    pub fn from_documents(
        schema: JsonDocument,
        evidence: JsonDocument,
        limits: &Limits,
    ) -> Result<Self, InferenceError> {
        let context = wire::validate_revision(&schema, &evidence, limits)?;
        Ok(Self {
            schema,
            evidence,
            context,
        })
    }
    /// Candidate schema document, with its own identity and no upstream mutation.
    pub fn schema(&self) -> &JsonDocument {
        &self.schema
    }
    /// Immutable identified observations, with value digests instead of raw values.
    pub fn evidence(&self) -> &JsonDocument {
        &self.evidence
    }
    /// Original selected capability/extraction-policy association.
    pub fn context(&self) -> InferenceContext {
        self.context
    }
    /// Explicitly chooses validation semantics; does not change an active executable.
    pub fn adoption(&self, mode: InferredSchemaPolicy) -> AdoptedInference {
        AdoptedInference {
            schema: self.schema.digest(),
            evidence: self.evidence.digest(),
            mode,
        }
    }
    /// Produces bounded changed schema paths, retaining both immutable identities.
    ///
    /// # Errors
    /// Returns context differences, output/work limits or representation failures.
    pub fn difference(
        &self,
        newer: &Self,
        policy: &InferencePolicy,
        limits: &Limits,
    ) -> Result<JsonDocument, InferenceError> {
        policy.document(limits)?;
        if self.context != newer.context {
            return Err(InferenceError::Context);
        }
        let mut budget = Budget::new(policy);
        let mut paths = Vec::new();
        merge::difference(
            self.schema.value(),
            newer.schema.value(),
            "",
            &mut paths,
            &mut budget,
            0,
        )?;
        let value = wire::object(vec![
            ("before", wire::digest(self.schema.digest())),
            ("after", wire::digest(newer.schema.digest())),
            (
                "changed_paths",
                Value::Array(paths.into_iter().map(Value::Text).collect()),
            ),
        ])?;
        finish_document(&value, &mut budget, limits)
    }
}

/// Infers one proposed revision from explicit samples; never dispatches a tool.
/// Samples are deduplicated by immutable reference and ordered before merging.
/// Errors/exhaustion produce no partially adoptable revision.
///
/// # Errors
/// Returns conflicting origins/references, no eligible samples, invalid policies,
/// or cumulative resource failures. No result authorizes a retry or schema adoption.
pub fn infer_schema(
    samples: &[InferenceSample],
    context: InferenceContext,
    policy: &InferencePolicy,
    limits: &Limits,
) -> Result<InferenceRevision, InferenceError> {
    let policy_document = policy.document(limits)?;
    let mut budget = Budget::new(policy);
    if samples.len() > policy.limits.samples {
        return Err(budget.limit("samples"));
    }
    let mut ordered = BTreeMap::new();
    for sample in samples {
        budget.visit(1)?;
        if sample.context != context {
            return Err(InferenceError::Context);
        }
        if sample.reference.is_empty() || (!sample.sanitized && !sample.excluded_fields.is_empty())
        {
            return Err(InferenceError::Shape("sample source or sanitization"));
        }
        budget.sample_bytes(
            sample
                .value
                .as_bytes()
                .len()
                .saturating_add(sample.reference.len()),
        )?;
        for field in &sample.excluded_fields {
            budget.visit(1)?;
            for token in field.tokens() {
                budget.sample_bytes(token.len().saturating_mul(2).saturating_add(1))?;
            }
        }
        budget.allocate(sample.reference.len().saturating_add(256))?;
        let levels = (usize::BITS - ordered.len().leading_zeros()).max(1) as usize;
        budget.visit(levels)?;
        if let Some(previous) = ordered.insert(sample.reference.as_str(), sample)
            && previous != sample
        {
            return Err(InferenceError::ConflictingSample);
        }
    }
    let mut values = Vec::new();
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    let mut flags = [false; 4];
    for sample in ordered.values() {
        let reason = exclusion(sample.observation, policy.sample_kind);
        flags[0] |= !sample.observation.successful;
        flags[1] |= !sample.observation.complete;
        flags[2] |= sample.observation.truncated;
        flags[3] |= sample.observation.uncertain;
        let mut metadata_bytes = sample
            .reference
            .len()
            .saturating_add(sample.excluded_fields.len().saturating_mul(128));
        for field in &sample.excluded_fields {
            for token in field.tokens() {
                metadata_bytes =
                    metadata_bytes.saturating_add(token.len().saturating_mul(2).saturating_add(1));
            }
        }
        budget.allocate(metadata_bytes.saturating_mul(16).saturating_add(4096))?;
        let metadata = wire::sample_value(sample, reason)?;
        if reason.is_none() {
            values
                .try_reserve(1)
                .map_err(|_| InferenceError::AllocationFailed)?;
            included
                .try_reserve(1)
                .map_err(|_| InferenceError::AllocationFailed)?;
            values.push(sample.value.value());
            included.push(metadata);
        } else {
            excluded
                .try_reserve(1)
                .map_err(|_| InferenceError::AllocationFailed)?;
            excluded.push(metadata);
        }
    }
    budget.selected_samples = values.len();
    if values.is_empty() {
        return Err(InferenceError::NoSamples);
    }
    let mut observations = BTreeMap::new();
    let schema = merge::merge(&values, values.len(), "", &mut observations, &mut budget, 0)?;
    let schema = finish_document(&schema, &mut budget, limits)?;
    let evidence = wire::evidence_value(
        context,
        &policy_document,
        schema.digest(),
        included,
        excluded,
        observations,
        flags,
    )?;
    let evidence = finish_document(&evidence, &mut budget, limits)?;
    Ok(InferenceRevision {
        schema,
        evidence,
        context,
    })
}

fn exclusion(observation: SampleObservation, kind: InferenceSampleKind) -> Option<&'static str> {
    if !observation.complete {
        Some("incomplete")
    } else if observation.truncated {
        Some("truncated")
    } else if observation.uncertain {
        Some("uncertain")
    } else if observation.successful != (kind == InferenceSampleKind::Success) {
        Some("observation_class")
    } else {
        None
    }
}

struct Budget<'a> {
    policy: &'a InferencePolicy,
    visited: usize,
    allocated: usize,
    samples_bytes: usize,
    output: usize,
    selected_samples: usize,
}
impl<'a> Budget<'a> {
    fn new(policy: &'a InferencePolicy) -> Self {
        Self {
            policy,
            visited: 0,
            allocated: 0,
            samples_bytes: 0,
            output: 0,
            selected_samples: 0,
        }
    }
    fn limit(&self, resource: &'static str) -> InferenceError {
        InferenceError::Limit {
            resource,
            selected_samples: self.selected_samples,
        }
    }
    fn visit(&mut self, count: usize) -> Result<(), InferenceError> {
        self.visited = self
            .visited
            .checked_add(count)
            .filter(|n| *n <= self.policy.limits.nodes)
            .ok_or_else(|| self.limit("nodes"))?;
        Ok(())
    }
    fn allocate(&mut self, bytes: usize) -> Result<(), InferenceError> {
        self.allocated = self
            .allocated
            .checked_add(bytes)
            .filter(|n| *n <= self.policy.limits.allocation_bytes)
            .ok_or_else(|| self.limit("allocation_bytes"))?;
        Ok(())
    }
    fn sample_bytes(&mut self, bytes: usize) -> Result<(), InferenceError> {
        self.samples_bytes = self
            .samples_bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.policy.limits.sample_bytes)
            .ok_or_else(|| self.limit("sample_bytes"))?;
        Ok(())
    }
    fn depth(&self, depth: usize) -> Result<(), InferenceError> {
        if depth > self.policy.limits.depth {
            Err(self.limit("depth"))
        } else {
            Ok(())
        }
    }
}
fn finish_document(
    value: &Value,
    budget: &mut Budget<'_>,
    limits: &Limits,
) -> Result<JsonDocument, InferenceError> {
    let effective = Limits {
        max_document_bytes: limits.max_document_bytes.min(
            budget
                .policy
                .limits
                .output_bytes
                .saturating_sub(budget.output),
        ),
        max_depth: limits.max_depth,
    };
    let document = JsonDocument::from_value(value, &effective).map_err(|error| match error {
        JsonError::LimitExceeded {
            limit: cbor::LimitKind::Depth,
            ..
        } => budget.limit("depth"),
        JsonError::LimitExceeded { .. } => budget.limit("output_bytes"),
        error => InferenceError::Json(error),
    })?;
    budget.output += document.as_bytes().len();
    Ok(document)
}
