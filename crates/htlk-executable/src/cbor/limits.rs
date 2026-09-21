use super::{Error, ErrorKind};

const DEFAULT_DEPTH: usize = 64;
const MAX_SUPPORTED_DEPTH: usize = 128;

/// Resource ceilings for one codec operation.
///
/// Limits never affect canonical bytes. Zero is a valid ceiling: for example,
/// depth zero allows only a root value with no children. A zero document byte
/// budget cannot admit any complete CBOR value.
///
/// These limits do not bound allocations already made by a caller constructing
/// a [`super::Value`]. Encoded size is not exact process heap usage.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Maximum complete document bytes, including headers.
    pub max_document_bytes: usize,
    /// Maximum value depth; root is zero and each child adds one.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_document_bytes: 16 * 1024 * 1024,
            max_depth: DEFAULT_DEPTH,
        }
    }
}

impl Limits {
    /// Validates implementation constraints on this configuration.
    ///
    /// The default depth is 64; depths above 128 are currently unsupported.
    /// The byte ceiling may be raised by trusted callers.
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
