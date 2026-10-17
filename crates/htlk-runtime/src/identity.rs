//! Draft 0.1 runtime identities, distinct from compiler record-digest domains.
//!
//! Parsing a name establishes its representation, not existence or authorization.
//! Identity exists before input selection. A new active graph fingerprint therefore
//! never participates in the identity of preserved invocations or their attempts.

use htlk_executable::{
    Identifier, TypeContext, ValueType,
    cbor::{self, Limits, Value},
    digest::{Digest, ParseDigestError},
    record_accounting::{EncodingLimitError, RecordAccounting},
};
use sha2::{Digest as _, Sha256};
use std::{fmt, str::FromStr};

/// An opaque host-selected run name. Exact Unicode spelling is preserved.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(String);
impl RunId {
    /// Checks the encoded string's byte/depth ceiling before retaining it.
    ///
    /// # Errors
    /// Returns invalid codec configuration or an encoded-size limit.
    pub fn new(value: String, limits: &Limits) -> Result<Self, IdentityError> {
        RecordAccounting::new(limits)?.text(&value, 0)?;
        Ok(Self(value))
    }
    /// Borrows the exact opaque name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! digest_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Digest);
        impl $name {
            /// Borrows the strict SHA-256 identity representation.
            pub const fn digest(&self) -> Digest {
                self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
        impl FromStr for $name {
            type Err = ParseDigestError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}
digest_id!(
    ScopeInstanceId,
    "One root, child, iteration, or extended-root scope instance."
);
digest_id!(
    InvocationId,
    "One local node occurrence in one immutable scope instance."
);
digest_id!(AttemptId, "One MCP delivery attempt inside an invocation.");
digest_id!(WaitId, "One durable input wait or approval generation.");
digest_id!(
    ArtifactId,
    "An immutable accepted value's producer/provenance identity."
);
digest_id!(
    ValueDigest,
    "Typed value content identity, independent of its producer and storage."
);

impl ScopeInstanceId {
    /// Computes the initial root identity for a run.
    ///
    /// # Errors
    /// Returns canonical preimage bounds or codec configuration failures.
    pub fn root(run: &RunId, limits: &Limits) -> Result<Self, IdentityError> {
        hash("root_scope", &[&Value::Text(run.0.clone())], limits).map(Self)
    }
    /// Computes the ordinary scope opened by an invocation.
    ///
    /// # Errors
    /// Returns canonical preimage bounds or codec configuration failures.
    pub fn child(invocation: InvocationId, limits: &Limits) -> Result<Self, IdentityError> {
        hash("child_scope", &[&name(invocation.0)], limits).map(Self)
    }
    /// Computes a zero-based loop iteration identity.
    ///
    /// # Errors
    /// Returns counter-range or canonical preimage failures.
    pub fn iteration(
        invocation: InvocationId,
        index: u64,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "iteration",
            &[&name(invocation.0), &counter(index, false)?],
            limits,
        )
        .map(Self)
    }
    /// Computes an additive root identity; the first extension is one.
    ///
    /// # Errors
    /// Returns zero/out-of-range extension or canonical preimage failures.
    pub fn extended_root(
        run: &RunId,
        extension: u64,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "extended_root",
            &[&Value::Text(run.0.clone()), &counter(extension, true)?],
            limits,
        )
        .map(Self)
    }
}
impl InvocationId {
    /// Computes identity before binding any input values.
    ///
    /// # Errors
    /// Returns canonical preimage bounds or codec configuration failures.
    pub fn new(
        scope: ScopeInstanceId,
        node: &Identifier,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "invocation",
            &[&name(scope.0), &Value::Text(node.to_string())],
            limits,
        )
        .map(Self)
    }
    /// Computes an invocation-stable external effect key. This grants no replay
    /// permission: trusted adapter policy must establish remote deduplication.
    ///
    /// # Errors
    /// Returns canonical preimage bounds or codec configuration failures.
    pub fn effect_key(self, limits: &Limits) -> Result<Digest, IdentityError> {
        hash("effect", &[&name(self.0)], limits)
    }
}
impl AttemptId {
    /// Computes an attempt identity; the first attempt is one.
    ///
    /// # Errors
    /// Returns zero/out-of-range attempt or canonical preimage failures.
    pub fn new(
        invocation: InvocationId,
        number: u64,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "attempt",
            &[&name(invocation.0), &counter(number, true)?],
            limits,
        )
        .map(Self)
    }
}
impl WaitId {
    /// Computes the single generation-zero ordinary input wait identity.
    ///
    /// # Errors
    /// Returns canonical preimage bounds or codec configuration failures.
    pub fn input(invocation: InvocationId, limits: &Limits) -> Result<Self, IdentityError> {
        Self::for_purpose(invocation, "input", 0, limits)
    }
    /// Computes an approval identity, starting at generation zero.
    ///
    /// # Errors
    /// Returns counter-range or canonical preimage failures.
    pub fn approval(
        invocation: InvocationId,
        generation: u64,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        Self::for_purpose(invocation, "approval", generation, limits)
    }
    fn for_purpose(
        invocation: InvocationId,
        purpose: &str,
        generation: u64,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "wait",
            &[
                &name(invocation.0),
                &Value::Text(purpose.into()),
                &counter(generation, false)?,
            ],
            limits,
        )
        .map(Self)
    }
}
impl ValueDigest {
    /// Hashes the normalized declared type and canonical value. This is content
    /// identity, not evidence of actual-value/schema validity or acceptance.
    /// Hashing borrows the value and does not clone its potentially large tree.
    ///
    /// # Errors
    /// Returns ordinary-type representation, codec or full preimage limits.
    pub fn new(
        value_type: &ValueType,
        value: &Value,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        let value_type = value_type.to_value(TypeContext::Value, limits)?;
        hash("value", &[&value_type, value], limits).map(Self)
    }
}
impl ArtifactId {
    /// Computes accepted node-output provenance identity from typed content.
    ///
    /// # Errors
    /// Returns canonical preimage bounds or codec configuration failures.
    pub fn node_output(
        invocation: InvocationId,
        port: &Identifier,
        content: ValueDigest,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "artifact",
            &[
                &name(invocation.0),
                &Value::Text(port.to_string()),
                &name(content.0),
            ],
            limits,
        )
        .map(Self)
    }
    /// Computes inline run-input provenance. Extension zero denotes initial inputs.
    /// Existing artifact references retain their existing ID instead.
    ///
    /// # Errors
    /// Returns counter-range or canonical preimage failures.
    pub fn run_input(
        run: &RunId,
        extension: u64,
        port: &Identifier,
        content: ValueDigest,
        limits: &Limits,
    ) -> Result<Self, IdentityError> {
        hash(
            "run_input",
            &[
                &Value::Text(run.0.clone()),
                &counter(extension, false)?,
                &Value::Text(port.to_string()),
                &name(content.0),
            ],
            limits,
        )
        .map(Self)
    }
}

fn name(digest: Digest) -> Value {
    Value::Text(digest.to_string())
}
fn counter(number: u64, positive: bool) -> Result<Value, IdentityError> {
    if positive && number == 0 {
        return Err(IdentityError::CounterRange);
    }
    i64::try_from(number)
        .map(Value::Integer)
        .map_err(|_| IdentityError::CounterRange)
}

// All callers use fewer than 24 items. Hash one canonical array header and each
// independently encoded child after accounting for the COMPLETE containing array.
// This avoids cloning/allocating the complete value tree or preimage buffer.
fn hash(domain: &str, arguments: &[&Value], limits: &Limits) -> Result<Digest, IdentityError> {
    let domain = Value::Text(format!("htlk.{domain}"));
    let version = Value::Text("0.1".into());
    let mut accounting = RecordAccounting::new(limits)?;
    let count = arguments.len() + 2;
    accounting.collection(count, 0)?;
    for child in [&domain, &version]
        .into_iter()
        .chain(arguments.iter().copied())
    {
        accounting.value(child, 1)?;
    }
    assert!(
        count < 24,
        "runtime identity preimages use short array headers"
    );
    let mut digest = Sha256::new();
    digest.update([0x80 | count as u8]);
    for child in [&domain, &version]
        .into_iter()
        .chain(arguments.iter().copied())
    {
        digest.update(cbor::encode(child, limits)?);
    }
    Ok(Digest::from_bytes(digest.finalize().into()))
}

/// Bounded runtime-identity construction failure; never retains submitted values.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityError {
    /// Invalid codec configuration or canonical encoding failure.
    Codec(cbor::Error),
    /// The complete preimage exceeds byte/depth limits.
    Encoding(EncodingLimitError),
    /// A type is not representable as an ordinary canonical value type.
    Type(htlk_executable::TypeError),
    /// Counter is outside signed i64, or zero at a one-based boundary.
    CounterRange,
}
impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(e) => write!(f, "runtime identity encoding: {e}"),
            Self::Encoding(e) => write!(f, "runtime identity preimage: {e}"),
            Self::Type(e) => write!(f, "runtime identity type: {e}"),
            Self::CounterRange => {
                f.write_str("runtime identity counter is outside its permitted range")
            }
        }
    }
}
impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Encoding(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::CounterRange => None,
        }
    }
}
impl From<cbor::Error> for IdentityError {
    fn from(error: cbor::Error) -> Self {
        Self::Codec(error)
    }
}
impl From<EncodingLimitError> for IdentityError {
    fn from(error: EncodingLimitError) -> Self {
        Self::Encoding(error)
    }
}
impl From<htlk_executable::TypeError> for IdentityError {
    fn from(error: htlk_executable::TypeError) -> Self {
        Self::Type(error)
    }
}
