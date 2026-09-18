//! Canonical graph definition records and local structural checks.

mod wire;

use crate::cbor as htlk_cbor;
use crate::digest::{Digest, RecordKind, record_digest};
use crate::{
    ExecutionLimits, Expression, ExpressionContext, Identifier, Port, RetryPolicy, ScalarLiteral,
};
use htlk_cbor::{Limits, Value};

pub use wire::GraphRecordError;

/// The use context of a scope definition; never a serialized scope label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeContext {
    /// Root/task use: no carried or next boundary.
    Ordinary,
    /// Loop iteration body: carried/next permitted, local contracts true and limits empty.
    LoopBody,
}
impl ScopeContext {
    fn guard(self) -> ExpressionContext {
        ExpressionContext::Guard {
            loop_body: self == Self::LoopBody,
        }
    }
}

/// Immutable identifier-keyed ordinary-value port declarations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortTable {
    entries: Vec<(Identifier, Port)>,
}
impl PortTable {
    /// Constructs and normalizes a port table; duplicate names are rejected.
    ///
    /// # Errors
    /// Returns type, duplicate-name, or codec/resource failures.
    pub fn new(
        entries: Vec<(Identifier, Port)>,
        limits: &Limits,
    ) -> Result<Self, GraphRecordError> {
        wire::parse_ports(&wire::ports(&entries, limits)?, limits)
    }
    /// Iterates in decoded UTF-8/ASCII name order, independently of wire map order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&Identifier, &Port)> {
        self.entries.iter().map(|(n, p)| (n, p))
    }
    /// Looks up an exact local name.
    pub fn get(&self, name: &str) -> Option<&Port> {
        self.entries
            .binary_search_by(|(n, _)| n.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i].1)
    }
    /// Number of declarations.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Whether there are no declarations.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Converts to canonical data with ordinary-value type context.
    ///
    /// # Errors
    /// Returns type or resource errors.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, GraphRecordError> {
        wire::ports(&self.entries, limits)
    }
    /// Parses canonical table data, validating names and port records.
    ///
    /// # Errors
    /// Returns identifier, type, or codec failures.
    pub fn from_value(v: &Value, limits: &Limits) -> Result<Self, GraphRecordError> {
        htlk_cbor::encode(v, limits)?;
        wire::parse_ports(v, limits)
    }
    /// Encodes one canonical port table.
    ///
    /// # Errors
    /// Returns codec/resource failures.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, GraphRecordError> {
        Ok(htlk_cbor::encode(&self.to_value(limits)?, limits)?)
    }
    /// Decodes exactly one canonical port table.
    ///
    /// # Errors
    /// Returns identifier, type, or codec failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, GraphRecordError> {
        wire::parse_ports(&htlk_cbor::decode(bytes, limits)?, limits)
    }
}

/// Whole-port source of a graph edge; projections belong in expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeSource {
    /// Containing-scope input.
    Input(Identifier),
    /// Local node output.
    Output {
        /// Local node name.
        node: Identifier,
        /// Output port name.
        port: Identifier,
    },
    /// Loop-carried boundary value.
    Carried(Identifier),
}
/// Destination boundary of a graph edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeDestination {
    /// Local node input.
    Input {
        /// Local node name.
        node: Identifier,
        /// Input port name.
        port: Identifier,
    },
    /// Containing-scope output.
    Output(Identifier),
    /// Proposed next value for a declared carried field.
    Next(Identifier),
}

/// An immutable edge definition with an explicit guard.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    id: Identifier,
    source: EdgeSource,
    destination: EdgeDestination,
    guard: Expression,
}
impl Edge {
    /// Creates an authored edge with a literal-true guard. The analyzer checks
    /// scope-role legality and endpoint membership in its containing scope.
    pub fn new(id: Identifier, source: EdgeSource, destination: EdgeDestination) -> Self {
        Self {
            id,
            source,
            destination,
            guard: truth(),
        }
    }
    /// Sets a guard on the owned definition; the analyzer checks use-site context.
    pub fn with_guard(mut self, guard: Expression) -> Self {
        self.guard = guard;
        self
    }
    /// Local edge ID.
    pub fn id(&self) -> &Identifier {
        &self.id
    }
    /// Complete source endpoint.
    pub fn source(&self) -> &EdgeSource {
        &self.source
    }
    /// Complete destination endpoint.
    pub fn destination(&self) -> &EdgeDestination {
        &self.destination
    }
    /// Explicit guard expression.
    pub fn guard(&self) -> &Expression {
        &self.guard
    }
    /// Produces a checked canonical edge record.
    ///
    /// # Errors
    /// Returns expression representation or resource failures.
    pub fn to_value(
        &self,
        context: ScopeContext,
        limits: &Limits,
    ) -> Result<Value, GraphRecordError> {
        wire::edge(self, context, limits)
    }
    /// Parses a complete canonical edge record; missing guard is not defaulted.
    ///
    /// # Errors
    /// Returns record-shape, identifier, or codec failures.
    pub fn from_value(
        v: &Value,
        context: ScopeContext,
        limits: &Limits,
    ) -> Result<Self, GraphRecordError> {
        htlk_cbor::encode(v, limits)?;
        wire::parse_edge(v, context, limits)
    }
    /// Encodes exactly one canonical edge.
    ///
    /// # Errors
    /// Returns representation or resource failures.
    pub fn encode(&self, c: ScopeContext, l: &Limits) -> Result<Vec<u8>, GraphRecordError> {
        Ok(htlk_cbor::encode(&self.to_value(c, l)?, l)?)
    }
    /// Decodes exactly one canonical edge.
    ///
    /// # Errors
    /// Returns record-shape or codec failures.
    pub fn decode(bytes: &[u8], c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        wire::parse_edge(&htlk_cbor::decode(bytes, l)?, c, l)
    }
}

/// Authored or decoded operation metadata. Only MCP has a retry-policy slot.
#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    /// Pure calculation using the node's own inputs.
    Eval(Expression),
    /// MCP operation selected by a binding digest.
    Mcp {
        /// Exact binding definition identity.
        binding: Digest,
        /// Explicit canonical retry policy.
        retry: RetryPolicy,
    },
    /// Ordinary scope use by definition digest.
    Scope(Digest),
    /// Bounded repeat-until operation.
    Loop {
        /// Body definition digest.
        body: Digest,
        /// Carried name to loop-input initializer; exact coverage is checked at linkage.
        initializers: Vec<(Identifier, Identifier)>,
        /// Termination condition in settled body context.
        until: Expression,
        /// Positive signed-i64 iteration bound.
        max_iterations: u64,
    },
    /// Typed outside-input wait; response type is the node's value output.
    Wait {
        /// Exact host routing topic, not an authority grant.
        topic: String,
        /// Positive signed-i64 deadline offset in milliseconds.
        timeout_ms: u64,
    },
}
impl Operation {
    /// Converts operation metadata to its canonical tagged array.
    ///
    /// # Errors
    /// Returns bound, expression, duplicate-initializer, or resource failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, GraphRecordError> {
        wire::operation(self, limits)
    }
    /// Parses a canonical operation. Source words such as use/call are not wire aliases.
    ///
    /// # Errors
    /// Returns schema, bound, identity, expression, or codec failures.
    pub fn from_value(v: &Value, l: &Limits) -> Result<Self, GraphRecordError> {
        htlk_cbor::encode(v, l)?;
        wire::parse_operation(v, l)
    }
    /// Encodes one canonical operation.
    ///
    /// # Errors
    /// Returns validation/resource failures.
    pub fn encode(&self, l: &Limits) -> Result<Vec<u8>, GraphRecordError> {
        Ok(htlk_cbor::encode(&self.to_value(l)?, l)?)
    }
    /// Decodes one canonical operation.
    ///
    /// # Errors
    /// Returns schema, bound, identity, or codec failures.
    pub fn decode(bytes: &[u8], l: &Limits) -> Result<Self, GraphRecordError> {
        wire::parse_operation(&htlk_cbor::decode(bytes, l)?, l)
    }
    fn post_context(&self) -> ExpressionContext {
        match self {
            Self::Scope(_) => ExpressionContext::WrapperPostconditions,
            Self::Loop { .. } => ExpressionContext::LoopPostconditions,
            _ => ExpressionContext::PrimitivePostconditions,
        }
    }
}

/// Authored node fields. Pass to Node::new to obtain an immutable checked record.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeFields {
    /// Local node ID; reserved roots are forbidden.
    pub id: Identifier,
    /// Declared input ports.
    pub inputs: PortTable,
    /// Declared output ports.
    pub outputs: PortTable,
    /// Guard in the containing-scope context.
    pub guard: Expression,
    /// Own-input precondition.
    pub preconditions: Expression,
    /// Postcondition in the operation's appropriate output context.
    pub postconditions: Expression,
    /// Local execution ceilings.
    pub limits: ExecutionLimits,
    /// Operation metadata.
    pub operation: Operation,
}
impl NodeFields {
    /// Starts an authored definition with empty ports/limits and true contracts.
    /// Fill primitive ports before calling Node::new; these defaults do not
    /// silently infer descriptor schemas or make an incomplete node valid.
    pub fn new(id: Identifier, operation: Operation) -> Self {
        Self {
            id,
            inputs: PortTable::default(),
            outputs: PortTable::default(),
            guard: truth(),
            preconditions: truth(),
            postconditions: truth(),
            limits: ExecutionLimits::new(),
            operation,
        }
    }
}

/// Immutable canonical node with closed local port-layout checks.
/// Use contexts, local names and target compatibility require semantic analysis.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    fields: Box<NodeFields>,
}
impl Node {
    /// Validates and normalizes authored fields, including initializer map order.
    ///
    /// # Errors
    /// Returns local record-shape, port-layout, or resource failures.
    pub fn new(fields: NodeFields, c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        let v = wire::node(&fields, c, l)?;
        wire::parse_node(&v, c, l)
    }
    /// Borrows all immutable definition fields.
    pub fn fields(&self) -> &NodeFields {
        &self.fields
    }
    /// Local node ID.
    pub fn id(&self) -> &Identifier {
        &self.fields.id
    }
    /// Produces a canonical record. Containing-scope semantics require analysis.
    ///
    /// # Errors
    /// Returns representation or resource failures.
    pub fn to_value(&self, c: ScopeContext, l: &Limits) -> Result<Value, GraphRecordError> {
        wire::node(&self.fields, c, l)
    }
    /// Parses all required fields from canonical data.
    ///
    /// # Errors
    /// Returns record-shape or codec failures.
    pub fn from_value(v: &Value, c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        htlk_cbor::encode(v, l)?;
        wire::parse_node(v, c, l)
    }
    /// Encodes one complete node.
    ///
    /// # Errors
    /// Returns validation/resource failures.
    pub fn encode(&self, c: ScopeContext, l: &Limits) -> Result<Vec<u8>, GraphRecordError> {
        Ok(htlk_cbor::encode(&self.to_value(c, l)?, l)?)
    }
    /// Decodes one complete node.
    ///
    /// # Errors
    /// Returns record-shape or codec failures.
    pub fn decode(bytes: &[u8], c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        wire::parse_node(&htlk_cbor::decode(bytes, l)?, c, l)
    }
    /// Computes the digest of the complete canonical node record.
    ///
    /// # Errors
    /// Returns validation/resource failures.
    pub fn digest(&self, c: ScopeContext, l: &Limits) -> Result<Digest, GraphRecordError> {
        Ok(record_digest(RecordKind::Node, &self.to_value(c, l)?, l)?)
    }
}

/// Authored scope fields, with empty tables and true contracts by default.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeFields {
    /// Public input declarations.
    pub inputs: PortTable,
    /// Public output declarations.
    pub outputs: PortTable,
    /// Loop-carried declarations; empty in ordinary scopes.
    pub carried: PortTable,
    /// Node occurrences, normalized by local ASCII ID on construction.
    pub nodes: Vec<Node>,
    /// Edges, normalized by local ASCII ID on construction.
    pub edges: Vec<Edge>,
    /// Scope input precondition.
    pub preconditions: Expression,
    /// Scope completion postcondition.
    pub postconditions: Expression,
    /// Local limits; empty on loop-body scopes.
    pub limits: ExecutionLimits,
}
impl Default for ScopeFields {
    fn default() -> Self {
        Self {
            inputs: PortTable::default(),
            outputs: PortTable::default(),
            carried: PortTable::default(),
            nodes: Vec::new(),
            edges: Vec::new(),
            preconditions: truth(),
            postconditions: truth(),
            limits: ExecutionLimits::new(),
        }
    }
}

/// Immutable canonical scope with bounded representation and normalized IDs.
/// No scope-role label is serialized. Graph cycles, observability, required
/// binding coverage, expression names/types, and referenced definitions are
/// checked by verify_scope_graph and the composed verifier before registration.
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    fields: Box<ScopeFields>,
}
impl Scope {
    /// Normalizes node/edge order and validates record representation. Local
    /// memberships and scope roles are checked by the analyzer.
    ///
    /// # Errors
    /// Returns duplicate/order, record-shape, or resource failures.
    pub fn new(fields: ScopeFields, c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        let v = wire::scope(&fields, c, l)?;
        wire::parse_scope(&v, c, l, true)
    }
    /// Borrows all immutable scope fields.
    pub fn fields(&self) -> &ScopeFields {
        &self.fields
    }
    /// Produces canonical data. The context argument does not establish semantic
    /// validity; ordinary and loop-body use roles are analyzed separately.
    ///
    /// # Errors
    /// Returns scope-role, expression, or resource failures.
    pub fn to_value(&self, c: ScopeContext, l: &Limits) -> Result<Value, GraphRecordError> {
        wire::scope(&self.fields, c, l)
    }
    /// Parses canonical data without repairing node/edge order.
    ///
    /// # Errors
    /// Returns record shape, ordering, or resource failures.
    pub fn from_value(v: &Value, c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        htlk_cbor::encode(v, l)?;
        wire::parse_scope(v, c, l, false)
    }
    /// Encodes one complete scope record.
    ///
    /// # Errors
    /// Returns validation/resource failures.
    pub fn encode(&self, c: ScopeContext, l: &Limits) -> Result<Vec<u8>, GraphRecordError> {
        Ok(htlk_cbor::encode(&self.to_value(c, l)?, l)?)
    }
    /// Decodes one complete canonical scope.
    ///
    /// # Errors
    /// Returns record shape, ordering, or resource failures.
    pub fn decode(bytes: &[u8], c: ScopeContext, l: &Limits) -> Result<Self, GraphRecordError> {
        wire::parse_scope(&htlk_cbor::decode(bytes, l)?, c, l, false)
    }
    /// Computes the context-independent digest of the valid canonical record.
    ///
    /// # Errors
    /// Returns representation or resource failures; context is not added to the hash.
    pub fn digest(&self, c: ScopeContext, l: &Limits) -> Result<Digest, GraphRecordError> {
        Ok(record_digest(RecordKind::Scope, &self.to_value(c, l)?, l)?)
    }
}

fn truth() -> Expression {
    Expression::literal(ScalarLiteral::Boolean(true))
}
