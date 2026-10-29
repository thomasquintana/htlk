//! Canonical host-selected MCP extraction and result-validation policies.

use std::fmt;

use crate::{
    JsonError,
    cbor::{self, Limits, Map, Value},
    digest::{Digest, hash_bytes},
};

/// Pinned strict extraction algorithm; changing its meaning requires a new profile.
pub const JSON_EXTRACTION_PROFILE: &str = "htlk.json-extraction/1";

/// Model or policy failure, containing no submitted content.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpResultError {
    /// Native codec failure, including original resource category.
    Codec(cbor::Error),
    /// Strict JSON ingress failure.
    Json(JsonError),
    /// Invalid closed record or inconsistent fields at a fixed schema site.
    Shape(&'static str),
    /// A derived-work allowance was exhausted, not an invalid application value.
    Limit(&'static str),
}
impl From<cbor::Error> for McpResultError {
    fn from(error: cbor::Error) -> Self {
        Self::Codec(error)
    }
}
impl From<JsonError> for McpResultError {
    fn from(error: JsonError) -> Self {
        Self::Json(error)
    }
}
impl fmt::Display for McpResultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(error) => write!(f, "MCP result codec: {error}"),
            Self::Json(error) => write!(f, "MCP result JSON: {error}"),
            Self::Shape(site) => write!(f, "invalid MCP result record: {site}"),
            Self::Limit(resource) => write!(f, "MCP result limit exceeded: {resource}"),
        }
    }
}
impl std::error::Error for McpResultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

/// Independent cumulative ceilings for one extraction, across every text block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionLimits {
    /// Total original text bytes inspected.
    pub inspected_bytes: usize,
    /// Distinct canonical JSON values retained.
    pub candidates: usize,
    /// Maximum JSON/container nesting depth, at most 128.
    pub depth: usize,
    /// Total scanning and attempted-parse byte work.
    pub work: usize,
    /// Cumulative candidate bytes submitted to strict parsing.
    pub parsed_bytes: usize,
    /// Conservatively charged cumulative derived storage.
    pub allocation_bytes: usize,
    /// Encoded extracted result size, independently of original protocol content.
    pub output_bytes: usize,
    /// Detailed diagnostic storage.
    pub diagnostic_bytes: usize,
}
impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            inspected_bytes: 1024 * 1024,
            candidates: 64,
            depth: 32,
            work: 8 * 1024 * 1024,
            parsed_bytes: 2 * 1024 * 1024,
            allocation_bytes: 16 * 1024 * 1024,
            output_bytes: 4 * 1024 * 1024,
            diagnostic_bytes: 64 * 1024,
        }
    }
}

/// Explicit deterministic selection, never chosen by upstream display text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CandidateSelection {
    /// Prefer original structured content; otherwise require one distinct value.
    #[default]
    Unique,
    /// Select an exact within-result candidate identifier.
    Candidate(String),
    /// Require one distinct candidate originating in this content block.
    Block(u32),
    /// Require one distinct candidate satisfying this explicitly adopted schema.
    Schema(Digest),
}

/// Pinned bounded strict-JSON extraction policy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractionPolicy {
    /// Cumulative resource ceilings.
    pub limits: ExtractionLimits,
    /// Frozen selection rule.
    pub selection: CandidateSelection,
}

/// Acceptance of a violation of an actually advertised output contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeclaredContractPolicy {
    /// Fail the invocation while retaining protected diagnostic evidence.
    #[default]
    Enforce,
    /// Publish a marked generic envelope if mandatory application checks pass.
    Report,
}

/// Validation of an explicitly attached inferred-schema revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InferredSchemaPolicy {
    /// Report observations without granting a refined application type.
    #[default]
    Observe,
    /// Require actual normalized data to satisfy the adopted schema.
    Enforce,
}

/// Immutable inference adoption, distinct from an upstream advertised schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedInference {
    /// Inferred schema document identity.
    pub schema: Digest,
    /// Separate immutable inference-provenance document identity.
    pub evidence: Digest,
    /// Frozen validation semantics.
    pub mode: InferredSchemaPolicy,
}

/// Complete authored tool-result semantics; canonical encoding never omits defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolResultPolicy {
    /// Pinned extraction and selection semantics.
    pub extraction: ExtractionPolicy,
    /// Declared contracts are enforced by default.
    pub declared_contract: DeclaredContractPolicy,
    /// Optional explicitly attached inference revision.
    pub inferred_schema: Option<AdoptedInference>,
    /// Optional mandatory application-result schema.
    pub application_schema: Option<Digest>,
}

impl ExtractionPolicy {
    /// Builds the complete canonical record after validating every ceiling.
    ///
    /// # Errors
    /// Returns invalid policy, representation, or serialization limits.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, McpResultError> {
        let l = &self.limits;
        if l.depth > 128 {
            return Err(McpResultError::Shape("extraction depth"));
        }
        let mut fields = Vec::new();
        for (name, value) in [
            ("inspected_bytes", l.inspected_bytes),
            ("candidates", l.candidates),
            ("depth", l.depth),
            ("work", l.work),
            ("parsed_bytes", l.parsed_bytes),
            ("allocation_bytes", l.allocation_bytes),
            ("output_bytes", l.output_bytes),
            ("diagnostic_bytes", l.diagnostic_bytes),
        ] {
            let value = i64::try_from(value)
                .ok()
                .filter(|v| *v > 0)
                .ok_or(McpResultError::Shape("positive extraction limit"))?;
            fields.push((name.to_owned(), Value::Integer(value)));
        }
        let selection = match &self.selection {
            CandidateSelection::Unique => Value::Array(vec![Value::Text("unique".into())]),
            CandidateSelection::Candidate(id) => {
                validate_candidate_id(id)?;
                Value::Array(vec![
                    Value::Text("candidate".into()),
                    Value::Text(id.clone()),
                ])
            }
            CandidateSelection::Block(index) => Value::Array(vec![
                Value::Text("block".into()),
                Value::Integer(i64::from(*index)),
            ]),
            CandidateSelection::Schema(id) => {
                Value::Array(vec![Value::Text("schema".into()), digest_value(*id)])
            }
        };
        let value = object([
            ("profile", Value::Text(JSON_EXTRACTION_PROFILE.into())),
            ("limits", Value::Map(Map::try_from_entries(fields)?)),
            ("selection", selection),
        ])?;
        cbor::encode(&value, limits)?;
        Ok(value)
    }

    /// Checks a complete canonical record without repairing missing fields.
    ///
    /// # Errors
    /// Returns invalid profiles, selections, closed records, or resource limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, McpResultError> {
        cbor::encode(value, limits)?;
        let record = closed(value, &["profile", "limits", "selection"], &[])?;
        if text(get(record, "profile")?)? != JSON_EXTRACTION_PROFILE {
            return Err(McpResultError::Shape("extraction profile"));
        }
        let l = closed(
            get(record, "limits")?,
            &[
                "inspected_bytes",
                "candidates",
                "depth",
                "work",
                "parsed_bytes",
                "allocation_bytes",
                "output_bytes",
                "diagnostic_bytes",
            ],
            &[],
        )?;
        let number = |name| positive(get(l, name)?);
        let limits_record = ExtractionLimits {
            inspected_bytes: number("inspected_bytes")?,
            candidates: number("candidates")?,
            depth: number("depth")?,
            work: number("work")?,
            parsed_bytes: number("parsed_bytes")?,
            allocation_bytes: number("allocation_bytes")?,
            output_bytes: number("output_bytes")?,
            diagnostic_bytes: number("diagnostic_bytes")?,
        };
        let Value::Array(selection) = get(record, "selection")? else {
            return Err(McpResultError::Shape("selection"));
        };
        let selection = match selection.as_slice() {
            [Value::Text(kind)] if kind == "unique" => CandidateSelection::Unique,
            [Value::Text(kind), Value::Text(id)] if kind == "candidate" => {
                validate_candidate_id(id)?;
                CandidateSelection::Candidate(id.clone())
            }
            [Value::Text(kind), Value::Integer(index)] if kind == "block" => {
                CandidateSelection::Block(
                    u32::try_from(*index).map_err(|_| McpResultError::Shape("block index"))?,
                )
            }
            [Value::Text(kind), id] if kind == "schema" => {
                CandidateSelection::Schema(parse_digest(id)?)
            }
            _ => return Err(McpResultError::Shape("selection")),
        };
        let policy = Self {
            limits: limits_record,
            selection,
        };
        policy.to_value(limits)?;
        Ok(policy)
    }

    /// Raw SHA-256 of the full canonical policy record, not an upstream identity.
    ///
    /// # Errors
    /// Returns policy or codec failures.
    pub fn digest(&self, limits: &Limits) -> Result<Digest, McpResultError> {
        Ok(hash_bytes(&cbor::encode(&self.to_value(limits)?, limits)?))
    }
}

impl ToolResultPolicy {
    /// Produces one complete bounded native policy record.
    ///
    /// # Errors
    /// Returns policy or codec failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, McpResultError> {
        let inferred = match &self.inferred_schema {
            None => Value::Null,
            Some(inferred) => object([
                ("schema", digest_value(inferred.schema)),
                ("evidence", digest_value(inferred.evidence)),
                (
                    "mode",
                    Value::Text(
                        match inferred.mode {
                            InferredSchemaPolicy::Observe => "observe",
                            InferredSchemaPolicy::Enforce => "enforce",
                        }
                        .into(),
                    ),
                ),
            ])?,
        };
        let value = object([
            ("extraction", self.extraction.to_value(limits)?),
            (
                "declared_contract",
                Value::Text(
                    match self.declared_contract {
                        DeclaredContractPolicy::Enforce => "enforce",
                        DeclaredContractPolicy::Report => "report",
                    }
                    .into(),
                ),
            ),
            ("inferred_schema", inferred),
            (
                "application_schema",
                self.application_schema.map_or(Value::Null, digest_value),
            ),
        ])?;
        cbor::encode(&value, limits)?;
        Ok(value)
    }

    /// Independently admits exact canonical fields, with no legacy/default decoding.
    ///
    /// # Errors
    /// Returns malformed modes, digests, fields, extraction rules, or codec limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, McpResultError> {
        cbor::encode(value, limits)?;
        let record = closed(
            value,
            &[
                "extraction",
                "declared_contract",
                "inferred_schema",
                "application_schema",
            ],
            &[],
        )?;
        let declared_contract = match text(get(record, "declared_contract")?)? {
            "enforce" => DeclaredContractPolicy::Enforce,
            "report" => DeclaredContractPolicy::Report,
            _ => return Err(McpResultError::Shape("declared contract mode")),
        };
        let inferred_schema = match get(record, "inferred_schema")? {
            Value::Null => None,
            value => {
                let value = closed(value, &["schema", "evidence", "mode"], &[])?;
                let mode = match text(get(value, "mode")?)? {
                    "observe" => InferredSchemaPolicy::Observe,
                    "enforce" => InferredSchemaPolicy::Enforce,
                    _ => return Err(McpResultError::Shape("inferred schema mode")),
                };
                Some(AdoptedInference {
                    schema: parse_digest(get(value, "schema")?)?,
                    evidence: parse_digest(get(value, "evidence")?)?,
                    mode,
                })
            }
        };
        Ok(Self {
            extraction: ExtractionPolicy::from_value(get(record, "extraction")?, limits)?,
            declared_contract,
            inferred_schema,
            application_schema: match get(record, "application_schema")? {
                Value::Null => None,
                value => Some(parse_digest(value)?),
            },
        })
    }

    /// Encodes one canonical record.
    ///
    /// # Errors
    /// Returns invalid policy or codec limits.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, McpResultError> {
        Ok(cbor::encode(&self.to_value(limits)?, limits)?)
    }

    /// Decodes one complete canonical record, rejecting trailing bytes.
    ///
    /// # Errors
    /// Returns malformed/noncanonical records or resource failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, McpResultError> {
        Self::from_value(&cbor::decode(bytes, limits)?, limits)
    }
}

pub(crate) fn object<const N: usize>(fields: [(&str, Value); N]) -> Result<Value, McpResultError> {
    Ok(Value::Map(Map::try_from_entries(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    )?))
}
pub(crate) fn closed<'a>(
    value: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map, McpResultError> {
    let Value::Map(record) = value else {
        return Err(McpResultError::Shape("object"));
    };
    if required.iter().any(|key| record.get(key).is_none())
        || record
            .iter()
            .any(|(key, _)| !required.contains(&key) && !optional.contains(&key))
    {
        return Err(McpResultError::Shape("closed fields"));
    }
    Ok(record)
}
pub(crate) fn get<'a>(record: &'a Map, name: &str) -> Result<&'a Value, McpResultError> {
    record
        .get(name)
        .ok_or(McpResultError::Shape("missing field"))
}
pub(crate) fn text(value: &Value) -> Result<&str, McpResultError> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(McpResultError::Shape("text")),
    }
}
fn positive(value: &Value) -> Result<usize, McpResultError> {
    match value {
        Value::Integer(value) if *value > 0 => {
            usize::try_from(*value).map_err(|_| McpResultError::Shape("positive limit"))
        }
        _ => Err(McpResultError::Shape("positive limit")),
    }
}
fn digest_value(value: Digest) -> Value {
    Value::Text(value.to_string())
}
fn parse_digest(value: &Value) -> Result<Digest, McpResultError> {
    text(value)?
        .parse()
        .map_err(|_| McpResultError::Shape("digest"))
}
fn validate_candidate_id(value: &str) -> Result<(), McpResultError> {
    let digits = value
        .strip_prefix("candidate_")
        .ok_or(McpResultError::Shape("candidate identifier"))?;
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
        || digits.parse::<u32>().is_err()
    {
        return Err(McpResultError::Shape("candidate identifier"));
    }
    Ok(())
}
