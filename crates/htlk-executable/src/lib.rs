#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod digest;

mod envelope;
mod identifier;
mod options;
mod record_accounting;
mod types;

pub use envelope::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};
pub use identifier::{Identifier, ParseIdentifierError};
pub use options::{ExecutionLimits, ExecutionOptionsError, RetryPolicy};
pub use types::{Port, PrimitiveType, TypeContext, TypeError, ValueType, ValueTypeKind};
