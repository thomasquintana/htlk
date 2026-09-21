//! Conservative representation-family refinement at original schema locations.
//! These hints do not replace complete native validation or perform schema subtyping.
use crate::{
    JsonPointer, NativeSchemaError as Error, PathStep, PrimitiveType as P, SchemaCatalog,
    SchemaResourceError, TypeContext, ValueType as T, ValueTypeKind as K,
};
use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use std::collections::BTreeSet;
const NULL: u8 = 1;
const BOOL: u8 = 2;
const NUMBER: u8 = 4;
const STRING: u8 = 8;
const ARRAY: u8 = 16;
const OBJECT: u8 = 32;
const ANY: u8 = 63;
pub(crate) fn projection_type(
    catalog: &SchemaCatalog,
    uri: &str,
    path: &[PathStep],
    limits: &Limits,
) -> Result<T, Error> {
    let pointer = JsonPointer::new("", limits).map_err(SchemaResourceError::from)?;
    let mut walker = Walker {
        catalog,
        limits,
        bytes: 0,
        active: BTreeSet::new(),
    };
    let mask = walker.walk(uri, &pointer, path, 0)?;
    // An empty present-value family can still describe a legitimately absent
    // optional property (for example properties: {x:false}); preserve its checks.
    if mask == ANY || mask == 0 {
        return Ok(T::primitive(P::Json));
    }
    let mut types = Vec::new();
    for (bit, primitive) in [(NULL, P::Null), (BOOL, P::Boolean), (STRING, P::String)] {
        if mask & bit != 0 {
            types.push(T::primitive(primitive));
        }
    }
    if mask & NUMBER != 0 {
        types.extend([T::primitive(P::Integer), T::primitive(P::Float)]);
    }
    if mask & ARRAY != 0 {
        types.push(T::new(
            K::List(Box::new(T::primitive(P::Json))),
            TypeContext::Value,
            limits,
        )?);
    }
    if mask & OBJECT != 0 {
        types.push(T::new(
            K::Map(Box::new(T::primitive(P::Json))),
            TypeContext::Value,
            limits,
        )?);
    }
    if types.len() == 1 {
        Ok(types.pop().expect("one type"))
    } else {
        Ok(T::new(K::Union(types), TypeContext::Value, limits)?)
    }
}
struct Walker<'a> {
    catalog: &'a SchemaCatalog,
    limits: &'a Limits,
    bytes: usize,
    active: BTreeSet<(String, String, usize)>,
}
impl Walker<'_> {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        // Include a one-byte visit marker in the derived byte budget.
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .and_then(|n| n.checked_add(1))
            .ok_or(Error::AdmissionLimit)?;
        if self.bytes > self.limits.max_document_bytes {
            return Err(Error::AdmissionLimit);
        }
        Ok(())
    }
    fn child(&mut self, pointer: &JsonPointer, token: &str) -> Result<JsonPointer, Error> {
        let parent = pointer.to_string();
        let size = parent
            .len()
            .checked_add(token.len().checked_mul(2).ok_or(Error::AdmissionLimit)?)
            .and_then(|n| n.checked_add(1))
            .ok_or(Error::AdmissionLimit)?;
        if size > self.limits.max_document_bytes {
            return Err(Error::AdmissionLimit);
        }
        self.charge(size)?;
        JsonPointer::new(
            &format!("{parent}/{}", token.replace('~', "~0").replace('/', "~1")),
            self.limits,
        )
        .map_err(SchemaResourceError::from)
        .map_err(Into::into)
    }
    fn walk(
        &mut self,
        uri: &str,
        pointer: &JsonPointer,
        path: &[PathStep],
        depth: usize,
    ) -> Result<u8, Error> {
        // A cycle/deeper reference proof is uncertain, not a false assertion.
        // Runtime validation retains the exact original resource context.
        if depth >= self.limits.max_depth {
            return Ok(ANY);
        }
        let text = pointer.to_string();
        self.charge(uri.len().saturating_add(text.len()))?;
        let key = (uri.to_owned(), text, path.len());
        if !self.active.insert(key.clone()) {
            return Ok(ANY);
        }
        let result = self.at(uri, pointer, path, depth);
        self.active.remove(&key);
        result
    }
    #[inline(never)]
    fn at(
        &mut self,
        uri: &str,
        pointer: &JsonPointer,
        path: &[PathStep],
        depth: usize,
    ) -> Result<u8, Error> {
        let document = self.catalog.document(uri).ok_or(Error::UnknownRoot)?;
        let value = pointer
            .resolve(document)
            .map_err(SchemaResourceError::from)?;
        let schema = match value {
            Value::Bool(true) => return Ok(ANY),
            Value::Bool(false) => return Ok(0),
            Value::Map(map) => map,
            _ => return Err(Error::InvalidSchema),
        };
        let mut own = type_mask(schema.get("type"));
        if let Some(value) = schema.get("const") {
            own &= value_mask(value);
        }
        if let Some(Value::Array(values)) = schema.get("enum") {
            let mut allowed = 0;
            for value in values {
                self.charge(0)?;
                allowed |= value_mask(value);
            }
            own &= allowed;
        }
        let mut result = if let Some((step, rest)) = path.split_first() {
            match step {
                PathStep::Field(name) if own & OBJECT != 0 => {
                    self.charge(name.len())?;
                    if let Some(Value::Map(properties)) = schema.get("properties")
                        && properties.get(name).is_some()
                    {
                        let parent = self.child(pointer, "properties")?;
                        let child = self.child(&parent, name)?;
                        self.walk(uri, &child, rest, depth + 1)?
                    } else if schema.get("patternProperties").is_some() {
                        ANY
                    } else if schema.get("additionalProperties").is_some() {
                        let child = self.child(pointer, "additionalProperties")?;
                        self.walk(uri, &child, rest, depth + 1)?
                    } else {
                        ANY
                    }
                }
                PathStep::Index(index) if own & ARRAY != 0 => {
                    if let Some(Value::Array(prefix)) = schema.get("prefixItems")
                        && usize::try_from(*index).is_ok_and(|i| i < prefix.len())
                    {
                        let parent = self.child(pointer, "prefixItems")?;
                        let child = self.child(&parent, &index.to_string())?;
                        self.walk(uri, &child, rest, depth + 1)?
                    } else if schema.get("items").is_some() {
                        let child = self.child(pointer, "items")?;
                        self.walk(uri, &child, rest, depth + 1)?
                    } else {
                        ANY
                    }
                }
                _ => 0,
            }
        } else {
            own
        };
        if let Some(Value::Text(reference)) = schema.get("$ref") {
            self.charge(reference.len())?;
            let target = self.catalog.resolve(uri, pointer, reference, self.limits)?;
            result &= self.walk(target.retrieval_uri(), target.pointer(), path, depth + 1)?;
        }
        // A dynamic reference may rebind. Its initial target cannot establish a
        // universal family constraint, so leave it to native checked validation.
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(Value::Array(children)) = schema.get(keyword) {
                let parent = self.child(pointer, keyword)?;
                let mut combined = if keyword == "allOf" { ANY } else { 0 };
                for index in 0..children.len() {
                    let child = self.child(&parent, &index.to_string())?;
                    let mask = self.walk(uri, &child, path, depth + 1)?;
                    if keyword == "allOf" {
                        combined &= mask;
                    } else {
                        combined |= mask;
                    }
                }
                result &= combined;
            }
        }
        Ok(result)
    }
}
fn value_mask(value: &Value) -> u8 {
    match value {
        Value::Null => NULL,
        Value::Bool(_) => BOOL,
        Value::Integer(_) | Value::Float(_) => NUMBER,
        Value::Text(_) => STRING,
        Value::Array(_) => ARRAY,
        Value::Map(_) => OBJECT,
        Value::Bytes(_) => 0,
    }
}
fn type_mask(value: Option<&Value>) -> u8 {
    match value {
        Some(Value::Text(name)) => match name.as_str() {
            "null" => NULL,
            "boolean" => BOOL,
            "integer" | "number" => NUMBER,
            "string" => STRING,
            "array" => ARRAY,
            "object" => OBJECT,
            _ => ANY,
        },
        Some(Value::Array(values)) => values
            .iter()
            .fold(0, |mask, value| mask | type_mask(Some(value))),
        _ => ANY,
    }
}
