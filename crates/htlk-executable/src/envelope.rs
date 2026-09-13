use std::fmt;

use htlk_cbor::{Limits, Map, Value};

use crate::digest::{Digest, ParseDigestError, hash_cbor};

/// The supported executable envelope format identifier.
pub const EXECUTABLE_FORMAT: &str = "htlk.executable";

/// The supported envelope version, independent of runtime/IR version identifiers.
pub const EXECUTABLE_VERSION: &str = "0.1";

// Decoded UTF-8 order, deliberately independent of canonical CBOR key order.
const FIELDS: [&str; 4] = ["fingerprint", "format", "payload", "version"];

/// An immutable, schema-checked executable envelope with a verified fingerprint.
///
/// Payload bytes are opaque and may be empty or contain invalid graph data.
/// This type verifies only the outer record, supported format/version, and its
/// fingerprint. Payload schemas, graph validity, and trust are registration's
/// responsibilities. Read-only access prevents mutation of fingerprinted bytes.
#[derive(Clone, PartialEq)]
pub struct ExecutableEnvelope {
    // Retain the validated record so access and encoding can borrow its payload
    // without cloning it. The fingerprint's typed form is cached separately.
    record: Value,
    fingerprint: Digest,
}

impl ExecutableEnvelope {
    /// Constructs an envelope and computes its version-0.1 fingerprint.
    ///
    /// Transfers ownership of the exact payload bytes. Both the fingerprint
    /// preimage and the final envelope must encode within the supplied limits;
    /// each encoding has fresh accounting. Empty payloads are permitted.
    ///
    /// # Errors
    /// Returns [`EnvelopeError::Codec`] for codec configuration, limit, or
    /// allocation failures. No envelope is returned unless all checks pass.
    pub fn new(payload: Vec<u8>, limits: &Limits) -> Result<Self, EnvelopeError> {
        limits.validate()?;
        let (fingerprint, payload) = fingerprint_payload(payload, limits)?;
        let record = Value::Map(Map::try_from_entries([
            ("format".into(), Value::Text(EXECUTABLE_FORMAT.into())),
            ("version".into(), Value::Text(EXECUTABLE_VERSION.into())),
            ("fingerprint".into(), Value::Text(fingerprint.to_string())),
            ("payload".into(), payload),
        ])?);
        let envelope = Self {
            record,
            fingerprint,
        };
        // Check the complete record too: its metadata consumes more space than
        // the fingerprint preimage. No retained serialized copy is needed.
        envelope.encode(limits)?;
        Ok(envelope)
    }

    /// Decodes one canonical envelope, checks its schema and format/version,
    /// and verifies its fingerprint over the exact payload bytes.
    ///
    /// Schema precedence is: non-record, unknown fields, missing fields, then
    /// incorrect field types. Missing/type errors choose the lowest known field
    /// in decoded UTF-8 order. Format precedes version, which precedes fingerprint
    /// parsing and comparison. Untrusted field names/values are not retained in
    /// errors. Underlying codec failures preserve their input byte offsets.
    ///
    /// # Errors
    /// Returns [`EnvelopeError`] for any codec, schema, format/version, fingerprint,
    /// or temporary payload-copy allocation failure. Partial results are dropped.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, EnvelopeError> {
        let record = htlk_cbor::decode(bytes, limits)?;
        let map = validate_schema(&record)?;
        if text(map, "format") != EXECUTABLE_FORMAT {
            return Err(EnvelopeError::UnsupportedFormat);
        }
        if text(map, "version") != EXECUTABLE_VERSION {
            return Err(EnvelopeError::UnsupportedVersion);
        }
        let fingerprint = text(map, "fingerprint")
            .parse::<Digest>()
            .map_err(EnvelopeError::InvalidFingerprint)?;
        let payload = payload(map);
        // The successful outer decode already bounded this byte string. Use a
        // fallible copy to build the canonical preimage without mutating record.
        let mut copy = Vec::new();
        copy.try_reserve_exact(payload.len())
            .map_err(|_| EnvelopeError::AllocationFailed)?;
        copy.extend_from_slice(payload);
        let (computed, _) = fingerprint_payload(copy, limits)?;
        if fingerprint != computed {
            return Err(EnvelopeError::FingerprintMismatch);
        }
        Ok(Self {
            record,
            fingerprint,
        })
    }

    /// Encodes the complete envelope in canonical CBOR without cloning payload.
    ///
    /// # Errors
    /// Returns [`EnvelopeError::Codec`] if the supplied limits or allocation
    /// prevent encoding. A failure does not modify this envelope.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, EnvelopeError> {
        Ok(htlk_cbor::encode(&self.record, limits)?)
    }

    /// Returns the validated supported format identifier.
    pub const fn format(&self) -> &'static str {
        EXECUTABLE_FORMAT
    }

    /// Returns the validated envelope version.
    pub const fn version(&self) -> &'static str {
        EXECUTABLE_VERSION
    }

    /// Returns the verified fingerprint of the prescribed payload preimage.
    pub const fn fingerprint(&self) -> Digest {
        self.fingerprint
    }

    /// Borrows the original, unmodified payload bytes.
    pub fn payload(&self) -> &[u8] {
        let Value::Map(map) = &self.record else {
            unreachable!("constructors establish an immutable map record");
        };
        payload(map)
    }
}

impl fmt::Debug for ExecutableEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutableEnvelope")
            .field("format", &self.format())
            .field("version", &self.version())
            .field("fingerprint", &self.fingerprint)
            .field("payload_len", &self.payload().len())
            .finish()
    }
}

fn fingerprint_payload(
    payload: Vec<u8>,
    limits: &Limits,
) -> Result<(Digest, Value), EnvelopeError> {
    let preimage = Value::Array(vec![
        Value::Text(EXECUTABLE_FORMAT.into()),
        Value::Text(EXECUTABLE_VERSION.into()),
        Value::Bytes(payload),
    ]);
    let fingerprint = hash_cbor(&preimage, limits)?;
    // Recover ownership of the payload after hashing rather than cloning it.
    let Value::Array(mut fields) = preimage else {
        unreachable!("preimage is constructed as an array");
    };
    Ok((
        fingerprint,
        fields.pop().expect("preimage always has a payload"),
    ))
}

fn validate_schema(record: &Value) -> Result<&Map, EnvelopeError> {
    let Value::Map(map) = record else {
        return Err(EnvelopeError::ExpectedRecord);
    };
    if map.iter().any(|(field, _)| !FIELDS.contains(&field)) {
        return Err(EnvelopeError::UnknownField);
    }
    for field in FIELDS {
        if map.get(field).is_none() {
            return Err(EnvelopeError::MissingField(field));
        }
    }
    for field in FIELDS {
        let valid = if field == "payload" {
            matches!(map.get(field), Some(Value::Bytes(_)))
        } else {
            matches!(map.get(field), Some(Value::Text(_)))
        };
        if !valid {
            return Err(EnvelopeError::InvalidFieldType(field));
        }
    }
    Ok(map)
}

fn text<'a>(map: &'a Map, field: &str) -> &'a str {
    match map.get(field) {
        Some(Value::Text(value)) => value,
        _ => unreachable!("field types checked before access"),
    }
}

fn payload(map: &Map) -> &[u8] {
    match map.get("payload") {
        Some(Value::Bytes(value)) => value,
        _ => unreachable!("payload type checked before access"),
    }
}

/// A failure to construct, encode, or validate an executable envelope.
///
/// Field names carried by missing/type errors are fixed schema names, never
/// names copied from untrusted input. Unsupported metadata values and payload
/// contents are omitted from diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvelopeError {
    /// A canonical CBOR configuration, encoding, or decoding failure.
    Codec(htlk_cbor::Error),
    /// The decoded value was not a map.
    ExpectedRecord,
    /// The record contained a field outside the exact envelope schema.
    UnknownField,
    /// A required field was missing; contains its fixed schema name.
    MissingField(&'static str),
    /// A field had the wrong type; contains its fixed schema name.
    InvalidFieldType(&'static str),
    /// The format identifier is unsupported.
    UnsupportedFormat,
    /// The envelope version is unsupported.
    UnsupportedVersion,
    /// Fingerprint text was not a canonical SHA-256 digest.
    InvalidFingerprint(ParseDigestError),
    /// The fingerprint did not match the prescribed payload preimage.
    FingerprintMismatch,
    /// Fallible temporary payload-copy allocation failed.
    AllocationFailed,
}

impl From<htlk_cbor::Error> for EnvelopeError {
    fn from(error: htlk_cbor::Error) -> Self {
        Self::Codec(error)
    }
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(error) => write!(f, "executable envelope: {error}"),
            Self::ExpectedRecord => f.write_str("executable envelope must be a record"),
            Self::UnknownField => f.write_str("unknown executable envelope field"),
            Self::MissingField(field) => write!(f, "missing executable envelope field: {field}"),
            Self::InvalidFieldType(field) => {
                write!(f, "invalid executable envelope field type: {field}")
            }
            Self::UnsupportedFormat => f.write_str("unsupported executable envelope format"),
            Self::UnsupportedVersion => f.write_str("unsupported executable envelope version"),
            Self::InvalidFingerprint(error) => write!(f, "invalid executable fingerprint: {error}"),
            Self::FingerprintMismatch => f.write_str("executable fingerprint mismatch"),
            Self::AllocationFailed => f.write_str("executable envelope allocation failed"),
        }
    }
}

impl std::error::Error for EnvelopeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            Self::InvalidFingerprint(error) => Some(error),
            _ => None,
        }
    }
}
