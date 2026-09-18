use super::{Error, ErrorKind, LimitKind, Limits};

/// Per-operation usage checked against borrowed, reusable limits.
/// Callers perform checks before allocation/descent.
pub(crate) struct Accounting<'a> {
    pub(crate) limits: &'a Limits,
    total_values: usize,
    total_payload_bytes: usize,
}

impl<'a> Accounting<'a> {
    pub(crate) fn new(limits: &'a Limits) -> Self {
        Self {
            limits,
            total_values: 0,
            total_payload_bytes: 0,
        }
    }

    pub(crate) fn enter(&mut self, depth: usize) -> Result<(), Error> {
        check(depth, self.limits.max_depth, LimitKind::Depth)?;
        self.total_values = add(
            self.total_values,
            1,
            self.limits.max_total_values,
            LimitKind::TotalValues,
        )?;
        Ok(())
    }

    pub(crate) fn collection(&self, entries: usize) -> Result<(), Error> {
        check(
            entries,
            self.limits.max_collection_entries,
            LimitKind::CollectionEntries,
        )
    }

    /// Checks a lower bound for not-yet-visited children without charging them.
    /// Actual entry still charges each child exactly once.
    pub(crate) fn ensure_values(&self, additional: usize) -> Result<(), Error> {
        add(
            self.total_values,
            additional,
            self.limits.max_total_values,
            LimitKind::TotalValues,
        )?;
        Ok(())
    }

    pub(crate) fn payload(&mut self, bytes: usize, text: bool) -> Result<(), Error> {
        let (maximum, kind) = if text {
            (self.limits.max_text_bytes, LimitKind::TextBytes)
        } else {
            (
                self.limits.max_byte_string_bytes,
                LimitKind::ByteStringBytes,
            )
        };
        check(bytes, maximum, kind)?;
        self.total_payload_bytes = add(
            self.total_payload_bytes,
            bytes,
            self.limits.max_total_payload_bytes,
            LimitKind::TotalPayloadBytes,
        )?;
        Ok(())
    }
}

pub(crate) fn check(value: usize, maximum: usize, limit: LimitKind) -> Result<(), Error> {
    if value > maximum {
        return Err(exceeded(limit, maximum));
    }
    Ok(())
}

pub(crate) fn add(
    current: usize,
    amount: usize,
    maximum: usize,
    limit: LimitKind,
) -> Result<usize, Error> {
    let total = current
        .checked_add(amount)
        .ok_or_else(|| exceeded(limit, maximum))?;
    check(total, maximum, limit)?;
    Ok(total)
}

fn exceeded(limit: LimitKind, maximum: usize) -> Error {
    Error::new(ErrorKind::LimitExceeded { limit, maximum })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_overflow_is_a_limit_error() {
        for kind in [
            LimitKind::DocumentBytes,
            LimitKind::TotalValues,
            LimitKind::TotalPayloadBytes,
        ] {
            assert_eq!(
                add(usize::MAX, 1, usize::MAX, kind).unwrap_err().kind(),
                &ErrorKind::LimitExceeded {
                    limit: kind,
                    maximum: usize::MAX
                }
            );
            assert_eq!(
                add(usize::MAX - 1, 1, usize::MAX, kind).unwrap(),
                usize::MAX
            );
        }
    }
}
