use std::{fmt, str::FromStr};

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
