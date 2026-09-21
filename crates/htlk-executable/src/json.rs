//! Bounded JSON parsing and RFC 8785 serialization under HTLK numeric rules.

use std::{collections::BTreeMap, fmt, io};

use crate::cbor as htlk_cbor;
use crate::digest::{Digest, hash_bytes};
use htlk_cbor::{FiniteFloat, LimitKind, Limits, Map, Value};

const SAFE: u64 = (1u64 << 53) - 1;

/// Immutable external JSON document with exact canonical JCS bytes and identity.
/// Numbers follow HTLK's explicit JSON boundary normalization. This is not a
/// general runtime `json` type validator and does not resolve schemas or URIs.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonDocument {
    bytes: Vec<u8>,
    value: Value,
}
impl JsonDocument {
    /// Parses bounded authored JSON and normalizes it to JCS.
    ///
    /// # Errors
    /// Rejects syntax, duplicate keys, Unicode/numeric-profile violations, or
    /// exhausted input/output/structure limits.
    pub fn new(bytes: &[u8], limits: &Limits) -> Result<Self, JsonError> {
        limits.validate()?;
        check(
            bytes.len(),
            limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        std::str::from_utf8(bytes).map_err(|e| JsonError::InvalidUtf8 {
            offset: e.valid_up_to(),
        })?;
        let mut parser = Parser {
            bytes,
            pos: 0,
            budget: Accounting::new(limits),
        };
        let value = parser.value(0)?;
        parser.space();
        if parser.pos != bytes.len() {
            return Err(JsonError::Syntax { offset: parser.pos });
        }
        let bytes = encode_json(&value, limits)?;
        Ok(Self { bytes, value })
    }
    /// Requires exact canonical JCS input; does not silently repair stored bytes.
    ///
    /// # Errors
    /// Returns the parsing failures above or NonCanonical.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, JsonError> {
        let result = Self::new(bytes, limits)?;
        if result.bytes != bytes {
            return Err(JsonError::NonCanonical);
        }
        Ok(result)
    }
    /// Explicitly crosses from native values to external JSON. Integral safe
    /// floats become native integers on the decoded side of this boundary.
    ///
    /// # Errors
    /// Rejects bytes, unsafe numbers, and resource failures.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, JsonError> {
        let bytes = encode_json(value, limits)?;
        Self::decode(&bytes, limits)
    }
    /// Borrows exact canonical JSON bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Borrows the normalized JSON value representation.
    pub fn value(&self) -> &Value {
        &self.value
    }
    /// Computes raw SHA-256 of JCS bytes, without an HTLK record prefix.
    pub fn digest(&self) -> Digest {
        hash_bytes(&self.bytes)
    }
}

/// JSON/JCS profile failure; input contents are never retained in diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JsonError {
    /// Invalid codec limit configuration.
    Codec(htlk_cbor::Error),
    /// Invalid UTF-8 at the indicated byte.
    InvalidUtf8 {
        /// Zero-based byte offset.
        offset: usize,
    },
    /// Malformed JSON syntax/string escape at the indicated byte.
    Syntax {
        /// Zero-based byte offset.
        offset: usize,
    },
    /// Repeated decoded object key.
    DuplicateKey {
        /// Start of the repeated key.
        offset: usize,
    },
    /// Number violates HTLK's safe-integer/finite-binary64 profile.
    UnsafeNumber,
    /// A native byte string cannot be represented as JSON.
    UnsupportedValue,
    /// Input is valid JSON but not exact JCS bytes.
    NonCanonical,
    /// A data or output resource ceiling was exceeded.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// A fallible storage reservation failed.
    AllocationFailed,
    /// String serialization failed independently of a tracked output limit.
    Serialization,
}
impl From<htlk_cbor::Error> for JsonError {
    fn from(e: htlk_cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(e) => write!(f, "JSON limits: {e}"),
            Self::InvalidUtf8 { offset } => write!(f, "invalid JSON UTF-8 at byte {offset}"),
            Self::Syntax { offset } => write!(f, "invalid JSON syntax at byte {offset}"),
            Self::DuplicateKey { offset } => write!(f, "duplicate JSON key at byte {offset}"),
            Self::UnsafeNumber => f.write_str("number violates HTLK JSON numeric profile"),
            Self::UnsupportedValue => f.write_str("native bytes are not JSON values"),
            Self::NonCanonical => f.write_str("JSON document is not canonical JCS"),
            Self::LimitExceeded { limit, maximum } => {
                write!(f, "JSON limit exceeded: {limit:?} ({maximum})")
            }
            Self::AllocationFailed => f.write_str("JSON allocation failed"),
            Self::Serialization => f.write_str("JSON serialization failed"),
        }
    }
}
impl std::error::Error for JsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            _ => None,
        }
    }
}
fn allocation(_: std::collections::TryReserveError) -> JsonError {
    JsonError::AllocationFailed
}
fn check(n: usize, max: usize, limit: LimitKind) -> Result<(), JsonError> {
    if n > max {
        Err(JsonError::LimitExceeded {
            limit,
            maximum: max,
        })
    } else {
        Ok(())
    }
}
fn add(a: usize, b: usize, max: usize, limit: LimitKind) -> Result<usize, JsonError> {
    let n = a.checked_add(b).ok_or(JsonError::LimitExceeded {
        limit,
        maximum: max,
    })?;
    check(n, max, limit)?;
    Ok(n)
}
struct Accounting<'a> {
    limits: &'a Limits,
}
impl<'a> Accounting<'a> {
    fn new(limits: &'a Limits) -> Self {
        Self { limits }
    }
    fn enter(&mut self, depth: usize) -> Result<(), JsonError> {
        check(depth, self.limits.max_depth, LimitKind::Depth)
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    budget: Accounting<'a>,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.pos += 1;
        }
    }
    fn syntax(&self) -> JsonError {
        JsonError::Syntax { offset: self.pos }
    }
    fn eat(&mut self, b: u8) -> Result<(), JsonError> {
        self.space();
        if self.bytes.get(self.pos) != Some(&b) {
            return Err(self.syntax());
        }
        self.pos += 1;
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.budget.enter(depth)?;
        self.space();
        match self.bytes.get(self.pos).copied() {
            Some(b'"') => Ok(Value::Text(self.string()?)),
            Some(b'[') => self.list(depth),
            Some(b'{') => self.object(depth),
            Some(b't') => {
                self.word(b"true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.word(b"false")?;
                Ok(Value::Bool(false))
            }
            Some(b'n') => {
                self.word(b"null")?;
                Ok(Value::Null)
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.syntax()),
        }
    }
    fn word(&mut self, word: &[u8]) -> Result<(), JsonError> {
        if !self.bytes[self.pos..].starts_with(word) {
            return Err(self.syntax());
        }
        self.pos += word.len();
        Ok(())
    }
    #[inline(never)]
    fn list(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.eat(b'[')?;
        self.space();
        let mut values = Vec::new();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(Value::Array(values));
        }
        loop {
            let value = self.value(depth + 1)?;
            values.try_reserve(1).map_err(allocation)?;
            values.push(value);
            self.space();
            if self.bytes.get(self.pos) == Some(&b']') {
                self.pos += 1;
                break;
            }
            self.eat(b',')?;
        }
        Ok(Value::Array(values))
    }
    #[inline(never)]
    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.eat(b'{')?;
        self.space();
        let mut fields = BTreeMap::new();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(Value::Map(Map::new()));
        }
        loop {
            self.budget.enter(depth + 1)?;
            self.space();
            let start = self.pos;
            let key = self.string()?;
            if fields.contains_key(&key) {
                return Err(JsonError::DuplicateKey { offset: start });
            }
            self.eat(b':')?;
            let value = self.value(depth + 1)?;
            fields.insert(key, value);
            self.space();
            if self.bytes.get(self.pos) == Some(&b'}') {
                self.pos += 1;
                break;
            }
            self.eat(b',')?;
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(fields.len())
            .map_err(allocation)?;
        entries.extend(fields);
        Ok(Value::Map(Map::try_from_entries(entries)?))
    }
    // Scan/validate escapes and decoded byte length before serde allocates the
    // decoded string. The enclosing input was already bounded and UTF-8 checked.
    fn string(&mut self) -> Result<String, JsonError> {
        self.space();
        let start = self.pos;
        self.eat(b'"')?;
        let mut len = 0;
        loop {
            let byte = *self.bytes.get(self.pos).ok_or_else(|| self.syntax())?;
            if byte == b'"' {
                self.pos += 1;
                break;
            }
            let count = match byte {
                0..=31 => return Err(self.syntax()),
                b'\\' => {
                    self.pos += 1;
                    match *self.bytes.get(self.pos).ok_or_else(|| self.syntax())? {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                            self.pos += 1;
                            1
                        }
                        b'u' => {
                            self.pos += 1;
                            let high = self.hex4()?;
                            let point = if (0xd800..=0xdbff).contains(&high) {
                                if !self.bytes[self.pos..].starts_with(b"\\u") {
                                    return Err(self.syntax());
                                }
                                self.pos += 2;
                                let low = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return Err(self.syntax());
                                }
                                0x10000 + ((high - 0xd800) << 10) + low - 0xdc00
                            } else {
                                high
                            };
                            char::from_u32(point)
                                .ok_or_else(|| self.syntax())?
                                .len_utf8()
                        }
                        _ => return Err(self.syntax()),
                    }
                }
                _ => {
                    self.pos += 1;
                    1
                }
            };
            len = add(
                len,
                count,
                self.budget.limits.max_document_bytes,
                LimitKind::DocumentBytes,
            )?;
        }
        serde_json::from_slice(&self.bytes[start..self.pos])
            .map_err(|_| JsonError::Syntax { offset: start })
    }
    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut n = 0;
        for _ in 0..4 {
            let b = *self.bytes.get(self.pos).ok_or_else(|| self.syntax())?;
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(self.syntax()),
            };
            self.pos += 1;
            n = (n << 4) | u32::from(digit);
        }
        Ok(n)
    }
    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.bytes.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.syntax()),
        }
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let begin = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == begin {
                return Err(self.syntax());
            }
        }
        let mantissa_end = self.pos;
        let mut exponent = 0i64;
        if self
            .bytes
            .get(self.pos)
            .is_some_and(|b| matches!(b, b'e' | b'E'))
        {
            self.pos += 1;
            let negative = self.bytes.get(self.pos) == Some(&b'-');
            if self
                .bytes
                .get(self.pos)
                .is_some_and(|b| matches!(b, b'+' | b'-'))
            {
                self.pos += 1;
            }
            let begin = self.pos;
            while let Some(b @ b'0'..=b'9') = self.bytes.get(self.pos) {
                exponent = exponent
                    .saturating_mul(10)
                    .saturating_add(i64::from(*b - b'0'));
                self.pos += 1;
            }
            if self.pos == begin {
                return Err(self.syntax());
            }
            if negative {
                exponent = -exponent;
            }
        }
        let mantissa = &self.bytes[start..mantissa_end];
        check_integral_token(mantissa, exponent)?;
        let token = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII numeric token");
        let number = token.parse::<f64>().map_err(|_| JsonError::UnsafeNumber)?;
        native_number(number)
    }
}

fn check_integral_token(mantissa: &[u8], exponent: i64) -> Result<(), JsonError> {
    let fraction = mantissa
        .iter()
        .position(|b| *b == b'.')
        .map_or(0, |p| mantissa.len() - p - 1) as i64;
    let mut digits = 0i64;
    let mut first = None;
    let mut last = 0i64;
    for b in mantissa.iter().filter(|b| b.is_ascii_digit()) {
        if *b != b'0' {
            first.get_or_insert(digits);
            last = digits;
        }
        digits += 1;
    }
    let Some(first) = first else {
        return Ok(());
    };
    let significant = last - first + 1;
    let shift = exponent
        .saturating_sub(fraction)
        .saturating_add(digits - last - 1);
    if shift < 0 {
        return Ok(());
    } // Mathematical fraction: check rounded value next.
    let width = significant.saturating_add(shift);
    if width > 16 {
        return Err(JsonError::UnsafeNumber);
    }
    if width == 16 {
        let mut decimal = [b'0'; 16];
        for (i, b) in mantissa
            .iter()
            .filter(|b| b.is_ascii_digit())
            .skip(first as usize)
            .take(significant as usize)
            .enumerate()
        {
            decimal[i] = *b;
        }
        if decimal > *b"9007199254740991" {
            return Err(JsonError::UnsafeNumber);
        }
    }
    Ok(())
}
fn native_number(n: f64) -> Result<Value, JsonError> {
    if !n.is_finite() {
        return Err(JsonError::UnsafeNumber);
    }
    if n.fract() == 0.0 {
        if n.abs() > SAFE as f64 {
            return Err(JsonError::UnsafeNumber);
        }
        Ok(Value::Integer(n as i64))
    } else {
        Ok(Value::Float(
            FiniteFloat::new(n).map_err(|_| JsonError::UnsafeNumber)?,
        ))
    }
}

struct Output {
    bytes: Vec<u8>,
    maximum: usize,
    failure: Option<JsonError>,
}
impl io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| {
            let needed = add(
                self.bytes.len(),
                bytes.len(),
                self.maximum,
                LimitKind::DocumentBytes,
            )?;
            if needed > self.bytes.capacity() {
                let target = needed.max(self.bytes.capacity().saturating_mul(2).min(self.maximum));
                self.bytes
                    .try_reserve_exact(target - self.bytes.len())
                    .map_err(allocation)?;
            }
            self.bytes.extend_from_slice(bytes);
            Ok::<_, JsonError>(bytes.len())
        })();
        match result {
            Ok(n) => Ok(n),
            Err(e) => {
                self.failure = Some(e);
                Err(io::Error::other("bounded JSON output failure"))
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Output {
    fn append(&mut self, bytes: &[u8]) -> Result<(), JsonError> {
        io::Write::write_all(self, bytes)
            .map_err(|_| self.failure.take().unwrap_or(JsonError::Serialization))
    }
    fn string(&mut self, s: &str) -> Result<(), JsonError> {
        serde_json::to_writer(&mut *self, s)
            .map_err(|_| self.failure.take().unwrap_or(JsonError::Serialization))
    }
    fn value(
        &mut self,
        v: &Value,
        budget: &mut Accounting<'_>,
        depth: usize,
    ) -> Result<(), JsonError> {
        budget.enter(depth)?;
        match v {
            Value::Null => self.append(b"null"),
            Value::Bool(true) => self.append(b"true"),
            Value::Bool(false) => self.append(b"false"),
            Value::Text(s) => self.string(s),
            Value::Bytes(_) => Err(JsonError::UnsupportedValue),
            Value::Integer(n) => {
                if n.unsigned_abs() > SAFE {
                    return Err(JsonError::UnsafeNumber);
                }
                self.append(ryu_js::Buffer::new().format_finite(*n as f64).as_bytes())
            }
            Value::Float(n) => {
                native_number(n.get())?;
                self.append(ryu_js::Buffer::new().format_finite(n.get()).as_bytes())
            }
            Value::Array(values) => self.list(values, budget, depth),
            Value::Map(values) => self.object(values, budget, depth),
        }
    }
    #[inline(never)]
    fn list(
        &mut self,
        values: &[Value],
        budget: &mut Accounting<'_>,
        depth: usize,
    ) -> Result<(), JsonError> {
        self.append(b"[")?;
        for (i, value) in values.iter().enumerate() {
            if i != 0 {
                self.append(b",")?;
            }
            self.value(value, budget, depth + 1)?;
        }
        self.append(b"]")
    }
    #[inline(never)]
    fn object(
        &mut self,
        values: &Map,
        budget: &mut Accounting<'_>,
        depth: usize,
    ) -> Result<(), JsonError> {
        // Even the smallest JSON member needs bytes. Preflight before allocating
        // the sorting index; exact punctuation, escaping and values charge below.
        add(
            self.bytes.len(),
            values.len(),
            self.maximum,
            LimitKind::DocumentBytes,
        )?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(values.len())
            .map_err(allocation)?;
        entries.extend(values.iter());
        entries.sort_unstable_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        self.append(b"{")?;
        for (i, (key, value)) in entries.into_iter().enumerate() {
            if i != 0 {
                self.append(b",")?;
            }
            budget.enter(depth + 1)?;
            self.string(key)?;
            self.append(b":")?;
            self.value(value, budget, depth + 1)?;
        }
        self.append(b"}")
    }
}
fn encode_json(v: &Value, limits: &Limits) -> Result<Vec<u8>, JsonError> {
    limits.validate()?;
    let mut out = Output {
        bytes: Vec::new(),
        maximum: limits.max_document_bytes,
        failure: None,
    };
    out.value(v, &mut Accounting::new(limits), 0)?;
    Ok(out.bytes)
}
