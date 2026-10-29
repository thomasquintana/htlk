//! Closed policy/revision records, without raw sample response capture.

use super::{
    Budget, InferenceContext, InferenceError as E, InferenceLimits, InferencePolicy,
    InferenceSample, InferenceSampleKind, SCHEMA_INFERENCE_PROFILE, SampleObservation, exclusion,
};
use htlk_executable::{
    JsonDocument, JsonPointer,
    cbor::{Limits, Map, Value},
    digest::Digest,
    record_accounting::RecordAccounting,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn object(fields: Vec<(&str, Value)>) -> Result<Value, E> {
    Ok(Value::Map(Map::try_from_entries(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    )?))
}
pub(super) fn digest(value: Digest) -> Value {
    Value::Text(value.to_string())
}
pub(super) fn number(value: usize) -> Result<Value, E> {
    Ok(Value::Integer(
        i64::try_from(value).map_err(|_| E::Shape("integer count"))?,
    ))
}
pub(super) fn policy_document(
    policy: &InferencePolicy,
    limits: &Limits,
) -> Result<JsonDocument, E> {
    let l = &policy.limits;
    if l.depth > 128 {
        return Err(E::Shape("inference depth"));
    }
    let mut fields = Vec::new();
    for (name, value) in [
        ("samples", l.samples),
        ("depth", l.depth),
        ("sample_bytes", l.sample_bytes),
        ("nodes", l.nodes),
        ("allocation_bytes", l.allocation_bytes),
        ("output_bytes", l.output_bytes),
    ] {
        if value == 0 {
            return Err(E::Shape("positive inference limit"));
        }
        fields.push((name.to_owned(), number(value)?));
    }
    Ok(JsonDocument::from_value(
        &object(vec![
            ("profile", Value::Text(SCHEMA_INFERENCE_PROFILE.into())),
            (
                "sample_kind",
                Value::Text(
                    match policy.sample_kind {
                        InferenceSampleKind::Success => "success",
                        InferenceSampleKind::ToolError => "tool_error",
                    }
                    .into(),
                ),
            ),
            ("limits", Value::Map(Map::try_from_entries(fields)?)),
        ])?,
        limits,
    )?)
}
pub(super) fn parse_policy(document: &JsonDocument, limits: &Limits) -> Result<InferencePolicy, E> {
    bounded(document, limits)?;
    parse_policy_value(document.value(), limits)
}
fn parse_policy_value(value: &Value, limits: &Limits) -> Result<InferencePolicy, E> {
    let root = closed(value, &["profile", "sample_kind", "limits"])?;
    if text(get(root, "profile")?)? != SCHEMA_INFERENCE_PROFILE {
        return Err(E::Shape("inference profile"));
    }
    let sample_kind = match text(get(root, "sample_kind")?)? {
        "success" => InferenceSampleKind::Success,
        "tool_error" => InferenceSampleKind::ToolError,
        _ => return Err(E::Shape("sample kind")),
    };
    let l = closed(
        get(root, "limits")?,
        &[
            "samples",
            "depth",
            "sample_bytes",
            "nodes",
            "allocation_bytes",
            "output_bytes",
        ],
    )?;
    let positive = |key| {
        let value = count(get(l, key)?)?;
        if value == 0 {
            Err(E::Shape("positive inference limit"))
        } else {
            Ok(value)
        }
    };
    let policy = InferencePolicy {
        sample_kind,
        limits: InferenceLimits {
            samples: positive("samples")?,
            depth: positive("depth")?,
            sample_bytes: positive("sample_bytes")?,
            nodes: positive("nodes")?,
            allocation_bytes: positive("allocation_bytes")?,
            output_bytes: positive("output_bytes")?,
        },
    };
    policy.document(limits)?;
    Ok(policy)
}

pub(super) fn sample_value(sample: &InferenceSample, reason: Option<&str>) -> Result<Value, E> {
    let mut fields = Vec::new();
    fields
        .try_reserve_exact(sample.excluded_fields.len())
        .map_err(|_| E::AllocationFailed)?;
    for field in &sample.excluded_fields {
        fields.push(Value::Text(field.to_string()));
    }
    object(vec![
        ("reference", Value::Text(sample.reference.clone())),
        ("value_digest", digest(sample.value.digest())),
        ("value_bytes", number(sample.value.as_bytes().len())?),
        ("observation", observation_value(sample.observation)?),
        ("sanitized", Value::Bool(sample.sanitized)),
        ("excluded_fields", Value::Array(fields)),
        (
            "excluded_reason",
            reason.map_or(Value::Null, |reason| Value::Text(reason.into())),
        ),
    ])
}
fn observation_value(value: SampleObservation) -> Result<Value, E> {
    object(vec![
        ("successful", Value::Bool(value.successful)),
        ("complete", Value::Bool(value.complete)),
        ("truncated", Value::Bool(value.truncated)),
        ("uncertain", Value::Bool(value.uncertain)),
    ])
}
pub(super) fn evidence_value(
    context: InferenceContext,
    policy: &JsonDocument,
    schema: Digest,
    included: Vec<Value>,
    excluded: Vec<Value>,
    observations: BTreeMap<String, Value>,
    flags: [bool; 4],
) -> Result<Value, E> {
    object(vec![
        ("algorithm", Value::Text(SCHEMA_INFERENCE_PROFILE.into())),
        ("server", digest(context.server)),
        ("descriptor", digest(context.descriptor)),
        ("extraction_policy", digest(context.extraction_policy)),
        ("schema", digest(schema)),
        ("policy_digest", digest(policy.digest())),
        ("policy", policy.value().clone()),
        ("sample_count", number(included.len())?),
        ("samples", Value::Array(included)),
        ("excluded_samples", Value::Array(excluded)),
        (
            "observations",
            Value::Map(Map::try_from_entries(observations)?),
        ),
        (
            "source_flags",
            object(vec![
                ("errors", Value::Bool(flags[0])),
                ("incomplete", Value::Bool(flags[1])),
                ("truncated", Value::Bool(flags[2])),
                ("uncertain", Value::Bool(flags[3])),
            ])?,
        ),
        ("complete", Value::Bool(true)),
        ("limit_exhausted", Value::Null),
    ])
}

pub(super) fn validate_revision(
    schema: &JsonDocument,
    evidence: &JsonDocument,
    limits: &Limits,
) -> Result<InferenceContext, E> {
    bounded(schema, limits)?;
    bounded(evidence, limits)?;
    let root = closed(
        evidence.value(),
        &[
            "algorithm",
            "server",
            "descriptor",
            "extraction_policy",
            "schema",
            "policy_digest",
            "policy",
            "sample_count",
            "samples",
            "excluded_samples",
            "observations",
            "source_flags",
            "complete",
            "limit_exhausted",
        ],
    )?;
    if text(get(root, "algorithm")?)? != SCHEMA_INFERENCE_PROFILE
        || !boolean(get(root, "complete")?)?
        || get(root, "limit_exhausted")? != &Value::Null
        || parse_digest(get(root, "schema")?)? != schema.digest()
    {
        return Err(E::Shape("inference revision identity or completion"));
    }
    let policy = parse_policy_value(get(root, "policy")?, limits)?;
    let policy_document = policy.document(limits)?;
    if policy_document.digest() != parse_digest(get(root, "policy_digest")?)? {
        return Err(E::Shape("inference policy identity"));
    }
    let mut budget = Budget::new(&policy);
    if schema
        .as_bytes()
        .len()
        .saturating_add(evidence.as_bytes().len())
        > policy.limits.output_bytes
    {
        return Err(budget.limit("output_bytes"));
    }
    let included = array(get(root, "samples")?)?;
    let excluded = array(get(root, "excluded_samples")?)?;
    if included.is_empty() || count(get(root, "sample_count")?)? != included.len() {
        return Err(E::Shape("inference sample count"));
    }
    if included.len().saturating_add(excluded.len()) > policy.limits.samples {
        return Err(budget.limit("samples"));
    }
    budget.selected_samples = included.len();
    let mut references = BTreeSet::new();
    let mut flags = [false; 4];
    for (samples, should_exclude) in [(included, false), (excluded, true)] {
        let mut previous: Option<&str> = None;
        for sample in samples {
            budget.visit(1)?;
            let sample = closed(
                sample,
                &[
                    "reference",
                    "value_digest",
                    "value_bytes",
                    "observation",
                    "sanitized",
                    "excluded_fields",
                    "excluded_reason",
                ],
            )?;
            let reference = text(get(sample, "reference")?)?;
            if reference.is_empty()
                || previous.is_some_and(|previous| previous >= reference)
                || !references.insert(reference)
            {
                return Err(E::ConflictingSample);
            }
            previous = Some(reference);
            parse_digest(get(sample, "value_digest")?)?;
            let bytes = count(get(sample, "value_bytes")?)?;
            if bytes == 0 {
                return Err(E::Shape("sample byte count"));
            }
            budget.sample_bytes(bytes.saturating_add(reference.len()))?;
            let status = closed(
                get(sample, "observation")?,
                &["successful", "complete", "truncated", "uncertain"],
            )?;
            let status = SampleObservation {
                successful: boolean(get(status, "successful")?)?,
                complete: boolean(get(status, "complete")?)?,
                truncated: boolean(get(status, "truncated")?)?,
                uncertain: boolean(get(status, "uncertain")?)?,
            };
            let reason = exclusion(status, policy.sample_kind);
            let stored = get(sample, "excluded_reason")?;
            if reason.is_some() != should_exclude
                || match reason {
                    None => stored != &Value::Null,
                    Some(reason) => stored != &Value::Text(reason.into()),
                }
            {
                return Err(E::Shape("sample exclusion"));
            }
            flags[0] |= !status.successful;
            flags[1] |= !status.complete;
            flags[2] |= status.truncated;
            flags[3] |= status.uncertain;
            let sanitized = boolean(get(sample, "sanitized")?)?;
            let fields = array(get(sample, "excluded_fields")?)?;
            if !sanitized && !fields.is_empty() {
                return Err(E::Shape("unidentified sanitization"));
            }
            for field in fields {
                let field = text(field)?;
                budget.sample_bytes(field.len())?;
                budget.visit(1)?;
                budget.allocate(field.len().saturating_add(128))?;
                JsonPointer::new(field, limits).map_err(|_| E::Shape("excluded field pointer"))?;
            }
        }
    }
    let stored_flags = closed(
        get(root, "source_flags")?,
        &["errors", "incomplete", "truncated", "uncertain"],
    )?;
    for (name, value) in ["errors", "incomplete", "truncated", "uncertain"]
        .into_iter()
        .zip(flags)
    {
        if boolean(get(stored_flags, name)?)? != value {
            return Err(E::Shape("source observation flags"));
        }
    }
    let Value::Map(observations) = get(root, "observations")? else {
        return Err(E::Shape("observations"));
    };
    if observations.get("").is_none() {
        return Err(E::Shape("root observation"));
    }
    for (path, observation) in observations.iter() {
        budget.visit(1)?;
        budget.allocate(path.len().saturating_add(128))?;
        JsonPointer::new(path, limits).map_err(|_| E::Shape("observation path"))?;
        let observation = closed(observation, &["present", "total", "types"])?;
        let present = count(get(observation, "present")?)?;
        let total = count(get(observation, "total")?)?;
        if present > total
            || total > policy.limits.nodes
            || (path.is_empty() && (present != included.len() || total != included.len()))
        {
            return Err(E::Shape("observation presence"));
        }
        let Value::Map(types) = get(observation, "types")? else {
            return Err(E::Shape("observed types"));
        };
        let mut sum = 0usize;
        for (name, value) in types.iter() {
            budget.visit(1)?;
            if ![
                "null", "boolean", "integer", "number", "string", "array", "object",
            ]
            .contains(&name)
            {
                return Err(E::Shape("observed type"));
            }
            let value = count(value)?;
            if value == 0 {
                return Err(E::Shape("type count"));
            }
            sum = sum
                .checked_add(value)
                .ok_or(E::Shape("type count overflow"))?;
        }
        if sum != present {
            return Err(E::Shape("observed type counts"));
        }
    }
    Ok(InferenceContext {
        server: parse_digest(get(root, "server")?)?,
        descriptor: parse_digest(get(root, "descriptor")?)?,
        extraction_policy: parse_digest(get(root, "extraction_policy")?)?,
    })
}

fn bounded(document: &JsonDocument, limits: &Limits) -> Result<(), E> {
    if document.as_bytes().len() > limits.max_document_bytes {
        return Err(E::Limit {
            resource: "output_bytes",
            selected_samples: 0,
        });
    }
    let mut accounting = RecordAccounting::new(limits)?;
    accounting
        .value(document.value(), 0)
        .map_err(|error| E::Limit {
            resource: if error.limit == htlk_executable::cbor::LimitKind::Depth {
                "depth"
            } else {
                "output_bytes"
            },
            selected_samples: 0,
        })?;
    Ok(())
}
fn closed<'a>(value: &'a Value, fields: &[&str]) -> Result<&'a Map, E> {
    let Value::Map(record) = value else {
        return Err(E::Shape("object"));
    };
    if record.len() != fields.len() || fields.iter().any(|key| record.get(key).is_none()) {
        return Err(E::Shape("closed fields"));
    }
    Ok(record)
}
fn get<'a>(value: &'a Map, key: &str) -> Result<&'a Value, E> {
    value.get(key).ok_or(E::Shape("missing field"))
}
fn text(value: &Value) -> Result<&str, E> {
    if let Value::Text(value) = value {
        Ok(value)
    } else {
        Err(E::Shape("text"))
    }
}
fn boolean(value: &Value) -> Result<bool, E> {
    if let Value::Bool(value) = value {
        Ok(*value)
    } else {
        Err(E::Shape("boolean"))
    }
}
fn count(value: &Value) -> Result<usize, E> {
    if let Value::Integer(value) = value {
        usize::try_from(*value).map_err(|_| E::Shape("nonnegative count"))
    } else {
        Err(E::Shape("nonnegative count"))
    }
}
fn parse_digest(value: &Value) -> Result<Digest, E> {
    text(value)?.parse().map_err(|_| E::Shape("digest"))
}
fn array(value: &Value) -> Result<&[Value], E> {
    if let Value::Array(value) = value {
        Ok(value)
    } else {
        Err(E::Shape("array"))
    }
}
