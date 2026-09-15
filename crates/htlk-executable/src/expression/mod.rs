//! Canonical expression descriptions and prompt templates, without evaluation.

mod template;
mod wire;

use htlk_cbor::{FiniteFloat, Limits, Value};

use crate::Identifier;
use crate::digest::Digest;

pub use template::{PromptTemplate, TemplatePart};
pub use wire::ExpressionError;

/// The owning expression location, chosen by the consuming schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpressionContext {
    /// A node/edge guard in its containing scope.
    Guard {
        /// Whether that scope is a loop body with carried values.
        loop_body: bool,
    },
    /// A pure node's calculation; only its own input roots.
    Eval,
    /// A node/scope precondition; only its own input roots.
    Preconditions,
    /// A primitive node's input and proposed-output postcondition.
    PrimitivePostconditions,
    /// A task/root scope's inputs, proposed outputs, and child outcomes.
    ScopePostconditions,
    /// A use-node wrapper's inputs and proposed outputs.
    WrapperPostconditions,
    /// A loop termination test after its body settles.
    LoopUntil,
    /// A loop's inputs and final proposed outputs.
    LoopPostconditions,
}

/// A scalar constant; collections have separate expression constructors.
#[derive(Clone, Debug, PartialEq)]
pub enum ScalarLiteral {
    /// Exact Unicode text.
    String(String),
    /// Signed 64-bit integer.
    Integer(i64),
    /// Finite, positive-zero-normalized floating point.
    Float(FiniteFloat),
    /// Boolean.
    Boolean(bool),
    /// Explicit null, never absence.
    Null,
    /// Opaque bytes.
    Bytes(Vec<u8>),
}

/// A statically named value root, resolved relative to the owning scope/node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ValueReference {
    /// The owning input named by its port.
    Input(Identifier),
    /// A sibling/body node's output.
    Output {
        /// Local node name.
        node: Identifier,
        /// Output port name.
        port: Identifier,
    },
    /// A proposed output of the current node/scope boundary.
    ScopeOutput(Identifier),
    /// A loop-carried value.
    Carried(Identifier),
    /// A proposed next value, readable only by loop termination tests.
    Next(Identifier),
}

/// One literal field/index selection in a projection path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathStep {
    /// Exact field name, including arbitrary Unicode or empty text.
    Field(String),
    /// Zero-based index, checked to fit 0..=i64::MAX at expression boundaries.
    Index(u64),
}

/// Closed ordinary core-call identifiers; other core forms have dedicated tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreFunction {
    /// Length of a supported value.
    Length,
    /// Settled optional presence; present null is present.
    Present,
}

/// Exact callable identity, resolved by the compiler rather than input data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionId {
    /// One of the supported single-argument core functions.
    Core(CoreFunction),
    /// A linked library function.
    Library {
        /// Exact library implementation identity.
        library: Digest,
        /// Function's local registry name.
        name: Identifier,
    },
}

/// The supported Boolean/comparison operators. Operand order is semantic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperator {
    /// Lazy left-to-right conjunction.
    And,
    /// Lazy left-to-right disjunction.
    Or,
    /// Equality.
    Eq,
    /// Inequality.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
}

impl BinaryOperator {
    /// Exact canonical operator tag.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Or => "or",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
        }
    }
    fn parse(tag: &str) -> Option<Self> {
        match tag {
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "eq" => Some(Self::Eq),
            "ne" => Some(Self::Ne),
            "lt" => Some(Self::Lt),
            "le" => Some(Self::Le),
            "gt" => Some(Self::Gt),
            "ge" => Some(Self::Ge),
            _ => None,
        }
    }
}

/// Authored shape or read-only normalized expression view.
#[derive(Clone, Debug, PartialEq)]
pub enum ExpressionKind {
    /// A scalar constant.
    Literal(ScalarLiteral),
    /// Pattern and flags; engine compilation belongs to the pinned verifier.
    Regex {
        /// Exact decoded pattern text.
        pattern: String,
        /// Unique flags; authored permutations normalize to ims order.
        flags: String,
    },
    /// Read a named value with an optional folded projection path.
    Ref {
        /// Named root.
        source: ValueReference,
        /// Zero or more field/index selections.
        path: Vec<PathStep>,
    },
    /// Project a non-reference expression result; path must be nonempty.
    Get {
        /// Base expression.
        value: Box<Expression>,
        /// One or more selections.
        path: Vec<PathStep>,
    },
    /// Elements in evaluation order.
    List(Vec<Expression>),
    /// Fields normalized to decoded-key UTF-8 evaluation order.
    Record(Vec<(String, Expression)>),
    /// A pure call with positional argument expressions.
    Call {
        /// Resolved function identity.
        function: FunctionId,
        /// Arguments in semantic order.
        arguments: Vec<Expression>,
    },
    /// Static library callable metadata, not an ordinary application value.
    FunctionRef {
        /// Library implementation identity.
        library: Digest,
        /// Local function name.
        name: Identifier,
    },
    /// A referenced template and exact named argument expressions.
    Render {
        /// Template content digest.
        template: Digest,
        /// Identifier-keyed arguments, exposed in UTF-8 evaluation order.
        arguments: Vec<(Identifier, Expression)>,
    },
    /// Terminal node status inspection.
    Status(Identifier),
    /// Terminal node error inspection.
    Error(Identifier),
    /// Boolean negation.
    Not(Box<Expression>),
    /// Boolean or scalar comparison.
    Binary {
        /// Operator tag.
        operator: BinaryOperator,
        /// Left operand, evaluated first.
        left: Box<Expression>,
        /// Right operand, retained even when evaluation may short circuit.
        right: Box<Expression>,
    },
}

/// Immutable canonical expression metadata with reference-category checks.
///
/// This does not evaluate, resolve names, infer types, compile regex engines,
/// or validate callback placement/signatures. Those require the surrounding
/// graph and exact linked profile. Function references remain explicit AST nodes,
/// never a ScalarLiteral or a runtime application-value representation.
#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    // Keep recursive parser/visitor return values small even for large variants.
    kind: Box<ExpressionKind>,
}

impl Expression {
    /// Constructs a scalar leaf, valid in every reference context. Value size is
    /// checked when embedded, converted, or encoded; no literal absence exists.
    pub fn literal(value: ScalarLiteral) -> Self {
        Self {
            kind: Box::new(ExpressionKind::Literal(value)),
        }
    }

    /// Normalizes authored paths, record fields, and regex flags under limits.
    ///
    /// # Errors
    /// Returns schema, context, normalization, or resource failures.
    pub fn new(
        kind: ExpressionKind,
        context: ExpressionContext,
        limits: &Limits,
    ) -> Result<Self, ExpressionError> {
        let value = wire::expression_value(&kind, context, limits)?;
        let result = wire::parse_expression(&value, context, limits, true)?;
        result.to_value(context, limits)?;
        Ok(result)
    }
    /// Borrows the immutable normalized shape.
    pub fn kind(&self) -> &ExpressionKind {
        &self.kind
    }
    /// Converts to canonical CBOR data, checking size before large allocations.
    ///
    /// # Errors
    /// Returns context, resource, or allocation failures.
    pub fn to_value(
        &self,
        context: ExpressionContext,
        limits: &Limits,
    ) -> Result<Value, ExpressionError> {
        wire::expression_value(&self.kind, context, limits)
    }
    /// Validates canonical expression data without repairing its structure.
    ///
    /// # Errors
    /// Returns codec, schema, context, or canonicality failures.
    pub fn from_value(
        value: &Value,
        context: ExpressionContext,
        limits: &Limits,
    ) -> Result<Self, ExpressionError> {
        htlk_cbor::encode(value, limits)?;
        wire::parse_expression(value, context, limits, false)
    }
    /// Encodes one canonical expression.
    ///
    /// # Errors
    /// Returns context, resource, or allocation failures.
    pub fn encode(
        &self,
        context: ExpressionContext,
        limits: &Limits,
    ) -> Result<Vec<u8>, ExpressionError> {
        Ok(htlk_cbor::encode(&self.to_value(context, limits)?, limits)?)
    }
    /// Decodes exactly one canonical expression under its owning context.
    ///
    /// # Errors
    /// Returns codec, schema, context, or canonicality failures.
    pub fn decode(
        bytes: &[u8],
        context: ExpressionContext,
        limits: &Limits,
    ) -> Result<Self, ExpressionError> {
        let value = htlk_cbor::decode(bytes, limits)?;
        wire::parse_expression(&value, context, limits, false)
    }
}
