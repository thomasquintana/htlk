//! RFC 6570 syntax and variable discovery, without expansion or URI resolution.

use crate::DocumentError;
use htlk_cbor::Value;
use iri_string::{
    spec::UriSpec,
    template::{
        UriTemplateStr,
        context::{Context, Visitor},
    },
};
use std::fmt::{self, Write as _};

/// Expands an RFC 6570 template with its exact required string argument record.
/// Expansion uses native Rust code and bounded output; no template script runs.
///
/// # Errors
/// Returns invalid syntax/arguments or input/output resource failures.
pub fn expand_uri_template(
    template: &str,
    arguments: &Value,
    limits: &Limits,
) -> Result<String, DocumentError> {
    limits.validate()?;
    for (maximum, limit) in [
        (limits.max_text_bytes, LimitKind::TextBytes),
        (limits.max_document_bytes, LimitKind::DocumentBytes),
        (limits.max_total_payload_bytes, LimitKind::TotalPayloadBytes),
    ] {
        if template.len() > maximum {
            return Err(DocumentError::LimitExceeded { limit, maximum });
        }
    }
    htlk_cbor::encode(arguments, limits)?;
    let names = variables(template, limits)?;
    let Value::Map(map) = arguments else {
        return Err(DocumentError::McpInterfaceMismatch);
    };
    if map.len() != names.len()
        || map
            .iter()
            .any(|(name, value)| !names.contains(name) || !matches!(value, Value::Text(_)))
    {
        return Err(DocumentError::McpInterfaceMismatch);
    }
    struct Args<'a>(&'a htlk_cbor::Map);
    impl Context for Args<'_> {
        fn visit<V: Visitor>(&self, visitor: V) -> V::Result {
            match self.0.get(visitor.var_name().as_str()) {
                Some(Value::Text(value)) => visitor.visit_string(value),
                _ => visitor.visit_undefined(),
            }
        }
    }
    let parsed = UriTemplateStr::new(template).map_err(|_| invalid(0))?;
    let args = Args(map);
    let expanded = parsed.expand::<UriSpec, _>(&args).map_err(|_| invalid(0))?;
    let mut output = Expansion {
        text: String::new(),
        limits,
        error: None,
    };
    if write!(output, "{expanded}").is_err() {
        return Err(output.error.unwrap_or_else(|| invalid(0)));
    }
    Ok(output.text)
}
struct Expansion<'a> {
    text: String,
    limits: &'a Limits,
    error: Option<DocumentError>,
}
impl fmt::Write for Expansion<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let result =
            (|| {
                let length = self.text.len().checked_add(text.len()).ok_or(
                    DocumentError::LimitExceeded {
                        limit: LimitKind::DocumentBytes,
                        maximum: self.limits.max_document_bytes,
                    },
                )?;
                for (maximum, limit) in [
                    (self.limits.max_text_bytes, LimitKind::TextBytes),
                    (self.limits.max_document_bytes, LimitKind::DocumentBytes),
                    (
                        self.limits.max_total_payload_bytes,
                        LimitKind::TotalPayloadBytes,
                    ),
                ] {
                    if length > maximum {
                        return Err(DocumentError::LimitExceeded { limit, maximum });
                    }
                }
                self.text
                    .try_reserve(text.len())
                    .map_err(|_| DocumentError::AllocationFailed)?;
                self.text.push_str(text);
                Ok(())
            })();
        if let Err(error) = result {
            self.error = Some(error);
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}
use htlk_cbor::{LimitKind, Limits};
use std::collections::BTreeSet;

pub(crate) fn variables<'a>(
    template: &'a str,
    limits: &Limits,
) -> Result<BTreeSet<&'a str>, DocumentError> {
    let mut names = BTreeSet::new();
    let mut mentions = 0usize;
    let mut pos = 0;
    while pos < template.len() {
        let c = template[pos..].chars().next().expect("remaining character");
        if c == '{' {
            let start = pos + 1;
            let end = template[start..]
                .find('}')
                .map(|n| start + n)
                .ok_or_else(|| invalid(pos))?;
            let mut expr = &template[start..end];
            let mut offset = start;
            if expr.starts_with(['+', '#', '.', '/', ';', '?', '&']) {
                expr = &expr[1..];
                offset += 1;
            }
            for spec in expr.split(',') {
                let name = if let Some((name, prefix)) = spec.split_once(':') {
                    if prefix.is_empty()
                        || prefix.len() > 4
                        || !matches!(prefix.as_bytes()[0], b'1'..=b'9')
                        || !prefix.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err(invalid(offset));
                    }
                    name
                } else {
                    spec.strip_suffix('*').unwrap_or(spec)
                };
                if !valid_name(name) {
                    return Err(invalid(offset));
                }
                mentions = mentions
                    .checked_add(1)
                    .ok_or(DocumentError::LimitExceeded {
                        limit: LimitKind::TotalValues,
                        maximum: limits.max_total_values,
                    })?;
                if mentions > limits.max_total_values {
                    return Err(DocumentError::LimitExceeded {
                        limit: LimitKind::TotalValues,
                        maximum: limits.max_total_values,
                    });
                }
                if !names.contains(name) && names.len() >= limits.max_collection_entries {
                    return Err(DocumentError::LimitExceeded {
                        limit: LimitKind::CollectionEntries,
                        maximum: limits.max_collection_entries,
                    });
                }
                names.insert(name);
                offset += spec.len() + 1;
            }
            pos = end + 1;
        } else if c == '%' {
            if !percent(template.as_bytes(), pos) {
                return Err(invalid(pos));
            }
            pos += 3;
        } else {
            if !literal(c) {
                return Err(invalid(pos));
            }
            pos += c.len_utf8();
        }
    }
    Ok(names)
}
fn invalid(offset: usize) -> DocumentError {
    DocumentError::InvalidUriTemplate { offset }
}
fn percent(bytes: &[u8], pos: usize) -> bool {
    bytes
        .get(pos + 1..pos + 3)
        .is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit))
}
fn valid_name(name: &str) -> bool {
    name.split('.').all(|part| {
        if part.is_empty() {
            return false;
        }
        let b = part.as_bytes();
        let mut pos = 0;
        while pos < b.len() {
            if b[pos].is_ascii_alphanumeric() || b[pos] == b'_' {
                pos += 1;
            } else if b[pos] == b'%' && percent(b, pos) {
                pos += 3;
            } else {
                return false;
            }
        }
        true
    })
}
fn literal(c: char) -> bool {
    let n = c as u32;
    matches!(n, 0x21 | 0x23..=0x24 | 0x26 | 0x28..=0x3b | 0x3d | 0x3f..=0x5b |
        0x5d | 0x5f | 0x61..=0x7a | 0x7e | 0xa0..=0xd7ff | 0xe000..=0xfdcf |
        0xfdf0..=0xffef | 0xe1000..=0xefffd)
        || ((0x10000..=0xdfffd).contains(&n) || (0xf0000..=0x10fffd).contains(&n))
            && n & 0xffff <= 0xfffd
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc_operators_modifiers_and_exact_names() {
        for op in ["", "+", "#", ".", "/", ";", "?", "&"] {
            let t = format!(
                "https://example.test/{{{op}Name:9999,a.b*,%61,a,%6A,%6a,Name}}/{{Name:1}}"
            );
            let names = variables(&t, &Limits::default()).unwrap();
            assert_eq!(
                names.into_iter().collect::<Vec<_>>(),
                ["%61", "%6A", "%6a", "Name", "a", "a.b"]
            );
        }
        for t in [
            "",
            "relative/path",
            "https://例.test/é",
            "%7bnot_a_variable%7D",
            "\u{e000}",
            "\u{10fffd}",
        ] {
            assert!(variables(t, &Limits::default()).unwrap().is_empty());
        }
    }
    #[test]
    fn malformed_syntax_and_reserved_extensions_fail() {
        for t in [
            "{",
            "}",
            "{}",
            "{?}",
            "{{x}}",
            "{x,}",
            "{,x}",
            "{x,,y}",
            "{x..y}",
            "{x.}",
            "{x-y}",
            "{é}",
            "{x:0}",
            "{x:01}",
            "{x:10000}",
            "{x:}",
            "{x:1*}",
            "{x**}",
            "{x%}",
            "{x%0g}",
            "{!x}",
            "{@x}",
            "{=x}",
            "{|x}",
            "{(x)}",
            "a b",
            "a'b",
            "%",
            "%gg",
            "\u{7f}",
            "\u{9f}",
            "\u{fdd0}",
            "\u{ffff}",
            "\u{1ffff}",
            "\u{e0000}",
        ] {
            assert!(
                matches!(
                    variables(t, &Limits::default()),
                    Err(DocumentError::InvalidUriTemplate { .. })
                ),
                "{t:?}"
            );
        }
    }
    #[test]
    fn variable_discovery_has_independent_logical_bounds() {
        let l = Limits {
            max_collection_entries: 1,
            max_total_values: 2,
            ..Limits::default()
        };
        assert!(variables("{x}{x}", &l).is_ok());
        assert!(matches!(
            variables("{x}{y}", &l),
            Err(DocumentError::LimitExceeded {
                limit: LimitKind::CollectionEntries,
                ..
            })
        ));
        assert!(matches!(
            variables("{x}{x}{x}", &l),
            Err(DocumentError::LimitExceeded {
                limit: LimitKind::TotalValues,
                ..
            })
        ));
    }
}
