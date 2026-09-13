use crate::{Error, ErrorKind};

/// A value in HTLK's restricted CBOR model.
///
/// Null is a value; absence belongs in runtime manifests. Integers and floats
/// remain distinct. Arrays and maps may contain further values.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Explicit null.
    Null,
    /// A Boolean.
    Bool(bool),
    /// A signed 64-bit integer.
    Integer(i64),
    /// A finite, positive-zero-normalized floating-point value.
    Float(FiniteFloat),
    /// Exact Unicode text, without Unicode normalization.
    Text(String),
    /// Opaque bytes.
    Bytes(Vec<u8>),
    /// An ordered sequence of values.
    Array(Vec<Value>),
    /// Unique exact string keys and their values.
    Map(Map),
}

/// A finite binary64 value with negative zero normalized to positive zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FiniteFloat(f64);

impl FiniteFloat {
    /// Constructs a finite float, normalizing either zero sign to positive.
    ///
    /// # Errors
    /// Returns [`ErrorKind::NonFiniteFloat`] for NaN or infinity.
    pub fn new(value: f64) -> Result<Self, Error> {
        if !value.is_finite() {
            return Err(Error::new(ErrorKind::NonFiniteFloat));
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }

    /// Returns the normalized binary64 value.
    pub fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for FiniteFloat {
    type Error = Error;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<FiniteFloat> for f64 {
    fn from(value: FiniteFloat) -> Self {
        value.get()
    }
}

/// Unique string-keyed entries stored in canonical CBOR key order.
///
/// For shortest-encoded text keys, encoded-byte ordering is equivalent to
/// UTF-8 byte length followed by bytewise text ordering. This is deliberately
/// different from runtime diagnostic port ordering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Map {
    entries: Vec<(String, Value)>,
}

impl Map {
    /// Decoder-only path: entries must have unique, canonical-order text keys.
    pub(crate) fn from_canonical_entries(entries: Vec<(String, Value)>) -> Self {
        Self { entries }
    }

    /// Creates an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Collects and canonically sorts entries, rejecting duplicate keys.
    ///
    /// This is an authored-value constructor, not an untrusted decoding API.
    /// The caller must provide a finite iterator and bound its constructed data.
    /// It does not apply document limits or normalize Unicode text.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateMapKey`] for repeated keys or
    /// [`ErrorKind::AllocationFailed`] if entry storage cannot be reserved.
    pub fn try_from_entries(
        entries: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<Self, Error> {
        let mut collected = Vec::new();
        for entry in entries {
            collected
                .try_reserve(1)
                .map_err(|_| Error::new(ErrorKind::AllocationFailed))?;
            collected.push(entry);
        }
        // In-place unstable sorting requires no additional heap allocation.
        collected.sort_unstable_by(|(a, _), (b, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        if collected.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(Error::new(ErrorKind::DuplicateMapKey));
        }
        Ok(Self { entries: collected })
    }

    /// Looks up a value by its exact key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .binary_search_by(|(candidate, _)| {
                candidate
                    .len()
                    .cmp(&key.len())
                    .then_with(|| candidate.as_str().cmp(key))
            })
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// Iterates over entries in canonical encoded-key order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Value)> + '_ {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// Returns the number of key/value pairs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the map contains no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
