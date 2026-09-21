use super::accounting::{Accounting, add};
use super::{Error, ErrorKind, LimitKind, Limits, Value};

/// Encodes exactly one value using the HTLK deterministic CBOR profile.
///
/// Maps use canonical key order, floats use the shortest exact width, and
/// integers remain distinct from floats. All headers and map keys count toward
/// the byte budget. No partial output is returned on failure.
///
/// # Errors
/// Returns [`ErrorKind::InvalidLimits`] for unsupported configuration,
/// [`ErrorKind::LimitExceeded`] for an exhausted ceiling, or
/// [`ErrorKind::AllocationFailed`] if output storage cannot be reserved.
///
/// # Examples
/// ```
/// use htlk_executable::cbor::{encode, Limits, Value};
/// assert_eq!(encode(&Value::Integer(24), &Limits::default())?, [0x18, 0x18]);
/// # Ok::<(), htlk_executable::cbor::Error>(())
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
            Value::Null | Value::Bool(_) | Value::Integer(_) | Value::Float(_) => {
                self.scalar(value)
            }
            Value::Text(value) => self.string(value.as_bytes(), true),
            Value::Bytes(value) => self.string(value, false),
            Value::Array(values) => {
                let child_depth = self.child_depth(depth, !values.is_empty())?;
                self.header(4, values.len() as u64)?;
                for value in values {
                    self.value(value, child_depth)?;
                }
                Ok(())
            }
            Value::Map(values) => {
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
        let (header, len) = header(if text { 3 } else { 2 }, bytes.len() as u64);
        // Account for header plus full payload before allocating either.
        self.append(&[&header[..len], bytes])
    }

    fn scalar(&mut self, value: &Value) -> Result<(), Error> {
        // Only fixed-size scalars enter this function. Let cbor2's Serde
        // serializer choose preferred integer and lossless float widths.
        let mut scratch = [0; 9];
        let bytes = cbor2::to_slice(value, &mut scratch)
            .expect("HTLK scalar serialization fits nine bytes");
        self.append(&[bytes])
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
    let len = cbor2::to_slice(&argument, &mut bytes)
        .expect("a u64 header fits nine bytes")
        .len();
    // All definite-length headers share the unsigned argument encoding.
    bytes[0] |= major << 5;
    (bytes, len)
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
