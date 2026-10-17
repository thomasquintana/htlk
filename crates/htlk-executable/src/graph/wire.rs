use std::{fmt, fmt::Write as _};

use crate::cbor as htlk_cbor;
use htlk_cbor::{LimitKind, Limits, Map, Value};

use super::*;
use crate::digest::ParseDigestError;
use crate::record_accounting::{EncodingLimitError, RecordAccounting};
use crate::{
    BuiltinType, ExecutionOptionsError, ExpressionError, ParseIdentifierError, TypeContext,
    TypeError, ValueTypeKind,
};

/// Graph model representation/local invariant failures and selected semantic
/// failures produced by analysis. Scope-role and endpoint-membership checks belong
/// to the analyzer, not model construction or codecs.
///
/// Resource failures may occur directly or through codec, type, expression, or
/// execution-option causes. Inspect structured variants and [`std::error::Error::source`]
/// chains rather than parsing Display text.
/// Validation-produced descriptions are fixed schema terms, not submitted data.
/// Unrecognized consumer-supplied descriptions receive generic Display prose;
/// Debug and structured inspection still expose those descriptions.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphRecordError {
    /// Underlying codec failure, including original input offsets.
    Codec(htlk_cbor::Error),
    /// Invalid local identifier.
    Identifier(ParseIdentifierError),
    /// Invalid referenced digest.
    Digest(ParseDigestError),
    /// Invalid data-port type.
    Type(TypeError),
    /// Invalid expression/context.
    Expression(ExpressionError),
    /// Invalid execution limits or retry policy.
    Options(ExecutionOptionsError),
    /// Incorrect array/map/scalar shape at a fixed site.
    InvalidShape(&'static str),
    /// A canonical record is missing a required field.
    MissingField(&'static str),
    /// A record contains a field outside its closed schema.
    UnknownField(&'static str),
    /// Operation tag is not a canonical operation.
    UnknownOperation,
    /// A loop/wait bound is not in 1..=i64::MAX.
    InvalidBound(&'static str),
    /// A node uses a reserved expression root as its name.
    ReservedNodeName,
    /// Analysis found an incompatible scope use role. Ordinary scopes cannot
    /// declare carried values or use carried/next endpoints. Loop bodies must
    /// have empty scope limits and literal-true preconditions and postconditions.
    InvalidScopeRole,
    /// A local node or edge ID occurs more than once.
    DuplicateId(&'static str),
    /// Canonical node/edge arrays are not in ASCII ID order.
    NonCanonicalOrder(&'static str),
    /// An edge/initializer does not identify a declared local endpoint.
    UnknownEndpoint(&'static str),
    /// A primitive node's ports violate its known operation layout.
    InvalidPorts(&'static str),
    /// Whole-record conversion exceeds a codec ceiling.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Fallible storage reservation failed.
    AllocationFailed,
}
macro_rules! wrap_error {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for GraphRecordError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}
wrap_error!(htlk_cbor::Error, Codec);
wrap_error!(ParseIdentifierError, Identifier);
wrap_error!(ParseDigestError, Digest);
wrap_error!(TypeError, Type);
wrap_error!(ExpressionError, Expression);
wrap_error!(ExecutionOptionsError, Options);
impl From<EncodingLimitError> for GraphRecordError {
    fn from(e: EncodingLimitError) -> Self {
        Self::LimitExceeded {
            limit: e.limit,
            maximum: e.maximum,
        }
    }
}
impl fmt::Display for GraphRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(e) => write!(f, "Could not process the graph record's CBOR representation: {e}"),
            Self::Identifier(e) => write!(f, "Could not process a graph identifier: {e}"),
            Self::Digest(e) => write!(f, "A graph digest is invalid: {e}."),
            Self::Type(e) => write!(f, "Could not process a graph port or type: {e}"),
            Self::Expression(e) => write!(f, "Could not process a graph expression: {e}"),
            Self::Options(ExecutionOptionsError::Codec(e)) => write!(f, "Could not process the graph's execution limits or retry policy: {e}"),
            Self::Options(ExecutionOptionsError::LimitExceeded { limit: LimitKind::DocumentBytes, maximum }) => write!(f, "The graph's execution limits or retry policy exceeds the configured maximum of {maximum} bytes."),
            Self::Options(ExecutionOptionsError::LimitExceeded { limit: LimitKind::Depth, maximum }) => write!(f, "The graph's execution limits or retry policy exceeds the configured maximum nesting depth of {maximum}."),
            Self::Options(_) => f.write_str("Could not process the graph's execution limits or retry policy. Inspect the source error for details."),
            Self::InvalidShape(site) => f.write_str(match *site {
                "identifier" => "A graph identifier must be text.",
                "digest" => "A graph digest must be text.",
                "constructor arity" => "The tagged graph array has an incorrect number of elements.",
                "port table" => "Port declarations must be a map of names to ports.",
                "source" => "An edge source must be a nonempty tagged array.",
                "destination" => "An edge destination must be a nonempty tagged array.",
                "source tag" => "An edge source tag must be input, output, or carried.",
                "destination tag" => "An edge destination tag must be output, input, or next.",
                "operation" => "A node operation must be an array.",
                "operation tag" => "A node operation must begin with a text operation tag.",
                "wait topic" => "A wait topic must be text.",
                "loop initializers" => "Loop initializers must be a map from carried value names to input names.",
                "edge" => "An edge record must be a map.",
                "node" => "A node record must be a map.",
                "scope" => "A scope record must be a map.",
                "nodes" => "Node declarations must be an array.",
                "edges" => "Edge declarations must be an array.",
                _ => "The graph record has an invalid shape.",
            }),
            Self::MissingField(field) => match *field {
                "destination" | "guard" | "id" | "source" | "inputs" | "limits"
                | "operation" | "outputs" | "postconditions" | "preconditions"
                | "carried" | "edges" | "nodes" => write!(f, "The graph record is missing the required {field} field."),
                _ => f.write_str("The graph record is missing a required field."),
            },
            Self::UnknownField(record) => f.write_str(match *record {
                "edge" => "The edge record contains a field that is not permitted.",
                "node" => "The node record contains a field that is not permitted.",
                "scope" => "The scope record contains a field that is not permitted.",
                _ => "The graph record contains a field that is not permitted.",
            }),
            Self::UnknownOperation => f.write_str("The operation tag is missing or is not a recognized canonical operation tag."),
            Self::InvalidBound(field) => f.write_str(match *field {
                "timeout_ms" => "The wait timeout must be an integer from 1 through 9223372036854775807 milliseconds.",
                "max_iterations" => "The loop iteration maximum must be an integer from 1 through 9223372036854775807.",
                _ => "The graph bound must be an integer from 1 through 9223372036854775807.",
            }),
            Self::ReservedNodeName => f.write_str("A node name must not use a reserved expression root or function name."),
            Self::InvalidScopeRole => f.write_str("The scope's declarations, endpoints, limits, or contracts are incompatible with its use role."),
            Self::DuplicateId(kind) => f.write_str(match *kind {
                "node" => "Node declarations must have unique local IDs.",
                "edge" => "Edge declarations must have unique local IDs.",
                _ => "Graph declarations must have unique local IDs within their category.",
            }),
            Self::NonCanonicalOrder(kind) => f.write_str(match *kind {
                "node" => "Node declarations must be sorted by their local IDs.",
                "edge" => "Edge declarations must be sorted by their local IDs.",
                _ => "Graph declarations must be sorted by their local IDs.",
            }),
            Self::UnknownEndpoint(site) => f.write_str(match *site {
                "source node" => "The edge refers to an undeclared source node.",
                "destination node" => "The edge refers to an undeclared destination node.",
                "source output" => "The edge refers to an undeclared output on its source node.",
                "destination input" => "The edge refers to an undeclared input on its destination node.",
                "scope input" => "The edge refers to an undeclared scope input.",
                "scope output" => "The edge refers to an undeclared scope output.",
                "carried source" => "The edge source refers to an undeclared loop-carried value.",
                "next destination" => "The edge's next-value destination refers to an undeclared loop-carried value.",
                "loop initializer input" => "A loop initializer refers to an undeclared input on the loop node.",
                _ => "A graph reference does not identify a declared local endpoint.",
            }),
            Self::InvalidPorts(site) => f.write_str(match *site {
                "primitive value output" => "An Eval, MCP, or wait node must have exactly one output named value; MCP and wait nodes must declare it as required.",
                "wait request input" => "A wait node must have exactly one input named request, declared as required JSON.",
                "MCP arguments input" => "An MCP node must have no inputs or exactly one required input named arguments.",
                _ => "The node's ports do not match its operation's requirements.",
            }),
            Self::LimitExceeded { limit: LimitKind::DocumentBytes, maximum } => write!(f, "The graph record exceeds the configured maximum of {maximum} bytes."),
            Self::LimitExceeded { limit: LimitKind::Depth, maximum } => write!(f, "The graph record exceeds the configured maximum nesting depth of {maximum}."),
            Self::AllocationFailed => f.write_str("Memory could not be allocated for the graph record."),
        }
    }
}
impl std::error::Error for GraphRecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Identifier(e) => Some(e),
            Self::Digest(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Expression(e) => Some(e),
            Self::Options(e) => Some(e),
            _ => None,
        }
    }
}

fn allocation(_: std::collections::TryReserveError) -> GraphRecordError {
    GraphRecordError::AllocationFailed
}
fn owned(s: &str) -> Result<String, GraphRecordError> {
    let mut v = String::new();
    v.try_reserve_exact(s.len()).map_err(allocation)?;
    v.push_str(s);
    Ok(v)
}
fn push<T>(v: &mut Vec<T>, item: T) -> Result<(), GraphRecordError> {
    v.try_reserve(1).map_err(allocation)?;
    v.push(item);
    Ok(())
}
fn digest_text(d: Digest) -> Result<String, GraphRecordError> {
    let mut s = String::new();
    s.try_reserve_exact(71).map_err(allocation)?;
    write!(&mut s, "{d}").expect("String formatting cannot fail");
    Ok(s)
}
fn array<'a>(v: &'a Value, site: &'static str) -> Result<&'a [Value], GraphRecordError> {
    match v {
        Value::Array(v) => Ok(v),
        _ => Err(GraphRecordError::InvalidShape(site)),
    }
}
fn text<'a>(v: &'a Value, site: &'static str) -> Result<&'a str, GraphRecordError> {
    match v {
        Value::Text(v) => Ok(v),
        _ => Err(GraphRecordError::InvalidShape(site)),
    }
}
fn id(v: &Value) -> Result<Identifier, GraphRecordError> {
    Ok(text(v, "identifier")?.parse()?)
}
fn digest(v: &Value) -> Result<Digest, GraphRecordError> {
    Ok(text(v, "digest")?.parse()?)
}
fn arity(v: &[Value], n: usize) -> Result<(), GraphRecordError> {
    if v.len() == n {
        Ok(())
    } else {
        Err(GraphRecordError::InvalidShape("constructor arity"))
    }
}
fn bound(n: u64, field: &'static str) -> Result<i64, GraphRecordError> {
    if n == 0 || n > i64::MAX as u64 {
        Err(GraphRecordError::InvalidBound(field))
    } else {
        Ok(n as i64)
    }
}
fn read_bound(v: &Value, field: &'static str) -> Result<u64, GraphRecordError> {
    match v {
        Value::Integer(n) if *n > 0 => Ok(*n as u64),
        _ => Err(GraphRecordError::InvalidBound(field)),
    }
}
fn closed<'a>(
    v: &'a Value,
    name: &'static str,
    fields: &[&'static str],
) -> Result<&'a Map, GraphRecordError> {
    let Value::Map(map) = v else {
        return Err(GraphRecordError::InvalidShape(name));
    };
    if map.iter().any(|(key, _)| !fields.contains(&key)) {
        return Err(GraphRecordError::UnknownField(name));
    }
    for field in fields {
        if map.get(field).is_none() {
            return Err(GraphRecordError::MissingField(field));
        }
    }
    Ok(map)
}
fn field<'a>(m: &'a Map, key: &str) -> &'a Value {
    m.get(key).expect("required fields checked")
}

struct Builder<'a> {
    limits: &'a Limits,
    accounting: RecordAccounting<'a>,
}
impl<'a> Builder<'a> {
    fn new(l: &'a Limits) -> Result<Self, GraphRecordError> {
        Ok(Self {
            limits: l,
            accounting: RecordAccounting::new(l)?,
        })
    }
    fn string(&mut self, s: &str, d: usize) -> Result<String, GraphRecordError> {
        self.accounting.text(s, d)?;
        owned(s)
    }
    fn text(&mut self, s: &str, d: usize) -> Result<Value, GraphRecordError> {
        Ok(Value::Text(self.string(s, d)?))
    }
    fn tag(&mut self, s: &str, n: usize, d: usize) -> Result<Vec<Value>, GraphRecordError> {
        self.accounting.collection(n, d)?;
        let mut v = Vec::new();
        push(&mut v, self.text(s, d + 1)?)?;
        Ok(v)
    }
    fn child(&mut self, value: Value, depth: usize) -> Result<Value, GraphRecordError> {
        self.accounting.value(&value, depth)?;
        Ok(value)
    }
    fn expression(&mut self, e: &Expression, depth: usize) -> Result<Value, GraphRecordError> {
        self.child(e.to_value(self.limits)?, depth)
    }
    fn ports(
        &mut self,
        entries: &[(Identifier, Port)],
        d: usize,
    ) -> Result<Value, GraphRecordError> {
        self.accounting.collection(entries.len(), d)?;
        let mut fields = Vec::new();
        for (name, port) in entries {
            let key = self.string(name.as_str(), d + 1)?;
            let value = self.child(port.to_value(TypeContext::Value, self.limits)?, d + 1)?;
            push(&mut fields, (key, value))?;
        }
        Ok(Value::Map(Map::try_from_entries(fields)?))
    }
    fn source(&mut self, source: &EdgeSource, d: usize) -> Result<Value, GraphRecordError> {
        match source {
            EdgeSource::Input(n) | EdgeSource::Carried(n) => {
                let mut v = self.tag(
                    if matches!(source, EdgeSource::Input(_)) {
                        "input"
                    } else {
                        "carried"
                    },
                    2,
                    d,
                )?;
                push(&mut v, self.text(n.as_str(), d + 1)?)?;
                Ok(Value::Array(v))
            }
            EdgeSource::Output { node, port } => {
                let mut v = self.tag("output", 3, d)?;
                push(&mut v, self.text(node.as_str(), d + 1)?)?;
                push(&mut v, self.text(port.as_str(), d + 1)?)?;
                Ok(Value::Array(v))
            }
        }
    }
    fn destination(
        &mut self,
        destination: &EdgeDestination,
        d: usize,
    ) -> Result<Value, GraphRecordError> {
        match destination {
            EdgeDestination::Output(n) | EdgeDestination::Next(n) => {
                let mut v = self.tag(
                    if matches!(destination, EdgeDestination::Output(_)) {
                        "output"
                    } else {
                        "next"
                    },
                    2,
                    d,
                )?;
                push(&mut v, self.text(n.as_str(), d + 1)?)?;
                Ok(Value::Array(v))
            }
            EdgeDestination::Input { node, port } => {
                let mut v = self.tag("input", 3, d)?;
                push(&mut v, self.text(node.as_str(), d + 1)?)?;
                push(&mut v, self.text(port.as_str(), d + 1)?)?;
                Ok(Value::Array(v))
            }
        }
    }
    fn edge(&mut self, edge: &Edge, d: usize) -> Result<Value, GraphRecordError> {
        self.accounting.collection(4, d)?;
        let id_key = self.string("id", d + 1)?;
        let id = self.text(edge.id.as_str(), d + 1)?;
        let source_key = self.string("source", d + 1)?;
        let source = self.source(&edge.source, d + 1)?;
        let target_key = self.string("destination", d + 1)?;
        let target = self.destination(&edge.destination, d + 1)?;
        let guard_key = self.string("guard", d + 1)?;
        let guard = self.expression(&edge.guard, d + 1)?;
        Ok(Value::Map(Map::try_from_entries([
            (id_key, id),
            (source_key, source),
            (target_key, target),
            (guard_key, guard),
        ])?))
    }
    fn operation(&mut self, op: &Operation, d: usize) -> Result<Value, GraphRecordError> {
        match op {
            Operation::Eval(expr) => {
                let mut v = self.tag("eval", 2, d)?;
                push(&mut v, self.expression(expr, d + 1)?)?;
                Ok(Value::Array(v))
            }
            Operation::Mcp { binding, retry } => {
                let mut v = self.tag("mcp", 3, d)?;
                push(&mut v, self.text(&digest_text(*binding)?, d + 1)?)?;
                push(&mut v, self.child(retry.to_value(self.limits)?, d + 1)?)?;
                Ok(Value::Array(v))
            }
            Operation::Scope(scope) => {
                let mut v = self.tag("scope", 2, d)?;
                push(&mut v, self.text(&digest_text(*scope)?, d + 1)?)?;
                Ok(Value::Array(v))
            }
            Operation::Loop {
                body,
                initializers,
                until,
                max_iterations,
            } => self.loop_op(*body, initializers, until, *max_iterations, d),
            Operation::Wait { topic, timeout_ms } => {
                let n = bound(*timeout_ms, "timeout_ms")?;
                let mut v = self.tag("wait", 3, d)?;
                push(&mut v, self.text(topic, d + 1)?)?;
                self.accounting.integer(n, d + 1)?;
                push(&mut v, Value::Integer(n))?;
                Ok(Value::Array(v))
            }
        }
    }
    fn loop_op(
        &mut self,
        body: Digest,
        initializers: &[(Identifier, Identifier)],
        until: &Expression,
        max: u64,
        d: usize,
    ) -> Result<Value, GraphRecordError> {
        let n = bound(max, "max_iterations")?;
        let mut v = self.tag("loop", 5, d)?;
        push(&mut v, self.text(&digest_text(body)?, d + 1)?)?;
        self.accounting.collection(initializers.len(), d + 1)?;
        let mut entries = Vec::new();
        for (carried, input) in initializers {
            let key = self.string(carried.as_str(), d + 2)?;
            push(&mut entries, (key, self.text(input.as_str(), d + 2)?))?;
        }
        push(&mut v, Value::Map(Map::try_from_entries(entries)?))?;
        push(&mut v, self.expression(until, d + 1)?)?;
        self.accounting.integer(n, d + 1)?;
        push(&mut v, Value::Integer(n))?;
        Ok(Value::Array(v))
    }
    fn node(&mut self, f: &NodeFields, d: usize) -> Result<Value, GraphRecordError> {
        validate_node(f)?;
        self.accounting.collection(8, d)?;
        let mut pairs = Vec::new();
        let key = self.string("id", d + 1)?;
        push(&mut pairs, (key, self.text(f.id.as_str(), d + 1)?))?;
        for (key, table) in [("inputs", &f.inputs), ("outputs", &f.outputs)] {
            let key = self.string(key, d + 1)?;
            push(&mut pairs, (key, self.ports(&table.entries, d + 1)?))?;
        }
        for (key, expr) in [
            ("guard", &f.guard),
            ("preconditions", &f.preconditions),
            ("postconditions", &f.postconditions),
        ] {
            let key = self.string(key, d + 1)?;
            push(&mut pairs, (key, self.expression(expr, d + 1)?))?;
        }
        let key = self.string("limits", d + 1)?;
        push(
            &mut pairs,
            (key, self.child(f.limits.to_value(self.limits)?, d + 1)?),
        )?;
        let key = self.string("operation", d + 1)?;
        push(&mut pairs, (key, self.operation(&f.operation, d + 1)?))?;
        Ok(Value::Map(Map::try_from_entries(pairs)?))
    }
    fn scope(&mut self, f: &ScopeFields) -> Result<Value, GraphRecordError> {
        self.accounting.collection(8, 0)?;
        let mut pairs = Vec::new();
        for (key, table) in [
            ("inputs", &f.inputs),
            ("outputs", &f.outputs),
            ("carried", &f.carried),
        ] {
            let key = self.string(key, 1)?;
            push(&mut pairs, (key, self.ports(&table.entries, 1)?))?;
        }
        let key = self.string("nodes", 1)?;
        self.accounting.collection(f.nodes.len(), 1)?;
        let mut nodes = Vec::new();
        for node in &f.nodes {
            push(&mut nodes, self.node(&node.fields, 2)?)?;
        }
        push(&mut pairs, (key, Value::Array(nodes)))?;
        let key = self.string("edges", 1)?;
        self.accounting.collection(f.edges.len(), 1)?;
        let mut edges = Vec::new();
        for edge in &f.edges {
            push(&mut edges, self.edge(edge, 2)?)?;
        }
        push(&mut pairs, (key, Value::Array(edges)))?;
        for (key, expr) in [
            ("preconditions", &f.preconditions),
            ("postconditions", &f.postconditions),
        ] {
            let key = self.string(key, 1)?;
            push(&mut pairs, (key, self.expression(expr, 1)?))?;
        }
        let key = self.string("limits", 1)?;
        push(
            &mut pairs,
            (key, self.child(f.limits.to_value(self.limits)?, 1)?),
        )?;
        Ok(Value::Map(Map::try_from_entries(pairs)?))
    }
}

pub(super) fn ports(entries: &[(Identifier, Port)], l: &Limits) -> Result<Value, GraphRecordError> {
    Builder::new(l)?.ports(entries, 0)
}
pub(super) fn edge(e: &Edge, l: &Limits) -> Result<Value, GraphRecordError> {
    Builder::new(l)?.edge(e, 0)
}
pub(super) fn operation(o: &Operation, l: &Limits) -> Result<Value, GraphRecordError> {
    Builder::new(l)?.operation(o, 0)
}
pub(super) fn node(n: &NodeFields, l: &Limits) -> Result<Value, GraphRecordError> {
    Builder::new(l)?.node(n, 0)
}
pub(super) fn scope(s: &ScopeFields, l: &Limits) -> Result<Value, GraphRecordError> {
    Builder::new(l)?.scope(s)
}

pub(super) fn parse_ports(v: &Value, l: &Limits) -> Result<PortTable, GraphRecordError> {
    let Value::Map(map) = v else {
        return Err(GraphRecordError::InvalidShape("port table"));
    };
    let mut entries = Vec::new();
    for (key, value) in map.iter() {
        push(
            &mut entries,
            (
                key.parse::<Identifier>()?,
                Port::from_value(value, TypeContext::Value, l)?,
            ),
        )?;
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(PortTable { entries })
}
fn parse_source(v: &Value) -> Result<EdgeSource, GraphRecordError> {
    let v = array(v, "source")?;
    let tag = text(
        v.first().ok_or(GraphRecordError::InvalidShape("source"))?,
        "source tag",
    )?;
    arity(v, if tag == "output" { 3 } else { 2 })?;
    match tag {
        "input" => Ok(EdgeSource::Input(id(&v[1])?)),
        "output" => Ok(EdgeSource::Output {
            node: id(&v[1])?,
            port: id(&v[2])?,
        }),
        "carried" => Ok(EdgeSource::Carried(id(&v[1])?)),
        _ => Err(GraphRecordError::InvalidShape("source tag")),
    }
}
fn parse_destination(v: &Value) -> Result<EdgeDestination, GraphRecordError> {
    let v = array(v, "destination")?;
    let tag = text(
        v.first()
            .ok_or(GraphRecordError::InvalidShape("destination"))?,
        "destination tag",
    )?;
    arity(v, if tag == "input" { 3 } else { 2 })?;
    match tag {
        "output" => Ok(EdgeDestination::Output(id(&v[1])?)),
        "input" => Ok(EdgeDestination::Input {
            node: id(&v[1])?,
            port: id(&v[2])?,
        }),
        "next" => Ok(EdgeDestination::Next(id(&v[1])?)),
        _ => Err(GraphRecordError::InvalidShape("destination tag")),
    }
}
pub(super) fn parse_edge(v: &Value, l: &Limits) -> Result<Edge, GraphRecordError> {
    let m = closed(v, "edge", &["destination", "guard", "id", "source"])?;
    Ok(Edge {
        id: id(field(m, "id"))?,
        source: parse_source(field(m, "source"))?,
        destination: parse_destination(field(m, "destination"))?,
        guard: Expression::from_value(field(m, "guard"), l)?,
    })
}
pub(super) fn parse_operation(v: &Value, l: &Limits) -> Result<Operation, GraphRecordError> {
    let v = array(v, "operation")?;
    let tag = text(
        v.first().ok_or(GraphRecordError::UnknownOperation)?,
        "operation tag",
    )?;
    let n = match tag {
        "eval" | "scope" => 2,
        "mcp" | "wait" => 3,
        "loop" => 5,
        _ => return Err(GraphRecordError::UnknownOperation),
    };
    arity(v, n)?;
    match tag {
        "eval" => Ok(Operation::Eval(Expression::from_value(&v[1], l)?)),
        "mcp" => Ok(Operation::Mcp {
            binding: digest(&v[1])?,
            retry: RetryPolicy::from_value(&v[2], l)?,
        }),
        "scope" => Ok(Operation::Scope(digest(&v[1])?)),
        "wait" => Ok(Operation::Wait {
            topic: owned(text(&v[1], "wait topic")?)?,
            timeout_ms: read_bound(&v[2], "timeout_ms")?,
        }),
        "loop" => {
            let body = digest(&v[1])?;
            let Value::Map(map) = &v[2] else {
                return Err(GraphRecordError::InvalidShape("loop initializers"));
            };
            let mut initializers = Vec::new();
            for (key, value) in map.iter() {
                push(&mut initializers, (key.parse::<Identifier>()?, id(value)?))?;
            }
            initializers.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            Ok(Operation::Loop {
                body,
                initializers,
                until: Expression::from_value(&v[3], l)?,
                max_iterations: read_bound(&v[4], "max_iterations")?,
            })
        }
        _ => unreachable!("tag checked above"),
    }
}
pub(super) fn parse_node(v: &Value, l: &Limits) -> Result<Node, GraphRecordError> {
    let m = closed(
        v,
        "node",
        &[
            "guard",
            "id",
            "inputs",
            "limits",
            "operation",
            "outputs",
            "postconditions",
            "preconditions",
        ],
    )?;
    let operation = parse_operation(field(m, "operation"), l)?;
    let fields = NodeFields {
        id: id(field(m, "id"))?,
        inputs: parse_ports(field(m, "inputs"), l)?,
        outputs: parse_ports(field(m, "outputs"), l)?,
        guard: Expression::from_value(field(m, "guard"), l)?,
        preconditions: Expression::from_value(field(m, "preconditions"), l)?,
        postconditions: Expression::from_value(field(m, "postconditions"), l)?,
        limits: ExecutionLimits::from_value(field(m, "limits"), l)?,
        operation,
    };
    validate_node(&fields)?;
    Ok(Node {
        fields: Box::new(fields),
    })
}
pub(super) fn parse_scope(
    v: &Value,
    l: &Limits,
    normalize: bool,
) -> Result<Scope, GraphRecordError> {
    let m = closed(
        v,
        "scope",
        &[
            "carried",
            "edges",
            "inputs",
            "limits",
            "nodes",
            "outputs",
            "postconditions",
            "preconditions",
        ],
    )?;
    let mut nodes = Vec::new();
    for v in array(field(m, "nodes"), "nodes")? {
        push(&mut nodes, parse_node(v, l)?)?;
    }
    let mut edges = Vec::new();
    for v in array(field(m, "edges"), "edges")? {
        push(&mut edges, parse_edge(v, l)?)?;
    }
    if normalize {
        nodes.sort_unstable_by(|a, b| a.id().cmp(b.id()));
        edges.sort_unstable_by(|a, b| a.id().cmp(b.id()));
    }
    ordered(nodes.iter().map(Node::id), "node")?;
    ordered(edges.iter().map(Edge::id), "edge")?;
    let fields = ScopeFields {
        inputs: parse_ports(field(m, "inputs"), l)?,
        outputs: parse_ports(field(m, "outputs"), l)?,
        carried: parse_ports(field(m, "carried"), l)?,
        nodes,
        edges,
        preconditions: Expression::from_value(field(m, "preconditions"), l)?,
        postconditions: Expression::from_value(field(m, "postconditions"), l)?,
        limits: ExecutionLimits::from_value(field(m, "limits"), l)?,
    };
    Ok(Scope {
        fields: Box::new(fields),
    })
}

fn ordered<'a>(
    ids: impl Iterator<Item = &'a Identifier>,
    kind: &'static str,
) -> Result<(), GraphRecordError> {
    let mut previous = None;
    for id in ids {
        if previous == Some(id) {
            return Err(GraphRecordError::DuplicateId(kind));
        }
        if previous.is_some_and(|old| old > id) {
            return Err(GraphRecordError::NonCanonicalOrder(kind));
        }
        previous = Some(id);
    }
    Ok(())
}
fn validate_node(f: &NodeFields) -> Result<(), GraphRecordError> {
    if [
        "inputs",
        "outputs",
        "carried",
        "next",
        "length",
        "present",
        "status",
        "error",
        "render",
        "mcp",
        "predicates",
    ]
    .contains(&f.id.as_str())
    {
        return Err(GraphRecordError::ReservedNodeName);
    }
    if matches!(
        f.operation,
        Operation::Eval(_) | Operation::Mcp { .. } | Operation::Wait { .. }
    ) {
        let Some(value) = f.outputs.get("value") else {
            return Err(GraphRecordError::InvalidPorts("primitive value output"));
        };
        if f.outputs.len() != 1 || (!matches!(f.operation, Operation::Eval(_)) && !value.required())
        {
            return Err(GraphRecordError::InvalidPorts("primitive value output"));
        }
    }
    if matches!(f.operation, Operation::Wait { .. })
        && (f.inputs.len() != 1
            || !f.inputs.get("request").is_some_and(|p| {
                p.required()
                    && matches!(
                        p.value_type().kind(),
                        ValueTypeKind::Builtin(BuiltinType::Json)
                    )
            }))
    {
        return Err(GraphRecordError::InvalidPorts("wait request input"));
    }
    if matches!(f.operation, Operation::Mcp { .. })
        && !f.inputs.is_empty()
        && (f.inputs.len() != 1 || !f.inputs.get("arguments").is_some_and(Port::required))
    {
        return Err(GraphRecordError::InvalidPorts("MCP arguments input"));
    }
    Ok(())
}
