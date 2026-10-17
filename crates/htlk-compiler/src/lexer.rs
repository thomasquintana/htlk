//! Bounded lexical analysis with exact decoded literal semantics.
use crate::{
    SourceLocation, SourceSpan,
    ast::Located,
    diagnostic::{Result, fail},
    executable::{JsonDocument, cbor::Limits},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    Word(String),
    String(String),
    Number(String),
    Regex(String, String),
    Symbol(String),
}
pub(crate) fn lex(
    source: &str,
    location: SourceLocation,
    max_tokens: usize,
    limits: &Limits,
) -> Result<Vec<Located<Token>>> {
    let mut result = Vec::new();
    let mut p = 0;
    let b = source.as_bytes();
    while p < b.len() {
        let start = p;
        if matches!(b[p], b' ' | b'\t' | b'\r' | b'\n') {
            p += 1;
            continue;
        }
        if source[p..].starts_with("--[[") {
            let Some(end) = source[p + 4..].find("]]") else {
                return Err(at("unterminated block comment", location, p, b.len()));
            };
            p += end + 6;
            continue;
        }
        if source[p..].starts_with("--") {
            while p < b.len() && b[p] != b'\r' && b[p] != b'\n' {
                p += 1;
            }
            continue;
        }
        let token = if b[p] == b'"' {
            let triple = source[p..].starts_with("\"\"\"");
            p += if triple { 3 } else { 1 };
            let mut encoded = String::from("\"");
            loop {
                if p >= b.len() {
                    return Err(at("unterminated string", location, start, p));
                }
                if (!triple && b[p] == b'"') || (triple && source[p..].starts_with("\"\"\"")) {
                    p += if triple { 3 } else { 1 };
                    break;
                }
                let ch = source[p..].chars().next().expect("in bounds");
                if ch == '\\' {
                    encoded.push(ch);
                    p += 1;
                    if p == b.len() {
                        return Err(at("unterminated escape", location, start, p));
                    }
                    let c = source[p..].chars().next().expect("in bounds");
                    encoded.push(c);
                    p += c.len_utf8();
                } else {
                    match ch {
                        '\n' if triple => encoded.push_str("\\n"),
                        '\r' if triple => encoded.push_str("\\r"),
                        '\t' if triple => encoded.push_str("\\t"),
                        '"' if triple => encoded.push_str("\\\""),
                        _ => encoded.push(ch),
                    }
                    p += ch.len_utf8();
                }
            }
            encoded.push('"');
            let doc = JsonDocument::new(encoded.as_bytes(), limits)
                .map_err(|e| at(&e.to_string(), location.clone(), start, p))?;
            let crate::executable::cbor::Value::Text(s) = doc.value() else {
                unreachable!()
            };
            Token::String(s.clone())
        } else if b[p] == b'/' {
            p += 1;
            let mut pattern = String::new();
            let mut ended = false;
            while p < b.len() {
                let ch = source[p..].chars().next().expect("in bounds");
                p += ch.len_utf8();
                if ch == '/' {
                    ended = true;
                    break;
                }
                if ch == '\r' || ch == '\n' {
                    return Err(at("line ending in regex", location, start, p));
                }
                if ch == '\\' {
                    if p == b.len() {
                        break;
                    }
                    let c = source[p..].chars().next().expect("in bounds");
                    p += c.len_utf8();
                    if c == '\r' || c == '\n' {
                        return Err(at("line ending in regex", location, start, p));
                    }
                    if c != '/' {
                        pattern.push('\\');
                    }
                    pattern.push(c);
                } else {
                    pattern.push(ch);
                }
            }
            if !ended {
                return Err(at("unterminated regex", location, start, p));
            }
            let flags_start = p;
            while p < b.len() && b[p].is_ascii_alphabetic() {
                p += 1;
            }
            let flags = &source[flags_start..p];
            if flags.chars().any(|c| !"ims".contains(c))
                || flags.chars().any(|c| flags.matches(c).count() != 1)
            {
                return Err(at("invalid or repeated regex flag", location, start, p));
            }
            Token::Regex(pattern, flags.into())
        } else if b[p].is_ascii_alphabetic() {
            p += 1;
            while p < b.len() && (b[p].is_ascii_alphanumeric() || b[p] == b'_') {
                p += 1;
            }
            Token::Word(source[start..p].into())
        } else if b[p].is_ascii_digit()
            || (b[p] == b'-' && b.get(p + 1).is_some_and(u8::is_ascii_digit))
        {
            if b[p] == b'-' {
                p += 1;
            }
            if b[p] == b'0' {
                p += 1;
            } else {
                while p < b.len() && b[p].is_ascii_digit() {
                    p += 1;
                }
            }
            if b.get(p) == Some(&b'.') && b.get(p + 1).is_some_and(u8::is_ascii_digit) {
                p += 1;
                while p < b.len() && b[p].is_ascii_digit() {
                    p += 1;
                }
            }
            if matches!(b.get(p), Some(b'e' | b'E')) {
                p += 1;
                if matches!(b.get(p), Some(b'+' | b'-')) {
                    p += 1;
                }
                let digits = p;
                while p < b.len() && b[p].is_ascii_digit() {
                    p += 1;
                }
                if digits == p {
                    return Err(at("missing exponent", location, start, p));
                }
            }
            if b.get(p)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                return Err(at("invalid numeric token", location, start, p));
            }
            Token::Number(source[start..p].into())
        } else {
            let two = b
                .get(p..p + 2)
                .and_then(|s| std::str::from_utf8(s).ok())
                .filter(|s| matches!(*s, "==" | "!=" | "<=" | ">="));
            if let Some(two) = two {
                p += 2;
                Token::Symbol(two.into())
            } else if b"{}()[]=,.&@<>".contains(&b[p]) {
                p += 1;
                Token::Symbol(source[start..p].into())
            } else {
                return Err(at(
                    "invalid source character",
                    location,
                    start,
                    start + source[start..].chars().next().unwrap().len_utf8(),
                ));
            }
        };
        if result.len() == max_tokens {
            return Err(at("token limit exceeded", location, start, p));
        }
        result.push(Located {
            value: token,
            span: SourceSpan {
                source: location.clone(),
                start_byte: start,
                end_byte: p,
            },
        });
    }
    Ok(result)
}
pub(crate) fn at(
    message: &str,
    source: SourceLocation,
    start_byte: usize,
    end_byte: usize,
) -> crate::CompilationFailure {
    let mut error = fail("C_SYNTAX", message);
    error.diagnostics[0].location = crate::DiagnosticLocation::SourceSpan(SourceSpan {
        source,
        start_byte,
        end_byte,
    });
    error
}
