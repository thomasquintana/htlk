use super::{Error, ErrorKind};

const DEFAULT_DEPTH: usize = 64;
const MAX_SUPPORTED_DEPTH: usize = 128;

/// Resource ceilings for one codec operation.
///
/// Limits never affect canonical bytes. Zero is a valid ceiling: for example,
/// depth zero allows only a root value with no children. A zero document or
/// total-value budget cannot admit any complete CBOR value.
///
/// These limits do not bound allocations already made by a caller constructing
/// a [`super::Value`]. Payload accounting is not exact process heap accounting.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Maximum complete document bytes, including headers.
    pub max_document_bytes: usize,
    /// Maximum UTF-8 bytes in one string, including a map key.
    pub max_text_bytes: usize,
    /// Maximum bytes in one byte string.
    pub max_byte_string_bytes: usize,
    /// Maximum value depth; root is zero and each child adds one.
    pub max_depth: usize,
    /// Maximum array elements or map pairs in one collection.
    pub max_collection_entries: usize,
    /// Maximum aggregate values, counting containers and map keys.
    pub max_total_values: usize,
    /// Maximum aggregate text and byte-string payload bytes.
    pub max_total_payload_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_document_bytes: 16 * 1024 * 1024,
            max_text_bytes: 1024 * 1024,
            max_byte_string_bytes: 16 * 1024 * 1024,
            max_depth: DEFAULT_DEPTH,
            max_collection_entries: 100_000,
            max_total_values: 1_000_000,
            max_total_payload_bytes: 16 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Validates implementation constraints on this configuration.
    ///
    /// The default depth is 64; depths above 128 are currently unsupported.
    /// Other ceilings may be raised
    /// by trusted callers; overlapping ceilings need not be equal or ordered.
    ///
    /// # Errors
    /// Returns [`ErrorKind::InvalidLimits`] if `max_depth` exceeds 128.
    pub fn validate(&self) -> Result<(), Error> {
        if self.max_depth > MAX_SUPPORTED_DEPTH {
            return Err(Error::new(ErrorKind::InvalidLimits));
        }
        Ok(())
    }
}
