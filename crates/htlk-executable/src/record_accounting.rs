//! Logical canonical encoded-byte and depth accounting for record conversion
//! and aggregate budgeting by consumers, including analyzer environments and
//! runtime metadata.

use crate::cbor as htlk_cbor;
use half::f16;
use htlk_cbor::{FiniteFloat, LimitKind, Limits};

/// An encoded representation exhausted a logical codec resource.
///
/// Display provides human-facing prose; use the fields for structured handling.
/// No submitted data is retained or included in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodingLimitError {
    /// Exhausted resource.
    pub limit: LimitKind,
    /// Configured maximum.
    pub maximum: usize,
}

impl std::fmt::Display for EncodingLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // LimitKind is non-exhaustive for downstream crates, but is defined in
        // this crate. Exhaustive matching here requires prose for any new kind.
        match self.limit {
            LimitKind::DocumentBytes => write!(
                f,
                "The encoded representation exceeds the configured maximum of {} bytes.",
                self.maximum
            ),
            LimitKind::Depth => write!(
                f,
                "The encoded representation exceeds the configured maximum depth of {}.",
                self.maximum
            ),
        }
    }
}
impl std::error::Error for EncodingLimitError {}

/// Incremental logical canonical encoded-byte cost and depth checks.
///
/// Supports record conversion and aggregate budgeting by consumers. Encoded-byte
/// cost is not exact heap usage. Callers must supply correct depths (root zero,
/// each child one deeper, including map keys) and account for every relevant
/// header, key, and value exactly once. This object does not independently
/// validate that the described representation is complete or structurally valid.
///
/// Individual components can be checked before allocation. Alternatively,
/// [`Self::value`] can charge an already constructed, separately bounded child;
/// it does not retroactively prevent that child's allocation.
///
/// After any error, abandon this accounting operation; charges are not rolled back.
pub struct RecordAccounting<'a> {
    limits: &'a Limits,
    bytes: usize,
}

impl<'a> RecordAccounting<'a> {
    /// Charges an already constructed, separately bounded child and all its
    /// descendants at their actual depths in the containing representation.
    ///
    /// Includes the child's headers, keys, and values; do not charge them again.
    /// This check does not retroactively prevent the child's allocation. Callers
    /// must abandon aggregate construction on failure.
    pub fn value(
        &mut self,
        value: &htlk_cbor::Value,
        depth: usize,
    ) -> Result<(), EncodingLimitError> {
        use htlk_cbor::Value;
        match value {
            Value::Text(v) => self.text(v, depth),
            Value::Bytes(v) => self.byte_string(v, depth),
            Value::Integer(v) => self.integer(*v, depth),
            Value::Float(v) => self.float(*v, depth),
            Value::Bool(_) => self.boolean(depth),
            Value::Null => self.null(depth),
            Value::Array(values) => {
                self.collection(values.len(), depth)?;
                for value in values {
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Map(values) => {
                self.collection(values.len(), depth)?;
                for (key, value) in values.iter() {
                    self.text(key, depth + 1)?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
        }
    }

    /// Starts fresh counters after validating the supplied codec limits.
    ///
    /// # Errors
    /// Returns unsupported limit configuration.
    pub fn new(limits: &'a Limits) -> Result<Self, htlk_cbor::Error> {
        limits.validate()?;
        Ok(Self { limits, bytes: 0 })
    }

    fn enter(&self, depth: usize) -> Result<(), EncodingLimitError> {
        check(depth, self.limits.max_depth, LimitKind::Depth)?;
        Ok(())
    }

    fn bytes(&mut self, bytes: usize) -> Result<(), EncodingLimitError> {
        self.bytes = add(
            self.bytes,
            bytes,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        Ok(())
    }

    /// Charges the collection header and preflights one byte per supplied entry
    /// against the remaining byte budget, without charging those child bytes yet.
    ///
    /// `len` is the number of array elements or map entries. Callers must charge
    /// children separately at their actual encoded sizes and depths. For maps,
    /// this is a weaker minimum-size preflight: both keys and values must
    /// subsequently be charged individually.
    pub fn collection(&mut self, len: usize, depth: usize) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        self.bytes(header_size(len as u64))?;
        add(
            self.bytes,
            len,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        Ok(())
    }

    /// Accounts for an exact UTF-8 string, including its canonical header.
    pub fn text(&mut self, text: &str, depth: usize) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        self.bytes(header_size(text.len() as u64))?;
        self.bytes(text.len())
    }

    pub(crate) fn boolean(&mut self, depth: usize) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        self.bytes(1)
    }

    pub(crate) fn null(&mut self, depth: usize) -> Result<(), EncodingLimitError> {
        self.boolean(depth)
    }

    pub(crate) fn byte_string(
        &mut self,
        value: &[u8],
        depth: usize,
    ) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        self.bytes(header_size(value.len() as u64))?;
        self.bytes(value.len())
    }

    pub(crate) fn float(
        &mut self,
        value: FiniteFloat,
        depth: usize,
    ) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        let value = value.get();
        let size = if f16::from_f64_const(value).to_f64_const().to_bits() == value.to_bits() {
            3
        } else if f64::from(value as f32).to_bits() == value.to_bits() {
            5
        } else {
            9
        };
        self.bytes(size)
    }

    pub(crate) fn integer(&mut self, value: i64, depth: usize) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        let argument = if value >= 0 {
            value as u64
        } else {
            (!value) as u64
        };
        self.bytes(header_size(argument))
    }
}

fn check(value: usize, maximum: usize, limit: LimitKind) -> Result<(), EncodingLimitError> {
    if value > maximum {
        return Err(EncodingLimitError { limit, maximum });
    }
    Ok(())
}

fn add(
    current: usize,
    amount: usize,
    maximum: usize,
    limit: LimitKind,
) -> Result<usize, EncodingLimitError> {
    let total = current
        .checked_add(amount)
        .ok_or(EncodingLimitError { limit, maximum })?;
    check(total, maximum, limit)?;
    Ok(total)
}

fn header_size(argument: u64) -> usize {
    match argument {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=0xffff_ffff => 5,
        _ => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_errors_use_resource_specific_prose() {
        for (limit, maximum, expected) in [
            (
                LimitKind::DocumentBytes,
                1024,
                "The encoded representation exceeds the configured maximum of 1024 bytes.",
            ),
            (
                LimitKind::Depth,
                64,
                "The encoded representation exceeds the configured maximum depth of 64.",
            ),
        ] {
            assert_eq!(EncodingLimitError { limit, maximum }.to_string(), expected);
        }
    }

    fn assert_float_accounting(value: f64, expected_size: usize) {
        // Use the public model for both paths, including negative-zero normalization.
        let value = FiniteFloat::new(value).unwrap();
        let size = htlk_cbor::encode(&htlk_cbor::Value::Float(value), &Limits::default())
            .unwrap()
            .len();
        assert_eq!(size, expected_size, "{value:?}");

        let exact = Limits {
            max_document_bytes: size,
            ..Limits::default()
        };
        let mut accounting = RecordAccounting::new(&exact).unwrap();
        accounting.float(value, 0).unwrap();
        assert_eq!(accounting.bytes, size, "{value:?}");

        let short = Limits {
            max_document_bytes: size - 1,
            ..exact
        };
        assert_eq!(
            RecordAccounting::new(&short).unwrap().float(value, 0),
            Err(EncodingLimitError {
                limit: LimitKind::DocumentBytes,
                maximum: size - 1,
            }),
            "{value:?}"
        );
    }

    #[test]
    fn every_finite_binary16_charge_matches_encoder() {
        for bits in 0..=u16::MAX {
            let value = f16::from_bits(bits);
            if value.is_finite() {
                assert_float_accounting(value.to_f64(), 3);
            }
        }
    }

    #[test]
    fn wider_float_charges_match_encoder() {
        // Binary32 values just beyond binary16 precision/range, plus binary32
        // subnormal boundaries, the smallest normal, and the finite maximum.
        for value in [
            1.0 + f64::from(f32::EPSILON),
            1.0 - f64::from(f32::EPSILON) / 2.0,
            2f64.powi(-25),
            65536.0,
            f64::from(f32::from_bits(1)),
            f64::from(f32::from_bits(f32::MIN_POSITIVE.to_bits() - 1)),
            f64::from(f32::MIN_POSITIVE),
            f64::from(f32::MAX),
        ] {
            for signed in [value, -value] {
                assert_float_accounting(signed, 5);
            }
        }

        // Binary64-only precision, subnormal boundaries, and finite extremes.
        for value in [
            1.0 + 2f64.powi(-24),
            1.1,
            f64::from_bits(1),
            f64::from_bits(f64::MIN_POSITIVE.to_bits() - 1),
            f64::MIN_POSITIVE,
            f64::MAX,
        ] {
            for signed in [value, -value] {
                assert_float_accounting(signed, 9);
            }
        }

        // Neighbors of exactly representable shorter values must not be
        // undercharged after rounding to binary16 or binary32.
        for value in [1.0, 65504.0, 2f64.powi(-24), f64::from(f32::MAX)] {
            for bits in [value.to_bits() - 1, value.to_bits() + 1] {
                for signed in [f64::from_bits(bits), -f64::from_bits(bits)] {
                    assert_float_accounting(signed, 9);
                }
            }
        }
    }

    #[test]
    fn conversion_arithmetic_is_checked() {
        assert_eq!(
            add(usize::MAX, 1, usize::MAX, LimitKind::DocumentBytes),
            Err(EncodingLimitError {
                limit: LimitKind::DocumentBytes,
                maximum: usize::MAX
            })
        );
        assert_eq!(
            add(usize::MAX - 1, 1, usize::MAX, LimitKind::DocumentBytes).unwrap(),
            usize::MAX
        );
    }

    #[test]
    fn integer_accounting_matches_codec_boundaries() {
        let limits = Limits::default();
        for value in [
            i64::MIN,
            -65537,
            -65536,
            -257,
            -256,
            -25,
            -24,
            -1,
            0,
            23,
            24,
            255,
            256,
            65535,
            65536,
            0xffff_ffff,
            0x1_0000_0000,
            i64::MAX,
        ] {
            let mut accounting = RecordAccounting::new(&limits).unwrap();
            accounting.integer(value, 0).unwrap();
            assert_eq!(
                accounting.bytes,
                htlk_cbor::encode(&htlk_cbor::Value::Integer(value), &limits)
                    .unwrap()
                    .len()
            );
        }
    }
}
