//! Bounded canonical document assembly and record identity validation.

use crate::cbor::{self, LimitKind, Limits, Map, Value};
use crate::digest::{Digest, ParseDigestError};
use crate::record_accounting::{EncodingLimitError, RecordAccounting};
use crate::{
    EnvelopeError, ExecutableEnvelope, ExecutionProfile, GraphRecordError, Identifier,
    JsonDocument, JsonError, Library, McpBinding, MetadataError, PromptTemplate, Scope,
    ScopeContext, TypeError,
};
use std::{collections::BTreeMap, fmt};

/// Authored canonical tables. Keys assert record identities and are never rekeyed.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentFields {
    /// Qualified snake_case graph name.
    pub graph_id: String,
    /// Pinned implementation and policy identities, without host admission.
    pub profile: ExecutionProfile,
    /// Root scope identity; semantic analysis resolves it.
    pub root_scope: Digest,
    /// Scope definitions.
    pub scopes: BTreeMap<Digest, Scope>,
    /// Local prompt templates.
    pub templates: BTreeMap<Digest, PromptTemplate>,
    /// MCP binding definitions.
    pub bindings: BTreeMap<Digest, McpBinding>,
    /// Complete library manifests, keyed by implementation identity.
    pub libraries: BTreeMap<Digest, Library>,
    /// Absolute, fragment-free schema retrieval roots.
    pub schema_uris: BTreeMap<String, Digest>,
    /// Exact JCS documents keyed by their raw SHA-256 identity.
    pub documents: BTreeMap<Digest, JsonDocument>,
}
impl DocumentFields {
    /// Starts authored fields with empty definition tables.
    pub fn new(graph_id: String, profile: ExecutionProfile, root_scope: Digest) -> Self {
        Self {
            graph_id,
            profile,
            root_scope,
            scopes: BTreeMap::new(),
            templates: BTreeMap::new(),
            bindings: BTreeMap::new(),
            libraries: BTreeMap::new(),
            schema_uris: BTreeMap::new(),
            documents: BTreeMap::new(),
        }
    }
}

/// Immutable canonical representation with checked record identities.
/// References, contextual legality, types, graph properties and host admission
/// are checked by the analyzer and runtime, rather than by this model.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalDocument {
    fields: Box<DocumentFields>,
}
impl CanonicalDocument {
    /// Bounds the complete representation and checks its asserted record keys.
    /// Semantically invalid but representable documents remain analyzable.
    ///
    /// # Errors
    /// Returns malformed representation, identity, or resource failures.
    pub fn new(fields: DocumentFields, limits: &Limits) -> Result<Self, DocumentError> {
        let result = Self {
            fields: Box::new(fields),
        };
        result.to_value(limits)?;
        result.validate_keys(limits)?;
        Ok(result)
    }
    /// Current canonical document version.
    pub const fn ir_version(&self) -> &'static str {
        "0.1"
    }
    /// Borrows the immutable document tables and metadata.
    pub fn fields(&self) -> &DocumentFields {
        &self.fields
    }
    /// Produces canonical data under this call's resource ceilings.
    ///
    /// # Errors
    /// Returns representation or resource failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, DocumentError> {
        let f = &self.fields;
        limits.validate()?;
        if f.graph_id
            .split('.')
            .any(|p| p.parse::<Identifier>().is_err())
        {
            return Err(DocumentError::InvalidGraphId);
        }
        let mut b = Builder::new(limits)?;
        b.text("ir_version", "0.1")?;
        b.text("graph_id", &f.graph_id)?;
        b.child("profile", f.profile.to_value(limits)?)?;
        b.text("root_scope", &f.root_scope.to_string())?;
        b.table(
            "scopes",
            f.scopes.len(),
            f.scopes
                .iter()
                .map(|(id, v)| Ok((id.to_string(), v.to_value(ScopeContext::Ordinary, limits)?))),
        )?;
        b.table(
            "templates",
            f.templates.len(),
            f.templates
                .iter()
                .map(|(id, v)| Ok((id.to_string(), v.to_value(limits)?))),
        )?;
        b.table(
            "bindings",
            f.bindings.len(),
            f.bindings
                .iter()
                .map(|(id, v)| Ok((id.to_string(), v.to_value(limits)?))),
        )?;
        b.table(
            "libraries",
            f.libraries.len(),
            f.libraries
                .iter()
                .map(|(id, v)| Ok((id.to_string(), v.to_value(limits)?))),
        )?;
        b.table(
            "schema_uris",
            f.schema_uris.len(),
            f.schema_uris.iter().map(|(uri, id)| {
                RecordAccounting::new(limits)?.text(uri, 0)?;
                validate_uri(uri)?;
                Ok((owned(uri)?, Value::Text(id.to_string())))
            }),
        )?;
        b.table(
            "documents",
            f.documents.len(),
            f.documents.iter().map(|(id, doc)| {
                check(
                    doc.as_bytes().len(),
                    limits.max_document_bytes,
                    LimitKind::DocumentBytes,
                )?;
                JsonDocument::decode(doc.as_bytes(), limits)?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(doc.as_bytes().len())
                    .map_err(allocation)?;
                bytes.extend_from_slice(doc.as_bytes());
                Ok((id.to_string(), Value::Bytes(bytes)))
            }),
        )?;
        b.finish()
    }
    /// Encodes one complete canonical document.
    ///
    /// # Errors
    /// Returns representation or codec resource failures.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, DocumentError> {
        Ok(cbor::encode(&self.to_value(limits)?, limits)?)
    }
    /// Wraps the document in its canonical fingerprinted envelope.
    ///
    /// # Errors
    /// Returns document or envelope resource failures.
    pub fn envelope(&self, limits: &Limits) -> Result<ExecutableEnvelope, DocumentError> {
        Ok(ExecutableEnvelope::new(self.encode(limits)?, limits)?)
    }
    /// Decodes exactly one canonical document, checking representation and keys.
    ///
    /// # Errors
    /// Returns malformed data, identity mismatch, or resource failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, DocumentError> {
        Self::parse(&cbor::decode(bytes, limits)?, limits)
    }
    /// Parses bounded canonical data without repairing noncanonical records.
    ///
    /// # Errors
    /// Returns malformed data, identity mismatch, or resource failures.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, DocumentError> {
        cbor::encode(value, limits)?;
        Self::parse(value, limits)
    }
    /// Decodes the opaque payload of an already checked envelope.
    ///
    /// # Errors
    /// Returns invalid nested document or resource failures.
    pub fn from_envelope(
        envelope: &ExecutableEnvelope,
        limits: &Limits,
    ) -> Result<Self, DocumentError> {
        Self::decode(envelope.payload(), limits)
    }
    fn parse(value: &Value, l: &Limits) -> Result<Self, DocumentError> {
        const FIELDS: [&str; 10] = [
            "bindings",
            "documents",
            "graph_id",
            "ir_version",
            "libraries",
            "profile",
            "root_scope",
            "schema_uris",
            "scopes",
            "templates",
        ];
        let m = map(value)?;
        if m.iter().any(|(key, _)| !FIELDS.contains(&key)) {
            return Err(DocumentError::UnknownField);
        }
        for key in FIELDS {
            if m.get(key).is_none() {
                return Err(DocumentError::MissingField(key));
            }
        }
        let get = |key| m.get(key).expect("closed required fields checked");
        if text(get("ir_version"))? != "0.1" {
            return Err(DocumentError::UnsupportedVersion);
        }
        let mut f = DocumentFields::new(
            owned(text(get("graph_id"))?)?,
            ExecutionProfile::from_value(get("profile"), l)?,
            digest(get("root_scope"))?,
        );
        for (key, v) in map(get("scopes"))?.iter() {
            f.scopes.insert(
                key.parse()?,
                Scope::from_value(v, ScopeContext::Ordinary, l)?,
            );
        }
        for (key, v) in map(get("templates"))?.iter() {
            f.templates
                .insert(key.parse()?, PromptTemplate::from_value(v, l)?);
        }
        for (key, v) in map(get("bindings"))?.iter() {
            f.bindings
                .insert(key.parse()?, McpBinding::from_value(v, l)?);
        }
        for (key, v) in map(get("libraries"))?.iter() {
            f.libraries.insert(key.parse()?, Library::from_value(v, l)?);
        }
        for (key, v) in map(get("schema_uris"))?.iter() {
            f.schema_uris.insert(owned(key)?, digest(v)?);
        }
        for (key, v) in map(get("documents"))?.iter() {
            let Value::Bytes(bytes) = v else {
                return Err(DocumentError::InvalidShape("document bytes"));
            };
            f.documents
                .insert(key.parse()?, JsonDocument::decode(bytes, l)?);
        }
        Self::new(f, l)
    }
    fn validate_keys(&self, l: &Limits) -> Result<(), DocumentError> {
        let f = &self.fields;
        for (id, v) in &f.scopes {
            if v.digest(ScopeContext::Ordinary, l)? != *id {
                return Err(DocumentError::DigestMismatch("scope"));
            }
        }
        for (id, v) in &f.templates {
            if v.digest(l)? != *id {
                return Err(DocumentError::DigestMismatch("template"));
            }
        }
        for (id, v) in &f.bindings {
            if v.digest(l)? != *id {
                return Err(DocumentError::DigestMismatch("binding"));
            }
        }
        for (id, v) in &f.libraries {
            if v.implementation_digest() != *id {
                return Err(DocumentError::DigestMismatch("library key"));
            }
        }
        for (id, v) in &f.documents {
            if v.digest() != *id {
                return Err(DocumentError::DigestMismatch("JSON document"));
            }
        }
        Ok(())
    }
}
fn validate_uri(uri: &str) -> Result<(), DocumentError> {
    let parsed = url::Url::parse(uri).map_err(|_| DocumentError::InvalidSchemaUri)?;
    if parsed.fragment().is_some()
        || uri.bytes().any(|b| {
            !b.is_ascii()
                || b.is_ascii_whitespace()
                || b.is_ascii_control()
                || matches!(
                    b,
                    b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
                )
        })
        || uri.as_bytes().iter().enumerate().any(|(i, b)| {
            *b == b'%'
                && !uri
                    .as_bytes()
                    .get(i + 1..i + 3)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        })
    {
        return Err(DocumentError::InvalidSchemaUri);
    }
    Ok(())
}
struct Builder<'a> {
    accounting: RecordAccounting<'a>,
    fields: Vec<(String, Value)>,
}
impl<'a> Builder<'a> {
    fn new(l: &'a Limits) -> Result<Self, DocumentError> {
        let mut accounting = RecordAccounting::new(l)?;
        accounting.collection(10, 0)?;
        Ok(Self {
            accounting,
            fields: Vec::new(),
        })
    }
    fn push(&mut self, key: &str, v: Value) -> Result<(), DocumentError> {
        self.fields.try_reserve(1).map_err(allocation)?;
        self.fields.push((owned(key)?, v));
        Ok(())
    }
    fn text(&mut self, key: &str, v: &str) -> Result<(), DocumentError> {
        self.accounting.text(key, 1)?;
        self.accounting.text(v, 1)?;
        self.push(key, Value::Text(owned(v)?))
    }
    fn child(&mut self, key: &str, v: Value) -> Result<(), DocumentError> {
        self.accounting.text(key, 1)?;
        self.accounting.value(&v, 1)?;
        self.push(key, v)
    }
    fn table(
        &mut self,
        key: &str,
        count: usize,
        entries: impl Iterator<Item = Result<(String, Value), DocumentError>>,
    ) -> Result<(), DocumentError> {
        self.accounting.text(key, 1)?;
        self.accounting.collection(count, 1)?;
        let mut pairs = Vec::new();
        for pair in entries {
            let (key, value) = pair?;
            self.accounting.text(&key, 2)?;
            self.accounting.value(&value, 2)?;
            pairs.try_reserve(1).map_err(allocation)?;
            pairs.push((key, value));
        }
        self.push(key, Value::Map(Map::try_from_entries(pairs)?))
    }
    fn finish(self) -> Result<Value, DocumentError> {
        Ok(Value::Map(Map::try_from_entries(self.fields)?))
    }
}
fn owned(s: &str) -> Result<String, DocumentError> {
    let mut v = String::new();
    v.try_reserve_exact(s.len()).map_err(allocation)?;
    v.push_str(s);
    Ok(v)
}
fn allocation(_: std::collections::TryReserveError) -> DocumentError {
    DocumentError::AllocationFailed
}
fn check(n: usize, maximum: usize, limit: LimitKind) -> Result<(), DocumentError> {
    if n > maximum {
        Err(DocumentError::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}
fn map(v: &Value) -> Result<&Map, DocumentError> {
    if let Value::Map(v) = v {
        Ok(v)
    } else {
        Err(DocumentError::InvalidShape("map"))
    }
}
fn text(v: &Value) -> Result<&str, DocumentError> {
    if let Value::Text(v) = v {
        Ok(v)
    } else {
        Err(DocumentError::InvalidShape("text"))
    }
}
fn digest(v: &Value) -> Result<Digest, DocumentError> {
    Ok(text(v)?.parse()?)
}

/// Canonical document representation or resource failure, without input contents.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentError {
    /// Codec failure.
    Codec(cbor::Error),
    /// Envelope failure.
    Envelope(EnvelopeError),
    /// Graph record representation failure.
    Graph(GraphRecordError),
    /// Metadata representation failure.
    Metadata(MetadataError),
    /// Expression or template representation failure.
    Expression(crate::ExpressionError),
    /// Type representation failure.
    Type(TypeError),
    /// Invalid digest spelling.
    Digest(ParseDigestError),
    /// Invalid canonical external JSON.
    Json(JsonError),
    /// Incorrect native shape.
    InvalidShape(&'static str),
    /// Missing required canonical field.
    MissingField(&'static str),
    /// Unknown canonical field.
    UnknownField,
    /// Unsupported document version.
    UnsupportedVersion,
    /// Graph name is not qualified snake_case.
    InvalidGraphId,
    /// An asserted record key does not match its identity.
    DigestMismatch(&'static str),
    /// A retrieval root is not an absolute fragment-free URI.
    InvalidSchemaUri,
    /// Invalid RFC 6570 template syntax.
    InvalidUriTemplate {
        /// Zero-based byte offset in the exact template.
        offset: usize,
    },
    /// Resource ceiling exceeded.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured maximum.
        maximum: usize,
    },
    /// Fallible allocation failed.
    AllocationFailed,
}
macro_rules! convert {
    ($ty:ty,$variant:ident) => {
        impl From<$ty> for DocumentError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}
convert!(cbor::Error, Codec);
convert!(EnvelopeError, Envelope);
convert!(GraphRecordError, Graph);
convert!(MetadataError, Metadata);
convert!(crate::ExpressionError, Expression);
convert!(TypeError, Type);
convert!(ParseDigestError, Digest);
convert!(JsonError, Json);
impl From<EncodingLimitError> for DocumentError {
    fn from(e: EncodingLimitError) -> Self {
        Self::LimitExceeded {
            limit: e.limit,
            maximum: e.maximum,
        }
    }
}
impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "canonical document error: {self:?}")
    }
}
impl std::error::Error for DocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Envelope(e) => Some(e),
            Self::Graph(e) => Some(e),
            Self::Metadata(e) => Some(e),
            Self::Expression(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Digest(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}
