//! Cross-record MCP selection, extracted schema, and tool-interface integrity.

use htlk_cbor::{Limits, Value};

use crate::digest::Digest;
use crate::{
    DocumentError as Error, DocumentFields, JsonDocument, McpBinding, McpBindingKind as Kind,
    NodeFields, PortTable, ValueTypeKind,
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

pub(crate) fn tool_ports(n: &NodeFields, binding: &McpBinding) -> Result<(), Error> {
    if let Kind::Tool {
        input_schema,
        output_schema,
        ..
    } = binding.kind()
    {
        schema_port(&n.inputs, "arguments", *input_schema)?;
        schema_port(&n.outputs, "value", *output_schema)?;
    }
    Ok(())
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
