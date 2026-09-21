use super::{Error, ErrorKind, LimitKind};

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
        for kind in [LimitKind::DocumentBytes, LimitKind::Depth] {
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
