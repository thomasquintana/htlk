use std::cmp::Ordering;

use half::f16;

use crate::accounting::{Accounting, add, check};
use crate::{Error, ErrorKind, FiniteFloat, LimitKind, Limits, Map, Value};

/// Decodes exactly one canonical HTLK CBOR value under fresh resource limits.
///
/// Noncanonical representations are rejected, never repaired. Byte strings are
/// opaque: a nested executable payload requires a separate explicit decode.
/// Map keys count toward both value and payload limits.
///
/// # Errors
/// Returns a structured error for invalid configuration, malformed, unsupported,
/// or noncanonical input, exhausted limits, or failed memory reservation.
/// Input errors carry a zero-based byte offset; truncation points to the input
/// length (the first missing byte). Configuration errors have no input offset.
/// No partially constructed value is returned.
///
/// # Examples
/// ```
/// use htlk_cbor::{decode, Limits, Value};
/// assert_eq!(decode(&[0x18, 0x18], &Limits::default())?, Value::Integer(24));
/// # Ok::<(), htlk_cbor::Error>(())
/// ```
pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Value, Error> {
    limits.validate()?;
    check(
        bytes.len(),
        limits.max_document_bytes,
        LimitKind::DocumentBytes,
    )
    .map_err(|error| error.at(0))?;
    let mut decoder = Decoder {
        input: bytes,
        position: 0,
        accounting: Accounting::new(limits),
    };
    let value = decoder.value(0)?;
    if decoder.position != bytes.len() {
        return Err(Error::new(ErrorKind::TrailingData).at(decoder.position));
    }
    Ok(value)
}

struct Decoder<'a> {
    input: &'a [u8],
    position: usize,
    accounting: Accounting<'a>,
}

impl<'a> Decoder<'a> {
    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        let start = self.position;
        self.value_inner(depth).map_err(|error| error.at(start))
    }

    fn value_inner(&mut self, depth: usize) -> Result<Value, Error> {
        self.accounting.enter(depth)?;
        let initial = self.take(1)?[0];
        let major = initial >> 5;
        let additional = initial & 31;
        if major == 7 {
            return self.simple(additional);
        }
        if major == 6 {
            return Err(Error::new(ErrorKind::UnsupportedType));
        }
        let argument = self.argument(additional)?;
        match major {
            0 | 1 => {
                let integer = i64::try_from(argument)
                    .map_err(|_| Error::new(ErrorKind::IntegerOutOfRange))?;
                Ok(Value::Integer(if major == 0 { integer } else { !integer }))
            }
            2 => {
                let bytes = self.payload(argument, false)?;
                let mut owned = Vec::new();
                owned.try_reserve_exact(bytes.len()).map_err(allocation)?;
                owned.extend_from_slice(bytes);
                Ok(Value::Bytes(owned))
            }
            3 => Ok(Value::Text(own_text(self.text(argument)?)?)),
            4 => self.array(argument, depth),
            5 => self.map(argument, depth),
            _ => unreachable!("major type is three bits; tags and simple values handled above"),
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        // Subtraction is safe: position only advances through this method.
        if count > self.input.len() - self.position {
            return Err(Error::new(ErrorKind::UnexpectedEnd).at(self.input.len()));
        }
        let start = self.position;
        self.position += count;
        Ok(&self.input[start..self.position])
    }

    fn argument(&mut self, additional: u8) -> Result<u64, Error> {
        let (width, minimum) = match additional {
            0..=23 => return Ok(u64::from(additional)),
            24 => (1, 24),
            25 => (2, 0x100),
            26 => (4, 0x1_0000),
            27 => (8, 0x1_0000_0000),
            31 => return Err(Error::new(ErrorKind::NonCanonicalEncoding)),
            _ => return Err(Error::new(ErrorKind::UnsupportedType)),
        };
        let mut bytes = [0; 8];
        bytes[8 - width..].copy_from_slice(self.take(width)?);
        let argument = u64::from_be_bytes(bytes);
        if argument < minimum {
            return Err(Error::new(ErrorKind::NonCanonicalEncoding));
        }
        Ok(argument)
    }

    fn payload(&mut self, argument: u64, text: bool) -> Result<&'a [u8], Error> {
        let (limit, maximum) = if text {
            (LimitKind::TextBytes, self.accounting.limits.max_text_bytes)
        } else {
            (
                LimitKind::ByteStringBytes,
                self.accounting.limits.max_byte_string_bytes,
            )
        };
        let len = length(argument, limit, maximum)?;
        self.accounting.payload(len, text)?;
        self.take(len)
    }

    fn text(&mut self, argument: u64) -> Result<&'a str, Error> {
        let payload_start = self.position;
        let bytes = self.payload(argument, true)?;
        std::str::from_utf8(bytes).map_err(|error| {
            Error::new(ErrorKind::InvalidUtf8).at(payload_start + error.valid_up_to())
        })
    }

    fn children(&self, argument: u64, depth: usize, map: bool) -> Result<(usize, usize), Error> {
        let len = length(
            argument,
            LimitKind::CollectionEntries,
            self.accounting.limits.max_collection_entries,
        )?;
        self.accounting.collection(len)?;
        // Check the minimum child count before allocation or descent. Map pairs
        // require two values. This is a preflight, not an upfront reservation.
        let children = if map {
            add(
                len,
                len,
                self.accounting.limits.max_total_values,
                LimitKind::TotalValues,
            )?
        } else {
            len
        };
        self.accounting.ensure_values(children)?;
        if children > self.input.len() - self.position {
            return Err(Error::new(ErrorKind::UnexpectedEnd).at(self.input.len()));
        }
        let child_depth = if len == 0 {
            depth
        } else {
            add(depth, 1, self.accounting.limits.max_depth, LimitKind::Depth)?
        };
        Ok((len, child_depth))
    }

    fn array(&mut self, argument: u64, depth: usize) -> Result<Value, Error> {
        let (len, child_depth) = self.children(argument, depth, false)?;
        let mut values = Vec::new();
        for _ in 0..len {
            let value = self.value(child_depth)?;
            push(&mut values, value, len)?;
        }
        Ok(Value::Array(values))
    }

    fn map(&mut self, argument: u64, depth: usize) -> Result<Value, Error> {
        let (len, child_depth) = self.children(argument, depth, true)?;
        let mut entries = Vec::new();
        let mut previous: Option<&[u8]> = None;
        for _ in 0..len {
            let key_start = self.position;
            let key = self.key(child_depth).map_err(|error| error.at(key_start))?;
            let encoded_key = &self.input[key_start..self.position];
            if let Some(previous) = previous {
                match previous.cmp(encoded_key) {
                    Ordering::Less => (),
                    Ordering::Equal => {
                        return Err(Error::new(ErrorKind::DuplicateMapKey).at(key_start));
                    }
                    Ordering::Greater => {
                        return Err(Error::new(ErrorKind::MapKeyOutOfOrder).at(key_start));
                    }
                }
            }
            previous = Some(encoded_key);
            // Validate order before allocating an owned key. The input slice
            // remains borrowed while its associated value is decoded.
            let value = self.value(child_depth)?;
            let owned_key = own_text(key).map_err(|error| error.at(key_start))?;
            push(&mut entries, (owned_key, value), len)?;
        }
        Ok(Value::Map(Map::from_canonical_entries(entries)))
    }

    fn key(&mut self, depth: usize) -> Result<&'a str, Error> {
        self.accounting.enter(depth)?;
        let initial = self.take(1)?[0];
        if initial >> 5 != 3 {
            return Err(Error::new(ErrorKind::UnsupportedType));
        }
        let argument = self.argument(initial & 31)?;
        self.text(argument)
    }

    fn simple(&mut self, additional: u8) -> Result<Value, Error> {
        let value = match additional {
            20 => return Ok(Value::Bool(false)),
            21 => return Ok(Value::Bool(true)),
            22 => return Ok(Value::Null),
            24 => {
                let simple = self.take(1)?[0];
                return Err(Error::new(if (20..=22).contains(&simple) {
                    ErrorKind::NonCanonicalEncoding
                } else {
                    ErrorKind::UnsupportedType
                }));
            }
            25 => {
                let mut bytes = [0; 2];
                bytes.copy_from_slice(self.take(2)?);
                f16::from_bits(u16::from_be_bytes(bytes)).to_f64_const()
            }
            26 => {
                let mut bytes = [0; 4];
                bytes.copy_from_slice(self.take(4)?);
                f64::from(f32::from_bits(u32::from_be_bytes(bytes)))
            }
            27 => {
                let mut bytes = [0; 8];
                bytes.copy_from_slice(self.take(8)?);
                f64::from_bits(u64::from_be_bytes(bytes))
            }
            _ => return Err(Error::new(ErrorKind::UnsupportedType)),
        };
        if !value.is_finite() {
            return Err(Error::new(ErrorKind::NonFiniteFloat));
        }
        if value == 0.0 && value.is_sign_negative() {
            return Err(Error::new(ErrorKind::NonCanonicalEncoding));
        }
        if additional > 25 && f16::from_f64_const(value).to_f64_const().to_bits() == value.to_bits()
        {
            return Err(Error::new(ErrorKind::NonCanonicalEncoding));
        }
        if additional == 27 && f64::from(value as f32).to_bits() == value.to_bits() {
            return Err(Error::new(ErrorKind::NonCanonicalEncoding));
        }
        Ok(Value::Float(FiniteFloat::new(value)?))
    }
}

fn length(argument: u64, limit: LimitKind, maximum: usize) -> Result<usize, Error> {
    let len = usize::try_from(argument)
        .map_err(|_| Error::new(ErrorKind::LimitExceeded { limit, maximum }))?;
    check(len, maximum, limit)?;
    Ok(len)
}

fn allocation(_: std::collections::TryReserveError) -> Error {
    Error::new(ErrorKind::AllocationFailed)
}

fn own_text(text: &str) -> Result<String, Error> {
    let mut owned = String::new();
    owned.try_reserve_exact(text.len()).map_err(allocation)?;
    owned.push_str(text);
    Ok(owned)
}

/// Grow only for completed children, never reserve an advertised collection in
/// full. Requested capacity is at most twice visited entries, capped by length.
fn push<T>(values: &mut Vec<T>, value: T, maximum: usize) -> Result<(), Error> {
    if values.len() == values.capacity() {
        let needed = add(values.len(), 1, maximum, LimitKind::CollectionEntries)?;
        let target = needed.max(values.capacity().saturating_mul(2).min(maximum));
        values
            .try_reserve_exact(target - values.len())
            .map_err(allocation)?;
    }
    values.push(value);
    Ok(())
}
