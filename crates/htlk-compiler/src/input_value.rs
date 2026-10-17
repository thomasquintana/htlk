//! Closed input-record helpers; all JSON enters through duplicate-aware parsing.
use crate::diagnostic::{Result, fail};
use crate::executable::cbor::{Map, Value};

pub(crate) fn map(v: &Value) -> Result<&Map> {
    match v {
        Value::Map(m) => Ok(m),
        _ => Err(fail("C_SYNTAX", "expected object")),
    }
}
pub(crate) fn text(v: &Value) -> Result<&str> {
    match v {
        Value::Text(s) => Ok(s),
        _ => Err(fail("C_SYNTAX", "expected string")),
    }
}
pub(crate) fn array(v: &Value) -> Result<&[Value]> {
    match v {
        Value::Array(a) => Ok(a),
        _ => Err(fail("C_SYNTAX", "expected array")),
    }
}
pub(crate) fn field<'a>(m: &'a Map, name: &str) -> Result<&'a Value> {
    m.get(name)
        .ok_or_else(|| fail("C_SYNTAX", format!("missing field {name}")))
}
pub(crate) fn closed<'a>(v: &'a Value, required: &[&str], optional: &[&str]) -> Result<&'a Map> {
    let m = map(v)?;
    if let Some((name, _)) = m
        .iter()
        .find(|(k, _)| !required.contains(k) && !optional.contains(k))
    {
        return Err(fail("C_SYNTAX", format!("unknown field {name}")));
    }
    for name in required {
        field(m, name)?;
    }
    Ok(m)
}
