//! Offline preparation of declared-field annotations for schema projections.
use htlk_executable::cbor::{Limits, Value};

/// Backend annotation identifying declared fields at the original instance root.
pub const DECLARED_FIELDS: &str = "x-htlk-private-declared-fields";

#[derive(Default)]
pub(crate) struct ProjectionAdmission {
    bytes: usize,
}
impl ProjectionAdmission {
    fn charge(&mut self, bytes: usize, limits: &Limits) -> Result<(), crate::NativeSchemaError> {
        // Include one byte for each derived entry, even when its text is empty.
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .and_then(|n| n.checked_add(1))
            .ok_or(crate::NativeSchemaError::AdmissionLimit)?;
        if self.bytes > limits.max_document_bytes {
            return Err(crate::NativeSchemaError::AdmissionLimit);
        }
        Ok(())
    }
    pub(crate) fn fields(
        &mut self,
        catalog: &crate::SchemaCatalog,
        uri: &str,
        pointer: &crate::JsonPointer,
        limits: &Limits,
    ) -> Result<Vec<String>, crate::NativeSchemaError> {
        use crate::{NativeSchemaError as N, SchemaResourceError as R};
        use std::collections::BTreeSet;
        let document = catalog.document(uri).ok_or(N::UnknownRoot)?;
        let value = pointer.resolve(document).map_err(R::from)?;
        self.charge(pointer.to_string().len(), limits)?;
        let mut pending = vec![(uri, pointer.clone(), value)];
        let mut visited = BTreeSet::new();
        let mut fields = BTreeSet::new();
        while let Some((uri, pointer, value)) = pending.pop() {
            let text = pointer.to_string();
            self.charge(uri.len().saturating_add(text.len()), limits)?;
            if !visited.insert((uri, text)) {
                continue;
            }
            let Value::Map(schema) = value else {
                continue;
            };
            if let Some(Value::Map(properties)) = schema.get("properties") {
                for (name, _) in properties.iter() {
                    self.charge(name.len(), limits)?;
                    fields.insert(name.to_owned());
                }
            }
            for keyword in ["$ref", "$dynamicRef"] {
                if let Some(Value::Text(reference)) = schema.get(keyword) {
                    self.charge(reference.len(), limits)?;
                    let target = catalog.resolve(uri, &pointer, reference, limits)?;
                    self.charge(target.pointer().to_string().len(), limits)?;
                    pending.push((
                        target.retrieval_uri(),
                        target.pointer().clone(),
                        target.value(),
                    ));
                }
            }
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(Value::Array(children)) = schema.get(keyword) {
                    let parent = self.child(&pointer, keyword, limits)?;
                    for (index, value) in children.iter().enumerate() {
                        let child = self.child(&parent, &index.to_string(), limits)?;
                        pending.push((uri, child, value));
                    }
                }
            }
            for keyword in ["then", "else"] {
                if let Some(value) = schema.get(keyword) {
                    pending.push((uri, self.child(&pointer, keyword, limits)?, value));
                }
            }
            if let Some(Value::Map(children)) = schema.get("dependentSchemas") {
                let parent = self.child(&pointer, "dependentSchemas", limits)?;
                for (name, value) in children.iter() {
                    pending.push((uri, self.child(&parent, name, limits)?, value));
                }
            }
        }
        Ok(fields.into_iter().collect())
    }
    fn child(
        &mut self,
        parent: &crate::JsonPointer,
        token: &str,
        limits: &Limits,
    ) -> Result<crate::JsonPointer, crate::NativeSchemaError> {
        let parent = parent.to_string();
        let size = token
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(parent.len()))
            .and_then(|n| n.checked_add(1))
            .ok_or(crate::NativeSchemaError::AdmissionLimit)?;
        if size > limits.max_document_bytes {
            return Err(crate::NativeSchemaError::AdmissionLimit);
        }
        self.charge(size, limits)?;
        let escaped = token.replace('~', "~0").replace('/', "~1");
        crate::JsonPointer::new(&format!("{parent}/{escaped}"), limits)
            .map_err(crate::SchemaResourceError::from)
            .map_err(Into::into)
    }
}
