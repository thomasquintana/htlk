use htlk_cbor::{LimitKind, Limits, Map, Value};
use sha2::{Digest as _, Sha256};

use super::{ExpressionContext, ExpressionError, wire};
use crate::digest::Digest;
use crate::{Identifier, Port};

/// One literal text segment or statically named substitution slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplatePart {
    /// Exact text, with whitespace and Unicode spelling preserved.
    Text(String),
    /// A parameter's local name. Repeated slots remain in order.
    Slot(Identifier),
}

/// Immutable canonical prompt-template metadata, not an LLM invocation.
///
/// Parameters are exposed in UTF-8 name order. Their names exactly match the
/// distinct slots and their types are string, integer, or Boolean. Rendering
/// arguments and presence/type compatibility at a use site require the graph
/// verifier/evaluator; this type does not execute or substitute text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptTemplate {
    parameters: Vec<(Identifier, Port)>,
    parts: Vec<TemplatePart>,
}

impl PromptTemplate {
    /// Normalizes authored parts by removing empty text and joining adjacent text.
    ///
    /// # Errors
    /// Returns record/type/name/coverage errors or resource/allocation failures.
    pub fn new(
        parameters: Vec<(Identifier, Port)>,
        parts: Vec<TemplatePart>,
        limits: &Limits,
    ) -> Result<Self, ExpressionError> {
        let authored = Self { parameters, parts };
        let value = authored.to_value(limits)?;
        let result = Self::parse(&value, limits, true)?;
        result.to_value(limits)?;
        Ok(result)
    }
    /// Borrows parameters in decoded UTF-8 name order.
    pub fn parameters(&self) -> &[(Identifier, Port)] {
        &self.parameters
    }
    /// Borrows canonical text/slot parts in semantic order.
    pub fn parts(&self) -> &[TemplatePart] {
        &self.parts
    }

    /// Produces the canonical template record with bounded conversion.
    ///
    /// # Errors
    /// Returns invalid parameter-type, limit, or allocation failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, ExpressionError> {
        let mut b = wire::Builder::new(ExpressionContext::Eval, limits)?;
        b.accounting.collection(2, 0)?;
        let parameters_key = b.string("parameters", 1)?;
        b.accounting.collection(self.parameters.len(), 1)?;
        let mut parameters = Vec::new();
        for (name, port) in &self.parameters {
            let key = b.string(name.as_str(), 2)?;
            wire::push(&mut parameters, (key, b.parameter(port, 2)?))?;
        }
        let parameters = Value::Map(Map::try_from_entries(parameters)?);
        let parts_key = b.string("parts", 1)?;
        b.accounting.collection(self.parts.len(), 1)?;
        let mut parts = Vec::new();
        for part in &self.parts {
            let value = match part {
                TemplatePart::Text(s) => b.text(s, 2)?,
                TemplatePart::Slot(name) => {
                    let mut v = b.tagged("slot", 2, 2)?;
                    wire::push(&mut v, b.text(name.as_str(), 3)?)?;
                    Value::Array(v)
                }
            };
            wire::push(&mut parts, value)?;
        }
        Ok(Value::Map(Map::try_from_entries([
            (parameters_key, parameters),
            (parts_key, Value::Array(parts)),
        ])?))
    }

    /// Reads canonical template data without repairing parts.
    ///
    /// # Errors
    /// Returns codec, schema, type, canonicality, or coverage failures.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, ExpressionError> {
        htlk_cbor::encode(value, limits)?;
        Self::parse(value, limits, false)
    }
    /// Encodes one canonical template record.
    ///
    /// # Errors
    /// Returns resource or allocation failures.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, ExpressionError> {
        Ok(htlk_cbor::encode(&self.to_value(limits)?, limits)?)
    }
    /// Decodes exactly one canonical template record.
    ///
    /// # Errors
    /// Returns codec, schema, type, canonicality, or coverage failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, ExpressionError> {
        Self::parse(&htlk_cbor::decode(bytes, limits)?, limits, false)
    }
    /// Computes record_digest("template", record) from the compiler specification:
    /// raw `htlk.template/0.1\n` prefix followed by canonical record bytes.
    ///
    /// # Errors
    /// Returns resource or allocation failures during canonical encoding.
    pub fn digest(&self, limits: &Limits) -> Result<Digest, ExpressionError> {
        let bytes = self.encode(limits)?;
        let mut hash = Sha256::new();
        hash.update(b"htlk.template/0.1\n");
        hash.update(bytes);
        Ok(Digest::from_bytes(hash.finalize().into()))
    }

    fn parse(value: &Value, limits: &Limits, normalize: bool) -> Result<Self, ExpressionError> {
        let Value::Map(map) = value else {
            return Err(ExpressionError::InvalidShape("template record"));
        };
        if map.len() != 2 || map.get("parameters").is_none() || map.get("parts").is_none() {
            return Err(ExpressionError::InvalidShape("template fields"));
        }
        let Some(Value::Map(params)) = map.get("parameters") else {
            return Err(ExpressionError::InvalidShape("template parameters"));
        };
        let mut parameters = Vec::new();
        for (key, value) in params.iter() {
            wire::push(
                &mut parameters,
                (
                    key.parse::<Identifier>()?,
                    wire::parse_parameter(value, limits)?,
                ),
            )?;
        }
        parameters.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut parts = Vec::new();
        for value in wire::array(map.get("parts").expect("field checked"), "template parts")? {
            match value {
                Value::Text(s) => {
                    if !normalize
                        && (s.is_empty() || matches!(parts.last(), Some(TemplatePart::Text(_))))
                    {
                        return Err(ExpressionError::NonCanonical("template literal segments"));
                    }
                    if s.is_empty() {
                        continue;
                    }
                    if let Some(TemplatePart::Text(previous)) = parts.last_mut() {
                        let maximum = limits.max_text_bytes;
                        let len = previous.len().checked_add(s.len()).ok_or(
                            ExpressionError::LimitExceeded {
                                limit: LimitKind::TextBytes,
                                maximum,
                            },
                        )?;
                        if len > maximum {
                            return Err(ExpressionError::LimitExceeded {
                                limit: LimitKind::TextBytes,
                                maximum,
                            });
                        }
                        previous
                            .try_reserve_exact(s.len())
                            .map_err(wire::allocation)?;
                        previous.push_str(s);
                    } else {
                        wire::push(&mut parts, TemplatePart::Text(wire::owned(s)?))?;
                    }
                }
                Value::Array(v)
                    if v.len() == 2 && matches!(&v[0], Value::Text(tag) if tag == "slot") =>
                {
                    wire::push(&mut parts, TemplatePart::Slot(wire::name(&v[1])?))?;
                }
                _ => return Err(ExpressionError::InvalidShape("template part")),
            }
        }
        let mut slots = Vec::new();
        for part in &parts {
            if let TemplatePart::Slot(name) = part {
                wire::push(&mut slots, name)?;
            }
        }
        slots.sort_unstable();
        slots.dedup();
        if slots.len() != parameters.len()
            || slots
                .iter()
                .zip(&parameters)
                .any(|(slot, (name, _))| *slot != name)
        {
            return Err(ExpressionError::TemplateParameterMismatch);
        }
        Ok(Self { parameters, parts })
    }
}
