//! Actual-value constraints and typed projections for checked expression execution.

use crate::{
    EvaluationError as Error, EvaluationMeter, EvaluationResult, EvaluationUsage,
    EvaluationValue as ResultValue, EvaluatorLimits, JsonDocument, JsonError, McpValidationError,
    NativeSchemas, PathStep, PrimitiveType as P, TypeContext, ValueType as T, ValueTypeKind as K,
};
use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;

/// Checks a native value against an ordinary HTLK type, without coercion.
/// Records allow extra fields; only declared optional members may be absent.
///
/// # Errors
/// Returns incompatible value/type, required native schema support, or limits.
pub fn validate_typed_value(
    value: &Value,
    ty: &T,
    schemas: Option<&NativeSchemas>,
    codec: &Limits,
    policy: &EvaluatorLimits,
) -> Result<EvaluationUsage, Error> {
    ty.to_value(TypeContext::Value, codec)?;
    let mut meter = EvaluationMeter::new(policy, codec)?;
    require(value, ty, schemas, &mut meter)?;
    Ok(meter.usage())
}
/// Projects a checked value according to its declared type. Union projection
/// considers matching variants, so undeclared extra fields cannot leak through
/// a variant that does not declare the requested member.
///
/// # Errors
/// Returns type/projection/absence errors, unresolved schema projection, or limits.
pub fn project_typed_value(
    value: &Value,
    ty: &T,
    path: &[PathStep],
    schemas: Option<&NativeSchemas>,
    codec: &Limits,
    policy: &EvaluatorLimits,
) -> Result<EvaluationResult, Error> {
    ty.to_value(TypeContext::Value, codec)?;
    let mut meter = EvaluationMeter::new(policy, codec)?;
    let result = project(value, ty, path, schemas, &mut meter)?;
    Ok(EvaluationResult {
        value: result,
        usage: meter.usage(),
    })
}
pub(crate) fn require(
    value: &Value,
    ty: &T,
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<(), Error> {
    if matches_type(value, ty, schemas, meter, 0)? {
        Ok(())
    } else {
        Err(Error::OperandType)
    }
}
fn matches_type(
    value: &Value,
    ty: &T,
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
    depth: usize,
) -> Result<bool, Error> {
    if depth > meter.program_limits().max_depth {
        return Err(Error::Limit("value validation depth"));
    }
    meter.charge(1)?;
    meter.inspect(value)?;
    match ty.kind() {
        K::Primitive(p) => matches_primitive(value, *p, schemas, meter, depth),
        K::List(item) => {
            let Value::Array(items) = value else {
                return Ok(false);
            };
            for value in items {
                meter.visit(1)?;
                if !matches_type(value, item, schemas, meter, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        K::Map(item) => {
            let Value::Map(items) = value else {
                return Ok(false);
            };
            for (_, value) in items.iter() {
                meter.visit(1)?;
                if !matches_type(value, item, schemas, meter, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        K::Record(fields) => {
            let Value::Map(map) = value else {
                return Ok(false);
            };
            for (name, port) in fields {
                meter.visit(1)?;
                meter.charge(name.len() as u64)?;
                match map.get(name) {
                    Some(value) => {
                        if !matches_type(value, port.value_type(), schemas, meter, depth + 1)? {
                            return Ok(false);
                        }
                    }
                    None if port.required() => return Ok(false),
                    None => (),
                }
            }
            Ok(true)
        }
        K::Enum(variants) => {
            let Value::Text(text) = value else {
                return Ok(false);
            };
            let levels = u64::from((usize::BITS - variants.len().leading_zeros()).max(1));
            meter.charge((text.len() as u64).saturating_mul(levels))?;
            Ok(variants.binary_search(text).is_ok())
        }
        K::Union(types) => {
            for ty in types {
                meter.visit(1)?;
                if matches_type(value, ty, schemas, meter, depth + 1)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        K::Schema(id) => {
            let native = schemas.ok_or(Error::UnresolvedType)?;
            let Some(document) = json_value(value, meter)? else {
                return Ok(false);
            };
            Ok(native.validate_schema_document(id, &document, meter.program_limits())?)
        }
        K::Var(_) | K::Function { .. } => Err(Error::UnresolvedType),
    }
}
// Keep protocol/regex temporaries out of recursive collection validation frames.
fn matches_primitive(
    value: &Value,
    p: P,
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
    depth: usize,
) -> Result<bool, Error> {
    match p {
        P::String => Ok(matches!(value, Value::Text(_))),
        P::Integer => Ok(matches!(value, Value::Integer(_))),
        P::Float => Ok(matches!(value, Value::Float(_))),
        P::Boolean => Ok(matches!(value, Value::Bool(_))),
        P::Null => Ok(matches!(value, Value::Null)),
        P::Bytes => Ok(matches!(value, Value::Bytes(_))),
        P::Json => Ok(json_value(value, meter)?.is_some()),
        P::Regex => {
            let Value::Map(m) = value else {
                return Ok(false);
            };
            let (Some(Value::Text(pattern)), Some(Value::Text(flags))) =
                (m.get("pattern"), m.get("flags"))
            else {
                return Ok(false);
            };
            if m.len() != 2 {
                return Ok(false);
            }
            match crate::evaluate::compile_regex(pattern, flags, meter) {
                Ok(()) => Ok(true),
                Err(Error::Regex) => Ok(false),
                Err(e) => Err(e),
            }
        }
        P::Error => {
            let shape = crate::builtin_record_type(p, meter.program_limits())?
                .ok_or(Error::UnresolvedType)?;
            matches_type(value, &shape, schemas, meter, depth + 1)
        }
        P::ResourceSnapshot => {
            inspect_collections(value, meter)?;
            match crate::mcp_protocol::snapshot_shape(value, meter.codec_limits()) {
                Ok(_) => Ok(true),
                Err(McpValidationError::SnapshotShape | McpValidationError::SnapshotIdentity) => {
                    Ok(false)
                }
                Err(McpValidationError::Codec(e)) => Err(e.into()),
                _ => Err(Error::OperandType),
            }
        }
        P::McpPromptResult => {
            let Some(json) = json_value(value, meter)? else {
                return Ok(false);
            };
            match crate::validate_mcp_prompt_result(&json, meter.program_limits()) {
                Ok(()) => Ok(true),
                Err(McpValidationError::PromptResult | McpValidationError::BinaryEncoding) => {
                    Ok(false)
                }
                Err(McpValidationError::BinaryLimit) => Err(Error::Limit("binary bytes")),
                Err(McpValidationError::Native(e)) => Err(e.into()),
                Err(McpValidationError::Codec(e)) => Err(e.into()),
                _ => Err(Error::OperandType),
            }
        }
    }
}
pub(crate) fn json_value(
    value: &Value,
    meter: &mut EvaluationMeter<'_>,
) -> Result<Option<JsonDocument>, Error> {
    inspect_collections(value, meter)?;
    match JsonDocument::from_value(value, meter.program_limits()) {
        Ok(value) => {
            meter.charge(value.as_bytes().len() as u64)?;
            Ok(Some(value))
        }
        Err(JsonError::UnsafeNumber | JsonError::UnsupportedValue) => Ok(None),
        Err(JsonError::Codec(e)) => Err(e.into()),
        Err(JsonError::LimitExceeded { .. }) => Err(Error::Limit("JSON value")),
        Err(_) => Err(Error::OperandType),
    }
}
pub(crate) fn inspect_collections(
    value: &Value,
    meter: &mut EvaluationMeter<'_>,
) -> Result<(), Error> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        meter.charge(1)?;
        match value {
            Value::Array(values) => {
                meter.visit(values.len() as u64)?;
                pending
                    .try_reserve(values.len())
                    .map_err(|_| Error::AllocationFailed)?;
                pending.extend(values);
            }
            Value::Map(values) => {
                meter.visit(values.len() as u64)?;
                pending
                    .try_reserve(values.len())
                    .map_err(|_| Error::AllocationFailed)?;
                pending.extend(values.iter().map(|(_, value)| value));
            }
            _ => (),
        }
    }
    Ok(())
}
fn copy_type(ty: &T, meter: &mut EvaluationMeter<'_>) -> Result<T, Error> {
    let bytes = ty.encode(TypeContext::Value, meter.program_limits())?;
    meter.charge(bytes.len() as u64)?;
    Ok(ty.clone())
}
enum Selection<'a> {
    Present(&'a Value),
    Absent,
    Invalid,
}

pub(crate) fn project(
    value: &Value,
    ty: &T,
    path: &[PathStep],
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<ResultValue, Error> {
    if let K::Schema(schema) = ty.kind() {
        return crate::schema_projection::project(
            schemas.ok_or(Error::UnresolvedType)?,
            *schema,
            value,
            path,
            meter,
        );
    }
    if let K::Union(members) = ty.kind()
        && members
            .iter()
            .any(|member| matches!(member.kind(), K::Schema(_)))
    {
        return project_schema_members(value, members, path, schemas, meter);
    }
    require(value, ty, schemas, meter)?;
    let mut value = value;
    let mut types = vec![copy_type(ty, meter)?];
    for (index, step) in path.iter().enumerate() {
        if types.iter().any(contains_schema_head) {
            return project_schema_members(value, &types, &path[index..], schemas, meter);
        }
        meter.visit(1)?;
        let mut selected = None;
        let mut next_types = Vec::new();
        let mut absent = false;
        for ty in &types {
            projected_types(ty, step, &mut next_types, meter, 0)?;
        }
        if next_types.is_empty() {
            return Err(Error::InvalidProjection);
        }
        for ty in &types {
            if !matches_type(value, ty, schemas, meter, 0)? {
                continue;
            }
            match select(value, ty, step, schemas, meter, types.len() > 1, 0)? {
                Selection::Present(v) => {
                    selected = Some(v);
                }
                Selection::Absent => absent = true,
                Selection::Invalid => (),
            }
        }
        if let Some(next) = selected {
            value = next;
            types = next_types;
        } else if absent && index + 1 == path.len() {
            return Ok(ResultValue::Absent);
        } else if absent {
            return Err(Error::AbsentOperand);
        } else {
            return Err(Error::InvalidProjection);
        }
    }
    Ok(ResultValue::Present(meter.copy_value(value)?))
}
fn contains_schema_head(ty: &T) -> bool {
    match ty.kind() {
        K::Schema(_) => true,
        K::Union(members) => members
            .iter()
            .any(|member| matches!(member.kind(), K::Schema(_))),
        _ => false,
    }
}
fn project_schema_members(
    value: &Value,
    members: &[T],
    path: &[PathStep],
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<ResultValue, Error> {
    let mut present = None;
    let mut absent = false;
    let mut absence_error = false;
    for member in members {
        meter.visit(1)?;
        if !matches_type(value, member, schemas, meter, 0)? {
            continue;
        }
        match project(value, member, path, schemas, meter) {
            Ok(ResultValue::Present(value)) => present = Some(value),
            Ok(ResultValue::Absent) => absent = true,
            Err(Error::InvalidProjection) => (),
            Err(Error::AbsentOperand) => absence_error = true,
            Err(error) => return Err(error),
            Ok(ResultValue::Pending) => return Err(Error::OperandType),
        }
    }
    if let Some(value) = present {
        Ok(ResultValue::Present(value))
    } else if absent {
        Ok(ResultValue::Absent)
    } else if absence_error {
        Err(Error::AbsentOperand)
    } else {
        Err(Error::InvalidProjection)
    }
}
fn projected_types(
    ty: &T,
    step: &PathStep,
    out: &mut Vec<T>,
    meter: &mut EvaluationMeter<'_>,
    depth: usize,
) -> Result<(), Error> {
    if depth > meter.program_limits().max_depth {
        return Err(Error::Limit("projection depth"));
    }
    meter.visit(1)?;
    let child = match (ty.kind(), step) {
        (K::Record(fields), PathStep::Field(name)) => {
            let mut child = None;
            for (key, port) in fields {
                meter.visit(1)?;
                meter.charge(key.len() as u64)?;
                if key == name {
                    child = Some(port.value_type());
                    break;
                }
            }
            child
        }
        (K::Map(item), PathStep::Field(_)) | (K::List(item), PathStep::Index(_)) => {
            Some(item.as_ref())
        }
        (K::Primitive(P::Json), _) => Some(ty),
        (K::Union(types), _) => {
            for ty in types {
                projected_types(ty, step, out, meter, depth + 1)?;
            }
            None
        }
        (K::Primitive(p), _) => {
            if let Some(record) = crate::builtin_record_type(*p, meter.program_limits())? {
                projected_types(&record, step, out, meter, depth + 1)?;
            }
            None
        }
        (K::Schema(_), _) => return Err(Error::UnresolvedType),
        _ => None,
    };
    if let Some(child) = child {
        out.try_reserve(1).map_err(|_| Error::AllocationFailed)?;
        out.push(copy_type(child, meter)?);
    }
    Ok(())
}
fn select<'a>(
    value: &'a Value,
    ty: &T,
    step: &PathStep,
    schemas: Option<&NativeSchemas>,
    meter: &mut EvaluationMeter<'_>,
    union_variant: bool,
    depth: usize,
) -> Result<Selection<'a>, Error> {
    if depth > meter.program_limits().max_depth {
        return Err(Error::Limit("projection depth"));
    }
    meter.charge(1)?;
    match ty.kind() {
        K::Union(types) => {
            let mut selected = None;
            let mut absent = false;
            for ty in types {
                meter.visit(1)?;
                if !matches_type(value, ty, schemas, meter, depth + 1)? {
                    continue;
                }
                match select(value, ty, step, schemas, meter, true, depth + 1)? {
                    Selection::Present(v) => {
                        selected = Some(v);
                    }
                    Selection::Absent => absent = true,
                    Selection::Invalid => (),
                }
            }
            Ok(if let Some(v) = selected {
                Selection::Present(v)
            } else if absent {
                Selection::Absent
            } else {
                Selection::Invalid
            })
        }
        K::Record(fields) => {
            let (Value::Map(map), PathStep::Field(name)) = (value, step) else {
                return Ok(Selection::Invalid);
            };
            let levels = u64::from((usize::BITS - fields.len().leading_zeros()).max(1));
            meter.charge((name.len() as u64).saturating_mul(levels))?;
            let field = fields
                .binary_search_by(|(key, _)| key.len().cmp(&name.len()).then_with(|| key.cmp(name)))
                .ok()
                .map(|i| &fields[i].1);
            let Some(field) = field else {
                return Ok(if union_variant {
                    Selection::Absent
                } else {
                    Selection::Invalid
                });
            };
            Ok(match map.get(name) {
                Some(value) => Selection::Present(value),
                None if !field.required() => Selection::Absent,
                None => Selection::Invalid,
            })
        }
        K::Map(_) => {
            let (Value::Map(map), PathStep::Field(name)) = (value, step) else {
                return Ok(Selection::Invalid);
            };
            meter.charge(
                (name.len() as u64)
                    .saturating_mul(u64::from((usize::BITS - map.len().leading_zeros()).max(1))),
            )?;
            Ok(match map.get(name) {
                Some(value) => Selection::Present(value),
                None => Selection::Invalid,
            })
        }
        K::List(_) => {
            let (Value::Array(values), PathStep::Index(index)) = (value, step) else {
                return Ok(Selection::Invalid);
            };
            Ok(
                match usize::try_from(*index).ok().and_then(|i| values.get(i)) {
                    Some(value) => Selection::Present(value),
                    None => Selection::Invalid,
                },
            )
        }
        K::Primitive(P::Json) => {
            let selected = match (value, step) {
                (Value::Map(m), PathStep::Field(name)) => {
                    meter.charge((name.len() as u64).saturating_mul(u64::from(
                        (usize::BITS - m.len().leading_zeros()).max(1),
                    )))?;
                    m.get(name)
                }
                (Value::Array(a), PathStep::Index(i)) => {
                    usize::try_from(*i).ok().and_then(|i| a.get(i))
                }
                _ => None,
            };
            Ok(match selected {
                Some(value) => Selection::Present(value),
                None => Selection::Invalid,
            })
        }
        K::Primitive(p) => {
            let Some(record) = crate::builtin_record_type(*p, meter.program_limits())? else {
                return Ok(Selection::Invalid);
            };
            select(
                value,
                &record,
                step,
                schemas,
                meter,
                union_variant,
                depth + 1,
            )
        }
        K::Schema(_) => Err(Error::UnresolvedType),
        _ => Ok(Selection::Invalid),
    }
}
