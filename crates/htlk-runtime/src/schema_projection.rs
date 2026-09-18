//! Schema-root-preserving native projection and declared optional-field presence.
use crate::{
    EvaluationError as Error, EvaluationMeter, EvaluationResult, EvaluationValue as V,
    EvaluatorLimits, JsonDocument, NativeSchemas, PathStep, digest::Digest,
};
use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use std::io::{self, Write};

use htlk_analyzer::DECLARED_FIELDS;

/// A projection tied to a prescribed schema root and its immutable offline native
/// validators. No context-free subschema digest is fabricated or serialized.
pub struct SchemaProjection<'a> {
    schemas: &'a NativeSchemas,
    schema: Digest,
    path: Vec<PathStep>,
    limits: Limits,
}
impl<'a> SchemaProjection<'a> {
    /// Admits a literal projection path at a compiled prescribed schema root.
    ///
    /// # Errors
    /// Returns missing root, invalid path representation, or path/codec limits.
    pub fn new(
        schemas: &'a NativeSchemas,
        schema: Digest,
        path: &[PathStep],
        limits: &Limits,
    ) -> Result<Self, Error> {
        limits.validate()?;
        if !schemas.has_schema_type(&schema) {
            return Err(Error::UnresolvedType);
        }
        if path.len() > limits.max_collection_entries {
            return Err(Error::Limit("schema projection path"));
        }
        let mut bytes = 0usize;
        for step in path {
            let size = match step {
                PathStep::Field(s) => {
                    if s.len() > limits.max_text_bytes {
                        return Err(Error::Limit("schema projection path"));
                    }
                    s.len()
                }
                PathStep::Index(i) => {
                    if *i > i64::MAX as u64 {
                        return Err(Error::InvalidProjection);
                    }
                    8
                }
            };
            bytes = bytes
                .checked_add(size)
                .and_then(|n| n.checked_add(9))
                .ok_or(Error::Limit("schema projection path"))?;
            if bytes > limits.max_document_bytes {
                return Err(Error::Limit("schema projection path"));
            }
        }
        Ok(Self {
            schemas,
            schema,
            path: path.to_vec(),
            limits: limits.clone(),
        })
    }
    /// The original root's raw JCS identity.
    pub fn schema(&self) -> Digest {
        self.schema
    }
    /// Literal path relative to the original instance root.
    pub fn path(&self) -> &[PathStep] {
        &self.path
    }
    /// Validates the complete root before projecting its original native values.
    /// Permitted present dynamic fields remain accessible. Missing declared
    /// optional fields yield absence; other missing keys and invalid indices fail.
    ///
    /// # Errors
    /// Returns invalid instance/projection, absence misuse, engine failure, or
    /// bounded evaluator/derived-evaluation-output failures. Native validation
    /// retains its documented non-fuel-metered execution contract.
    pub fn evaluate(
        &self,
        value: &Value,
        policy: &EvaluatorLimits,
    ) -> Result<EvaluationResult, Error> {
        let mut meter = EvaluationMeter::new(policy, &self.limits)?;
        let value = project(self.schemas, self.schema, value, &self.path, &mut meter)?;
        Ok(EvaluationResult {
            value,
            usage: meter.usage(),
        })
    }
}

pub(crate) fn project(
    schemas: &NativeSchemas,
    schema: Digest,
    root: &Value,
    path: &[PathStep],
    meter: &mut EvaluationMeter<'_>,
) -> Result<V, Error> {
    meter.inspect(root)?;
    let instance = crate::runtime_type::json_value(root, meter)?.ok_or(Error::OperandType)?;
    if !schemas.validate_schema_document(&schema, &instance, meter.program_limits())? {
        return Err(Error::OperandType);
    }
    let mut value = root;
    let mut pointer = String::new();
    for (index, step) in path.iter().enumerate() {
        meter.visit(1)?;
        match (value, step) {
            (Value::Map(map), PathStep::Field(name)) => {
                meter.charge(
                    (name.len() as u64).saturating_mul(u64::from(
                        (usize::BITS - map.len().leading_zeros()).max(1),
                    )),
                )?;
                let Some(child) = map.get(name) else {
                    if !declared(schemas, schema, &instance, &pointer, name, meter)? {
                        return Err(Error::InvalidProjection);
                    }
                    return if index + 1 == path.len() {
                        Ok(V::Absent)
                    } else {
                        Err(Error::AbsentOperand)
                    };
                };
                value = child;
                append_pointer(&mut pointer, name, meter)?;
            }
            (Value::Array(array), PathStep::Index(index)) => {
                value = usize::try_from(*index)
                    .ok()
                    .and_then(|i| array.get(i))
                    .ok_or(Error::InvalidProjection)?;
                append_pointer(&mut pointer, &index.to_string(), meter)?;
            }
            _ => return Err(Error::InvalidProjection),
        }
    }
    Ok(V::Present(meter.copy_value(value)?))
}
fn append_pointer(
    pointer: &mut String,
    token: &str,
    meter: &mut EvaluationMeter<'_>,
) -> Result<(), Error> {
    let size = token
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_add(pointer.len()))
        .ok_or(Error::Limit("schema projection path"))?;
    if size > meter.program_limits().max_text_bytes {
        return Err(Error::Limit("schema projection path"));
    }
    meter.charge(size as u64)?;
    pointer
        .try_reserve(size - pointer.len())
        .map_err(|_| Error::AllocationFailed)?;
    pointer.push('/');
    for c in token.chars() {
        match c {
            '~' => pointer.push_str("~0"),
            '/' => pointer.push_str("~1"),
            _ => pointer.push(c),
        }
    }
    Ok(())
}
fn declared(
    schemas: &NativeSchemas,
    schema: Digest,
    instance: &JsonDocument,
    pointer: &str,
    field: &str,
    meter: &mut EvaluationMeter<'_>,
) -> Result<bool, Error> {
    let limits = meter.program_limits().clone();
    let mut output = ProjectionOutput {
        bytes: Vec::new(),
        maximum: limits.max_document_bytes,
        meter,
        failure: None,
    };
    let result = schemas.projection_evaluation(&schema, instance, &mut output, &limits);
    if let Some(error) = output.failure {
        return Err(error);
    }
    result?;
    let tree = JsonDocument::new(&output.bytes, &limits)
        .map_err(|_| Error::Limit("schema projection evaluation"))?;
    let mut pending = vec![(tree.value(), false, "")];
    while let Some((value, parent_schema, parent_path)) = pending.pop() {
        meter.visit(1)?;
        let Value::Map(node) = value else {
            return Err(Error::UnresolvedType);
        };
        let Some(Value::Text(evaluation_path)) = node.get("evaluationPath") else {
            return Err(Error::UnresolvedType);
        };
        meter.charge(evaluation_path.len() as u64)?;
        // Predicate schemas constrain selection, not projected declarations.
        // Failed record alternatives can still declare optional union members.
        if parent_schema
            && evaluation_path
                .strip_prefix(parent_path)
                .is_some_and(|suffix| matches!(suffix, "/not" | "/if"))
        {
            continue;
        }
        let annotations = match node
            .get("annotations")
            .or_else(|| node.get("droppedAnnotations"))
        {
            Some(Value::Map(annotations)) => Some(annotations),
            _ => None,
        };
        let fields = annotations.and_then(|annotations| annotations.get(DECLARED_FIELDS));
        let is_schema = fields.is_some();
        if let Some(Value::Text(location)) = node.get("instanceLocation") {
            meter.charge(location.len() as u64)?;
            if location == pointer
                && let Some(Value::Array(fields)) = fields
            {
                for name in fields {
                    meter.visit(1)?;
                    if let Value::Text(name) = name {
                        meter.charge(name.len() as u64)?;
                        if name == field {
                            return Ok(true);
                        }
                    }
                }
            }
        }
        if let Some(Value::Array(children)) = node.get("details") {
            if pending.len().saturating_add(children.len()) > limits.max_total_values {
                return Err(Error::Limit("schema projection evaluation"));
            }
            pending
                .try_reserve(children.len())
                .map_err(|_| Error::AllocationFailed)?;
            pending.extend(
                children
                    .iter()
                    .map(|child| (child, is_schema, evaluation_path.as_str())),
            );
        }
    }
    Ok(false)
}
struct ProjectionOutput<'a, 'b> {
    bytes: Vec<u8>,
    maximum: usize,
    meter: &'a mut EvaluationMeter<'b>,
    failure: Option<Error>,
}
impl Write for ProjectionOutput<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| {
            if self.bytes.len().saturating_add(bytes.len()) > self.maximum {
                return Err(Error::Limit("schema projection evaluation"));
            }
            self.meter.charge(bytes.len() as u64)?;
            self.bytes
                .try_reserve(bytes.len())
                .map_err(|_| Error::AllocationFailed)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        })();
        result.map_err(|e| {
            self.failure = Some(e);
            io::Error::other("schema projection output limit")
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
