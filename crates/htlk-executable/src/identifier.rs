use std::{borrow::Borrow, fmt, str::FromStr};

/// An exact local name matching `[a-z][a-z0-9]*(_[a-z0-9]+)*`.
///
/// Validates ASCII spelling only, without trimming or normalization. Reserved
/// roots, visibility, and uniqueness are checked by consumers in their use
/// context. For example, `inputs` is lexically valid but cannot name a node.
/// Qualified names, module paths, and arbitrary quoted record fields are not
/// represented by this type.
///
/// Ordering is ordinary ASCII/UTF-8 lexical order. Canonical CBOR map-key
/// ordering remains the codec's responsibility. Callers bound input size before
/// parsing; this lexical rule introduces no additional length ceiling.
///
/// ```
/// use htlk_executable::Identifier;
/// let name: Identifier = "review_result_2".parse()?;
/// assert_eq!(name.as_str(), "review_result_2");
/// assert!("ReviewResult".parse::<Identifier>().is_err());
/// # Ok::<(), htlk_executable::ParseIdentifierError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identifier {
    value: String,
}

impl Identifier {
    /// Validates an owned name without copying its string storage.
    ///
    /// # Errors
    /// Returns a spelling error if the name does not match the identifier rule.
    pub fn new(value: String) -> Result<Self, ParseIdentifierError> {
        validate(&value)?;
        Ok(Self { value })
    }

    /// Borrows the exact validated name.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns the owned string without copying it.
    pub fn into_string(self) -> String {
        self.value
    }
}

impl FromStr for Identifier {
    type Err = ParseIdentifierError;

    /// Checks spelling before reserving owned storage. Characters are checked
    /// left-to-right, then a trailing underscore is rejected.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        validate(value)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| ParseIdentifierError::AllocationFailed)?;
        owned.push_str(value);
        Ok(Self { value: owned })
    }
}

impl TryFrom<String> for Identifier {
    type Error = ParseIdentifierError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Identifier {
    type Error = ParseIdentifierError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl AsRef<str> for Identifier {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for Identifier {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn validate(value: &str) -> Result<(), ParseIdentifierError> {
    let Some((&first, rest)) = value.as_bytes().split_first() else {
        return Err(ParseIdentifierError::Empty);
    };
    if !first.is_ascii_lowercase() {
        return Err(ParseIdentifierError::InvalidStart);
    }
    let mut underscore = false;
    for (index, &byte) in rest.iter().enumerate() {
        let offset = index + 1;
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => underscore = false,
            b'_' if underscore => return Err(ParseIdentifierError::InvalidSeparator { offset }),
            b'_' => underscore = true,
            _ => return Err(ParseIdentifierError::InvalidCharacter { offset }),
        }
    }
    if underscore {
        return Err(ParseIdentifierError::InvalidSeparator {
            offset: value.len() - 1,
        });
    }
    Ok(())
}

/// A lexical identifier failure, without retaining the submitted text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseIdentifierError {
    /// The name is empty.
    Empty,
    /// The first byte is not an ASCII lowercase letter.
    InvalidStart,
    /// A later byte is outside ASCII lowercase letters, digits, and underscore.
    InvalidCharacter {
        /// Zero-based byte offset of the invalid character's first byte.
        offset: usize,
    },
    /// An underscore is repeated or ends the name.
    InvalidSeparator {
        /// Zero-based byte offset of the repeated or trailing underscore.
        offset: usize,
    },
    /// Allocating the validated borrowed name failed.
    AllocationFailed,
}

impl fmt::Display for ParseIdentifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identifier must not be empty"),
            Self::InvalidStart => {
                f.write_str("identifier must start with an ASCII lowercase letter")
            }
            Self::InvalidCharacter { offset } => {
                write!(f, "invalid identifier character at byte {offset}")
            }
            Self::InvalidSeparator { offset } => {
                write!(f, "invalid identifier underscore at byte {offset}")
            }
            Self::AllocationFailed => f.write_str("identifier allocation failed"),
        }
    }
}

impl std::error::Error for ParseIdentifierError {}
