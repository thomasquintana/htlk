//! Immutable native result envelopes with independently checked provenance links.

use std::collections::BTreeSet;

use crate::mcp_policy::{closed, get, text};
use crate::{
    JsonDocument, McpResultError,
    cbor::{self, Limits, Map, Value},
};

/// A checked envelope, not proof of tool success, authorization, or schema compliance.
/// Actual protocol and adopted-schema admission remain independent runtime checks.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolResult {
    document: JsonDocument,
}

impl McpToolResult {
    /// Admits a bounded JSON-compatible envelope and verifies all cross-field links.
    ///
    /// # Errors
    /// Rejects malformed records, invented source spans/values, inconsistent
    /// selection, duplicate candidates/sources, unsafe JSON, or exhausted limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, McpResultError> {
        let document = JsonDocument::from_value(value, limits)?;
        validate(document.value(), limits)?;
        Ok(Self { document })
    }

    /// Borrows the complete immutable native value.
    pub fn value(&self) -> &Value {
        self.document.value()
    }

    /// Borrows its canonical JSON representation and content identity.
    pub fn document(&self) -> &JsonDocument {
        &self.document
    }

    /// Produces a bounded native value without defaulting absent data to null.
    ///
    /// # Errors
    /// Returns codec limits if the complete envelope exceeds the supplied bounds.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, McpResultError> {
        cbor::encode(self.value(), limits)?;
        Ok(self.value().clone())
    }

    /// Encodes one canonical native envelope.
    ///
    /// # Errors
    /// Returns codec limits.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, McpResultError> {
        Ok(cbor::encode(self.value(), limits)?)
    }

    /// Decodes and independently checks one complete canonical envelope.
    ///
    /// # Errors
    /// Returns codec, JSON, provenance, or selection-integrity failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, McpResultError> {
        Self::from_value(&cbor::decode(bytes, limits)?, limits)
    }
}

fn validate(value: &Value, limits: &Limits) -> Result<(), McpResultError> {
    let result = closed(
        value,
        &["protocol_result", "extraction", "validation"],
        &["data"],
    )?;
    let protocol = match get(result, "protocol_result")? {
        Value::Map(protocol) => protocol,
        _ => return shape("protocol result"),
    };
    let blocks = array(get(protocol, "content")?)?;
    let extraction = closed(
        get(result, "extraction")?,
        &["status", "candidates"],
        &["selected_candidate_id", "exhausted_limit"],
    )?;
    let candidates = array(get(extraction, "candidates")?)?;
    let mut distinct = BTreeSet::new();
    let mut all_sources = BTreeSet::new();
    let mut source_bytes = 0usize;
    let mut selected = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let candidate = closed(candidate, &["id", "value", "sources", "validation"], &[])?;
        let id = text(get(candidate, "id")?)?;
        if id != format!("candidate_{index}") || text(get(candidate, "validation")?)? != "parsed" {
            return shape("candidate identity or validation");
        }
        let document = JsonDocument::from_value(get(candidate, "value")?, limits)?;
        if !distinct.insert(document.digest()) {
            return shape("duplicate candidate");
        }
        let sources = array(get(candidate, "sources")?)?;
        if sources.is_empty() {
            return shape("missing candidate source");
        }
        for source in sources {
            let source_identity = crate::digest::hash_bytes(&cbor::encode(source, limits)?);
            if !all_sources.insert(source_identity) {
                return shape("duplicate source");
            }
            validate_source(
                source,
                &document,
                protocol,
                blocks,
                limits,
                &mut source_bytes,
            )?;
        }
        if extraction
            .get("selected_candidate_id")
            .is_some_and(|selected| selected == &Value::Text(id.to_owned()))
        {
            selected = Some(get(candidate, "value")?);
        }
    }
    let status = text(get(extraction, "status")?)?;
    match status {
        "selected"
            if selected.is_some()
                && result.get("data") == selected
                && extraction.get("exhausted_limit").is_none() => {}
        "none" | "ambiguous" | "exhausted" => {
            if result.get("data").is_some() || extraction.get("selected_candidate_id").is_some() {
                return shape("data without complete selection");
            }
            if status == "ambiguous" && candidates.len() < 2 {
                return shape("ambiguity");
            }
            match (status, extraction.get("exhausted_limit")) {
                ("exhausted", Some(Value::Text(limit)))
                    if matches!(
                        limit.as_str(),
                        "inspected_bytes"
                            | "candidates"
                            | "depth"
                            | "work"
                            | "parsed_bytes"
                            | "allocation_bytes"
                            | "output_bytes"
                            | "diagnostic_bytes"
                    ) => {}
                ("exhausted", _) => return shape("exhausted limit"),
                (_, None) => (),
                _ => return shape("unexpected exhausted limit"),
            }
        }
        _ => return shape("extraction status or selected data"),
    }
    let validation = closed(
        get(result, "validation")?,
        &["declared_schema", "inferred_schema", "application_schema"],
        &[],
    )?;
    for (key, allowed) in [
        ("declared_schema", &["enforce", "report"][..]),
        ("inferred_schema", &["observe", "enforce"][..]),
        ("application_schema", &["enforce"][..]),
    ] {
        let assessment = closed(get(validation, key)?, &["status"], &["schema", "mode"])?;
        match text(get(assessment, "status")?)? {
            "not_configured" if assessment.len() == 1 => (),
            "valid" | "missing" | "invalid" if assessment.len() == 3 => {
                text(get(assessment, "schema")?)?
                    .parse::<crate::digest::Digest>()
                    .map_err(|_| McpResultError::Shape("assessment schema digest"))?;
                if !allowed.contains(&text(get(assessment, "mode")?)?) {
                    return shape("assessment mode");
                }
            }
            _ => return shape("assessment fields"),
        }
    }
    Ok(())
}

fn validate_source(
    value: &Value,
    candidate: &JsonDocument,
    protocol: &Map,
    blocks: &[Value],
    limits: &Limits,
    inspected: &mut usize,
) -> Result<(), McpResultError> {
    let source = closed(
        value,
        &["origin", "method"],
        &["content_block_index", "byte_start", "byte_end"],
    )?;
    let origin = text(get(source, "origin")?)?;
    let method = text(get(source, "method")?)?;
    if origin == "structured_content" && method == "structured_content" && source.len() == 2 {
        if protocol.get("structuredContent") != Some(candidate.value()) {
            return shape("structured candidate value");
        }
        return Ok(());
    }
    if origin != "text"
        || !matches!(method, "whole_block" | "fence" | "embedded")
        || source.len() != 5
    {
        return shape("candidate source");
    }
    let index = index_value(get(source, "content_block_index")?)?;
    let Some(Value::Map(block)) = blocks.get(index) else {
        return shape("source content block");
    };
    if block.get("type") != Some(&Value::Text("text".into())) {
        return shape("source content kind");
    }
    let original = text(get(block, "text")?)?;
    let start = index_value(get(source, "byte_start")?)?;
    let end = index_value(get(source, "byte_end")?)?;
    let span = original
        .get(start..end)
        .ok_or(McpResultError::Shape("UTF-8 source span"))?;
    *inspected = inspected
        .checked_add(span.len())
        .ok_or(McpResultError::Limit("source validation work"))?;
    if *inspected > limits.max_document_bytes.saturating_mul(4) {
        return Err(McpResultError::Limit("source validation work"));
    }
    if JsonDocument::new(span.as_bytes(), limits)?.as_bytes() != candidate.as_bytes() {
        return shape("text candidate value");
    }
    Ok(())
}
fn array(value: &Value) -> Result<&[Value], McpResultError> {
    match value {
        Value::Array(values) => Ok(values),
        _ => Err(McpResultError::Shape("array")),
    }
}
fn index_value(value: &Value) -> Result<usize, McpResultError> {
    match value {
        Value::Integer(value) => {
            usize::try_from(*value).map_err(|_| McpResultError::Shape("nonnegative index"))
        }
        _ => Err(McpResultError::Shape("nonnegative index")),
    }
}
fn shape<T>(site: &'static str) -> Result<T, McpResultError> {
    Err(McpResultError::Shape(site))
}
