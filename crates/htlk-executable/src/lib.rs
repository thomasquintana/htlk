#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod digest;

mod envelope;
mod expression;
mod identifier;
mod options;
mod record_accounting;
mod types;

pub use envelope::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};
pub use expression::{
    BinaryOperator, CoreFunction, Expression, ExpressionContext, ExpressionError, ExpressionKind,
    FunctionId, PathStep, PromptTemplate, ScalarLiteral, TemplatePart, ValueReference,
};
pub use identifier::{Identifier, ParseIdentifierError};
pub use options::{ExecutionLimits, ExecutionOptionsError, RetryPolicy};
pub use types::{Port, PrimitiveType, TypeContext, TypeError, ValueType, ValueTypeKind};
