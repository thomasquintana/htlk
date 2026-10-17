//! Finite MCP inventory ingestion and exact selection.
use crate::executable::{
    cbor::{Limits, Value},
    digest::Digest,
};
use crate::{
    analyzer::{McpDescriptorKind, validate_mcp_descriptor},
    diagnostic::{Result, fail},
    executable::*,
    input_value::*,
};
use std::collections::BTreeMap;

/// Validated, immutable snapshots of exactly three MCP catalogs.
#[derive(Clone, Debug)]
pub struct MCPCatalogInputs {
    bytes: usize,
    depth: usize,
    pub(crate) servers: BTreeMap<String, ServerIdentity>,
    pub(crate) entries: BTreeMap<(Digest, String, String), JsonDocument>,
    pub(crate) schemas: BTreeMap<String, JsonDocument>,
}
impl MCPCatalogInputs {
    /// Parses the complete three-catalog JSON input, rejecting duplicate keys.
    ///
    /// # Errors
    /// Rejects malformed/unknown records, versions, conflicting aliases or entries,
    /// protocol-invalid descriptors, invalid retrieval roots, pins and size limits.
    pub fn from_json(bytes: &[u8], limits: &Limits) -> Result<Self> {
        let json = JsonDocument::new(bytes, limits).map_err(|e| fail("C_MCP", e))?;
        Self::from_value(json.value(), limits)
            .map(|mut result| {
                result.bytes = json.as_bytes().len();
                result.depth = value_depth(json.value());
                result
            })
            .map_err(|mut e| {
                for d in &mut e.diagnostics {
                    if d.code == "C_SYNTAX" {
                        d.code = "C_MCP".into();
                    }
                }
                e
            })
    }
    fn from_value(v: &Value, l: &Limits) -> Result<Self> {
        let root = closed(v, &["tools", "resources", "prompts"], &[])?;
        let mut result = Self {
            bytes: 0,
            depth: 0,
            servers: BTreeMap::new(),
            entries: BTreeMap::new(),
            schemas: BTreeMap::new(),
        };
        for (category, collections) in [
            ("tools", &["tools"][..]),
            ("resources", &["resources", "templates"][..]),
            ("prompts", &["prompts"][..]),
        ] {
            let mut names = vec![
                "catalog_version",
                "mcp_protocol_version",
                "servers",
                "schema_documents",
            ];
            names.extend_from_slice(collections);
            let c = closed(field(root, category)?, &names, &[])?;
            if text(field(c, "catalog_version")?)? != "0.1"
                || text(field(c, "mcp_protocol_version")?)? != MCP_PROTOCOL_VERSION
            {
                return Err(fail("C_VERSION", "unsupported catalog or MCP version"));
            }
            let mut local = BTreeMap::new();
            for (alias, value) in map(field(c, "servers")?)?.iter() {
                if alias.is_empty() {
                    return Err(fail("C_MCP", "empty server alias"));
                }
                let server = ServerIdentity::from_value(value, l).map_err(|e| fail("C_MCP", e))?;
                if result.servers.get(alias).is_some_and(|s| s != &server) {
                    return Err(fail("C_MCP", "conflicting server alias"));
                }
                local.insert(alias.to_owned(), server.clone());
                result.servers.insert(alias.to_owned(), server);
            }
            for (uri, value) in map(field(c, "schema_documents")?)?.iter() {
                let doc = JsonDocument::from_value(value, l).map_err(|e| fail("C_SCHEMA", e))?;
                // Validate retrieval spelling and root shape without selecting a closure.
                crate::analyzer::SchemaResources::new(&doc, uri, l)
                    .map_err(|e| fail("C_SCHEMA", e))?;
                if result.schemas.get(uri).is_some_and(|d| d != &doc) {
                    return Err(fail("C_SCHEMA", "conflicting schema retrieval URI"));
                }
                result.schemas.insert(uri.to_owned(), doc);
            }
            for collection in collections {
                let (kind, key) = match *collection {
                    "tools" => (McpDescriptorKind::Tool, "name"),
                    "resources" => (McpDescriptorKind::Resource, "uri"),
                    "templates" => (McpDescriptorKind::ResourceTemplate, "uriTemplate"),
                    _ => (McpDescriptorKind::Prompt, "name"),
                };
                for value in array(field(c, collection)?)? {
                    let e = closed(
                        value,
                        &["server", "descriptor"],
                        &["expected_descriptor_digest"],
                    )?;
                    let alias = text(field(e, "server")?)?;
                    let server = local
                        .get(alias)
                        .ok_or_else(|| fail("C_MCP", "unknown catalog server alias"))?;
                    let descriptor = JsonDocument::from_value(field(e, "descriptor")?, l)
                        .map_err(|e| fail("C_MCP", e))?;
                    validate_mcp_descriptor(kind, &descriptor, l).map_err(|e| fail("C_MCP", e))?;
                    if let Some(pin) = e.get("expected_descriptor_digest") {
                        let pin: Digest = text(pin)?.parse().map_err(|e| fail("C_MCP", e))?;
                        if pin != descriptor.digest() {
                            return Err(fail("C_MCP", "descriptor digest assertion mismatch"));
                        }
                    }
                    let selection = text(field(map(descriptor.value())?, key)?)?.to_owned();
                    let id = (
                        server.digest(l).map_err(|e| fail("C_MCP", e))?,
                        collection.to_string(),
                        selection,
                    );
                    if result.entries.get(&id).is_some_and(|d| d != &descriptor) {
                        return Err(fail("C_MCP", "conflicting operation descriptors"));
                    }
                    result.entries.insert(id, descriptor);
                }
            }
        }
        Ok(result)
    }
    pub(crate) fn check_limits(&self, l: &Limits) -> Result<()> {
        if self.bytes > l.max_document_bytes || self.depth > l.max_depth {
            return Err(fail(
                "C_SOURCE_LIMIT",
                "catalog exceeds compiler input limits",
            ));
        }
        Ok(())
    }
    pub(crate) fn descriptor(&self, binding: &McpBinding, l: &Limits) -> Result<&JsonDocument> {
        let (category, selection) = match binding.kind() {
            McpBindingKind::Tool { name, .. } => ("tools", name),
            McpBindingKind::Resource { uri } => ("resources", uri),
            McpBindingKind::Template { uri_template } => ("templates", uri_template),
            McpBindingKind::Prompt { name } => ("prompts", name),
        };
        let id = (
            binding.server().digest(l).map_err(|e| fail("C_MCP", e))?,
            category.into(),
            selection.clone(),
        );
        self.entries
            .get(&id)
            .filter(|d| d.digest() == binding.descriptor())
            .ok_or_else(|| {
                fail(
                    "C_MCP",
                    "selected descriptor does not match supplied catalogs",
                )
            })
    }
}
fn value_depth(v: &Value) -> usize {
    match v {
        Value::Map(m) => m.iter().map(|(_, v)| 1 + value_depth(v)).max().unwrap_or(0),
        Value::Array(v) => v.iter().map(|v| 1 + value_depth(v)).max().unwrap_or(0),
        _ => 0,
    }
}
