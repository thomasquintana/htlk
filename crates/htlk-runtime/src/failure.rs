//! Stable redacted graph-visible failures, separate from protected diagnostics.

use crate::EvaluationError;
use htlk_executable::{
    cbor::{self, Limits, Map, Value},
    record_accounting::{EncodingLimitError, RecordAccounting},
};
use std::{fmt, str::FromStr};

/// Protected causal metadata, stored separately from graph-visible error values.
/// Inspection must authorize access to both the source outcome and this location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureCause {
    /// Immutable originating invocation.
    pub invocation: crate::identity::InvocationId,
    /// Stable classification retained from the originating outcome.
    pub code: FailureCode,
    /// Optional canonical expression/binding location in its origin executable.
    pub location: Option<htlk_executable::JsonPointer>,
}

macro_rules! codes {
    ($($variant:ident => ($code:literal, $message:literal)),+ $(,)?) => {
        /// Stable Draft 0.1 runtime and MCP failure classification.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[non_exhaustive]
        pub enum FailureCode {
            $(#[doc = $message] $variant),+
        }
        impl FailureCode {
            /// Borrows the specification's machine-readable spelling.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code),+ }
            }
            /// Fixed message that contains no caller values or remote diagnostics.
            pub const fn message(self) -> &'static str {
                match self { $(Self::$variant => $message),+ }
            }
        }
        impl FromStr for FailureCode {
            type Err = FailureRecordError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value { $($code => Ok(Self::$variant)),+, _ => Err(FailureRecordError::Shape) }
            }
        }
    };
}
codes! {
    Executable => ("E_EXECUTABLE", "The executable is invalid or unsupported."),
    Digest => ("E_DIGEST", "Recorded content failed integrity verification."),
    Profile => ("E_PROFILE", "The exact required implementation is unavailable."),
    Input => ("E_INPUT", "An input violates its declared boundary."),
    BindingConflict => ("E_BINDING_CONFLICT", "More than one candidate guard selected a binding."),
    Dependency => ("E_DEPENDENCY", "A selected dependency failed or was cancelled."),
    OutputMissing => ("E_OUTPUT_MISSING", "A required boundary value is absent."),
    Expression => ("E_EXPRESSION", "An expression could not be evaluated."),
    ExpressionAbsent => ("E_EXPRESSION_ABSENT", "An expression required a value that was absent."),
    ExpressionLimit => ("E_EXPRESSION_LIMIT", "The deterministic expression allowance was exhausted."),
    Preconditions => ("E_PRECONDITIONS", "The input precondition was false."),
    Postconditions => ("E_POSTCONDITIONS", "The proposed result postcondition was false."),
    McpIdentity => ("MCP_IDENTITY", "The connected server differs from its pinned identity."),
    McpDescriptor => ("MCP_DESCRIPTOR", "The live operation differs from its pinned descriptor."),
    McpTransport => ("MCP_TRANSPORT", "The external transport failed."),
    McpTimeout => ("MCP_TIMEOUT", "The external attempt deadline expired."),
    McpProtocol => ("MCP_PROTOCOL", "The external response violates the pinned protocol."),
    McpToolError => ("MCP_TOOL_ERROR", "The external tool returned an error."),
    OutputSchema => ("E_OUTPUT_SCHEMA", "A proposed output violates its declared boundary."),
    EffectUncertain => ("E_EFFECT_UNCERTAIN", "External delivery is uncertain and replay is not authorized."),
    WaitExpired => ("E_WAIT_EXPIRED", "The external input wait expired."),
    ApprovalDenied => ("E_APPROVAL_DENIED", "Exact-action approval was denied or expired."),
    Policy => ("E_POLICY", "Deployment authorization denied the requested action."),
    Budget => ("E_BUDGET", "The applicable execution budget is exhausted."),
    BudgetUnsupported => ("E_BUDGET_UNSUPPORTED", "The requested hard budget cannot be enforced."),
    Timeout => ("E_TIMEOUT", "The admitted execution deadline expired."),
    LoopLimit => ("E_LOOP_LIMIT", "The loop did not terminate within its iteration bound."),
    ExtensionConflict => ("E_EXTENSION_CONFLICT", "The expected active graph is stale."),
    Extension => ("E_EXTENSION", "The proposed extension is not a legal additive installation."),
    IdempotencyConflict => ("E_IDEMPOTENCY_CONFLICT", "The idempotency key identifies a different request."),
    Cancelled => ("E_CANCELLED", "Execution was cancelled."),
    Fenced => ("E_FENCED", "The result belongs to stale execution authority."),
    Invariant => ("E_INVARIANT", "Execution cannot satisfy a verified runtime invariant."),
}
impl fmt::Display for FailureCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Site-specific actual-value failure interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureBoundary {
    /// Ordinary authored expression evaluation.
    Expression,
    /// A host or selected input boundary.
    Input,
    /// A proposed output boundary.
    Output,
}
impl FailureCode {
    /// Maps actual-value checks without turning operational errors, limit
    /// exhaustion, absence or unavailable dependencies into schema mismatches.
    pub fn for_evaluation(error: &EvaluationError, boundary: FailureBoundary) -> Self {
        match error {
            EvaluationError::UnavailableSource => Self::Dependency,
            EvaluationError::Registry(_) => Self::Profile,
            EvaluationError::AbsentOperand => Self::ExpressionAbsent,
            EvaluationError::OperandType => match boundary {
                FailureBoundary::Expression => Self::Expression,
                FailureBoundary::Input => Self::Input,
                FailureBoundary::Output => Self::OutputSchema,
            },
            _ if error.code() == "E_EXPRESSION_LIMIT" => Self::ExpressionLimit,
            _ => Self::Expression,
        }
    }
}

/// An immutable, bounded graph-visible error record. Protected causal IDs,
/// submitted values, remote errors and viewer-specific redaction live separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFailure {
    code: FailureCode,
    message: String,
}
impl RuntimeFailure {
    /// Creates the fixed redacted message for a failure classification.
    pub fn new(code: FailureCode) -> Self {
        Self {
            code,
            message: code.message().into(),
        }
    }
    /// Stable machine-readable classification.
    pub const fn code(&self) -> FailureCode {
        self.code
    }
    /// Message frozen when the outcome is committed.
    pub fn message(&self) -> &str {
        &self.message
    }
    /// Produces exactly the graph-visible `code` and `message` fields.
    ///
    /// # Errors
    /// Returns complete record byte/depth or allocation failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, FailureRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(2, 0)?;
        for text in ["code", self.code.as_str(), "message", &self.message] {
            accounting.text(text, 1)?;
        }
        Ok(Value::Map(Map::try_from_entries([
            ("code".into(), Value::Text(self.code.to_string())),
            ("message".into(), Value::Text(self.message.clone())),
        ])?))
    }
    /// Reads a previously recorded redacted message without substituting a new
    /// deployment's message. Input must be trusted stored error metadata; ordinary
    /// application data does not acquire terminal-outcome authority by parsing it.
    ///
    /// # Errors
    /// Returns malformed fields, unknown code, codec or byte/depth failures.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, FailureRecordError> {
        RecordAccounting::new(limits)?.value(value, 0)?;
        let Value::Map(fields) = value else {
            return Err(FailureRecordError::Shape);
        };
        let (Some(Value::Text(code)), Some(Value::Text(message))) =
            (fields.get("code"), fields.get("message"))
        else {
            return Err(FailureRecordError::Shape);
        };
        if fields.len() != 2 {
            return Err(FailureRecordError::Shape);
        }
        Ok(Self {
            code: code.parse()?,
            message: message.clone(),
        })
    }
}
impl fmt::Display for RuntimeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RuntimeFailure {}

/// Runtime error-record conversion failure, without submitted contents.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureRecordError {
    /// Unknown code or wrong closed record shape.
    Shape,
    /// Complete record limits exceeded.
    Encoding(EncodingLimitError),
    /// Codec configuration or allocation failed.
    Codec(cbor::Error),
}
impl fmt::Display for FailureRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape => f.write_str("invalid runtime failure record"),
            Self::Encoding(e) => write!(f, "runtime failure record: {e}"),
            Self::Codec(e) => write!(f, "runtime failure encoding: {e}"),
        }
    }
}
impl std::error::Error for FailureRecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shape => None,
            Self::Encoding(e) => Some(e),
            Self::Codec(e) => Some(e),
        }
    }
}
impl From<EncodingLimitError> for FailureRecordError {
    fn from(error: EncodingLimitError) -> Self {
        Self::Encoding(error)
    }
}
impl From<cbor::Error> for FailureRecordError {
    fn from(error: cbor::Error) -> Self {
        Self::Codec(error)
    }
}
