//! Extract an exact top-level value only after complete strict JSON admission.
//! This scanner establishes a byte span, never salvages malformed protocol ingress.
use super::*;

pub(super) fn member<'a>(
    bytes: &'a [u8],
    name: &str,
    limits: &Limits,
) -> Result<&'a [u8], RuntimeFailure> {
    let invalid = || fail(FailureCode::McpProtocol);
    let mut at = whitespace(bytes, 0);
    if bytes.get(at) != Some(&b'{') {
        return Err(invalid());
    }
    at += 1;
    loop {
        at = whitespace(bytes, at);
        if bytes.get(at) == Some(&b'}') {
            return Err(invalid());
        }
        let key_end = string_end(bytes, at)?;
        let key = JsonDocument::new(&bytes[at..key_end], limits).map_err(|_| invalid())?;
        let selected = key.value() == &Value::Text(name.into());
        at = whitespace(bytes, key_end);
        if bytes.get(at) != Some(&b':') {
            return Err(invalid());
        }
        at = whitespace(bytes, at + 1);
        let end = value_end(bytes, at)?;
        if selected {
            return Ok(&bytes[at..end]);
        }
        at = whitespace(bytes, end);
        if bytes.get(at) != Some(&b',') {
            return Err(invalid());
        }
        at += 1;
    }
}
fn whitespace(bytes: &[u8], mut at: usize) -> usize {
    while bytes
        .get(at)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        at += 1;
    }
    at
}
fn string_end(bytes: &[u8], start: usize) -> Result<usize, RuntimeFailure> {
    if bytes.get(start) != Some(&b'"') {
        return Err(fail(FailureCode::McpProtocol));
    }
    let mut at = start + 1;
    while let Some(byte) = bytes.get(at) {
        match byte {
            b'"' => return Ok(at + 1),
            b'\\' => at += 2,
            _ => at += 1,
        }
    }
    Err(fail(FailureCode::McpProtocol))
}
fn value_end(bytes: &[u8], start: usize) -> Result<usize, RuntimeFailure> {
    match bytes.get(start) {
        Some(b'"') => string_end(bytes, start),
        Some(b'{' | b'[') => {
            let mut depth = 0usize;
            let mut at = start;
            while let Some(byte) = bytes.get(at) {
                match byte {
                    b'"' => {
                        at = string_end(bytes, at)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth
                            .checked_sub(1)
                            .ok_or_else(|| fail(FailureCode::McpProtocol))?;
                        if depth == 0 {
                            return Ok(at + 1);
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
            Err(fail(FailureCode::McpProtocol))
        }
        Some(_) => {
            let mut at = start;
            while bytes
                .get(at)
                .is_some_and(|b| !matches!(b, b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n'))
            {
                at += 1;
            }
            if at == start {
                Err(fail(FailureCode::McpProtocol))
            } else {
                Ok(at)
            }
        }
        None => Err(fail(FailureCode::McpProtocol)),
    }
}
