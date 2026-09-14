use std::fmt;

use htlk_cbor::{LimitKind, Limits, Map, Value};

use crate::record_accounting::{EncodingLimitError, RecordAccounting};

// UTF-8 order gives deterministic validation precedence. The Boolean marks
// fields that require a positive (rather than nonnegative) integer.
const LIMIT_FIELDS: [(&str, bool); 6] = [
    ("attempt_timeout_ms", true),
    ("max_concurrency", true),
    ("max_cost_units", false),
    ("max_mcp_calls", false),
    ("max_tokens", false),
    ("timeout_ms", true),
];
const RETRY_FIELDS: [&str; 3] = ["backoff_ms", "max_attempts", "on"];
const MAX_INTEGER: u64 = i64::MAX as u64;

macro_rules! limit_accessors {
    ($get:ident, $with:ident, $index:literal, $doc:literal) => {
        #[doc = $doc]
        pub const fn $get(&self) -> Option<u64> {
            self.values[$index]
        }

        #[doc = concat!("Sets `", stringify!($get), "` on this owned configuration.")]
        ///
        /// # Errors
        /// Returns a numeric-range error for values above i64::MAX, or zero
        /// where the field requires a positive value.
        pub fn $with(mut self, value: u64) -> Result<Self, ExecutionOptionsError> {
            let (field, positive) = LIMIT_FIELDS[$index];
            validate_integer(value, field, positive)?;
            self.values[$index] = Some(value);
            Ok(self)
        }
    };
}

/// Optional node/scope execution ceilings, separate from codec limits and usage.
///
/// Empty/default is an empty canonical map and means inheritance, not unlimited
/// execution or zero capacity. Configured call/token/cost budgets may be zero;
/// timeouts and concurrency must be positive. Every number fits signed i64.
/// No clock, reservation, authority, metering, or effective-limit calculation is
/// performed here. A policy-default record's required fields are checked by its
/// containing profile validator, not this reusable local-limits record.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExecutionLimits {
    values: [Option<u64>; 6],
}

impl ExecutionLimits {
    /// Creates empty local limits, inheriting all applicable ceilings.
    pub const fn new() -> Self {
        Self { values: [None; 6] }
    }

    limit_accessors!(
        attempt_timeout_ms,
        with_attempt_timeout_ms,
        0,
        "Per-attempt duration ceiling in milliseconds, or inheritance."
    );
    limit_accessors!(
        max_concurrency,
        with_max_concurrency,
        1,
        "Concurrent descendant MCP dispatch ceiling, or inheritance."
    );
    limit_accessors!(
        max_cost_units,
        with_max_cost_units,
        2,
        "Cost-unit budget, including explicit zero, or inheritance."
    );
    limit_accessors!(
        max_mcp_calls,
        with_max_mcp_calls,
        3,
        "MCP dispatch-count budget, including explicit zero, or inheritance."
    );
    limit_accessors!(
        max_tokens,
        with_max_tokens,
        4,
        "Token budget, including explicit zero, or inheritance."
    );
    limit_accessors!(
        timeout_ms,
        with_timeout_ms,
        5,
        "Admitted node/scope duration ceiling in milliseconds, or inheritance."
    );

    /// Produces the canonical record, omitting unset fields.
    ///
    /// # Errors
    /// Returns codec configuration, conversion-limit, or allocation errors.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, ExecutionOptionsError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(self.values.iter().flatten().count(), 0)?;
        // Bound all conversion before constructing the map.
        for ((field, _), value) in LIMIT_FIELDS.iter().zip(&self.values) {
            if let Some(value) = value {
                accounting.text(field, 1)?;
                accounting.integer(*value as i64, 1)?;
            }
        }
        let mut fields = Vec::new();
        for ((field, _), value) in LIMIT_FIELDS.iter().zip(&self.values) {
            if let Some(value) = value {
                push(&mut fields, (owned(field)?, Value::Integer(*value as i64)))?;
            }
        }
        Ok(Value::Map(Map::try_from_entries(fields)?))
    }

    /// Validates an already decoded record under the codec limits. Null is not
    /// treated as an omitted field, and floats/strings are not coerced to integers.
    ///
    /// # Errors
    /// Returns codec, schema, numeric-range, or allocation errors.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, ExecutionOptionsError> {
        htlk_cbor::encode(value, limits)?;
        Self::parse(value)
    }

    /// Encodes one canonical local-limits map.
    ///
    /// # Errors
    /// Returns configuration, resource, or allocation errors.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, ExecutionOptionsError> {
        Ok(htlk_cbor::encode(&self.to_value(limits)?, limits)?)
    }

    /// Decodes exactly one canonical local-limits map.
    ///
    /// # Errors
    /// Returns codec, schema, or numeric-range errors.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, ExecutionOptionsError> {
        Self::parse(&htlk_cbor::decode(bytes, limits)?)
    }

    fn parse(value: &Value) -> Result<Self, ExecutionOptionsError> {
        let Value::Map(map) = value else {
            return Err(ExecutionOptionsError::ExpectedRecord("execution limits"));
        };
        if map
            .iter()
            .any(|(field, _)| !LIMIT_FIELDS.iter().any(|(known, _)| field == *known))
        {
            return Err(ExecutionOptionsError::UnknownField("execution limits"));
        }
        let mut result = Self::new();
        for (index, (field, positive)) in LIMIT_FIELDS.iter().enumerate() {
            if let Some(value) = map.get(field) {
                result.values[index] = Some(integer(value, field, *positive)?);
            }
        }
        Ok(result)
    }
}

impl fmt::Debug for ExecutionLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut record = f.debug_struct("ExecutionLimits");
        for ((field, _), value) in LIMIT_FIELDS.iter().zip(&self.values) {
            if let Some(value) = value {
                record.field(field, value);
            }
        }
        record.finish()
    }
}

/// Canonical MCP retry metadata: total attempts, failure-code set, and delays.
///
/// An attempt count includes the initial dispatch. Delay entry k-2 precedes
/// attempt k (one-based), so the delay vector has exactly max_attempts - 1 items.
/// Codes are opaque exact strings, unique and sorted by UTF-8 bytes. A valid
/// policy does not authorize replay: delivery state, trusted operation policy,
/// approvals, effective deadlines, and remaining budgets are runtime checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u64,
    on: Vec<String>,
    backoff_ms: Vec<u64>,
}

impl RetryPolicy {
    /// Constructs an authored policy, bounding input before sorting/deduplicating
    /// the failure-code set. Delay order is never changed.
    ///
    /// # Errors
    /// Returns numeric-range, delay-count, codec configuration, or limit errors.
    pub fn new(
        max_attempts: u64,
        on: Vec<String>,
        backoff_ms: Vec<u64>,
        limits: &Limits,
    ) -> Result<Self, ExecutionOptionsError> {
        let mut result = Self {
            max_attempts,
            on,
            backoff_ms,
        };
        result.check_wire(limits)?;
        result.on.sort_unstable();
        result.on.dedup();
        Ok(result)
    }

    /// The normalized omitted-policy value: one attempt and empty lists.
    pub const fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            on: Vec::new(),
            backoff_ms: Vec::new(),
        }
    }
    /// Total allowed attempts, including the initial dispatch.
    pub const fn max_attempts(&self) -> u64 {
        self.max_attempts
    }
    /// Borrows the unique UTF-8-sorted failure-code set.
    pub fn on(&self) -> &[String] {
        &self.on
    }
    /// Borrows delays in semantic attempt order, including any explicit zeros.
    pub fn backoff_ms(&self) -> &[u64] {
        &self.backoff_ms
    }

    /// Produces exactly the three required canonical policy fields.
    ///
    /// # Errors
    /// Returns codec configuration, conversion-limit, or allocation errors.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, ExecutionOptionsError> {
        self.check_wire(limits)?;
        let mut codes = Vec::new();
        codes.try_reserve_exact(self.on.len()).map_err(allocation)?;
        for code in &self.on {
            codes.push(Value::Text(owned(code)?));
        }
        let mut delays = Vec::new();
        delays
            .try_reserve_exact(self.backoff_ms.len())
            .map_err(allocation)?;
        for delay in &self.backoff_ms {
            delays.push(Value::Integer(*delay as i64));
        }
        Ok(Value::Map(Map::try_from_entries([
            (owned("on")?, Value::Array(codes)),
            (owned("backoff_ms")?, Value::Array(delays)),
            (
                owned("max_attempts")?,
                Value::Integer(self.max_attempts as i64),
            ),
        ])?))
    }

    /// Reads canonical policy data without repairing order or inferring fields.
    ///
    /// # Errors
    /// Returns codec, schema, numeric, cardinality, order, or allocation errors.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, ExecutionOptionsError> {
        htlk_cbor::encode(value, limits)?;
        Self::parse(value)
    }

    /// Encodes one canonical policy under the supplied codec limits.
    ///
    /// # Errors
    /// Returns configuration, resource, or allocation errors.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, ExecutionOptionsError> {
        Ok(htlk_cbor::encode(&self.to_value(limits)?, limits)?)
    }

    /// Decodes exactly one canonical retry-policy map.
    ///
    /// # Errors
    /// Returns codec, schema, numeric, cardinality, order, or allocation errors.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, ExecutionOptionsError> {
        Self::parse(&htlk_cbor::decode(bytes, limits)?)
    }

    fn check_wire(&self, limits: &Limits) -> Result<(), ExecutionOptionsError> {
        let mut accounting = RecordAccounting::new(limits)?;
        validate_integer(self.max_attempts, "max_attempts", true)?;
        delay_count(self.max_attempts, self.backoff_ms.len())?;
        accounting.collection(3, 0)?;
        accounting.text("on", 1)?;
        accounting.collection(self.on.len(), 1)?;
        for code in &self.on {
            accounting.text(code, 2)?;
        }
        accounting.text("backoff_ms", 1)?;
        accounting.collection(self.backoff_ms.len(), 1)?;
        for delay in &self.backoff_ms {
            validate_integer(*delay, "backoff_ms item", false)?;
            accounting.integer(*delay as i64, 2)?;
        }
        accounting.text("max_attempts", 1)?;
        accounting.integer(self.max_attempts as i64, 1)?;
        Ok(())
    }

    fn parse(value: &Value) -> Result<Self, ExecutionOptionsError> {
        let Value::Map(map) = value else {
            return Err(ExecutionOptionsError::ExpectedRecord("retry policy"));
        };
        if map.iter().any(|(field, _)| !RETRY_FIELDS.contains(&field)) {
            return Err(ExecutionOptionsError::UnknownField("retry policy"));
        }
        for field in RETRY_FIELDS {
            if map.get(field).is_none() {
                return Err(ExecutionOptionsError::MissingField(field));
            }
        }
        // Establish every top-level field's representation before numeric or
        // list-content checks. All names are fixed schema terms.
        let backoff = array(map.get("backoff_ms").expect("field checked"), "backoff_ms")?;
        let attempts = map.get("max_attempts").expect("field checked");
        if !matches!(attempts, Value::Integer(_)) {
            return Err(ExecutionOptionsError::InvalidFieldType("max_attempts"));
        }
        let codes = array(map.get("on").expect("field checked"), "on")?;
        let max_attempts = integer(attempts, "max_attempts", true)?;
        delay_count(max_attempts, backoff.len())?;
        for delay in backoff {
            integer(delay, "backoff_ms item", false)?;
        }
        let mut previous: Option<&str> = None;
        for code in codes {
            let Value::Text(code) = code else {
                return Err(ExecutionOptionsError::InvalidFieldType("on item"));
            };
            if previous.is_some_and(|previous| previous >= code.as_str()) {
                return Err(ExecutionOptionsError::NonCanonicalCodes);
            }
            previous = Some(code);
        }
        // Input was bounded before parse; validate the entire policy before
        // copying its variable-sized lists. Never allocate from max_attempts.
        let mut on = Vec::new();
        on.try_reserve_exact(codes.len()).map_err(allocation)?;
        for code in codes {
            let Value::Text(code) = code else {
                unreachable!("code type checked above");
            };
            on.push(owned(code)?);
        }
        let mut backoff_ms = Vec::new();
        backoff_ms
            .try_reserve_exact(backoff.len())
            .map_err(allocation)?;
        for delay in backoff {
            backoff_ms.push(integer(delay, "backoff_ms item", false)?);
        }
        Ok(Self {
            max_attempts,
            on,
            backoff_ms,
        })
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::no_retry()
    }
}

fn validate_integer(
    value: u64,
    field: &'static str,
    positive: bool,
) -> Result<(), ExecutionOptionsError> {
    if value > MAX_INTEGER {
        return Err(ExecutionOptionsError::OutOfRange(field));
    }
    if positive && value == 0 {
        return Err(ExecutionOptionsError::ZeroNotAllowed(field));
    }
    Ok(())
}
fn integer(
    value: &Value,
    field: &'static str,
    positive: bool,
) -> Result<u64, ExecutionOptionsError> {
    let Value::Integer(value) = value else {
        return Err(ExecutionOptionsError::InvalidFieldType(field));
    };
    let value = u64::try_from(*value).map_err(|_| ExecutionOptionsError::OutOfRange(field))?;
    validate_integer(value, field, positive)?;
    Ok(value)
}
fn array<'a>(value: &'a Value, field: &'static str) -> Result<&'a [Value], ExecutionOptionsError> {
    match value {
        Value::Array(values) => Ok(values),
        _ => Err(ExecutionOptionsError::InvalidFieldType(field)),
    }
}
fn delay_count(max_attempts: u64, actual: usize) -> Result<(), ExecutionOptionsError> {
    // Call only after checking positivity, so subtraction cannot underflow.
    let expected = max_attempts - 1;
    if expected != actual as u64 {
        return Err(ExecutionOptionsError::DelayCountMismatch { expected, actual });
    }
    Ok(())
}
fn allocation(_: std::collections::TryReserveError) -> ExecutionOptionsError {
    ExecutionOptionsError::AllocationFailed
}
fn owned(text: &str) -> Result<String, ExecutionOptionsError> {
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(allocation)?;
    result.push_str(text);
    Ok(result)
}
fn push<T>(values: &mut Vec<T>, value: T) -> Result<(), ExecutionOptionsError> {
    values.try_reserve(1).map_err(allocation)?;
    values.push(value);
    Ok(())
}

/// A closed execution-option record failed validation or bounded conversion.
/// Errors retain only static schema descriptions/counts and codec causes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecutionOptionsError {
    /// Canonical CBOR configuration/encoding/decoding failure.
    Codec(htlk_cbor::Error),
    /// The named record was not a map.
    ExpectedRecord(&'static str),
    /// The named record contained an unknown field; its input name is omitted.
    UnknownField(&'static str),
    /// A required retry-policy field is absent.
    MissingField(&'static str),
    /// A field or list item has the wrong native representation.
    InvalidFieldType(&'static str),
    /// A number is outside 0..=i64::MAX.
    OutOfRange(&'static str),
    /// A positive field was explicitly zero.
    ZeroNotAllowed(&'static str),
    /// Delay count differs from max_attempts - 1.
    DelayCountMismatch {
        /// Number of delay entries required by the attempt count.
        expected: u64,
        /// Number of supplied delay entries.
        actual: usize,
    },
    /// Code strings are not strictly increasing in UTF-8 order.
    NonCanonicalCodes,
    /// Conversion would exceed a codec resource ceiling.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// A fallible storage reservation failed.
    AllocationFailed,
}

impl From<htlk_cbor::Error> for ExecutionOptionsError {
    fn from(error: htlk_cbor::Error) -> Self {
        Self::Codec(error)
    }
}
impl From<EncodingLimitError> for ExecutionOptionsError {
    fn from(error: EncodingLimitError) -> Self {
        Self::LimitExceeded {
            limit: error.limit,
            maximum: error.maximum,
        }
    }
}
impl fmt::Display for ExecutionOptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(error) => write!(f, "execution options: {error}"),
            Self::ExpectedRecord(record) => write!(f, "expected {record} record"),
            Self::UnknownField(record) => write!(f, "unknown field in {record}"),
            Self::MissingField(field) => write!(f, "missing execution option field: {field}"),
            Self::InvalidFieldType(field) => {
                write!(f, "invalid execution option field type: {field}")
            }
            Self::OutOfRange(field) => {
                write!(f, "execution option must be in 0..=i64::MAX: {field}")
            }
            Self::ZeroNotAllowed(field) => write!(f, "execution option must be positive: {field}"),
            Self::DelayCountMismatch { expected, actual } => {
                write!(f, "expected {expected} retry delays, got {actual}")
            }
            Self::NonCanonicalCodes => f.write_str("retry codes must be unique and UTF-8 sorted"),
            Self::LimitExceeded { limit, maximum } => {
                write!(f, "execution option limit exceeded: {limit:?} ({maximum})")
            }
            Self::AllocationFailed => f.write_str("execution option allocation failed"),
        }
    }
}
impl std::error::Error for ExecutionOptionsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}
