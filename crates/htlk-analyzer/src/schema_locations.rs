//! Standard JSON Schema 2020-12 locations, independent of resource URI resolution.

use crate::digest::Digest;
use crate::{JsonDocument, JsonError, JsonPointer, JsonPointerError};
use htlk_cbor::{LimitKind, Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use std::fmt;

/// The JSON Schema dialect selected by HTLK Draft 0.1.
pub const JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

const SINGLE: [&str; 11] = [
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];
const ARRAYS: [&str; 4] = ["allOf", "anyOf", "oneOf", "prefixItems"];
const MAPS: [&str; 4] = [
    "$defs",
    "dependentSchemas",
    "patternProperties",
    "properties",
];

/// Immutable index of standard schema-bearing locations in one exact JSON document.
/// It carries the document digest and decoded-token-sorted pointers, not schemas
/// copied out of their base context. This index is not part of executable identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaLocations {
    document: Digest,
    pointers: Vec<JsonPointer>,
}
impl SchemaLocations {
    /// Discovers standard 2020-12 subschemas under bounded input/index limits.
    /// Examples and other instance/annotation data are never traversed as schemas.
    ///
    /// # Errors
    /// Rejects non-schema roots, malformed standard applicator containers,
    /// explicitly different dialects, and input or derived-index resource failures.
    /// Does not validate other schema keywords, vocabularies, or reference targets.
    pub fn new(document: &JsonDocument, limits: &Limits) -> Result<Self, SchemaLocationError> {
        JsonDocument::decode(document.as_bytes(), limits)?;
        let mut builder = Builder {
            limits,
            bytes: 0,
            work: Vec::new(),
        };
        builder.push(document.value(), &[], &[])?;
        let mut pointers = Vec::new();
        while let Some((value, pointer)) = builder.work.pop() {
            match value {
                Value::Bool(_) => (),
                Value::Map(m) => {
                    if let Some(dialect) = m.get("$schema") {
                        let Value::Text(dialect) = dialect else {
                            return Err(SchemaLocationError::InvalidKeyword("$schema"));
                        };
                        // An empty URI fragment selects the same dialect resource.
                        if dialect.strip_suffix('#').unwrap_or(dialect) != JSON_SCHEMA_DIALECT {
                            return Err(SchemaLocationError::UnsupportedDialect);
                        }
                    }
                    for keyword in SINGLE {
                        if let Some(child) = m.get(keyword) {
                            builder.push(child, pointer.tokens(), &[keyword])?;
                        }
                    }
                    for keyword in ARRAYS {
                        if let Some(v) = m.get(keyword) {
                            let Value::Array(children) = v else {
                                return Err(SchemaLocationError::InvalidKeyword(keyword));
                            };
                            if children.is_empty() {
                                return Err(SchemaLocationError::InvalidKeyword(keyword));
                            }
                            for (i, child) in children.iter().enumerate() {
                                builder.push(
                                    child,
                                    pointer.tokens(),
                                    &[keyword, &i.to_string()],
                                )?;
                            }
                        }
                    }
                    for keyword in MAPS {
                        if let Some(v) = m.get(keyword) {
                            let Value::Map(children) = v else {
                                return Err(SchemaLocationError::InvalidKeyword(keyword));
                            };
                            for (name, child) in children.iter() {
                                builder.push(child, pointer.tokens(), &[keyword, name])?;
                            }
                        }
                    }
                }
                _ => return Err(SchemaLocationError::InvalidSchema),
            }
            pointers.try_reserve(1).map_err(allocation)?;
            pointers.push(pointer);
        }
        pointers.sort_unstable_by(|a, b| a.tokens().cmp(b.tokens()));
        Ok(Self {
            document: document.digest(),
            pointers,
        })
    }
    /// Identity of the complete source JSON document, preserving base context.
    pub const fn document_digest(&self) -> Digest {
        self.document
    }
    /// Standard schema locations ordered lexicographically by decoded tokens.
    pub fn pointers(&self) -> &[JsonPointer] {
        &self.pointers
    }
    /// Whether a pointer selects a discovered schema rather than instance data.
    pub fn contains(&self, pointer: &JsonPointer) -> bool {
        self.pointers
            .binary_search_by(|p| p.tokens().cmp(pointer.tokens()))
            .is_ok()
    }
}

struct Builder<'a, 'd> {
    limits: &'a Limits,
    bytes: usize,
    work: Vec<(&'d Value, JsonPointer)>,
}
impl<'d> Builder<'_, 'd> {
    fn push(
        &mut self,
        value: &'d Value,
        parent: &[String],
        suffix: &[&str],
    ) -> Result<(), SchemaLocationError> {
        if !matches!(value, Value::Map(_) | Value::Bool(_)) {
            return Err(SchemaLocationError::InvalidSchema);
        }
        let depth = add(
            parent.len(),
            suffix.len(),
            self.limits.max_depth,
            LimitKind::Depth,
        )?;
        // Charge one byte per index/token marker, including empty tokens.
        self.bytes = add(
            self.bytes,
            1,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        self.bytes = add(
            self.bytes,
            depth,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        let mut length = 0;
        for token in parent
            .iter()
            .map(String::as_str)
            .chain(suffix.iter().copied())
        {
            length = add(
                length,
                1,
                self.limits.max_document_bytes,
                LimitKind::DocumentBytes,
            )?;
            for b in token.bytes() {
                length = add(
                    length,
                    if matches!(b, b'~' | b'/') { 2 } else { 1 },
                    self.limits.max_document_bytes,
                    LimitKind::DocumentBytes,
                )?;
            }
        }
        // Encoded pointer bytes conservatively bound all retained token text.
        self.bytes = add(
            self.bytes,
            length,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        let mut text = String::new();
        text.try_reserve_exact(length).map_err(allocation)?;
        for token in parent
            .iter()
            .map(String::as_str)
            .chain(suffix.iter().copied())
        {
            text.push('/');
            for c in token.chars() {
                match c {
                    '~' => text.push_str("~0"),
                    '/' => text.push_str("~1"),
                    _ => text.push(c),
                }
            }
        }
        let pointer = JsonPointer::new(&text, self.limits)?;
        self.work.try_reserve(1).map_err(allocation)?;
        self.work.push((value, pointer));
        Ok(())
    }
}
fn add(a: usize, b: usize, maximum: usize, limit: LimitKind) -> Result<usize, SchemaLocationError> {
    let value = a
        .checked_add(b)
        .ok_or(SchemaLocationError::LimitExceeded { limit, maximum })?;
    if value > maximum {
        Err(SchemaLocationError::LimitExceeded { limit, maximum })
    } else {
        Ok(value)
    }
}
fn allocation(_: std::collections::TryReserveError) -> SchemaLocationError {
    SchemaLocationError::AllocationFailed
}

/// Static schema-location and resource errors, without submitted schema contents.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaLocationError {
    /// JSON revalidation failure under effective limits.
    Json(JsonError),
    /// Generated pointer exceeds pointer limits.
    Pointer(JsonPointerError),
    /// A schema-bearing position contains neither an object nor Boolean.
    InvalidSchema,
    /// Incorrect shape for a standard keyword.
    InvalidKeyword(&'static str),
    /// An explicit dialect differs from JSON Schema 2020-12.
    UnsupportedDialect,
    /// Derived index ceiling exceeded.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Fallible storage reservation failed.
    AllocationFailed,
}
impl From<JsonError> for SchemaLocationError {
    fn from(e: JsonError) -> Self {
        Self::Json(e)
    }
}
impl From<JsonPointerError> for SchemaLocationError {
    fn from(e: JsonPointerError) -> Self {
        Self::Pointer(e)
    }
}
impl fmt::Display for SchemaLocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "schema location error: {self:?}")
    }
}
impl std::error::Error for SchemaLocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Pointer(e) => Some(e),
            _ => None,
        }
    }
}
