//! Canonical type descriptions and port presence metadata.

mod wire;

use std::fmt;

use htlk_cbor::{Limits, Value};

use crate::Identifier;
use crate::digest::Digest;

pub use wire::TypeError;

/// The context in which a type description is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeContext {
    /// Ordinary graph ports, record fields, and artifact types; no var/function.
    Value,
    /// Library signatures, where var/function constructors are permitted.
    Signature,
}

/// The closed set of canonical HTLK primitive type names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    /// Unicode scalar text.
    String,
    /// Signed 64-bit integer.
    Integer,
    /// Finite binary64 float.
    Float,
    /// Boolean.
    Boolean,
    /// Explicit null.
    Null,
    /// Opaque bytes.
    Bytes,
    /// JSON-compatible data under the compiler numeric profile.
    Json,
    /// A pattern/flags record validated by the pinned regex engine.
    Regex,
    /// Ordered normalized MCP resource contents.
    ResourceSnapshot,
    /// A validated MCP prompt result.
    McpPromptResult,
    /// The runtime's structured error value.
    Error,
}

impl PrimitiveType {
    /// Returns the exact canonical spelling, without source aliases.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::Null => "null",
            Self::Bytes => "bytes",
            Self::Json => "json",
            Self::Regex => "regex",
            Self::ResourceSnapshot => "ResourceSnapshot",
            Self::McpPromptResult => "McpPromptResult",
            Self::Error => "Error",
        }
    }

    fn parse(text: &str) -> Result<Self, TypeError> {
        match text {
            "string" => Ok(Self::String),
            "integer" => Ok(Self::Integer),
            "float" => Ok(Self::Float),
            "boolean" => Ok(Self::Boolean),
            "null" => Ok(Self::Null),
            "bytes" => Ok(Self::Bytes),
            "json" => Ok(Self::Json),
            "regex" => Ok(Self::Regex),
            "ResourceSnapshot" => Ok(Self::ResourceSnapshot),
            "McpPromptResult" => Ok(Self::McpPromptResult),
            "Error" => Ok(Self::Error),
            _ => Err(TypeError::UnknownPrimitive),
        }
    }
}

impl fmt::Display for PrimitiveType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Authored type shape, or a read-only view of a normalized [`ValueType`].
///
/// Pass authored shapes to [`ValueType::new`] to normalize and validate them.
/// Record vectors preserve duplicate keys until checked; validated records are
/// ordered by canonical CBOR key bytes. Function parameter order is semantic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueTypeKind {
    /// One of the closed primitive type names.
    Primitive(PrimitiveType),
    /// Homogeneous list.
    List(Box<ValueType>),
    /// Arbitrary string-keyed homogeneous map.
    Map(Box<ValueType>),
    /// Declared record fields, with arbitrary exact string names and presence.
    Record(Vec<(String, Port)>),
    /// Union members; construction flattens, sorts, and deduplicates them.
    Union(Vec<ValueType>),
    /// Nonempty unique string membership; construction sorts by UTF-8 bytes.
    Enum(Vec<String>),
    /// Schema digest; resolution and suitability are checked by the verifier.
    Schema(Digest),
    /// Signature-only variable; declaration/binding checks belong to the verifier.
    Var(Identifier),
    /// Signature-only function type.
    Function {
        /// Positional parameter declarations, in semantic order.
        parameters: Vec<Port>,
        /// Result type and whether absence can be returned.
        returns: Box<Port>,
    },
}

/// An immutable, normalized type description, not an application data value.
///
/// Construction normalizes authored union/enum order; canonical ingress rejects
/// noncanonical forms. Context checks occur at each boundary, including nested
/// types. Schema resolution, variable binding, subtyping, and actual runtime-value
/// validation are separate verifier/evaluator responsibilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueType {
    kind: ValueTypeKind,
}

impl ValueType {
    /// Constructs a primitive type, which is valid in either context.
    pub const fn primitive(primitive: PrimitiveType) -> Self {
        Self {
            kind: ValueTypeKind::Primitive(primitive),
        }
    }

    /// Normalizes an authored shape under the chosen context and codec limits.
    ///
    /// Unions must retain at least two distinct members after flattening and
    /// deduplication; enums must be nonempty and unique. Source aliases must
    /// already be resolved by the compiler.
    ///
    /// # Errors
    /// Returns [`TypeError`] for invalid cardinality, duplicate fields/enums,
    /// signature-only forms in value context, or resource/allocation failure.
    pub fn new(
        kind: ValueTypeKind,
        context: TypeContext,
        limits: &Limits,
    ) -> Result<Self, TypeError> {
        let value = wire::type_value(&kind, context, limits)?;
        let result = wire::parse_type(&value, context, limits, true)?;
        // Flattening can increase the size of an individual union array even
        // though the total representation shrinks. Check the normalized result.
        result.to_value(context, limits)?;
        Ok(result)
    }

    /// Borrows the normalized typed shape without allowing mutation.
    pub const fn kind(&self) -> &ValueTypeKind {
        &self.kind
    }

    /// Produces the canonical CBOR value, bounding conversion before allocation.
    ///
    /// # Errors
    /// Returns [`TypeError`] for incompatible context or exhausted limits/storage.
    pub fn to_value(&self, context: TypeContext, limits: &Limits) -> Result<Value, TypeError> {
        wire::type_value(&self.kind, context, limits)
    }

    /// Reads an already decoded canonical type without normalizing its structure.
    ///
    /// # Errors
    /// Returns [`TypeError`] for malformed/noncanonical types, context violations,
    /// or codec/resource failures. The input is bounded before type traversal.
    pub fn from_value(
        value: &Value,
        context: TypeContext,
        limits: &Limits,
    ) -> Result<Self, TypeError> {
        htlk_cbor::encode(value, limits)?;
        wire::parse_type(value, context, limits, false)
    }

    /// Encodes this type as deterministic CBOR under the selected context.
    ///
    /// # Errors
    /// Returns [`TypeError`] for context, resource, or allocation failure.
    pub fn encode(&self, context: TypeContext, limits: &Limits) -> Result<Vec<u8>, TypeError> {
        Ok(htlk_cbor::encode(&self.to_value(context, limits)?, limits)?)
    }

    /// Decodes exactly one canonical type record, rejecting trailing bytes.
    ///
    /// # Errors
    /// Returns [`TypeError`] for codec, schema, canonicality, or context failures.
    pub fn decode(bytes: &[u8], context: TypeContext, limits: &Limits) -> Result<Self, TypeError> {
        let value = htlk_cbor::decode(bytes, limits)?;
        wire::parse_type(&value, context, limits, false)
    }
}

/// A normalized declared type plus an independent presence requirement.
///
/// Required does not mean non-null. A nullable type may require a present value;
/// an optional non-null type may permit absence. The same record shape is used
/// for record fields and function parameters/results. Port names belong to the
/// containing table and are not serialized inside this record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    value_type: ValueType,
    required: bool,
}

impl Port {
    /// Combines an already normalized type and a presence flag. Use-site context
    /// and whole-record resource limits are checked on encoding or embedding.
    pub const fn new(value_type: ValueType, required: bool) -> Self {
        Self {
            value_type,
            required,
        }
    }
    /// Borrows the declared normalized type.
    pub const fn value_type(&self) -> &ValueType {
        &self.value_type
    }
    /// Returns whether a value must be present.
    pub const fn required(&self) -> bool {
        self.required
    }

    /// Produces exactly `{ type, required }`, with bounded conversion.
    ///
    /// # Errors
    /// Returns [`TypeError`] for context, resource, or allocation failure.
    pub fn to_value(&self, context: TypeContext, limits: &Limits) -> Result<Value, TypeError> {
        wire::port_value(self, context, limits)
    }

    /// Parses an exact canonical port record without inferring missing fields.
    ///
    /// # Errors
    /// Returns [`TypeError`] for codec, schema, canonicality, or context failures.
    pub fn from_value(
        value: &Value,
        context: TypeContext,
        limits: &Limits,
    ) -> Result<Self, TypeError> {
        htlk_cbor::encode(value, limits)?;
        wire::parse_port(value, context, limits, false)
    }

    /// Encodes the port as one deterministic CBOR record.
    ///
    /// # Errors
    /// Returns [`TypeError`] for context, resource, or allocation failure.
    pub fn encode(&self, context: TypeContext, limits: &Limits) -> Result<Vec<u8>, TypeError> {
        Ok(htlk_cbor::encode(&self.to_value(context, limits)?, limits)?)
    }

    /// Decodes exactly one canonical port record.
    ///
    /// # Errors
    /// Returns [`TypeError`] for codec, schema, canonicality, or context failures.
    pub fn decode(bytes: &[u8], context: TypeContext, limits: &Limits) -> Result<Self, TypeError> {
        let value = htlk_cbor::decode(bytes, limits)?;
        wire::parse_port(&value, context, limits, false)
    }
}
