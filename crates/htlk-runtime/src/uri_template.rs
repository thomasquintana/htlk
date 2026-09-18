//! Bounded native RFC 6570 expansion.
use htlk_analyzer::LinkageError as DocumentError;
use htlk_executable::{
    cbor::{self, LimitKind, Limits, Value},
    uri_template_variables,
};
use iri_string::{
    spec::UriSpec,
    template::{
        UriTemplateStr,
        context::{Context, Visitor},
    },
};
use std::fmt::{self, Write as _};

/// Expands a template with its exact required string argument record.
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
    cbor::encode(arguments, limits)?;
    let names = uri_template_variables(template, limits)?;
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
    struct Args<'a>(&'a cbor::Map);
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
fn invalid(offset: usize) -> DocumentError {
    DocumentError::InvalidUriTemplate { offset }
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
