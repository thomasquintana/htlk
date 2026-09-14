use std::fmt::{self, Write as _};

use htlk_cbor::{LimitKind, Limits, Map, Value};

use super::*;
use crate::digest::ParseDigestError;
use crate::record_accounting::{EncodingLimitError, RecordAccounting};
use crate::{ParseIdentifierError, Port, PrimitiveType, TypeContext, TypeError, ValueTypeKind};

/// Expression/template record validation and bounded-conversion failure.
/// Only static schema descriptions are retained, not untrusted input contents.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpressionError {
    /// Underlying canonical CBOR failure, with original offsets.
    Codec(htlk_cbor::Error),
    /// Malformed identifier.
    Identifier(ParseIdentifierError),
    /// Malformed digest.
    Digest(ParseDigestError),
    /// Invalid template parameter port/type record.
    Type(TypeError),
    /// Incorrect record/array/scalar shape at a fixed schema site.
    InvalidShape(&'static str),
    /// Unknown expression constructor.
    UnknownConstructor,
    /// Unknown function-identity form or core function.
    UnknownFunction,
    /// Index outside the nonnegative signed-i64 range.
    InvalidIndex,
    /// Unknown or repeated regex flags.
    InvalidRegexFlags,
    /// Duplicate exact record/argument/parameter name.
    DuplicateField,
    /// Canonical ingress violated a normalization rule.
    NonCanonical(&'static str),
    /// A reference/outcome category is forbidden at this expression location.
    ForbiddenReference(&'static str),
    /// Template parameter type is not string, integer, or Boolean.
    InvalidTemplateParameter,
    /// Parameter names do not exactly cover distinct template slots.
    TemplateParameterMismatch,
    /// Conversion would exceed a codec ceiling.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Fallible storage reservation failed.
    AllocationFailed,
}

impl From<htlk_cbor::Error> for ExpressionError {
    fn from(e: htlk_cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<ParseIdentifierError> for ExpressionError {
    fn from(e: ParseIdentifierError) -> Self {
        Self::Identifier(e)
    }
}
impl From<ParseDigestError> for ExpressionError {
    fn from(e: ParseDigestError) -> Self {
        Self::Digest(e)
    }
}
impl From<TypeError> for ExpressionError {
    fn from(e: TypeError) -> Self {
        Self::Type(e)
    }
}
impl From<EncodingLimitError> for ExpressionError {
    fn from(e: EncodingLimitError) -> Self {
        Self::LimitExceeded {
            limit: e.limit,
            maximum: e.maximum,
        }
    }
}
impl fmt::Display for ExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(e) => write!(f, "expression record: {e}"),
            Self::Identifier(e) => write!(f, "expression identifier: {e}"),
            Self::Digest(e) => write!(f, "expression digest: {e}"),
            Self::Type(e) => write!(f, "template parameter: {e}"),
            Self::InvalidShape(site) => write!(f, "invalid expression/template shape: {site}"),
            Self::UnknownConstructor => f.write_str("unknown expression constructor"),
            Self::UnknownFunction => f.write_str("unknown function identity"),
            Self::InvalidIndex => f.write_str("path index must be in 0..=i64::MAX"),
            Self::InvalidRegexFlags => f.write_str("regex flags must be unique members of ims"),
            Self::DuplicateField => f.write_str("duplicate expression/template name"),
            Self::NonCanonical(rule) => write!(f, "noncanonical expression/template: {rule}"),
            Self::ForbiddenReference(kind) => {
                write!(f, "reference is forbidden in this context: {kind}")
            }
            Self::InvalidTemplateParameter => {
                f.write_str("template parameters must be string, integer, or Boolean")
            }
            Self::TemplateParameterMismatch => {
                f.write_str("template parameters must exactly cover distinct slots")
            }
            Self::LimitExceeded { limit, maximum } => {
                write!(f, "expression limit exceeded: {limit:?} ({maximum})")
            }
            Self::AllocationFailed => f.write_str("expression allocation failed"),
        }
    }
}
impl std::error::Error for ExpressionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Identifier(e) => Some(e),
            Self::Digest(e) => Some(e),
            Self::Type(e) => Some(e),
            _ => None,
        }
    }
}

pub(super) fn allocation(_: std::collections::TryReserveError) -> ExpressionError {
    ExpressionError::AllocationFailed
}
pub(super) fn push<T>(v: &mut Vec<T>, value: T) -> Result<(), ExpressionError> {
    v.try_reserve(1).map_err(allocation)?;
    v.push(value);
    Ok(())
}
pub(super) fn owned(s: &str) -> Result<String, ExpressionError> {
    let mut result = String::new();
    result.try_reserve_exact(s.len()).map_err(allocation)?;
    result.push_str(s);
    Ok(result)
}
pub(super) fn text<'a>(v: &'a Value, site: &'static str) -> Result<&'a str, ExpressionError> {
    if let Value::Text(s) = v {
        Ok(s)
    } else {
        Err(ExpressionError::InvalidShape(site))
    }
}
pub(super) fn array<'a>(v: &'a Value, site: &'static str) -> Result<&'a [Value], ExpressionError> {
    if let Value::Array(v) = v {
        Ok(v)
    } else {
        Err(ExpressionError::InvalidShape(site))
    }
}
pub(super) fn name(v: &Value) -> Result<Identifier, ExpressionError> {
    Ok(text(v, "identifier")?.parse()?)
}
fn digest(v: &Value) -> Result<Digest, ExpressionError> {
    Ok(text(v, "digest")?.parse()?)
}
fn digest_text(value: Digest) -> Result<String, ExpressionError> {
    let mut text = String::new();
    text.try_reserve_exact(71).map_err(allocation)?;
    write!(&mut text, "{value}").expect("String formatting cannot fail");
    Ok(text)
}
fn arity(v: &[Value], n: usize) -> Result<(), ExpressionError> {
    if v.len() == n {
        Ok(())
    } else {
        Err(ExpressionError::InvalidShape("constructor arity"))
    }
}
fn check_reference(r: &ValueReference, c: ExpressionContext) -> Result<(), ExpressionError> {
    use ExpressionContext as C;
    let (allowed, label) = match r {
        ValueReference::Input(_) => (true, "input"),
        ValueReference::Output { .. } => (matches!(c, C::Guard { .. } | C::LoopUntil), "output"),
        ValueReference::ScopeOutput(_) => (
            matches!(
                c,
                C::PrimitivePostconditions
                    | C::ScopePostconditions
                    | C::WrapperPostconditions
                    | C::LoopUntil
                    | C::LoopPostconditions
            ),
            "scope_output",
        ),
        ValueReference::Carried(_) => (
            matches!(c, C::Guard { loop_body: true } | C::LoopUntil),
            "carried",
        ),
        ValueReference::Next(_) => (matches!(c, C::LoopUntil), "next"),
    };
    if allowed {
        Ok(())
    } else {
        Err(ExpressionError::ForbiddenReference(label))
    }
}
fn check_outcome(c: ExpressionContext) -> Result<(), ExpressionError> {
    if matches!(
        c,
        ExpressionContext::Guard { .. }
            | ExpressionContext::ScopePostconditions
            | ExpressionContext::LoopUntil
    ) {
        Ok(())
    } else {
        Err(ExpressionError::ForbiddenReference("node outcome"))
    }
}
fn regex_flags(flags: &str, normalize: bool) -> Result<String, ExpressionError> {
    let mut mask = 0;
    for flag in flags.bytes() {
        let bit = match flag {
            b'i' => 1,
            b'm' => 2,
            b's' => 4,
            _ => return Err(ExpressionError::InvalidRegexFlags),
        };
        if mask & bit != 0 {
            return Err(ExpressionError::InvalidRegexFlags);
        }
        mask |= bit;
    }
    let canonical = ["", "i", "m", "im", "s", "is", "ms", "ims"][mask];
    if !normalize && flags != canonical {
        return Err(ExpressionError::NonCanonical("regex flag order"));
    }
    owned(canonical)
}

pub(super) struct Builder<'a> {
    pub(super) accounting: RecordAccounting<'a>,
    context: ExpressionContext,
}
impl<'a> Builder<'a> {
    pub(super) fn new(
        context: ExpressionContext,
        limits: &'a Limits,
    ) -> Result<Self, ExpressionError> {
        Ok(Self {
            accounting: RecordAccounting::new(limits)?,
            context,
        })
    }
    pub(super) fn string(&mut self, s: &str, depth: usize) -> Result<String, ExpressionError> {
        self.accounting.text(s, depth)?;
        owned(s)
    }
    pub(super) fn text(&mut self, s: &str, depth: usize) -> Result<Value, ExpressionError> {
        Ok(Value::Text(self.string(s, depth)?))
    }
    pub(super) fn tagged(
        &mut self,
        tag: &str,
        n: usize,
        depth: usize,
    ) -> Result<Vec<Value>, ExpressionError> {
        self.accounting.collection(n, depth)?;
        let mut values = Vec::new();
        push(&mut values, self.text(tag, depth + 1)?)?;
        Ok(values)
    }
    fn scalar(&mut self, s: &ScalarLiteral, depth: usize) -> Result<Value, ExpressionError> {
        match s {
            ScalarLiteral::String(s) => self.text(s, depth),
            ScalarLiteral::Integer(n) => {
                self.accounting.integer(*n, depth)?;
                Ok(Value::Integer(*n))
            }
            ScalarLiteral::Float(n) => {
                self.accounting.float(*n, depth)?;
                Ok(Value::Float(*n))
            }
            ScalarLiteral::Boolean(b) => {
                self.accounting.boolean(depth)?;
                Ok(Value::Bool(*b))
            }
            ScalarLiteral::Null => {
                self.accounting.null(depth)?;
                Ok(Value::Null)
            }
            ScalarLiteral::Bytes(bytes) => {
                self.accounting.byte_string(bytes, depth)?;
                let mut copy = Vec::new();
                copy.try_reserve_exact(bytes.len()).map_err(allocation)?;
                copy.extend_from_slice(bytes);
                Ok(Value::Bytes(copy))
            }
        }
    }
    fn path(&mut self, path: &[PathStep], depth: usize) -> Result<Value, ExpressionError> {
        self.accounting.collection(path.len(), depth)?;
        let mut values = Vec::new();
        for step in path {
            let value = match step {
                PathStep::Field(s) => self.text(s, depth + 1)?,
                PathStep::Index(n) => {
                    let n = i64::try_from(*n).map_err(|_| ExpressionError::InvalidIndex)?;
                    self.accounting.integer(n, depth + 1)?;
                    Value::Integer(n)
                }
            };
            push(&mut values, value)?;
        }
        Ok(Value::Array(values))
    }
    fn reference(&mut self, r: &ValueReference, depth: usize) -> Result<Value, ExpressionError> {
        check_reference(r, self.context)?;
        let (tag, name) = match r {
            ValueReference::Input(n) => ("input", n),
            ValueReference::ScopeOutput(n) => ("scope_output", n),
            ValueReference::Carried(n) => ("carried", n),
            ValueReference::Next(n) => ("next", n),
            ValueReference::Output { node, port } => {
                let mut v = self.tagged("output", 3, depth)?;
                push(&mut v, self.text(node.as_str(), depth + 1)?)?;
                push(&mut v, self.text(port.as_str(), depth + 1)?)?;
                return Ok(Value::Array(v));
            }
        };
        let mut v = self.tagged(tag, 2, depth)?;
        push(&mut v, self.text(name.as_str(), depth + 1)?)?;
        Ok(Value::Array(v))
    }
    fn function(&mut self, id: &FunctionId, depth: usize) -> Result<Value, ExpressionError> {
        let mut v = match id {
            FunctionId::Core(_) => self.tagged("core", 2, depth)?,
            FunctionId::Library { .. } => self.tagged("library", 3, depth)?,
        };
        match id {
            FunctionId::Core(f) => push(
                &mut v,
                self.text(
                    match f {
                        CoreFunction::Length => "length",
                        CoreFunction::Present => "present",
                    },
                    depth + 1,
                )?,
            )?,
            FunctionId::Library { library, name } => {
                push(&mut v, self.text(&digest_text(*library)?, depth + 1)?)?;
                push(&mut v, self.text(name.as_str(), depth + 1)?)?;
            }
        }
        Ok(Value::Array(v))
    }
    fn expressions(
        &mut self,
        expressions: &[Expression],
        depth: usize,
    ) -> Result<Value, ExpressionError> {
        self.accounting.collection(expressions.len(), depth)?;
        let mut values = Vec::new();
        for expression in expressions {
            push(&mut values, self.expr(expression.kind(), depth + 1)?)?;
        }
        Ok(Value::Array(values))
    }
    fn expr(&mut self, k: &ExpressionKind, depth: usize) -> Result<Value, ExpressionError> {
        use ExpressionKind as K;
        match k {
            K::Literal(s) => self.literal(s, depth),
            K::Regex { pattern, flags } => self.regex(pattern, flags, depth),
            K::Ref { source, path } => self.ref_expr(source, path, depth),
            K::Get { value, path } => self.get_expr(value, path, depth),
            K::List(items) => self.list(items, depth),
            K::Record(fields) => self.record(fields, depth),
            K::Call {
                function,
                arguments,
            } => self.call(function, arguments, depth),
            K::FunctionRef { library, name } => self.function_ref(*library, name, depth),
            K::Render {
                template,
                arguments,
            } => self.render(*template, arguments, depth),
            K::Status(name) => self.outcome("status", name, depth),
            K::Error(name) => self.outcome("error", name, depth),
            K::Not(child) => self.not(child, depth),
            K::Binary {
                operator,
                left,
                right,
            } => self.binary(*operator, left, right, depth),
        }
    }
    // Keep large branch-local temporaries out of recursive dispatch frames.
    #[inline(never)]
    fn literal(&mut self, s: &ScalarLiteral, d: usize) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("literal", 2, d)?;
        push(&mut v, self.scalar(s, d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn list(&mut self, items: &[Expression], d: usize) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("list", 2, d)?;
        push(&mut v, self.expressions(items, d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn not(&mut self, child: &Expression, d: usize) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("not", 2, d)?;
        push(&mut v, self.expr(child.kind(), d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn outcome(
        &mut self,
        tag: &str,
        name: &Identifier,
        d: usize,
    ) -> Result<Value, ExpressionError> {
        check_outcome(self.context)?;
        let mut v = self.tagged(tag, 2, d)?;
        push(&mut v, self.text(name.as_str(), d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn regex(&mut self, pattern: &str, flags: &str, d: usize) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("regex", 3, d)?;
        push(&mut v, self.text(pattern, d + 1)?)?;
        push(&mut v, self.text(flags, d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn ref_expr(
        &mut self,
        source: &ValueReference,
        path: &[PathStep],
        d: usize,
    ) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("ref", 3, d)?;
        push(&mut v, self.reference(source, d + 1)?)?;
        push(&mut v, self.path(path, d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn get_expr(
        &mut self,
        value: &Expression,
        path: &[PathStep],
        d: usize,
    ) -> Result<Value, ExpressionError> {
        if path.is_empty() {
            return Err(ExpressionError::InvalidShape("nonempty get path"));
        }
        let mut v = self.tagged("get", 3, d)?;
        push(&mut v, self.expr(value.kind(), d + 1)?)?;
        push(&mut v, self.path(path, d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn record(
        &mut self,
        fields: &[(String, Expression)],
        d: usize,
    ) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("record", 2, d)?;
        self.accounting.collection(fields.len(), d + 1)?;
        let mut pairs = Vec::new();
        for (name, expression) in fields {
            self.accounting.collection(2, d + 2)?;
            let mut pair = Vec::new();
            push(&mut pair, self.text(name, d + 3)?)?;
            push(&mut pair, self.expr(expression.kind(), d + 3)?)?;
            push(&mut pairs, Value::Array(pair))?;
        }
        push(&mut v, Value::Array(pairs))?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn call(
        &mut self,
        function: &FunctionId,
        args: &[Expression],
        d: usize,
    ) -> Result<Value, ExpressionError> {
        if matches!(function, FunctionId::Core(_)) && args.len() != 1 {
            return Err(ExpressionError::InvalidShape("core function arity"));
        }
        let mut v = self.tagged("call", 3, d)?;
        push(&mut v, self.function(function, d + 1)?)?;
        push(&mut v, self.expressions(args, d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn function_ref(
        &mut self,
        library: Digest,
        name: &Identifier,
        d: usize,
    ) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("function_ref", 3, d)?;
        push(&mut v, self.text(&digest_text(library)?, d + 1)?)?;
        push(&mut v, self.text(name.as_str(), d + 1)?)?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn render(
        &mut self,
        template: Digest,
        args: &[(Identifier, Expression)],
        d: usize,
    ) -> Result<Value, ExpressionError> {
        let mut v = self.tagged("render", 3, d)?;
        push(&mut v, self.text(&digest_text(template)?, d + 1)?)?;
        self.accounting.collection(args.len(), d + 1)?;
        let mut pairs = Vec::new();
        for (name, expr) in args {
            let key = self.string(name.as_str(), d + 2)?;
            let value = self.expr(expr.kind(), d + 2)?;
            push(&mut pairs, (key, value))?;
        }
        push(&mut v, Value::Map(Map::try_from_entries(pairs)?))?;
        Ok(Value::Array(v))
    }
    #[inline(never)]
    fn binary(
        &mut self,
        op: BinaryOperator,
        left: &Expression,
        right: &Expression,
        d: usize,
    ) -> Result<Value, ExpressionError> {
        let mut v = self.tagged(op.as_str(), 3, d)?;
        push(&mut v, self.expr(left.kind(), d + 1)?)?;
        push(&mut v, self.expr(right.kind(), d + 1)?)?;
        Ok(Value::Array(v))
    }
    pub(super) fn parameter(&mut self, port: &Port, d: usize) -> Result<Value, ExpressionError> {
        let p = match port.value_type().kind() {
            ValueTypeKind::Primitive(
                p @ (PrimitiveType::String | PrimitiveType::Integer | PrimitiveType::Boolean),
            ) => p,
            _ => return Err(ExpressionError::InvalidTemplateParameter),
        };
        self.accounting.collection(2, d)?;
        let ty = self.string("type", d + 1)?;
        let value = self.text(p.as_str(), d + 1)?;
        let required = self.string("required", d + 1)?;
        self.accounting.boolean(d + 1)?;
        Ok(Value::Map(Map::try_from_entries([
            (ty, value),
            (required, Value::Bool(port.required())),
        ])?))
    }
}

pub(super) fn expression_value(
    kind: &ExpressionKind,
    context: ExpressionContext,
    limits: &Limits,
) -> Result<Value, ExpressionError> {
    Builder::new(context, limits)?.expr(kind, 0)
}

fn parse_scalar(v: &Value) -> Result<ScalarLiteral, ExpressionError> {
    Ok(match v {
        Value::Text(s) => ScalarLiteral::String(owned(s)?),
        Value::Integer(n) => ScalarLiteral::Integer(*n),
        Value::Float(n) => ScalarLiteral::Float(*n),
        Value::Bool(b) => ScalarLiteral::Boolean(*b),
        Value::Null => ScalarLiteral::Null,
        Value::Bytes(v) => {
            let mut copy = Vec::new();
            copy.try_reserve_exact(v.len()).map_err(allocation)?;
            copy.extend_from_slice(v);
            ScalarLiteral::Bytes(copy)
        }
        _ => return Err(ExpressionError::InvalidShape("scalar literal")),
    })
}
fn parse_path(v: &Value) -> Result<Vec<PathStep>, ExpressionError> {
    let mut path = Vec::new();
    for v in array(v, "path")? {
        push(
            &mut path,
            match v {
                Value::Text(s) => PathStep::Field(owned(s)?),
                Value::Integer(n) => {
                    PathStep::Index(u64::try_from(*n).map_err(|_| ExpressionError::InvalidIndex)?)
                }
                _ => return Err(ExpressionError::InvalidShape("path step")),
            },
        )?;
    }
    Ok(path)
}
fn parse_reference(
    v: &Value,
    context: ExpressionContext,
) -> Result<ValueReference, ExpressionError> {
    let v = array(v, "value reference")?;
    let tag = text(
        v.first()
            .ok_or(ExpressionError::InvalidShape("value reference"))?,
        "reference tag",
    )?;
    arity(v, if tag == "output" { 3 } else { 2 })?;
    let r = match tag {
        "input" => ValueReference::Input(name(&v[1])?),
        "scope_output" => ValueReference::ScopeOutput(name(&v[1])?),
        "carried" => ValueReference::Carried(name(&v[1])?),
        "next" => ValueReference::Next(name(&v[1])?),
        "output" => ValueReference::Output {
            node: name(&v[1])?,
            port: name(&v[2])?,
        },
        _ => return Err(ExpressionError::InvalidShape("reference tag")),
    };
    check_reference(&r, context)?;
    Ok(r)
}
fn parse_function(v: &Value) -> Result<FunctionId, ExpressionError> {
    let v = array(v, "function id")?;
    let tag = text(
        v.first().ok_or(ExpressionError::UnknownFunction)?,
        "function tag",
    )?;
    match tag {
        "core" => {
            arity(v, 2)?;
            Ok(FunctionId::Core(match text(&v[1], "core function")? {
                "length" => CoreFunction::Length,
                "present" => CoreFunction::Present,
                _ => return Err(ExpressionError::UnknownFunction),
            }))
        }
        "library" => {
            arity(v, 3)?;
            Ok(FunctionId::Library {
                library: digest(&v[1])?,
                name: name(&v[2])?,
            })
        }
        _ => Err(ExpressionError::UnknownFunction),
    }
}

pub(super) fn parse_expression(
    value: &Value,
    context: ExpressionContext,
    limits: &Limits,
    normalize: bool,
) -> Result<Expression, ExpressionError> {
    let v = array(value, "expression")?;
    let tag = text(
        v.first()
            .ok_or(ExpressionError::InvalidShape("expression tag"))?,
        "expression tag",
    )?;
    let n = match tag {
        "literal" | "list" | "record" | "status" | "error" | "not" => 2,
        "regex" | "ref" | "get" | "call" | "function_ref" | "render" => 3,
        _ if BinaryOperator::parse(tag).is_some() => 3,
        _ => return Err(ExpressionError::UnknownConstructor),
    };
    arity(v, n)?;
    match tag {
        "literal" | "regex" | "ref" | "function_ref" | "status" | "error" => {
            parse_leaf(tag, v, context, normalize)
        }
        "get" => parse_get(v, context, limits, normalize),
        "list" => parse_list(&v[1], context, limits, normalize),
        "record" => parse_record(&v[1], context, limits, normalize),
        "call" => parse_call(v, context, limits, normalize),
        "render" => parse_render(v, context, limits, normalize),
        "not" => parse_not(&v[1], context, limits, normalize),
        _ => parse_binary(
            BinaryOperator::parse(tag).expect("operator checked above"),
            v,
            context,
            limits,
            normalize,
        ),
    }
}

fn wrap(kind: ExpressionKind) -> Expression {
    Expression {
        kind: Box::new(kind),
    }
}

#[inline(never)]
fn parse_not(
    v: &Value,
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    Ok(wrap(ExpressionKind::Not(Box::new(parse_expression(
        v, c, l, n,
    )?))))
}

#[inline(never)]
fn parse_list(
    v: &Value,
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    Ok(wrap(ExpressionKind::List(parse_expressions(v, c, l, n)?)))
}

// Leaf parsing has no expression recursion. Keep its larger temporary values
// out of every recursive dispatch frame.
#[inline(never)]
fn parse_leaf(
    tag: &str,
    v: &[Value],
    context: ExpressionContext,
    normalize: bool,
) -> Result<Expression, ExpressionError> {
    use ExpressionKind as K;
    Ok(wrap(match tag {
        "literal" => K::Literal(parse_scalar(&v[1])?),
        "regex" => K::Regex {
            pattern: owned(text(&v[1], "regex pattern")?)?,
            flags: regex_flags(text(&v[2], "regex flags")?, normalize)?,
        },
        "ref" => K::Ref {
            source: parse_reference(&v[1], context)?,
            path: parse_path(&v[2])?,
        },
        "function_ref" => K::FunctionRef {
            library: digest(&v[1])?,
            name: name(&v[2])?,
        },
        "status" | "error" => {
            check_outcome(context)?;
            if tag == "status" {
                K::Status(name(&v[1])?)
            } else {
                K::Error(name(&v[1])?)
            }
        }
        _ => unreachable!("leaf tag checked by dispatcher"),
    }))
}
#[inline(never)]
fn parse_expressions(
    v: &Value,
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Vec<Expression>, ExpressionError> {
    let mut result = Vec::new();
    for v in array(v, "expression list")? {
        push(&mut result, parse_expression(v, c, l, n)?)?;
    }
    Ok(result)
}
#[inline(never)]
fn parse_get(
    v: &[Value],
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    let base = parse_expression(&v[1], c, l, n)?;
    let mut path = parse_path(&v[2])?;
    if path.is_empty() {
        return Err(ExpressionError::InvalidShape("nonempty get path"));
    }
    let kind = match *base.kind {
        ExpressionKind::Ref {
            source,
            path: mut before,
        } => {
            if !n {
                return Err(ExpressionError::NonCanonical("get on reference"));
            }
            before.try_reserve(path.len()).map_err(allocation)?;
            before.append(&mut path);
            ExpressionKind::Ref {
                source,
                path: before,
            }
        }
        ExpressionKind::Get {
            value,
            path: mut before,
        } => {
            if !n {
                return Err(ExpressionError::NonCanonical("nested get"));
            }
            before.try_reserve(path.len()).map_err(allocation)?;
            before.append(&mut path);
            ExpressionKind::Get {
                value,
                path: before,
            }
        }
        _ => ExpressionKind::Get {
            value: Box::new(base),
            path,
        },
    };
    Ok(wrap(kind))
}
#[inline(never)]
fn parse_record(
    v: &Value,
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    let mut fields = Vec::new();
    for v in array(v, "record expression fields")? {
        let pair = array(v, "record expression pair")?;
        arity(pair, 2)?;
        push(
            &mut fields,
            (
                owned(text(&pair[0], "record key")?)?,
                parse_expression(&pair[1], c, l, n)?,
            ),
        )?;
    }
    if n {
        fields.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    }
    for pair in fields.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(ExpressionError::DuplicateField);
        }
        if pair[0].0 > pair[1].0 {
            return Err(ExpressionError::NonCanonical("record field UTF-8 order"));
        }
    }
    Ok(wrap(ExpressionKind::Record(fields)))
}
#[inline(never)]
fn parse_call(
    v: &[Value],
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    let function = parse_function(&v[1])?;
    if matches!(function, FunctionId::Core(_)) && array(&v[2], "call arguments")?.len() != 1 {
        return Err(ExpressionError::InvalidShape("core function arity"));
    }
    Ok(wrap(ExpressionKind::Call {
        function,
        arguments: parse_expressions(&v[2], c, l, n)?,
    }))
}
#[inline(never)]
fn parse_render(
    v: &[Value],
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    let template = digest(&v[1])?;
    let Value::Map(map) = &v[2] else {
        return Err(ExpressionError::InvalidShape("render arguments"));
    };
    let mut arguments = Vec::new();
    for (key, value) in map.iter() {
        push(
            &mut arguments,
            (
                key.parse::<Identifier>()?,
                parse_expression(value, c, l, n)?,
            ),
        )?;
    }
    // Wire map order is the codec's order; evaluation order is decoded UTF-8.
    arguments.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(wrap(ExpressionKind::Render {
        template,
        arguments,
    }))
}
#[inline(never)]
fn parse_binary(
    op: BinaryOperator,
    v: &[Value],
    c: ExpressionContext,
    l: &Limits,
    n: bool,
) -> Result<Expression, ExpressionError> {
    Ok(wrap(ExpressionKind::Binary {
        operator: op,
        left: Box::new(parse_expression(&v[1], c, l, n)?),
        right: Box::new(parse_expression(&v[2], c, l, n)?),
    }))
}

pub(super) fn parse_parameter(v: &Value, limits: &Limits) -> Result<Port, ExpressionError> {
    let port = Port::from_value(v, TypeContext::Value, limits)?;
    if !matches!(
        port.value_type().kind(),
        ValueTypeKind::Primitive(
            PrimitiveType::String | PrimitiveType::Integer | PrimitiveType::Boolean
        )
    ) {
        return Err(ExpressionError::InvalidTemplateParameter);
    }
    Ok(port)
}
