use std::fmt::{self, Write as _};

use htlk_cbor::{LimitKind, Limits, Map, Value};

use super::{Port, PrimitiveType, TypeContext, ValueType, ValueTypeKind};
use crate::digest::{Digest, ParseDigestError};
use crate::{Identifier, ParseIdentifierError};

/// A canonical type/port construction or decoding failure.
///
/// Descriptions and field names in errors are fixed schema terms, not submitted
/// values. Codec, identifier, and digest failures remain available as sources.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TypeError {
    /// Underlying canonical CBOR failure, including input byte offsets.
    Codec(htlk_cbor::Error),
    /// A record/array/scalar has the wrong shape at the named schema site.
    InvalidShape(&'static str),
    /// Text is not a supported canonical primitive (aliases are not accepted).
    UnknownPrimitive,
    /// A tagged type array uses an unknown constructor.
    UnknownConstructor,
    /// A port contains an undeclared metadata field.
    UnknownPortField,
    /// A required port metadata field is missing.
    MissingPortField(&'static str),
    /// A union has fewer than two distinct normalized members.
    UnionCardinality,
    /// An enum contains no members.
    EmptyEnum,
    /// An enum repeats the same exact string.
    DuplicateEnumMember,
    /// The canonical record violates the stated ordering/normalization rule.
    NonCanonical(&'static str),
    /// A signature-only constructor appeared in ordinary value context.
    SignatureOnly(&'static str),
    /// A signature variable's identifier is malformed.
    Identifier(ParseIdentifierError),
    /// A schema digest is malformed.
    Digest(ParseDigestError),
    /// Building the canonical representation would exceed a codec limit.
    LimitExceeded {
        /// Resource whose ceiling was exceeded.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Reserving conversion/normalization storage failed.
    AllocationFailed,
}

impl From<htlk_cbor::Error> for TypeError {
    fn from(error: htlk_cbor::Error) -> Self {
        Self::Codec(error)
    }
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(error) => write!(f, "type record: {error}"),
            Self::InvalidShape(site) => write!(f, "invalid type record shape: {site}"),
            Self::UnknownPrimitive => f.write_str("unknown canonical primitive type"),
            Self::UnknownConstructor => f.write_str("unknown canonical type constructor"),
            Self::UnknownPortField => f.write_str("unknown port field"),
            Self::MissingPortField(field) => write!(f, "missing port field: {field}"),
            Self::UnionCardinality => f.write_str("union requires at least two distinct members"),
            Self::EmptyEnum => f.write_str("enum requires at least one member"),
            Self::DuplicateEnumMember => f.write_str("duplicate enum member"),
            Self::NonCanonical(rule) => write!(f, "noncanonical type record: {rule}"),
            Self::SignatureOnly(kind) => {
                write!(f, "type constructor requires signature context: {kind}")
            }
            Self::Identifier(error) => write!(f, "type variable: {error}"),
            Self::Digest(error) => write!(f, "schema type: {error}"),
            Self::LimitExceeded { limit, maximum } => {
                write!(f, "type conversion limit exceeded: {limit:?} ({maximum})")
            }
            Self::AllocationFailed => f.write_str("type record allocation failed"),
        }
    }
}

impl std::error::Error for TypeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            Self::Identifier(error) => Some(error),
            Self::Digest(error) => Some(error),
            _ => None,
        }
    }
}

fn allocation(_: std::collections::TryReserveError) -> TypeError {
    TypeError::AllocationFailed
}
fn push<T>(values: &mut Vec<T>, value: T) -> Result<(), TypeError> {
    values.try_reserve(1).map_err(allocation)?;
    values.push(value);
    Ok(())
}
fn owned(text: &str) -> Result<String, TypeError> {
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(allocation)?;
    result.push_str(text);
    Ok(result)
}
fn signature(context: TypeContext, tag: &'static str) -> Result<(), TypeError> {
    if context == TypeContext::Value {
        Err(TypeError::SignatureOnly(tag))
    } else {
        Ok(())
    }
}

// Conversion accounts for the actual CBOR subset used by type records (text,
// arrays, maps, and booleans) BEFORE cloning strings or growing collections.
// This is not execution-budget accounting. The final encoder still owns bytes.
struct Builder<'a> {
    limits: &'a Limits,
    context: TypeContext,
    values: usize,
    payload: usize,
    bytes: usize,
}

fn check(value: usize, maximum: usize, limit: LimitKind) -> Result<(), TypeError> {
    if value > maximum {
        return Err(TypeError::LimitExceeded { limit, maximum });
    }
    Ok(())
}
fn add(
    current: usize,
    amount: usize,
    maximum: usize,
    limit: LimitKind,
) -> Result<usize, TypeError> {
    let total = current
        .checked_add(amount)
        .ok_or(TypeError::LimitExceeded { limit, maximum })?;
    check(total, maximum, limit)?;
    Ok(total)
}
fn header_size(len: usize) -> usize {
    match len {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=0xffff_ffff => 5,
        _ => 9,
    }
}

impl<'a> Builder<'a> {
    fn new(context: TypeContext, limits: &'a Limits) -> Result<Self, TypeError> {
        limits.validate()?;
        Ok(Self {
            limits,
            context,
            values: 0,
            payload: 0,
            bytes: 0,
        })
    }
    fn enter(&mut self, depth: usize) -> Result<(), TypeError> {
        check(depth, self.limits.max_depth, LimitKind::Depth)?;
        self.values = add(
            self.values,
            1,
            self.limits.max_total_values,
            LimitKind::TotalValues,
        )?;
        Ok(())
    }
    fn bytes(&mut self, bytes: usize) -> Result<(), TypeError> {
        self.bytes = add(
            self.bytes,
            bytes,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        Ok(())
    }
    fn collection(&mut self, len: usize, depth: usize) -> Result<(), TypeError> {
        self.enter(depth)?;
        check(
            len,
            self.limits.max_collection_entries,
            LimitKind::CollectionEntries,
        )?;
        self.bytes(header_size(len))
    }
    fn string(&mut self, text: &str, depth: usize) -> Result<String, TypeError> {
        self.enter(depth)?;
        check(text.len(), self.limits.max_text_bytes, LimitKind::TextBytes)?;
        self.payload = add(
            self.payload,
            text.len(),
            self.limits.max_total_payload_bytes,
            LimitKind::TotalPayloadBytes,
        )?;
        self.bytes(header_size(text.len()))?;
        self.bytes(text.len())?;
        owned(text)
    }
    fn text(&mut self, text: &str, depth: usize) -> Result<Value, TypeError> {
        Ok(Value::Text(self.string(text, depth)?))
    }
    fn tagged(&mut self, tag: &str, len: usize, depth: usize) -> Result<Vec<Value>, TypeError> {
        self.collection(len, depth)?;
        let mut values = Vec::new();
        push(&mut values, self.text(tag, depth + 1)?)?;
        Ok(values)
    }
    fn port(&mut self, port: &Port, depth: usize) -> Result<Value, TypeError> {
        self.collection(2, depth)?;
        let ty_key = self.string("type", depth + 1)?;
        let ty = self.ty(&port.value_type.kind, depth + 1)?;
        let required_key = self.string("required", depth + 1)?;
        self.enter(depth + 1)?;
        self.bytes(1)?;
        Ok(Value::Map(Map::try_from_entries([
            (ty_key, ty),
            (required_key, Value::Bool(port.required)),
        ])?))
    }
    fn ty(&mut self, kind: &ValueTypeKind, depth: usize) -> Result<Value, TypeError> {
        use ValueTypeKind as K;
        match kind {
            K::Primitive(primitive) => self.text(primitive.as_str(), depth),
            K::List(child) | K::Map(child) => {
                let mut values = self.tagged(
                    if matches!(kind, K::List(_)) {
                        "list"
                    } else {
                        "map"
                    },
                    2,
                    depth,
                )?;
                push(&mut values, self.ty(&child.kind, depth + 1)?)?;
                Ok(Value::Array(values))
            }
            K::Record(fields) => self.record(fields, depth),
            K::Union(members) => self.union(members, depth),
            K::Enum(members) => self.enumeration(members, depth),
            K::Schema(digest) => {
                let mut values = self.tagged("schema", 2, depth)?;
                let mut text = String::new();
                text.try_reserve_exact(71).map_err(allocation)?;
                write!(&mut text, "{digest}").expect("String formatting cannot fail");
                push(&mut values, self.text(&text, depth + 1)?)?;
                Ok(Value::Array(values))
            }
            K::Var(name) => {
                signature(self.context, "var")?;
                let mut values = self.tagged("var", 2, depth)?;
                push(&mut values, self.text(name.as_str(), depth + 1)?)?;
                Ok(Value::Array(values))
            }
            K::Function {
                parameters,
                returns,
            } => self.function(parameters, returns, depth),
        }
    }

    // Separate collection branches keep the recursive dispatch frame small.
    #[inline(never)]
    fn record(&mut self, fields: &[(String, Port)], depth: usize) -> Result<Value, TypeError> {
        let mut values = self.tagged("record", 2, depth)?;
        self.collection(fields.len(), depth + 1)?;
        let mut entries = Vec::new();
        for (name, port) in fields {
            let key = self.string(name, depth + 2)?;
            let value = self.port(port, depth + 2)?;
            push(&mut entries, (key, value))?;
        }
        push(&mut values, Value::Map(Map::try_from_entries(entries)?))?;
        Ok(Value::Array(values))
    }

    #[inline(never)]
    fn union(&mut self, members: &[ValueType], depth: usize) -> Result<Value, TypeError> {
        let mut values = self.tagged("union", 2, depth)?;
        self.collection(members.len(), depth + 1)?;
        let mut children = Vec::new();
        for member in members {
            push(&mut children, self.ty(&member.kind, depth + 2)?)?;
        }
        push(&mut values, Value::Array(children))?;
        Ok(Value::Array(values))
    }

    #[inline(never)]
    fn enumeration(&mut self, members: &[String], depth: usize) -> Result<Value, TypeError> {
        let mut values = self.tagged("enum", 2, depth)?;
        self.collection(members.len(), depth + 1)?;
        let mut children = Vec::new();
        for member in members {
            push(&mut children, self.text(member, depth + 2)?)?;
        }
        push(&mut values, Value::Array(children))?;
        Ok(Value::Array(values))
    }

    #[inline(never)]
    fn function(
        &mut self,
        parameters: &[Port],
        returns: &Port,
        depth: usize,
    ) -> Result<Value, TypeError> {
        signature(self.context, "function")?;
        let mut values = self.tagged("function", 3, depth)?;
        self.collection(parameters.len(), depth + 1)?;
        let mut params = Vec::new();
        for port in parameters {
            push(&mut params, self.port(port, depth + 2)?)?;
        }
        push(&mut values, Value::Array(params))?;
        push(&mut values, self.port(returns, depth + 1)?)?;
        Ok(Value::Array(values))
    }
}

pub(super) fn type_value(
    kind: &ValueTypeKind,
    context: TypeContext,
    limits: &Limits,
) -> Result<Value, TypeError> {
    Builder::new(context, limits)?.ty(kind, 0)
}
pub(super) fn port_value(
    port: &Port,
    context: TypeContext,
    limits: &Limits,
) -> Result<Value, TypeError> {
    Builder::new(context, limits)?.port(port, 0)
}

fn array<'a>(value: &'a Value, site: &'static str) -> Result<&'a [Value], TypeError> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(TypeError::InvalidShape(site)),
    }
}
fn text<'a>(value: &'a Value, site: &'static str) -> Result<&'a str, TypeError> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(TypeError::InvalidShape(site)),
    }
}

// The public entry points bound raw CBOR before reaching these recursive parsers.
pub(super) fn parse_port(
    value: &Value,
    context: TypeContext,
    limits: &Limits,
    normalize: bool,
) -> Result<Port, TypeError> {
    let Value::Map(map) = value else {
        return Err(TypeError::InvalidShape("port record"));
    };
    if map
        .iter()
        .any(|(key, _)| key != "type" && key != "required")
    {
        return Err(TypeError::UnknownPortField);
    }
    for field in ["required", "type"] {
        if map.get(field).is_none() {
            return Err(TypeError::MissingPortField(field));
        }
    }
    let Some(Value::Bool(required)) = map.get("required") else {
        return Err(TypeError::InvalidShape("required boolean"));
    };
    let ty = parse_type(
        map.get("type").expect("field checked above"),
        context,
        limits,
        normalize,
    )?;
    Ok(Port::new(ty, *required))
}

pub(super) fn parse_type(
    value: &Value,
    context: TypeContext,
    limits: &Limits,
    normalize: bool,
) -> Result<ValueType, TypeError> {
    use ValueTypeKind as K;
    if let Value::Text(name) = value {
        return Ok(ValueType::primitive(PrimitiveType::parse(name)?));
    }
    let values = array(value, "type")?;
    let Some(first) = values.first() else {
        return Err(TypeError::InvalidShape("type constructor"));
    };
    let tag = text(first, "type constructor")?;
    let expected = match tag {
        "list" | "map" | "record" | "union" | "enum" | "schema" | "var" => 2,
        "function" => 3,
        _ => return Err(TypeError::UnknownConstructor),
    };
    if values.len() != expected {
        return Err(TypeError::InvalidShape("constructor arity"));
    }
    let kind = match tag {
        "list" | "map" => {
            let child = Box::new(parse_type(&values[1], context, limits, normalize)?);
            if tag == "list" {
                K::List(child)
            } else {
                K::Map(child)
            }
        }
        "record" => parse_record(&values[1], context, limits, normalize)?,
        "enum" => parse_enum(&values[1], normalize)?,
        "union" => parse_union(&values[1], context, limits, normalize)?,
        "schema" => K::Schema(
            text(&values[1], "schema digest")?
                .parse::<Digest>()
                .map_err(TypeError::Digest)?,
        ),
        "var" => {
            signature(context, "var")?;
            K::Var(
                Identifier::try_from(text(&values[1], "type variable")?)
                    .map_err(TypeError::Identifier)?,
            )
        }
        "function" => parse_function(&values[1], &values[2], context, limits, normalize)?,
        _ => unreachable!("constructor checked above"),
    };
    Ok(ValueType { kind })
}

// Keep branch-local collection/sort temporaries out of the recursive dispatch
// frame, including in optimized builds.
#[inline(never)]
fn parse_record(
    value: &Value,
    context: TypeContext,
    limits: &Limits,
    normalize: bool,
) -> Result<ValueTypeKind, TypeError> {
    let Value::Map(fields) = value else {
        return Err(TypeError::InvalidShape("record fields"));
    };
    let mut ports = Vec::new();
    for (name, port) in fields.iter() {
        push(
            &mut ports,
            (owned(name)?, parse_port(port, context, limits, normalize)?),
        )?;
    }
    Ok(ValueTypeKind::Record(ports))
}

#[inline(never)]
fn parse_enum(value: &Value, normalize: bool) -> Result<ValueTypeKind, TypeError> {
    let items = array(value, "enum members")?;
    if items.is_empty() {
        return Err(TypeError::EmptyEnum);
    }
    let mut members = Vec::new();
    for item in items {
        push(&mut members, owned(text(item, "enum member")?)?)?;
    }
    if normalize {
        members.sort_unstable();
    }
    for pair in members.windows(2) {
        if pair[0] == pair[1] {
            return Err(TypeError::DuplicateEnumMember);
        }
        if pair[0] > pair[1] {
            return Err(TypeError::NonCanonical("enum UTF-8 ordering"));
        }
    }
    Ok(ValueTypeKind::Enum(members))
}

#[inline(never)]
fn parse_union(
    value: &Value,
    context: TypeContext,
    limits: &Limits,
    normalize: bool,
) -> Result<ValueTypeKind, TypeError> {
    let mut members = Vec::new();
    for item in array(value, "union members")? {
        let member = parse_type(item, context, limits, normalize)?;
        if let ValueTypeKind::Union(mut children) = member.kind {
            if !normalize {
                return Err(TypeError::NonCanonical("nested union"));
            }
            members.try_reserve(children.len()).map_err(allocation)?;
            members.append(&mut children);
        } else {
            push(&mut members, member)?;
        }
    }
    let mut keyed = Vec::new();
    for member in members {
        let bytes = member.encode(context, limits)?;
        push(&mut keyed, (bytes, member))?;
    }
    if normalize {
        keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        keyed.dedup_by(|a, b| a.0 == b.0);
    } else if keyed.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(TypeError::NonCanonical(
            "union encoded-byte ordering/uniqueness",
        ));
    }
    if keyed.len() < 2 {
        return Err(TypeError::UnionCardinality);
    }
    let mut members = Vec::new();
    for (_, member) in keyed {
        push(&mut members, member)?;
    }
    Ok(ValueTypeKind::Union(members))
}

#[inline(never)]
fn parse_function(
    params: &Value,
    result: &Value,
    context: TypeContext,
    limits: &Limits,
    normalize: bool,
) -> Result<ValueTypeKind, TypeError> {
    signature(context, "function")?;
    let mut parameters = Vec::new();
    for value in array(params, "function parameters")? {
        push(
            &mut parameters,
            parse_port(value, context, limits, normalize)?,
        )?;
    }
    Ok(ValueTypeKind::Function {
        parameters,
        returns: Box::new(parse_port(result, context, limits, normalize)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_arithmetic_is_checked() {
        assert!(matches!(
            add(usize::MAX, 1, usize::MAX, LimitKind::DocumentBytes),
            Err(TypeError::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                ..
            })
        ));
        assert_eq!(
            add(usize::MAX - 1, 1, usize::MAX, LimitKind::TotalValues).unwrap(),
            usize::MAX
        );
    }
}
