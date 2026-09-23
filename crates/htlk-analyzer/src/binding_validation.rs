//! Cross-record MCP selection, extracted schema, and tool-interface integrity.

use htlk_cbor::{Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use std::collections::{BTreeMap, BTreeSet};

use crate::digest::Digest;
use crate::{
    BuiltinType, DocumentError as Error, DocumentFields, JsonDocument, McpBinding,
    McpBindingKind as Kind, NodeFields, PortTable, ValueTypeKind,
};

pub(crate) fn descriptor(
    binding: &McpBinding,
    f: &DocumentFields,
    l: &Limits,
) -> Result<(), Error> {
    let json = f
        .documents
        .get(&binding.descriptor())
        .ok_or(Error::MissingRecord("descriptor document"))?;
    let Value::Map(m) = json.value() else {
        return Err(Error::InvalidDescriptor("object"));
    };
    let (key, selected) = match binding.kind() {
        Kind::Tool { name, .. } | Kind::Prompt { name } => ("name", name),
        Kind::Resource { uri } => ("uri", uri),
        Kind::Template { uri_template } => ("uriTemplate", uri_template),
    };
    match m.get(key) {
        Some(Value::Text(actual)) if actual == selected => (),
        _ => return Err(Error::InvalidDescriptor(key)),
    }
    if matches!(binding.kind(), Kind::Prompt { .. }) {
        prompt_arguments(m)?;
    }
    if let Kind::Tool {
        input_schema,
        output_schema,
        ..
    } = binding.kind()
    {
        for (key, id) in [
            ("inputSchema", input_schema),
            ("outputSchema", output_schema),
        ] {
            let stored = f
                .documents
                .get(id)
                .ok_or(Error::MissingRecord("tool schema document"))?;
            let schema = m.get(key).ok_or(Error::InvalidDescriptor(key))?;
            // A callable schema must establish an object-root constraint. Full
            // admission (including reference chains) belongs to schema resolution.
            if !matches!(schema, Value::Map(_)) {
                return Err(Error::InvalidDescriptor(key));
            }
            let extracted = JsonDocument::from_value(schema, l)?;
            if extracted.digest() != *id || extracted.as_bytes() != stored.as_bytes() {
                return Err(Error::ToolSchemaMismatch(key));
            }
        }
    }
    Ok(())
}

pub(crate) fn ports(n: &NodeFields, binding: &McpBinding, f: &DocumentFields) -> Result<(), Error> {
    if let Kind::Tool {
        input_schema,
        output_schema,
        ..
    } = binding.kind()
    {
        schema_port(&n.inputs, "arguments", *input_schema)?;
        schema_port(&n.outputs, "value", *output_schema)?;
    }
    match binding.kind() {
        Kind::Resource { .. } => {
            if !n.inputs.is_empty() {
                return Err(Error::McpInterfaceMismatch);
            }
            builtin_output(&n.outputs, BuiltinType::McpResourceResult)?;
        }
        Kind::Prompt { .. } => {
            builtin_output(&n.outputs, BuiltinType::McpPromptResult)?;
            let port = n
                .inputs
                .get("arguments")
                .filter(|p| p.required())
                .ok_or(Error::McpInterfaceMismatch)?;
            if n.inputs.len() != 1 {
                return Err(Error::McpInterfaceMismatch);
            }
            let ValueTypeKind::Record(fields) = port.value_type().kind() else {
                return Err(Error::McpInterfaceMismatch);
            };
            let descriptor = f
                .documents
                .get(&binding.descriptor())
                .ok_or(Error::MissingRecord("descriptor document"))?;
            let Value::Map(m) = descriptor.value() else {
                return Err(Error::InvalidDescriptor("object"));
            };
            // Bound repeated-use work by each node's encoded argument fields.
            // The descriptor itself is fully checked once by descriptor().
            let count = argument_array(m)?.len();
            if fields.len() != count {
                return Err(Error::McpInterfaceMismatch);
            }
            let expected = prompt_arguments(m)?;
            for (name, port) in fields {
                if expected.get(name.as_str()) != Some(&port.required())
                    || !matches!(
                        port.value_type().kind(),
                        ValueTypeKind::Builtin(BuiltinType::String)
                    )
                {
                    return Err(Error::McpInterfaceMismatch);
                }
            }
        }
        _ => (),
    }
    Ok(())
}
fn builtin_output(table: &PortTable, ty: BuiltinType) -> Result<(), Error> {
    if table.len() == 1
        && table.get("value").is_some_and(|p| {
            p.required()
                && matches!(p.value_type().kind(), ValueTypeKind::Builtin(actual) if *actual == ty)
        })
    {
        Ok(())
    } else {
        Err(Error::McpInterfaceMismatch)
    }
}
pub(crate) fn template_ports(n: &NodeFields, variables: &BTreeSet<&str>) -> Result<(), Error> {
    builtin_output(&n.outputs, BuiltinType::McpResourceResult)?;
    let port = n
        .inputs
        .get("arguments")
        .filter(|p| p.required())
        .ok_or(Error::McpInterfaceMismatch)?;
    if n.inputs.len() != 1 {
        return Err(Error::McpInterfaceMismatch);
    }
    let ValueTypeKind::Record(fields) = port.value_type().kind() else {
        return Err(Error::McpInterfaceMismatch);
    };
    if fields.len() != variables.len()
        || fields.iter().any(|(name, p)| {
            !variables.contains(name.as_str())
                || !p.required()
                || !matches!(
                    p.value_type().kind(),
                    ValueTypeKind::Builtin(BuiltinType::String)
                )
        })
    {
        return Err(Error::McpInterfaceMismatch);
    }
    Ok(())
}
fn argument_array(m: &Map) -> Result<&[Value], Error> {
    match m.get("arguments") {
        None => Ok(&[]),
        Some(Value::Array(v)) => Ok(v),
        _ => Err(Error::InvalidDescriptor("arguments")),
    }
}
fn prompt_arguments(m: &Map) -> Result<BTreeMap<&str, bool>, Error> {
    let mut result = BTreeMap::new();
    for arg in argument_array(m)? {
        let Value::Map(arg) = arg else {
            return Err(Error::InvalidDescriptor("prompt argument"));
        };
        let Some(Value::Text(name)) = arg.get("name") else {
            return Err(Error::InvalidDescriptor("argument name"));
        };
        let required = match arg.get("required") {
            None => false,
            Some(Value::Bool(v)) => *v,
            _ => return Err(Error::InvalidDescriptor("argument required")),
        };
        if arg
            .get("description")
            .is_some_and(|v| !matches!(v, Value::Text(_)))
        {
            return Err(Error::InvalidDescriptor("argument description"));
        }
        if result.insert(name.as_str(), required).is_some() {
            return Err(Error::InvalidDescriptor("duplicate argument"));
        }
    }
    Ok(result)
}
fn schema_port(table: &PortTable, name: &str, digest: Digest) -> Result<(), Error> {
    let valid = table.len() == 1
        && table.get(name).is_some_and(|p| {
            p.required()
                && matches!(p.value_type().kind(), ValueTypeKind::Schema(id) if *id == digest)
        });
    if valid {
        Ok(())
    } else {
        Err(Error::McpInterfaceMismatch)
    }
}
