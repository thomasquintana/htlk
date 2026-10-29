//! Explicit host-linked, metered content access and checked JSON utilities.

use crate::{
    EvaluationArgument as A, EvaluationError as E, EvaluationMeter, EvaluationValue as V,
    NativeCallContext, NativeFunction, NativeFunctionImpl, NativeRegistry, NativeRegistryError,
};
use htlk_executable::{
    BuiltinType as P, FunctionSignature, Identifier, JsonDocument, JsonPointer, Library, Port,
    TypeContext, ValueType as T, ValueTypeKind as K,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

/// Builds the complete source-pinned `htlk.mcp-content` version-1 manifest.
/// This does not link implementations or grant their use to an executable.
///
/// # Errors
/// Returns bounded type/signature/manifest representation failures.
pub fn mcp_content_library(limits: &Limits) -> Result<Library, NativeRegistryError> {
    let records = functions(limits)?;
    let mut hash = Sha256::new();
    hash.update(b"htlk.mcp-content/1\n");
    for source in [
        include_bytes!("mcp_content.rs").as_slice(),
        htlk_executable::implementation_digest().as_bytes(),
    ] {
        hash.update((source.len() as u64).to_be_bytes());
        hash.update(source);
    }
    Ok(Library::new(
        "htlk.mcp-content".into(),
        "1".into(),
        Digest::from_bytes(hash.finalize().into()),
        records
            .into_iter()
            .map(|(name, signature, _)| (name, signature))
            .collect(),
        limits,
    )?)
}

/// Explicitly links the complete shipped pure content library and returns its manifest.
/// The empty registry remains empty until a trusted host calls this function.
///
/// # Errors
/// Returns duplicate linking, incomplete implementation or bounded metadata failures.
pub fn link_mcp_content_library(
    registry: &mut NativeRegistry,
    limits: &Limits,
) -> Result<Library, NativeRegistryError> {
    let manifest = mcp_content_library(limits)?;
    let implementations = functions(limits)?
        .into_iter()
        .map(|(name, _, implementation)| Ok((name, NativeFunction::new(implementation, 1)?)))
        .collect::<Result<BTreeMap<_, _>, NativeRegistryError>>()?;
    registry.register(manifest.clone(), implementations)?;
    Ok(manifest)
}

fn functions(
    limits: &Limits,
) -> Result<Vec<(Identifier, FunctionSignature, NativeFunctionImpl)>, NativeRegistryError> {
    let port = |kind, required| Port::new(T::builtin(kind), required);
    let list = |kind| {
        T::new(
            K::List(Box::new(T::builtin(kind))),
            TypeContext::Signature,
            limits,
        )
        .map_err(htlk_executable::MetadataError::from)
    };
    let mcp = port(P::McpToolResult, true);
    let json = port(P::Json, true);
    let string = port(P::String, true);
    let mut result = Vec::new();
    for (name, parameters, returns, implementation) in [
        (
            "text_blocks",
            vec![mcp.clone()],
            Port::new(list(P::String)?, true),
            text_blocks as NativeFunctionImpl,
        ),
        (
            "content_kinds",
            vec![mcp.clone()],
            Port::new(list(P::String)?, true),
            content_kinds as NativeFunctionImpl,
        ),
        (
            "structured_content",
            vec![mcp.clone()],
            port(P::Json, false),
            structured_content as NativeFunctionImpl,
        ),
        (
            "candidates",
            vec![mcp.clone()],
            Port::new(list(P::Json)?, true),
            candidates as NativeFunctionImpl,
        ),
        (
            "data",
            vec![mcp.clone()],
            port(P::Json, false),
            data as NativeFunctionImpl,
        ),
        (
            "validation",
            vec![mcp.clone()],
            json.clone(),
            validation as NativeFunctionImpl,
        ),
        (
            "select_candidate",
            vec![mcp, string.clone()],
            port(P::Json, false),
            select_candidate as NativeFunctionImpl,
        ),
        (
            "parse_json",
            vec![string.clone()],
            json.clone(),
            parse_json as NativeFunctionImpl,
        ),
        (
            "json_at",
            vec![json.clone(), string.clone()],
            port(P::Json, false),
            json_at as NativeFunctionImpl,
        ),
        (
            "join_text",
            vec![Port::new(list(P::String)?, true), string.clone()],
            string.clone(),
            join_text as NativeFunctionImpl,
        ),
        (
            "text_contains",
            vec![string.clone(), string.clone()],
            port(P::Boolean, true),
            text_contains as NativeFunctionImpl,
        ),
        (
            "regex_capture",
            vec![string, port(P::Regex, true), port(P::Integer, true)],
            port(P::String, false),
            regex_capture as NativeFunctionImpl,
        ),
    ] {
        result.push((
            name.parse().expect("fixed function identifier"),
            FunctionSignature::new(vec![], parameters, returns, limits)?,
            implementation,
        ));
    }
    let variable: Identifier = "t".parse().expect("fixed variable");
    let returns = Port::new(
        T::new(K::Var(variable.clone()), TypeContext::Signature, limits)
            .map_err(htlk_executable::MetadataError::from)?,
        true,
    );
    result.push((
        "checked".parse().expect("fixed function identifier"),
        FunctionSignature::new(vec![variable], vec![json], returns, limits)?,
        checked,
    ));
    Ok(result)
}
fn value(arguments: &[A], index: usize) -> Result<&Value, E> {
    match arguments.get(index) {
        Some(A::Value(V::Present(value))) => Ok(value),
        _ => Err(E::OperandType),
    }
}
fn map(value: &Value) -> Result<&Map, E> {
    if let Value::Map(value) = value {
        Ok(value)
    } else {
        Err(E::OperandType)
    }
}
fn text(value: &Value) -> Result<&str, E> {
    if let Value::Text(value) = value {
        Ok(value)
    } else {
        Err(E::OperandType)
    }
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, E> {
    map(value)?.get(name).ok_or(E::InvalidProjection)
}
fn array(value: &Value) -> Result<&[Value], E> {
    if let Value::Array(value) = value {
        Ok(value)
    } else {
        Err(E::OperandType)
    }
}
fn optional(value: Option<&Value>, meter: &mut EvaluationMeter<'_>) -> Result<V, E> {
    value.map_or(Ok(V::Absent), |value| {
        Ok(V::Present(meter.copy_value(value)?))
    })
}
fn content(arguments: &[A]) -> Result<&[Value], E> {
    array(field(
        field(value(arguments, 0)?, "protocol_result")?,
        "content",
    )?)
}
fn text_blocks(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let blocks = content(arguments)?;
    meter.visit(blocks.len() as u64)?;
    let mut values = Vec::new();
    for block in blocks {
        if text(field(block, "type")?)? == "text" {
            values.try_reserve(1).map_err(|_| E::AllocationFailed)?;
            values.push(meter.copy_value(field(block, "text")?)?);
        }
    }
    Ok(V::Present(meter.copy_value(&Value::Array(values))?))
}
fn content_kinds(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let blocks = content(arguments)?;
    meter.visit(blocks.len() as u64)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(blocks.len())
        .map_err(|_| E::AllocationFailed)?;
    for block in blocks {
        values.push(meter.copy_value(field(block, "type")?)?);
    }
    Ok(V::Present(meter.copy_value(&Value::Array(values))?))
}
fn structured_content(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    optional(
        map(field(value(arguments, 0)?, "protocol_result")?)?.get("structuredContent"),
        meter,
    )
}
fn candidates(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    Ok(V::Present(meter.copy_value(field(
        field(value(arguments, 0)?, "extraction")?,
        "candidates",
    )?)?))
}
fn data(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    optional(map(value(arguments, 0)?)?.get("data"), meter)
}
fn validation(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    Ok(V::Present(
        meter.copy_value(field(value(arguments, 0)?, "validation")?)?,
    ))
}
fn select_candidate(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let requested = text(value(arguments, 1)?)?;
    let candidates = array(field(
        field(value(arguments, 0)?, "extraction")?,
        "candidates",
    )?)?;
    meter.visit(candidates.len() as u64)?;
    for candidate in candidates {
        let id = text(field(candidate, "id")?)?;
        meter.charge(id.len().saturating_add(requested.len()) as u64)?;
        if id == requested {
            return Ok(V::Present(meter.copy_value(field(candidate, "value")?)?));
        }
    }
    Ok(V::Absent)
}
fn parse_json(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let input = text(value(arguments, 0)?)?;
    meter.charge((input.len() as u64).saturating_mul(8))?;
    let document = JsonDocument::new(input.as_bytes(), meter.program_limits())?;
    Ok(V::Present(meter.copy_value(document.value())?))
}
fn json_at(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let input = value(arguments, 0)?;
    let pointer = text(value(arguments, 1)?)?;
    let levels = (usize::BITS - meter.codec_limits().max_document_bytes.leading_zeros()).max(1);
    meter.charge(
        (pointer.len() as u64)
            .saturating_add(1)
            .saturating_mul(u64::from(levels)),
    )?;
    let pointer = JsonPointer::new(pointer, meter.program_limits())?;
    meter.visit(pointer.tokens().len() as u64)?;
    match pointer.resolve_value(input) {
        Ok(value) => Ok(V::Present(meter.copy_value(value)?)),
        Err(htlk_executable::JsonPointerError::MissingTarget { .. }) => Ok(V::Absent),
        Err(error) => Err(error.into()),
    }
}
fn checked(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    // The checked registry enforces the concrete expected generic return type.
    Ok(V::Present(meter.copy_value(value(arguments, 0)?)?))
}
fn join_text(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let values = array(value(arguments, 0)?)?;
    let separator = text(value(arguments, 1)?)?;
    meter.visit(values.len() as u64)?;
    let mut size = separator
        .len()
        .checked_mul(values.len().saturating_sub(1))
        .ok_or(E::Limit("joined text"))?;
    for value in values {
        size = size
            .checked_add(text(value)?.len())
            .ok_or(E::Limit("joined text"))?;
    }
    let header = match size {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=4294967295 => 5,
        _ => 9,
    };
    if size
        .checked_add(header)
        .is_none_or(|size| size > meter.codec_limits().max_document_bytes)
    {
        return Err(E::Limit("joined text"));
    }
    meter.charge(size as u64)?;
    let mut output = String::new();
    output
        .try_reserve_exact(size)
        .map_err(|_| E::AllocationFailed)?;
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push_str(separator);
        }
        output.push_str(text(value)?);
    }
    Ok(V::Present(meter.copy_value(&Value::Text(output))?))
}
fn text_contains(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let input = text(value(arguments, 0)?)?;
    let needle = text(value(arguments, 1)?)?;
    meter.charge(input.len().saturating_add(needle.len()) as u64)?;
    Ok(V::Present(Value::Bool(input.contains(needle))))
}
fn regex_capture(
    arguments: &[A],
    _: &NativeCallContext<'_>,
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, E> {
    let input = text(value(arguments, 0)?)?;
    let expression = value(arguments, 1)?;
    let pattern = text(field(expression, "pattern")?)?;
    let flags = text(field(expression, "flags")?)?;
    let Value::Integer(index) = value(arguments, 2)? else {
        return Err(E::OperandType);
    };
    let index = usize::try_from(*index).map_err(|_| E::InvalidProjection)?;
    meter.charge(
        (input.len() as u64)
            .saturating_add(1)
            .saturating_mul((pattern.len() as u64).saturating_add(1))
            .saturating_mul(2),
    )?;
    let expression = crate::evaluate::compile_regex_for_match(pattern, flags, meter)?;
    if index >= expression.captures_len() {
        return Err(E::InvalidProjection);
    }
    let mut matches = expression.captures_iter(input);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(E::OperandType);
    }
    match first.and_then(|captures| captures.get(index)) {
        Some(value) => Ok(V::Present(
            meter.copy_value(&Value::Text(value.as_str().into()))?,
        )),
        None => Ok(V::Absent),
    }
}
