use std::fmt;

/// A codec failure with an optional zero-based input byte offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    offset: Option<usize>,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind) -> Self {
        Self { kind, offset: None }
    }

    /// Supplies context without replacing a more precise nested error offset.
    pub(crate) fn at(mut self, offset: usize) -> Self {
        self.offset.get_or_insert(offset);
        self
    }

    /// Returns the structured failure category.
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Returns the input byte offset, if this failure concerns encoded input.
    pub fn offset(&self) -> Option<usize> {
        self.offset
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
    /// Bytes in one text string, including map keys.
    TextBytes,
    /// Bytes in one byte string.
    ByteStringBytes,
    /// Value depth, with the root at zero.
    Depth,
    /// Elements in one array or pairs in one map.
    CollectionEntries,
    /// Aggregate values, including containers and map keys.
    TotalValues,
    /// Aggregate text and byte-string payload bytes.
    TotalPayloadBytes,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Categories and offsets contain no caller-supplied value contents.
        write!(f, "CBOR error: {:?}", self.kind)?;
        if let Some(offset) = self.offset {
            write!(f, " at byte {offset}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}
