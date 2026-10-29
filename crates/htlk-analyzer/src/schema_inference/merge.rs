//! Bounded family merge and schema-revision path differences.

use super::{Budget, InferenceError, wire};
use htlk_executable::cbor::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn merge(
    values: &[&Value],
    total: usize,
    path: &str,
    observations: &mut BTreeMap<String, Value>,
    budget: &mut Budget<'_>,
    depth: usize,
) -> Result<Value, InferenceError> {
    budget.depth(depth)?;
    budget.allocate(path.len().saturating_add(512))?;
    let mut groups: [Vec<&Value>; 6] = std::array::from_fn(|_| Vec::new());
    let mut types = BTreeMap::<&str, usize>::new();
    for value in values {
        budget.visit(1)?;
        budget.allocate(64)?;
        let (rank, name) = match value {
            Value::Null => (0, "null"),
            Value::Bool(_) => (1, "boolean"),
            Value::Integer(_) => (2, "integer"),
            Value::Float(_) => (2, "number"),
            Value::Text(_) => (3, "string"),
            Value::Array(_) => (4, "array"),
            Value::Map(_) => (5, "object"),
            Value::Bytes(_) => return Err(InferenceError::Shape("JSON sample")),
        };
        groups[rank]
            .try_reserve(1)
            .map_err(|_| InferenceError::AllocationFailed)?;
        groups[rank].push(value);
        *types.entry(name).or_default() += 1;
    }
    let types = Value::Map(Map::try_from_entries(
        types
            .into_iter()
            .map(|(key, count)| Ok((key.into(), wire::number(count)?)))
            .collect::<Result<Vec<_>, InferenceError>>()?,
    )?);
    observations.insert(
        path.into(),
        wire::object(vec![
            ("present", wire::number(values.len())?),
            ("total", wire::number(total)?),
            ("types", types),
        ])?,
    );
    let mut alternatives = Vec::new();
    for (rank, group) in groups.into_iter().enumerate() {
        if group.is_empty() {
            continue;
        }
        budget.allocate(512)?;
        let schema = match rank {
            0 => type_schema("null")?,
            1 => type_schema("boolean")?,
            2 => type_schema(
                if group.iter().all(|value| matches!(value, Value::Integer(_))) {
                    "integer"
                } else {
                    "number"
                },
            )?,
            3 => type_schema("string")?,
            4 => {
                let mut items = Vec::new();
                for array in group {
                    let Value::Array(values) = array else {
                        unreachable!("array group")
                    };
                    budget.allocate(values.len().saturating_mul(64))?;
                    items
                        .try_reserve(values.len())
                        .map_err(|_| InferenceError::AllocationFailed)?;
                    items.extend(values);
                }
                let item_path = child_path(path, "items", None, budget)?;
                let item = merge(
                    &items,
                    items.len(),
                    &item_path,
                    observations,
                    budget,
                    depth + 1,
                )?;
                wire::object(vec![("type", Value::Text("array".into())), ("items", item)])?
            }
            5 => {
                let count = group.len();
                let mut properties = BTreeMap::<&str, Vec<&Value>>::new();
                for object in group {
                    let Value::Map(fields) = object else {
                        unreachable!("object group")
                    };
                    for (key, value) in fields.iter() {
                        budget.visit(key.len().saturating_add(1))?;
                        budget.allocate(key.len().saturating_add(128))?;
                        let field = properties.entry(key).or_default();
                        field
                            .try_reserve(1)
                            .map_err(|_| InferenceError::AllocationFailed)?;
                        field.push(value);
                    }
                }
                let mut fields = Vec::new();
                let mut required = Vec::new();
                for (key, values) in properties {
                    let field_path = child_path(path, "properties", Some(key), budget)?;
                    let schema =
                        merge(&values, count, &field_path, observations, budget, depth + 1)?;
                    fields
                        .try_reserve(1)
                        .map_err(|_| InferenceError::AllocationFailed)?;
                    if values.len() == count {
                        required
                            .try_reserve(1)
                            .map_err(|_| InferenceError::AllocationFailed)?;
                        required.push(Value::Text(key.into()));
                    }
                    fields.push((key.into(), schema));
                }
                wire::object(vec![
                    ("type", Value::Text("object".into())),
                    ("additionalProperties", Value::Bool(true)),
                    ("properties", Value::Map(Map::try_from_entries(fields)?)),
                    ("required", Value::Array(required)),
                ])?
            }
            _ => unreachable!("fixed type family"),
        };
        alternatives
            .try_reserve(1)
            .map_err(|_| InferenceError::AllocationFailed)?;
        alternatives.push(schema);
    }
    Ok(match alternatives.len() {
        0 => Value::Bool(true),
        1 => alternatives.pop().expect("one alternative"),
        _ => wire::object(vec![("anyOf", Value::Array(alternatives))])?,
    })
}

fn type_schema(name: &str) -> Result<Value, InferenceError> {
    wire::object(vec![("type", Value::Text(name.into()))])
}
fn child_path(
    path: &str,
    kind: &str,
    name: Option<&str>,
    budget: &mut Budget<'_>,
) -> Result<String, InferenceError> {
    let length = path
        .len()
        .saturating_add(kind.len())
        .saturating_add(name.map_or(0, |name| name.len().saturating_mul(2)))
        .saturating_add(2);
    budget.allocate(length.saturating_mul(4))?;
    budget.visit(length)?;
    let mut result = format!("{path}/{kind}");
    if let Some(name) = name {
        result.push('/');
        result.push_str(&name.replace('~', "~0").replace('/', "~1"));
    }
    Ok(result)
}

pub(super) fn difference(
    old: &Value,
    new: &Value,
    path: &str,
    paths: &mut Vec<String>,
    budget: &mut Budget<'_>,
    depth: usize,
) -> Result<(), InferenceError> {
    budget.depth(depth)?;
    budget.visit(1)?;
    if let (Value::Map(old), Value::Map(new)) = (old, new) {
        let mut keys = BTreeSet::new();
        for (key, _) in old.iter().chain(new.iter()) {
            budget.allocate(key.len().saturating_add(64))?;
            budget.visit(key.len().saturating_add(1))?;
            keys.insert(key);
        }
        for key in keys {
            let child = child_path(path, key, None, budget)?;
            // Escape the actual schema member rather than the structural-merge token.
            let child = if key.contains(['~', '/']) {
                child_path(
                    path,
                    &key.replace('~', "~0").replace('/', "~1"),
                    None,
                    budget,
                )?
            } else {
                child
            };
            match (old.get(key), new.get(key)) {
                (Some(old), Some(new)) => difference(old, new, &child, paths, budget, depth + 1)?,
                _ => {
                    paths
                        .try_reserve(1)
                        .map_err(|_| InferenceError::AllocationFailed)?;
                    paths.push(child);
                }
            }
        }
    } else {
        // Arrays are atomic schema terms; charge every descendant before equality.
        charge_value(old, budget, depth)?;
        charge_value(new, budget, depth)?;
        if old != new {
            budget.allocate(path.len().saturating_add(64))?;
            paths
                .try_reserve(1)
                .map_err(|_| InferenceError::AllocationFailed)?;
            paths.push(path.into());
        }
    }
    Ok(())
}
fn charge_value(
    value: &Value,
    budget: &mut Budget<'_>,
    depth: usize,
) -> Result<(), InferenceError> {
    budget.depth(depth)?;
    budget.visit(1)?;
    match value {
        Value::Array(values) => {
            for value in values {
                charge_value(value, budget, depth + 1)?;
            }
        }
        Value::Map(values) => {
            for (key, value) in values.iter() {
                budget.visit(key.len())?;
                charge_value(value, budget, depth + 1)?;
            }
        }
        Value::Text(value) => budget.visit(value.len())?,
        _ => (),
    }
    Ok(())
}
