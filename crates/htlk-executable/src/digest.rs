//! SHA-256 digest representation, strict text parsing, and canonical-CBOR hashing.

use std::{fmt, str::FromStr};

use sha2::{Digest as _, Sha256};

/// A 32-byte SHA-256 digest, displayed as `sha256:` plus 64 lowercase hex digits.
///
/// Construction and parsing validate representation, not the content or producer
/// identified by the digest. Ordering compares the stored bytes lexicographically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest {
    bytes: [u8; 32],
}

impl Digest {
    /// Constructs a digest from its exact bytes without hashing or validation
    /// against any content. Every 32-byte sequence is a valid representation.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    /// Borrows the digest's exact 32 bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        for byte in self.bytes {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Digest {
    type Err = ParseDigestError;

    /// Parses canonical digest text, checking prefix, length, then hex digits.
    /// No whitespace trimming, case folding, or Unicode normalization occurs.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix("sha256:")
            .ok_or(ParseDigestError::InvalidPrefix)?;
        if hex.len() != 64 {
            return Err(ParseDigestError::InvalidLength);
        }
        let mut bytes = [0; 32];
        let (pairs, _) = hex.as_bytes().as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            *byte = digit(pair[0])? << 4 | digit(pair[1])?;
        }
        Ok(Self::from_bytes(bytes))
    }
}

fn digit(byte: u8) -> Result<u8, ParseDigestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ParseDigestError::InvalidHex),
    }
}

/// A canonical digest-text parsing failure. Never retains input contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseDigestError {
    /// The text does not begin with the exact `sha256:` prefix.
    InvalidPrefix,
    /// The prefix is followed by something other than exactly 64 bytes.
    InvalidLength,
    /// The 64-byte suffix contains characters outside `0-9` and `a-f`.
    InvalidHex,
}

impl fmt::Display for ParseDigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPrefix => "digest must start with sha256:",
            Self::InvalidLength => "digest must contain exactly 64 hexadecimal digits",
            Self::InvalidHex => "digest must use lowercase hexadecimal digits",
        })
    }
}

impl std::error::Error for ParseDigestError {}

/// Computes SHA-256 of the exact supplied bytes.
///
/// Adds no domain label, version, length prefix, or encoding. The caller is
/// responsible for constructing its preimage and bounding input size. The hash
/// operation borrows the input and uses fixed-size hashing state.
///
/// ```
/// use htlk_executable::digest::hash_bytes;
/// assert_eq!(hash_bytes(b"abc").to_string(),
///     "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
/// ```
pub fn hash_bytes(bytes: &[u8]) -> Digest {
    Digest::from_bytes(Sha256::digest(bytes).into())
}

/// Computes SHA-256 of a value's deterministic CBOR encoding.
///
/// Encodes exactly once using [`htlk_cbor::encode`] and fresh per-operation
/// accounting, then hashes the complete resulting bytes. No implicit domain or
/// version labels are added. Different valid limits do not change the digest.
/// The temporary encoded buffer is dropped before returning.
///
/// # Errors
/// Propagates the codec's configuration, limit, and allocation errors unchanged.
/// No digest is produced unless encoding succeeds.
pub fn hash_cbor(
    value: &htlk_cbor::Value,
    limits: &htlk_cbor::Limits,
) -> Result<Digest, htlk_cbor::Error> {
    let bytes = htlk_cbor::encode(value, limits)?;
    Ok(hash_bytes(&bytes))
}

/// Closed compiler record-content digest domains. Libraries use linked
/// implementation identities rather than a caller-authored record hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    /// Root/task/loop-body scope definition.
    Scope,
    /// Complete node definition.
    Node,
    /// Prompt-template definition.
    Template,
    /// MCP binding definition.
    Binding,
    /// Compound MCP server identity.
    Server,
}

/// Hashes the exact canonical record under its version-0.1 domain:
/// `SHA256(UTF8("htlk.<kind>/0.1\n") || deterministic_cbor(record))`.
/// This validates encoding limits, not the record's schema or references.
///
/// # Errors
/// Propagates codec failures unchanged; no digest is returned on failure.
pub fn record_digest(
    kind: RecordKind,
    record: &htlk_cbor::Value,
    limits: &htlk_cbor::Limits,
) -> Result<Digest, htlk_cbor::Error> {
    let bytes = htlk_cbor::encode(record, limits)?;
    let prefix: &[u8] = match kind {
        RecordKind::Scope => b"htlk.scope/0.1\n",
        RecordKind::Node => b"htlk.node/0.1\n",
        RecordKind::Template => b"htlk.template/0.1\n",
        RecordKind::Binding => b"htlk.binding/0.1\n",
        RecordKind::Server => b"htlk.server/0.1\n",
    };
    let mut hash = Sha256::new();
    hash.update(prefix);
    hash.update(bytes);
    Ok(Digest::from_bytes(hash.finalize().into()))
}
