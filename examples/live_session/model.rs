//! Small strict configuration/JSON helpers, with no credential serialization.

use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Map, Value},
};
use std::{error::Error, path::Path};

pub(super) type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(super) fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    let Value::Map(map) = value else {
        return Err("object required".into());
    };
    map.get(key).ok_or_else(|| "required field absent".into())
}

pub(super) fn text(value: &Value) -> Result<&str> {
    if let Value::Text(text) = value {
        Ok(text)
    } else {
        Err("text required".into())
    }
}

pub(super) fn object(fields: Vec<(&str, Value)>) -> Result<Value> {
    Ok(Value::Map(Map::try_from_entries(
        fields.into_iter().map(|(key, value)| (key.into(), value)),
    )?))
}

pub(super) fn read(path: &Path) -> Result<JsonDocument> {
    Ok(JsonDocument::new(
        &std::fs::read(path)?,
        &Limits::default(),
    )?)
}

pub(super) fn write(path: &Path, value: &Value) -> Result<()> {
    let json = JsonDocument::from_value(value, &Limits::default())?;
    std::fs::write(path, json.as_bytes())?;
    Ok(())
}

pub(super) fn quote(text: &str) -> Result<String> {
    Ok(std::str::from_utf8(
        JsonDocument::from_value(&Value::Text(text.into()), &Limits::default())?.as_bytes(),
    )?
    .to_owned())
}

pub(super) fn emit(value: &Value) -> Result<()> {
    println!(
        "{}",
        std::str::from_utf8(JsonDocument::from_value(value, &Limits::default())?.as_bytes())?
    );
    Ok(())
}
