//! RFC 6901 location parsing and lookup, independent of schema-resource selection.

use crate::JsonDocument;
use crate::cbor as htlk_cbor;
use htlk_cbor::{LimitKind, Limits, Value};
use std::fmt;

/// Immutable decoded JSON Pointer tokens. Parsing does not select a schema
/// resource, resolve anchors, or prove the target is a schema-bearing location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonPointer {
    tokens: Vec<String>,
}
impl JsonPointer {
    /// Parses the plain RFC 6901 string form. Empty text selects the root; every
    /// nonempty pointer starts with `/`. No Unicode or percent normalization occurs.
    ///
    /// # Errors
    /// Returns invalid escape/syntax, codec configuration, allocation, or limit errors.
    pub fn new(text: &str, limits: &Limits) -> Result<Self, JsonPointerError> {
        limits.validate()?;
        check(
            text.len(),
            limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        let mut tokens = Vec::new();
        let mut payload = 0;
        if text.is_empty() {
            return Ok(Self { tokens });
        }
        if !text.starts_with('/') {
            return Err(JsonPointerError::InvalidSyntax { offset: 0 });
        }
        let mut offset = 1;
        for raw in text[1..].split('/') {
            let count = tokens.len() + 1;
            check(count, limits.max_depth, LimitKind::Depth)?;
            check(
                count,
                limits.max_collection_entries,
                LimitKind::CollectionEntries,
            )?;
            check(count, limits.max_total_values, LimitKind::TotalValues)?;
            // Validate and count exact decoded UTF-8 bytes before copying a token.
            let mut pos = 0;
            let mut length = 0;
            while pos < raw.len() {
                if raw.as_bytes()[pos] == b'~' {
                    if !matches!(raw.as_bytes().get(pos + 1), Some(b'0' | b'1')) {
                        return Err(JsonPointerError::InvalidSyntax {
                            offset: offset + pos,
                        });
                    }
                    pos += 2;
                } else {
                    pos += 1;
                }
                length += 1;
            }
            check(length, limits.max_text_bytes, LimitKind::TextBytes)?;
            payload = add(
                payload,
                length,
                limits.max_total_payload_bytes,
                LimitKind::TotalPayloadBytes,
            )?;
            let mut token = String::new();
            token.try_reserve_exact(length).map_err(allocation)?;
            let mut chars = raw.chars();
            while let Some(c) = chars.next() {
                token.push(if c == '~' {
                    if chars.next() == Some('0') { '~' } else { '/' }
                } else {
                    c
                });
            }
            tokens.try_reserve(1).map_err(allocation)?;
            tokens.push(token);
            offset += raw.len() + 1;
        }
        Ok(Self { tokens })
    }
    /// Parses a URI fragment beginning with `#`. Percent decoding precedes JSON
    /// Pointer escape decoding; `+` remains a plus. Non-ASCII fragment characters
    /// must use UTF-8 percent encoding. A named anchor is not a JSON Pointer.
    ///
    /// # Errors
    /// Returns malformed fragment/UTF-8/pointer errors or resource failures.
    pub fn from_fragment(fragment: &str, limits: &Limits) -> Result<Self, JsonPointerError> {
        limits.validate()?;
        check(
            fragment.len(),
            limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        if !fragment.starts_with('#') {
            return Err(JsonPointerError::InvalidFragment { offset: 0 });
        }
        let bytes = fragment.as_bytes();
        let mut pos = 1;
        let mut length = 0;
        while pos < bytes.len() {
            fragment_byte(bytes, &mut pos)?;
            length += 1;
        }
        // One bounded temporary holds the decoded fragment, before individually
        // bounded token copies. Source and decoded fragment bytes are not identities.
        let mut decoded = Vec::new();
        decoded.try_reserve_exact(length).map_err(allocation)?;
        pos = 1;
        while pos < bytes.len() {
            decoded.push(fragment_byte(bytes, &mut pos)?);
        }
        let text = std::str::from_utf8(&decoded).map_err(|e| JsonPointerError::InvalidUtf8 {
            offset: e.valid_up_to(),
        })?;
        Self::new(text, limits)
    }
    /// Exact decoded reference tokens; an empty sequence selects the root.
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }
    /// Resolves a location in the supplied JSON document without copying values.
    /// Object keys are exact strings; arrays require canonical decimal indices.
    ///
    /// # Errors
    /// Returns missing target, invalid array index, or scalar-traversal errors.
    /// A present JSON null is a successful result, never a missing target.
    pub fn resolve<'a>(&self, document: &'a JsonDocument) -> Result<&'a Value, JsonPointerError> {
        let mut current = document.value();
        for (segment, token) in self.tokens.iter().enumerate() {
            current = match current {
                Value::Map(m) => m
                    .get(token)
                    .ok_or(JsonPointerError::MissingTarget { segment })?,
                Value::Array(a) => {
                    if token == "-" {
                        return Err(JsonPointerError::MissingTarget { segment });
                    }
                    if token.is_empty()
                        || token.len() > 1 && token.starts_with('0')
                        || !token.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err(JsonPointerError::InvalidIndex { segment });
                    }
                    // A syntactically valid index larger than usize cannot name
                    // an element of any allocated array; it is a missing target.
                    let index = token
                        .parse::<usize>()
                        .map_err(|_| JsonPointerError::MissingTarget { segment })?;
                    a.get(index)
                        .ok_or(JsonPointerError::MissingTarget { segment })?
                }
                _ => return Err(JsonPointerError::ScalarTraversal { segment }),
            };
        }
        Ok(current)
    }
}
impl fmt::Display for JsonPointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for token in &self.tokens {
            f.write_str("/")?;
            for c in token.chars() {
                match c {
                    '~' => f.write_str("~0")?,
                    '/' => f.write_str("~1")?,
                    _ => write!(f, "{c}")?,
                }
            }
        }
        Ok(())
    }
}
fn fragment_byte(bytes: &[u8], pos: &mut usize) -> Result<u8, JsonPointerError> {
    let offset = *pos;
    let b = bytes[offset];
    if b == b'%' {
        let hi = bytes.get(offset + 1).copied().and_then(hex);
        let lo = bytes.get(offset + 2).copied().and_then(hex);
        let (Some(hi), Some(lo)) = (hi, lo) else {
            return Err(JsonPointerError::InvalidFragment { offset });
        };
        *pos += 3;
        Ok(hi * 16 + lo)
    } else if b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
                | b'?'
        )
    {
        *pos += 1;
        Ok(b)
    } else {
        Err(JsonPointerError::InvalidFragment { offset })
    }
}
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
fn check(n: usize, maximum: usize, limit: LimitKind) -> Result<(), JsonPointerError> {
    if n > maximum {
        Err(JsonPointerError::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}
fn add(a: usize, b: usize, maximum: usize, limit: LimitKind) -> Result<usize, JsonPointerError> {
    let n = a
        .checked_add(b)
        .ok_or(JsonPointerError::LimitExceeded { limit, maximum })?;
    check(n, maximum, limit)?;
    Ok(n)
}
fn allocation(_: std::collections::TryReserveError) -> JsonPointerError {
    JsonPointerError::AllocationFailed
}

/// Pointer errors retain locations and static categories, never submitted keys.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JsonPointerError {
    /// Invalid codec-limit configuration.
    Codec(htlk_cbor::Error),
    /// Invalid plain pointer, at a byte offset in the percent-decoded pointer.
    InvalidSyntax {
        /// Zero-based byte offset.
        offset: usize,
    },
    /// Invalid URI fragment character or percent escape.
    InvalidFragment {
        /// Zero-based byte offset in the submitted fragment.
        offset: usize,
    },
    /// Percent-decoded fragment is not UTF-8.
    InvalidUtf8 {
        /// Zero-based byte offset in the decoded fragment.
        offset: usize,
    },
    /// The indicated token names no object member or array element.
    MissingTarget {
        /// Zero-based token index.
        segment: usize,
    },
    /// An array reference token is not canonical unsigned decimal.
    InvalidIndex {
        /// Zero-based token index.
        segment: usize,
    },
    /// The indicated token attempts to traverse a scalar value.
    ScalarTraversal {
        /// Zero-based token index.
        segment: usize,
    },
    /// Pointer data or token-count ceiling exceeded.
    LimitExceeded {
        /// Exhausted logical resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Fallible storage reservation failed.
    AllocationFailed,
}
impl From<htlk_cbor::Error> for JsonPointerError {
    fn from(e: htlk_cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl fmt::Display for JsonPointerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON Pointer error: {self:?}")
    }
}
impl std::error::Error for JsonPointerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Codec(e) = self {
            Some(e)
        } else {
            None
        }
    }
}
