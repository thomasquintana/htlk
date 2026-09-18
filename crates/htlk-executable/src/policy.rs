//! The pinned external JCS policy document shape.

use crate::cbor as htlk_cbor;
use crate::digest::Digest;
use crate::{ExecutionLimits, ExecutionOptionsError, JsonDocument, JsonError};
use htlk_cbor::{Limits, Map, Value};
use std::fmt;

/// Authored evaluator ceilings. Every value must be positive and fit the JSON
/// numeric profile when incorporated into a PolicyDocument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluatorLimits {
    /// Maximum evaluator expression depth.
    pub max_expression_depth: u64,
    /// Maximum input value bytes.
    pub max_value_bytes: u64,
    /// Maximum collection elements visited.
    pub max_collection_visits: u64,
    /// Maximum regex pattern bytes.
    pub max_regex_bytes: u64,
    /// Maximum compiled regex representation bytes.
    pub max_regex_compiled_bytes: u64,
    /// Maximum evaluator output bytes.
    pub max_output_bytes: u64,
    /// Maximum deterministic work steps.
    pub max_steps: u64,
}
/// Authored policy fields, validated when constructing PolicyDocument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyFields {
    /// Exact unit used for cost budgets.
    pub cost_unit: String,
    /// Must provide timeout, attempt timeout, and concurrency defaults.
    pub defaults: ExecutionLimits,
    /// Positive deterministic evaluator ceilings.
    pub evaluator_limits: EvaluatorLimits,
    /// Maximum semantic scope depth, root counted as one.
    pub maximum_scope_depth: u64,
    /// Maximum expanded invocation occurrences, including loop multipliers.
    pub maximum_expanded_nodes: u64,
}
/// Immutable shape-checked policy and its canonical JCS document identity.
/// This configures ceilings; it cannot grant capabilities or enforce metering.
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyDocument {
    fields: PolicyFields,
    document: JsonDocument,
}
impl PolicyDocument {
    /// Builds the exact policy record and canonical JCS bytes.
    ///
    /// # Errors
    /// Returns missing defaults, invalid positive values, JSON numeric or resource failures.
    pub fn new(fields: PolicyFields, limits: &Limits) -> Result<Self, PolicyError> {
        validate_defaults(&fields.defaults)?;
        let e = &fields.evaluator_limits;
        let evaluator = object(vec![
            (
                "max_expression_depth",
                positive(e.max_expression_depth, "max_expression_depth")?,
            ),
            (
                "max_value_bytes",
                positive(e.max_value_bytes, "max_value_bytes")?,
            ),
            (
                "max_collection_visits",
                positive(e.max_collection_visits, "max_collection_visits")?,
            ),
            (
                "max_regex_bytes",
                positive(e.max_regex_bytes, "max_regex_bytes")?,
            ),
            (
                "max_regex_compiled_bytes",
                positive(e.max_regex_compiled_bytes, "max_regex_compiled_bytes")?,
            ),
            (
                "max_output_bytes",
                positive(e.max_output_bytes, "max_output_bytes")?,
            ),
            ("max_steps", positive(e.max_steps, "max_steps")?),
        ])?;
        // Transfer the caller-owned cost string; JSON limits precede any copy.
        let cost = Value::Text(fields.cost_unit);
        let value = object(vec![
            ("cost_unit", cost),
            ("defaults", fields.defaults.to_value(limits)?),
            ("evaluator_limits", evaluator),
            (
                "maximum_scope_depth",
                positive(fields.maximum_scope_depth, "maximum_scope_depth")?,
            ),
            (
                "maximum_expanded_nodes",
                positive(fields.maximum_expanded_nodes, "maximum_expanded_nodes")?,
            ),
        ])?;
        let document = JsonDocument::from_value(&value, limits)?;
        Self::from_document(document, limits)
    }
    /// Validates an existing canonical JSON document against the closed policy schema.
    ///
    /// # Errors
    /// Returns schema, default, positive-value, or resource failures.
    pub fn from_document(document: JsonDocument, limits: &Limits) -> Result<Self, PolicyError> {
        // Recheck the supplied document under this call's effective limits.
        let document = JsonDocument::decode(document.as_bytes(), limits)?;
        Self::parse(document, limits)
    }
    fn parse(document: JsonDocument, limits: &Limits) -> Result<Self, PolicyError> {
        let m = closed(
            document.value(),
            &[
                "cost_unit",
                "defaults",
                "evaluator_limits",
                "maximum_expanded_nodes",
                "maximum_scope_depth",
            ],
        )?;
        let Value::Text(cost) = field(m, "cost_unit") else {
            return Err(PolicyError::InvalidShape("cost_unit"));
        };
        let mut cost_unit = String::new();
        cost_unit
            .try_reserve_exact(cost.len())
            .map_err(|_| PolicyError::Json(JsonError::AllocationFailed))?;
        cost_unit.push_str(cost);
        let defaults = ExecutionLimits::from_value(field(m, "defaults"), limits)?;
        validate_defaults(&defaults)?;
        let e = closed(
            field(m, "evaluator_limits"),
            &[
                "max_collection_visits",
                "max_expression_depth",
                "max_output_bytes",
                "max_regex_bytes",
                "max_regex_compiled_bytes",
                "max_steps",
                "max_value_bytes",
            ],
        )?;
        let fields = PolicyFields {
            cost_unit,
            defaults,
            evaluator_limits: EvaluatorLimits {
                max_expression_depth: integer(e, "max_expression_depth")?,
                max_value_bytes: integer(e, "max_value_bytes")?,
                max_collection_visits: integer(e, "max_collection_visits")?,
                max_regex_bytes: integer(e, "max_regex_bytes")?,
                max_regex_compiled_bytes: integer(e, "max_regex_compiled_bytes")?,
                max_output_bytes: integer(e, "max_output_bytes")?,
                max_steps: integer(e, "max_steps")?,
            },
            maximum_scope_depth: integer(m, "maximum_scope_depth")?,
            maximum_expanded_nodes: integer(m, "maximum_expanded_nodes")?,
        };
        Ok(Self { fields, document })
    }
    /// Decodes exact JCS bytes and validates the policy schema.
    ///
    /// # Errors
    /// Returns JSON, schema, default, numeric or resource failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, PolicyError> {
        Self::parse(JsonDocument::decode(bytes, limits)?, limits)
    }
    /// Borrows validated immutable policy fields.
    pub fn fields(&self) -> &PolicyFields {
        &self.fields
    }
    /// Borrows the canonical external JSON document.
    pub fn document(&self) -> &JsonDocument {
        &self.document
    }
    /// Raw SHA-256 of the canonical JCS bytes.
    pub fn digest(&self) -> Digest {
        self.document.digest()
    }
}

fn validate_defaults(d: &ExecutionLimits) -> Result<(), PolicyError> {
    if d.timeout_ms().is_none() || d.attempt_timeout_ms().is_none() || d.max_concurrency().is_none()
    {
        Err(PolicyError::IncompleteDefaults)
    } else {
        Ok(())
    }
}
fn object(fields: Vec<(&str, Value)>) -> Result<Value, PolicyError> {
    Ok(Value::Map(
        Map::try_from_entries(fields.into_iter().map(|(k, v)| (k.to_owned(), v)))
            .map_err(JsonError::from)?,
    ))
}
fn positive(n: u64, field: &'static str) -> Result<Value, PolicyError> {
    if n == 0 || n > i64::MAX as u64 {
        Err(PolicyError::InvalidPositive(field))
    } else {
        Ok(Value::Integer(n as i64))
    }
}
fn closed<'a>(v: &'a Value, fields: &[&'static str]) -> Result<&'a Map, PolicyError> {
    let Value::Map(m) = v else {
        return Err(PolicyError::InvalidShape("record"));
    };
    if m.iter().any(|(k, _)| !fields.contains(&k)) {
        return Err(PolicyError::UnknownField);
    }
    for name in fields {
        if m.get(name).is_none() {
            return Err(PolicyError::MissingField(name));
        }
    }
    Ok(m)
}
fn field<'a>(m: &'a Map, name: &str) -> &'a Value {
    m.get(name).expect("field checked")
}
fn integer(m: &Map, name: &'static str) -> Result<u64, PolicyError> {
    match field(m, name) {
        Value::Integer(n) if *n > 0 => Ok(*n as u64),
        _ => Err(PolicyError::InvalidPositive(name)),
    }
}

/// Policy-schema error with redacted, static field descriptions.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyError {
    /// JSON/JCS failure.
    Json(JsonError),
    /// Invalid execution-limit defaults.
    Options(ExecutionOptionsError),
    /// Incorrect native shape.
    InvalidShape(&'static str),
    /// Unknown policy/evaluator field.
    UnknownField,
    /// Missing required field.
    MissingField(&'static str),
    /// Invalid positive value.
    InvalidPositive(&'static str),
    /// Defaults omit a required timeout/attempt-timeout/concurrency field.
    IncompleteDefaults,
}
impl From<JsonError> for PolicyError {
    fn from(e: JsonError) -> Self {
        Self::Json(e)
    }
}
impl From<ExecutionOptionsError> for PolicyError {
    fn from(e: ExecutionOptionsError) -> Self {
        Self::Options(e)
    }
}
impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "policy: {e}"),
            Self::Options(e) => write!(f, "policy defaults: {e}"),
            Self::InvalidShape(s) => write!(f, "invalid policy shape: {s}"),
            Self::UnknownField => f.write_str("unknown policy field"),
            Self::MissingField(s) => write!(f, "missing policy field: {s}"),
            Self::InvalidPositive(s) => write!(f, "policy field must be positive: {s}"),
            Self::IncompleteDefaults => {
                f.write_str("policy defaults require timeout, attempt timeout, and concurrency")
            }
        }
    }
}
impl std::error::Error for PolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Options(e) => Some(e),
            _ => None,
        }
    }
}
