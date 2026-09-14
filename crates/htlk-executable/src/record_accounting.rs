//! Shared pre-allocation accounting for canonical executable record conversion.

use half::f16;
use htlk_cbor::{FiniteFloat, LimitKind, Limits};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EncodingLimitError {
    pub(crate) limit: LimitKind,
    pub(crate) maximum: usize,
}

pub(crate) struct RecordAccounting<'a> {
    limits: &'a Limits,
    values: usize,
    payload: usize,
    bytes: usize,
}

impl<'a> RecordAccounting<'a> {
    pub(crate) fn new(limits: &'a Limits) -> Result<Self, htlk_cbor::Error> {
        limits.validate()?;
        Ok(Self {
            limits,
            values: 0,
            payload: 0,
            bytes: 0,
        })
    }

    fn enter(&mut self, depth: usize) -> Result<(), EncodingLimitError> {
        check(depth, self.limits.max_depth, LimitKind::Depth)?;
        self.values = add(
            self.values,
            1,
            self.limits.max_total_values,
            LimitKind::TotalValues,
        )?;
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

    pub(crate) fn collection(
        &mut self,
        len: usize,
        depth: usize,
    ) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        check(
            len,
            self.limits.max_collection_entries,
            LimitKind::CollectionEntries,
        )?;
        self.bytes(header_size(len as u64))
    }

    pub(crate) fn text(&mut self, text: &str, depth: usize) -> Result<(), EncodingLimitError> {
        self.enter(depth)?;
        check(text.len(), self.limits.max_text_bytes, LimitKind::TextBytes)?;
        self.payload = add(
            self.payload,
            text.len(),
            self.limits.max_total_payload_bytes,
            LimitKind::TotalPayloadBytes,
        )?;
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
        check(
            value.len(),
            self.limits.max_byte_string_bytes,
            LimitKind::ByteStringBytes,
        )?;
        self.payload = add(
            self.payload,
            value.len(),
            self.limits.max_total_payload_bytes,
            LimitKind::TotalPayloadBytes,
        )?;
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
    fn conversion_arithmetic_is_checked() {
        assert_eq!(
            add(usize::MAX, 1, usize::MAX, LimitKind::DocumentBytes),
            Err(EncodingLimitError {
                limit: LimitKind::DocumentBytes,
                maximum: usize::MAX
            })
        );
        assert_eq!(
            add(usize::MAX - 1, 1, usize::MAX, LimitKind::TotalValues).unwrap(),
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
            assert_eq!(accounting.values, 1);
            assert_eq!(accounting.payload, 0);
        }
    }
}
