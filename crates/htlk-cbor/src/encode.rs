use half::f16;

use crate::accounting::{Accounting, add};
use crate::{Error, ErrorKind, LimitKind, Limits, Value};

/// Encodes exactly one value using the HTLK deterministic CBOR profile.
///
/// Maps use canonical key order, floats use the shortest exact width, and
/// integers remain distinct from floats. Both the root and every map key count
/// toward the value budget. No partial output is returned on failure.
///
/// # Errors
/// Returns [`ErrorKind::InvalidLimits`] for unsupported configuration,
/// [`ErrorKind::LimitExceeded`] for an exhausted ceiling, or
/// [`ErrorKind::AllocationFailed`] if output storage cannot be reserved.
///
/// # Examples
/// ```
/// use htlk_cbor::{encode, Limits, Value};
/// assert_eq!(encode(&Value::Integer(24), &Limits::default())?, [0x18, 0x18]);
/// # Ok::<(), htlk_cbor::Error>(())
/// ```
pub fn encode(value: &Value, limits: &Limits) -> Result<Vec<u8>, Error> {
    limits.validate()?;
    let mut encoder = Encoder {
        output: Vec::new(),
        accounting: Accounting::new(limits),
    };
    encoder.value(value, 0)?;
    Ok(encoder.output)
}

struct Encoder<'a> {
    output: Vec<u8>,
    accounting: Accounting<'a>,
}

impl Encoder<'_> {
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), Error> {
        self.accounting.enter(depth)?;
        match value {
            Value::Null => self.append(&[&[0xf6]]),
            Value::Bool(value) => self.append(&[&[if *value { 0xf5 } else { 0xf4 }]]),
            Value::Integer(value) => {
                // Complement before conversion avoids negating i64::MIN.
                let (major, argument) = if *value >= 0 {
                    (0, *value as u64)
                } else {
                    (1, (!*value) as u64)
                };
                self.header(major, argument)
            }
            Value::Float(value) => self.float(value.get()),
            Value::Text(value) => self.string(value.as_bytes(), true),
            Value::Bytes(value) => self.string(value, false),
            Value::Array(values) => {
                self.accounting.collection(values.len())?;
                let child_depth = self.child_depth(depth, !values.is_empty())?;
                self.header(4, values.len() as u64)?;
                for value in values {
                    self.value(value, child_depth)?;
                }
                Ok(())
            }
            Value::Map(values) => {
                self.accounting.collection(values.len())?;
                let child_depth = self.child_depth(depth, !values.is_empty())?;
                self.header(5, values.len() as u64)?;
                for (key, value) in values.iter() {
                    self.accounting.enter(child_depth)?;
                    self.string(key.as_bytes(), true)?;
                    self.value(value, child_depth)?;
                }
                Ok(())
            }
        }
    }

    fn child_depth(&self, depth: usize, has_children: bool) -> Result<usize, Error> {
        if has_children {
            // Checked before the next recursive call, not after entering it.
            add(depth, 1, self.accounting.limits.max_depth, LimitKind::Depth)
        } else {
            Ok(depth)
        }
    }

    fn header(&mut self, major: u8, argument: u64) -> Result<(), Error> {
        let (bytes, len) = header(major, argument);
        self.append(&[&bytes[..len]])
    }

    fn string(&mut self, bytes: &[u8], text: bool) -> Result<(), Error> {
        self.accounting.payload(bytes.len(), text)?;
        let (header, len) = header(if text { 3 } else { 2 }, bytes.len() as u64);
        // Account for header plus full payload before allocating either.
        self.append(&[&header[..len], bytes])
    }

    fn float(&mut self, value: f64) -> Result<(), Error> {
        // Software conversion avoids dependence on optional CPU intrinsics.
        // Compare round-tripped bits so no narrowing conversion loses precision.
        let half = f16::from_f64_const(value);
        if half.to_f64_const().to_bits() == value.to_bits() {
            return self.append(&[&[0xf9], &half.to_bits().to_be_bytes()]);
        }
        let single = value as f32;
        if f64::from(single).to_bits() == value.to_bits() {
            self.append(&[&[0xfa], &single.to_bits().to_be_bytes()])
        } else {
            self.append(&[&[0xfb], &value.to_bits().to_be_bytes()])
        }
    }

    fn append(&mut self, parts: &[&[u8]]) -> Result<(), Error> {
        let maximum = self.accounting.limits.max_document_bytes;
        let mut needed = self.output.len();
        for part in parts {
            needed = add(needed, part.len(), maximum, LimitKind::DocumentBytes)?;
        }
        if needed > self.output.capacity() {
            // Amortized growth, with every requested capacity bounded by the
            // document ceiling. The allocator may supply more than requested.
            let target = needed.max(self.output.capacity().saturating_mul(2).min(maximum));
            self.output
                .try_reserve_exact(target - self.output.len())
                .map_err(|_| Error::new(ErrorKind::AllocationFailed))?;
        }
        for part in parts {
            self.output.extend_from_slice(part);
        }
        Ok(())
    }
}

/// Shortest header, using fixed stack scratch even for the largest argument.
fn header(major: u8, argument: u64) -> ([u8; 9], usize) {
    let mut bytes = [0; 9];
    let (additional, width) = match argument {
        0..=23 => (argument as u8, 0),
        24..=0xff => (24, 1),
        0x100..=0xffff => (25, 2),
        0x1_0000..=0xffff_ffff => (26, 4),
        _ => (27, 8),
    };
    bytes[0] = major << 5 | additional;
    bytes[1..1 + width].copy_from_slice(&argument.to_be_bytes()[8 - width..]);
    (bytes, 1 + width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_length_headers_need_no_large_allocation() {
        for major in 2..=5 {
            for (argument, expected) in [
                (0xffff_ffff, vec![major << 5 | 26, 0xff, 0xff, 0xff, 0xff]),
                (0x1_0000_0000, vec![major << 5 | 27, 0, 0, 0, 1, 0, 0, 0, 0]),
            ] {
                let (bytes, len) = header(major, argument);
                assert_eq!(&bytes[..len], expected);
            }
            let (bytes, len) = header(major, u64::MAX);
            assert_eq!(len, 9);
            assert_eq!(bytes[0], major << 5 | 27);
            assert_eq!(&bytes[1..], &[0xff; 8]);
        }
    }

    #[test]
    fn failed_document_reservation_leaves_output_unallocated() {
        let limits = Limits {
            max_document_bytes: 2,
            ..Limits::default()
        };
        let mut encoder = Encoder {
            output: Vec::new(),
            accounting: Accounting::new(&limits),
        };
        assert!(encoder.string(b"abc", true).is_err());
        assert_eq!(encoder.output.capacity(), 0);
    }
}
