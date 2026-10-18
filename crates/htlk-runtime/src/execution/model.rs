//! Private checkpoint helpers; canonical executable definitions remain authoritative.
use crate::{
    RuntimeError,
    failure::{FailureCode, RuntimeFailure},
    identity::ArtifactId,
    records::SecurityLabels,
    store::{RecordFamily, RecordKey, RecordWrite, StoredRecord},
};
use htlk_executable::{
    cbor::{Limits, Map, Value},
    digest::Digest,
};
pub(super) fn err() -> RuntimeError {
    RuntimeError::failure(FailureCode::Invariant)
}
pub(super) fn object(
    entries: impl IntoIterator<Item = (&'static str, Value)>,
) -> Result<Value, RuntimeError> {
    Ok(Value::Map(Map::try_from_entries(
        entries.into_iter().map(|(k, v)| (k.into(), v)),
    )?))
}
pub(super) fn map(value: &Value) -> Result<&Map, RuntimeError> {
    if let Value::Map(map) = value {
        Ok(map)
    } else {
        Err(err())
    }
}
pub(super) fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, RuntimeError> {
    map(value)?.get(name).ok_or_else(err)
}
pub(super) fn text(value: &Value) -> Result<&str, RuntimeError> {
    if let Value::Text(s) = value {
        Ok(s)
    } else {
        Err(err())
    }
}
pub(super) fn string(value: &Value, name: &str) -> Result<String, RuntimeError> {
    Ok(text(field(value, name)?)?.into())
}
pub(super) fn digest(value: &Value, name: &str) -> Result<Digest, RuntimeError> {
    string(value, name)?.parse().map_err(|_| err())
}
pub(super) fn number(value: &Value, name: &str) -> Result<i64, RuntimeError> {
    if let Value::Integer(n) = field(value, name)? {
        Ok(*n)
    } else {
        Err(err())
    }
}
pub(super) fn labels(value: &Value, limits: &Limits) -> Result<SecurityLabels, RuntimeError> {
    Ok(SecurityLabels::from_value(
        field(value, "influence_labels")?,
        limits,
    )?)
}
pub(super) fn replace(
    value: &Value,
    updates: impl IntoIterator<Item = (&'static str, Value)>,
) -> Result<Value, RuntimeError> {
    let mut entries = map(value)?
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (k, v) in updates {
        entries.insert(k.into(), v);
    }
    Ok(Value::Map(Map::try_from_entries(entries)?))
}
pub(super) fn key(family: RecordFamily, id: impl ToString) -> RecordKey {
    RecordKey {
        family,
        id: id.to_string(),
    }
}
pub(super) fn update(record: &StoredRecord, payload: Value, final_record: bool) -> RecordWrite {
    RecordWrite {
        key: record.key.clone(),
        expected_revision: Some(record.revision),
        final_record,
        payload,
    }
}
pub(super) fn insert(
    family: RecordFamily,
    id: impl ToString,
    payload: Value,
    final_record: bool,
) -> RecordWrite {
    RecordWrite {
        key: key(family, id),
        expected_revision: None,
        final_record,
        payload,
    }
}
pub(super) fn absent() -> Result<Value, RuntimeError> {
    object([("state", Value::Text("absent".into()))])
}
pub(super) fn present(id: ArtifactId) -> Result<Value, RuntimeError> {
    object([
        ("state", Value::Text("value".into())),
        ("artifact_id", Value::Text(id.to_string())),
    ])
}
pub(super) fn entry(value: &Value) -> Result<Option<ArtifactId>, RuntimeError> {
    match text(field(value, "state")?)? {
        "absent" => Ok(None),
        "value" => Ok(Some(
            string(value, "artifact_id")?.parse().map_err(|_| err())?,
        )),
        _ => Err(err()),
    }
}
pub(super) fn failure(value: &Value, limits: &Limits) -> Result<RuntimeFailure, RuntimeError> {
    RuntimeFailure::from_value(field(value, "failure")?, limits).map_err(|_| err())
}
pub(super) fn use_site(value: &Value) -> Result<htlk_analyzer::ScopeUse, RuntimeError> {
    match field(value, "scope_use")? {
        Value::Array(a) if a.len() == 1 && text(&a[0])? == "root" => {
            Ok(htlk_analyzer::ScopeUse::Root)
        }
        Value::Array(a) if a.len() == 3 && text(&a[0])? == "node" => {
            Ok(htlk_analyzer::ScopeUse::Node {
                scope: text(&a[1])?.parse().map_err(|_| err())?,
                node: text(&a[2])?.parse().map_err(|_| err())?,
            })
        }
        _ => Err(err()),
    }
}
pub(super) fn encode_use(site: &htlk_analyzer::ScopeUse) -> Value {
    match site {
        htlk_analyzer::ScopeUse::Root => Value::Array(vec![Value::Text("root".into())]),
        htlk_analyzer::ScopeUse::Node { scope, node } => Value::Array(vec![
            Value::Text("node".into()),
            Value::Text(scope.to_string()),
            Value::Text(node.to_string()),
        ]),
    }
}
pub(super) fn encode_causes(
    causes: &[crate::failure::FailureCause],
) -> Result<Value, RuntimeError> {
    Ok(Value::Array(
        causes
            .iter()
            .map(|c| {
                object([
                    ("invocation_id", Value::Text(c.invocation.to_string())),
                    ("code", Value::Text(c.code.to_string())),
                    (
                        "location",
                        c.location
                            .as_ref()
                            .map_or(Value::Null, |p| Value::Text(p.to_string())),
                    ),
                ])
            })
            .collect::<Result<Vec<_>, _>>()?,
    ))
}
pub(super) fn decode_causes(
    value: Option<&Value>,
    limits: &Limits,
) -> Result<Vec<crate::failure::FailureCause>, RuntimeError> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    let Value::Array(causes) = value else {
        return Err(err());
    };
    causes
        .iter()
        .map(|c| {
            Ok(crate::failure::FailureCause {
                invocation: string(c, "invocation_id")?.parse().map_err(|_| err())?,
                code: string(c, "code")?.parse().map_err(|_| err())?,
                location: match field(c, "location")? {
                    Value::Null => None,
                    value => Some(
                        htlk_executable::JsonPointer::new(text(value)?, limits)
                            .map_err(|_| err())?,
                    ),
                },
            })
        })
        .collect()
}
