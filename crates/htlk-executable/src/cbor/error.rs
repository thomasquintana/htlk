use std::fmt;

use super::limits::MAX_SUPPORTED_DEPTH;

/// A codec failure with an optional zero-based input byte offset.
///
/// Display provides human-facing prose; use [`Self::kind`] and [`Self::offset`]
/// for structured handling rather than parsing message text. Equality compares
/// only the category and offset, not private diagnostic details. Neither Display
/// nor Debug includes input contents. Applications choose their own log format.
#[derive(Clone)]
pub struct Error {
    kind: ErrorKind,
    // Store presence separately so it and the reason share alignment padding,
    // rather than enlarging errors carried through recursive decoder frames.
    offset: usize,
    has_offset: bool,
    reason: Option<CanonicalityReason>,
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("offset", &self.offset())
            .field("reason", &self.reason)
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum CanonicalityReason {
    NonminimalHeader,
    IndefiniteLength,
    WideFloat,
    NegativeZero,
    WideSimple,
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.offset() == other.offset()
    }
}

impl Eq for Error {}

impl Error {
    pub(crate) fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            offset: 0,
            has_offset: false,
            reason: None,
        }
    }

    pub(super) fn noncanonical(reason: CanonicalityReason) -> Self {
        Self {
            reason: Some(reason),
            ..Self::new(ErrorKind::NonCanonicalEncoding)
        }
    }

    /// Supplies context without replacing a more precise nested error offset.
    pub(crate) fn at(mut self, offset: usize) -> Self {
        if !self.has_offset {
            self.offset = offset;
            self.has_offset = true;
        }
        self
    }

    /// Returns the structured failure category.
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Returns the zero-based input byte offset, if this failure concerns encoded input.
    pub fn offset(&self) -> Option<usize> {
        self.has_offset.then_some(self.offset)
    }
}

/// A codec failure category, independent of runtime error codes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The input ended before a complete value could be read.
    UnexpectedEnd,
    /// Bytes followed the complete top-level value.
    TrailingData,
    /// Text was not valid UTF-8.
    InvalidUtf8,
    /// The CBOR type is outside the HTLK profile.
    UnsupportedType,
    /// An integer cannot be represented as a signed 64-bit value.
    IntegerOutOfRange,
    /// A float was NaN or infinite.
    NonFiniteFloat,
    /// A value did not use its required canonical representation.
    NonCanonicalEncoding,
    /// Two map entries have the same exact key.
    DuplicateMapKey,
    /// Encoded map keys were not in canonical order.
    MapKeyOutOfOrder,
    /// A configured resource ceiling was exceeded.
    LimitExceeded {
        /// The exhausted resource.
        limit: LimitKind,
        /// The configured ceiling.
        maximum: usize,
    },
    /// A limit configuration exceeds implementation capabilities.
    InvalidLimits,
    /// A fallible memory reservation failed.
    AllocationFailed,
}

/// Resources bounded by codec limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitKind {
    /// Complete encoded document size, including headers.
    DocumentBytes,
    /// Value depth, with the root at zero.
    Depth,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::UnexpectedEnd => {
                f.write_str("CBOR input ended before the value was complete")?
            }
            ErrorKind::TrailingData => {
                f.write_str("CBOR input contains bytes after the complete top-level value")?
            }
            ErrorKind::InvalidUtf8 => f.write_str("CBOR text is not valid UTF-8")?,
            ErrorKind::UnsupportedType => {
                f.write_str("The CBOR type is not supported by the HTLK profile in this position")?
            }
            ErrorKind::IntegerOutOfRange => {
                f.write_str("CBOR integer is outside the signed 64-bit range")?
            }
            ErrorKind::NonFiniteFloat => {
                f.write_str("CBOR floating-point value is NaN or infinite")?
            }
            ErrorKind::NonCanonicalEncoding => f.write_str(match self.reason {
                None => "CBOR value does not use its required canonical encoding",
                Some(CanonicalityReason::NonminimalHeader) => {
                    "CBOR integer or length header is wider than necessary"
                }
                Some(CanonicalityReason::IndefiniteLength) => {
                    "The CBOR header uses an indefinite-length marker, which HTLK does not allow"
                }
                Some(CanonicalityReason::WideFloat) => {
                    "CBOR floating-point encoding is wider than necessary"
                }
                Some(CanonicalityReason::NegativeZero) => {
                    "CBOR encodes negative zero, but canonical encoding requires positive zero"
                }
                Some(CanonicalityReason::WideSimple) => {
                    "CBOR Boolean or null encoding is wider than necessary"
                }
            })?,
            ErrorKind::DuplicateMapKey => f.write_str("CBOR map contains a duplicate key")?,
            ErrorKind::MapKeyOutOfOrder => {
                f.write_str("CBOR map keys are not in canonical order")?
            }
            ErrorKind::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                maximum,
            } => write!(
                f,
                "CBOR document size exceeds the configured maximum of {maximum} bytes"
            )?,
            ErrorKind::LimitExceeded {
                limit: LimitKind::Depth,
                maximum,
            } => write!(
                f,
                "CBOR nesting exceeds the configured maximum depth of {maximum}"
            )?,
            ErrorKind::InvalidLimits => write!(
                f,
                "The configured CBOR depth limit exceeds the supported maximum of {MAX_SUPPORTED_DEPTH}"
            )?,
            ErrorKind::AllocationFailed => {
                f.write_str("Could not allocate memory to process the CBOR value")?
            }
        }
        if let Some(offset) = self.offset() {
            write!(f, " at byte {offset}")?;
        }
        f.write_str(".")
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_prose_with_and_without_offsets() {
        let cases = [
            (
                ErrorKind::UnexpectedEnd,
                "CBOR input ended before the value was complete",
            ),
            (
                ErrorKind::TrailingData,
                "CBOR input contains bytes after the complete top-level value",
            ),
            (ErrorKind::InvalidUtf8, "CBOR text is not valid UTF-8"),
            (
                ErrorKind::UnsupportedType,
                "The CBOR type is not supported by the HTLK profile in this position",
            ),
            (
                ErrorKind::IntegerOutOfRange,
                "CBOR integer is outside the signed 64-bit range",
            ),
            (
                ErrorKind::NonFiniteFloat,
                "CBOR floating-point value is NaN or infinite",
            ),
            (
                ErrorKind::NonCanonicalEncoding,
                "CBOR value does not use its required canonical encoding",
            ),
            (
                ErrorKind::DuplicateMapKey,
                "CBOR map contains a duplicate key",
            ),
            (
                ErrorKind::MapKeyOutOfOrder,
                "CBOR map keys are not in canonical order",
            ),
            (
                ErrorKind::LimitExceeded {
                    limit: LimitKind::DocumentBytes,
                    maximum: 1024,
                },
                "CBOR document size exceeds the configured maximum of 1024 bytes",
            ),
            (
                ErrorKind::LimitExceeded {
                    limit: LimitKind::Depth,
                    maximum: 64,
                },
                "CBOR nesting exceeds the configured maximum depth of 64",
            ),
            (
                ErrorKind::InvalidLimits,
                "The configured CBOR depth limit exceeds the supported maximum of 128",
            ),
            (
                ErrorKind::AllocationFailed,
                "Could not allocate memory to process the CBOR value",
            ),
        ];
        for (kind, prose) in cases {
            let error = Error::new(kind.clone());
            assert_eq!(error.to_string(), format!("{prose}."));
            assert_eq!(error.kind(), &kind);
            assert_eq!(error.offset(), None);
            let error = error.at(3).at(0);
            assert_eq!(error.to_string(), format!("{prose} at byte 3."));
            assert_eq!(error.offset(), Some(3));
        }
    }

    #[test]
    fn equality_ignores_private_reasons_but_preserves_category_and_offset() {
        let generic = Error::new(ErrorKind::NonCanonicalEncoding);
        for reason in [
            CanonicalityReason::NonminimalHeader,
            CanonicalityReason::IndefiniteLength,
            CanonicalityReason::WideFloat,
            CanonicalityReason::NegativeZero,
            CanonicalityReason::WideSimple,
        ] {
            let detailed = Error::noncanonical(reason);
            assert_eq!(detailed, generic);
            assert_eq!(detailed.clone().at(7), generic.clone().at(7));
            assert_ne!(detailed.clone().at(7), generic.clone().at(8));
            assert_ne!(detailed.clone().at(7), generic);
            assert_ne!(detailed, Error::new(ErrorKind::UnsupportedType));
        }
        assert_eq!(generic.at(usize::MAX).at(0).offset(), Some(usize::MAX));
    }

    #[test]
    fn canonicality_prose_without_input_offsets() {
        for (reason, expected) in [
            (
                CanonicalityReason::NonminimalHeader,
                "CBOR integer or length header is wider than necessary.",
            ),
            (
                CanonicalityReason::IndefiniteLength,
                "The CBOR header uses an indefinite-length marker, which HTLK does not allow.",
            ),
            (
                CanonicalityReason::WideFloat,
                "CBOR floating-point encoding is wider than necessary.",
            ),
            (
                CanonicalityReason::NegativeZero,
                "CBOR encodes negative zero, but canonical encoding requires positive zero.",
            ),
            (
                CanonicalityReason::WideSimple,
                "CBOR Boolean or null encoding is wider than necessary.",
            ),
        ] {
            assert_eq!(Error::noncanonical(reason).to_string(), expected);
        }
    }
}
