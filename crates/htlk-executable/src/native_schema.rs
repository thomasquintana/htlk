//! Native JSON Schema validation with offline, context-preserving compilation.

use crate::digest::{Digest, hash_bytes};
use crate::{
    EngineIdentity, JsonDocument, JsonError, JsonPointer, MetadataError, SchemaCatalog,
    SchemaLocations, SchemaResourceError, SchemaResources,
};
use htlk_cbor::{Limits, Value};
use serde::Deserialize;
use serde_json::Value as Json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Supported native backend controls. These are not a whole-validation fuel or
/// deadline guarantee. The linear regex backend rejects unsupported ECMA features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeSchemaOptions {
    /// Maximum UTF-8 bytes in an individual schema regex pattern.
    pub max_pattern_bytes: usize,
    /// Native regex compiler size ceiling per pattern.
    pub max_regex_compiled_bytes: usize,
}
impl Default for NativeSchemaOptions {
    fn default() -> Self {
        Self {
            max_pattern_bytes: 64 * 1024,
            max_regex_compiled_bytes: 2 * 1024 * 1024,
        }
    }
}

struct Root {
    validator: jsonschema::Validator,
    digest: Digest,
    object_only: bool,
}

/// Ready-to-use native validators for a complete supplied schema catalog.
/// Original JCS documents retain their identities. Only private compiler copies
/// have resource URIs replaced with opaque keys to preserve HTLK URI resolution.
pub struct NativeSchemas {
    roots: BTreeMap<String, Root>,
    schema_types: BTreeMap<Digest, String>,
    catalog: SchemaCatalog,
}
impl NativeSchemas {
    /// Compiles all supplied schema documents without filesystem/network retrieval.
    /// Validates standard keyword shapes, supported vocabularies and regex limits.
    ///
    /// # Errors
    /// Returns invalid/unsupported schemas, unavailable references, options, or
    /// input/index limits. Native whole-validation instruction limits are not exposed.
    pub fn compile(
        catalog: &SchemaCatalog,
        options: NativeSchemaOptions,
        limits: &Limits,
    ) -> Result<Self, NativeSchemaError> {
        Self::compile_diagnostic(catalog, options, limits).map_err(|diagnostic| diagnostic.error)
    }
    /// Compiles with original schema-document/pointer locations on failures.
    ///
    /// # Errors
    /// Returns redacted schema/profile/resource errors and their location when known.
    pub fn compile_diagnostic(
        catalog: &SchemaCatalog,
        options: NativeSchemaOptions,
        limits: &Limits,
    ) -> Result<Self, NativeSchemaDiagnostic> {
        let mut location = None;
        Self::compile_at(catalog, options, limits, &mut location)
            .map_err(|error| NativeSchemaDiagnostic { location, error })
    }
    fn compile_at(
        catalog: &SchemaCatalog,
        options: NativeSchemaOptions,
        limits: &Limits,
        location: &mut Option<SchemaDiagnosticLocation>,
    ) -> Result<Self, NativeSchemaError> {
        if options.max_pattern_bytes == 0 || options.max_regex_compiled_bytes == 0 {
            return Err(NativeSchemaError::InvalidOptions);
        }
        let uris: Vec<_> = catalog.retrieval_uris().collect();
        catalog.reference_closure(&uris, limits)?;
        let mut documents = BTreeMap::new();
        let mut projection_admission = crate::schema_projection::ProjectionAdmission::default();
        let mut plan_size = PlanSize {
            total: 0,
            document: 0,
            limits,
        };
        for uri in &uris {
            let document = catalog
                .document(uri)
                .ok_or(SchemaResourceError::UnknownDocument)?;
            *location = Some(SchemaDiagnosticLocation {
                document: document.digest(),
                pointer: JsonPointer::new("", limits).map_err(SchemaResourceError::from)?,
            });
            let locations =
                SchemaLocations::new(document, limits).map_err(SchemaResourceError::from)?;
            let resources = SchemaResources::new(document, uri, limits)?;
            let mut native = parse(document, limits)?;
            for pointer in locations.pointers() {
                *location = Some(SchemaDiagnosticLocation {
                    document: document.digest(),
                    pointer: pointer.clone(),
                });
                let value = pointer
                    .resolve(document)
                    .map_err(SchemaResourceError::from)?;
                check_profile(value, options)?;
            }
            jsonschema::draft202012::meta::validate(&native).map_err(|error| {
                if let Ok(pointer) = JsonPointer::new(error.instance_path().as_str(), limits) {
                    *location = Some(SchemaDiagnosticLocation {
                        document: document.digest(),
                        pointer,
                    });
                }
                NativeSchemaError::InvalidSchema
            })?;
            for pointer in locations.pointers() {
                *location = Some(SchemaDiagnosticLocation {
                    document: document.digest(),
                    pointer: pointer.clone(),
                });
                let Some(map) = native
                    .pointer_mut(&pointer.to_string())
                    .and_then(Json::as_object_mut)
                else {
                    continue;
                };
                // Optional unknown annotations do not affect validation. Strip
                // them from compiler copies so legacy keyword extensions cannot
                // introduce resources absent from the standard location index.
                map.retain(|key, _| standard_keyword(key));
                // Private, non-asserting annotations preserve declared-field
                // presence through the native evaluator's reference/dynamic scope.
                // Authored unknown annotations were removed above, so they cannot
                // impersonate this marker. Original JCS bytes are unchanged.
                let fields = projection_admission
                    .fields(catalog, uri, pointer, limits)?
                    .into_iter()
                    .map(Json::String)
                    .collect();
                map.insert(
                    crate::schema_projection::DECLARED_FIELDS.into(),
                    Json::Array(fields),
                );
                if pointer.tokens().is_empty() || map.contains_key("$id") {
                    map.insert(
                        "$id".into(),
                        Json::String(internal_uri(
                            resources
                                .base_uri(pointer)
                                .ok_or(SchemaResourceError::UnknownLocation)?,
                        )),
                    );
                }
                for key in ["$ref", "$dynamicRef"] {
                    if let Some(Json::String(reference)) = map.get(key) {
                        let absolute = resources.reference_uri(pointer, reference, limits)?;
                        let (resource, fragment) =
                            absolute.split_once('#').unwrap_or((&absolute, ""));
                        let linked = if absolute.contains('#') {
                            format!("{}#{fragment}", internal_uri(resource))
                        } else {
                            internal_uri(resource)
                        };
                        map.insert(key.into(), Json::String(linked));
                    }
                }
            }
            plan_size.document = 0;
            serde_json::to_writer(&mut plan_size, &native)
                .map_err(|_| NativeSchemaError::CompilationInputLimit)?;
            documents.insert((*uri).to_owned(), native);
        }
        let mut registry = jsonschema::Registry::new()
            .draft(jsonschema::Draft::Draft202012)
            .retriever(NoRetrieval);
        *location = None;
        for (uri, schema) in &documents {
            registry = registry
                .add(internal_uri(uri), schema)
                .map_err(|_| NativeSchemaError::InvalidSchema)?;
        }
        let registry = registry
            .prepare()
            .map_err(|_| NativeSchemaError::InvalidSchema)?;
        let mut roots = BTreeMap::new();
        let mut schema_types = BTreeMap::new();
        let mut admission_work = 0usize;
        let mut admission_bytes = 0usize;
        for (uri, schema) in &documents {
            let original = catalog
                .document(uri)
                .ok_or(NativeSchemaError::UnknownRoot)?;
            *location = Some(SchemaDiagnosticLocation {
                document: original.digest(),
                pointer: JsonPointer::new("", limits).map_err(SchemaResourceError::from)?,
            });
            if crate::embedded_schema_base(original, limits)? == *uri {
                schema_types.insert(original.digest(), uri.clone());
            }
            let object_only = object_root(
                catalog,
                uri,
                limits,
                &mut admission_work,
                &mut admission_bytes,
            )?;
            let validator = jsonschema::options()
                .with_draft(jsonschema::Draft::Draft202012)
                .with_registry(&registry)
                .with_base_uri(internal_uri(uri))
                .offline()
                .should_validate_formats(false)
                .with_pattern_options(
                    jsonschema::PatternOptions::regex()
                        .size_limit(options.max_regex_compiled_bytes)
                        .dfa_size_limit(options.max_regex_compiled_bytes),
                )
                .build(schema)
                .map_err(|_| NativeSchemaError::InvalidSchema)?;
            roots.insert(
                uri.clone(),
                Root {
                    validator,
                    digest: catalog
                        .document(uri)
                        .ok_or(NativeSchemaError::UnknownRoot)?
                        .digest(),
                    object_only,
                },
            );
        }
        Ok(Self {
            roots,
            schema_types,
            catalog: catalog.clone(),
        })
    }
    /// Validates a JSON instance without mutating it or inserting defaults.
    ///
    /// # Errors
    /// Returns unknown root, input limits, or native engine failures. `Ok(false)`
    /// means ordinary instance rejection, not an operational timeout or budget failure.
    pub fn validate(
        &self,
        root_uri: &str,
        instance: &JsonDocument,
        limits: &Limits,
    ) -> Result<bool, NativeSchemaError> {
        let root = self
            .roots
            .get(root_uri)
            .ok_or(NativeSchemaError::UnknownRoot)?;
        let instance = parse(instance, limits)?;
        match root.validator.validate(&instance) {
            Ok(()) => Ok(true),
            Err(error) => {
                engine_error(&error)?;
                Ok(false)
            }
        }
    }
    /// Applies JSON numeric semantics to a native value through a bounded temporary
    /// JSON representation; the caller's native integer/float/absence data is unchanged.
    ///
    /// # Errors
    /// Returns unsupported native JSON values, input limits, or validation failures.
    pub fn validate_value(
        &self,
        root_uri: &str,
        instance: &Value,
        limits: &Limits,
    ) -> Result<bool, NativeSchemaError> {
        self.validate(
            root_uri,
            &JsonDocument::from_value(instance, limits)?,
            limits,
        )
    }
    /// Confirms the draft's callable-root admission rule: an explicit object
    /// constraint at the root or along a direct static reference chain.
    ///
    /// # Errors
    /// Returns an unknown root or a missing enforceable object-root constraint.
    pub fn require_object_root(&self, root_uri: &str) -> Result<(), NativeSchemaError> {
        if self
            .roots
            .get(root_uri)
            .ok_or(NativeSchemaError::UnknownRoot)?
            .object_only
        {
            Ok(())
        } else {
            Err(NativeSchemaError::ObjectRootRequired)
        }
    }
    /// Original JCS identity of a compiled retrieval root.
    pub fn document_digest(&self, root_uri: &str) -> Option<Digest> {
        self.roots.get(root_uri).map(|r| r.digest)
    }
    /// Validates a schema-typed native value at its prescribed embedded root,
    /// preserving the original native representation.
    ///
    /// # Errors
    /// Returns missing prescribed root, input limits, or native engine failure.
    pub fn validate_schema_value(
        &self,
        schema: &Digest,
        value: &Value,
        limits: &Limits,
    ) -> Result<bool, NativeSchemaError> {
        let uri = self
            .schema_types
            .get(schema)
            .ok_or(NativeSchemaError::UnknownRoot)?;
        self.validate_value(uri, value, limits)
    }
    pub(crate) fn validate_schema_document(
        &self,
        schema: &Digest,
        value: &JsonDocument,
        limits: &Limits,
    ) -> Result<bool, NativeSchemaError> {
        let uri = self
            .schema_types
            .get(schema)
            .ok_or(NativeSchemaError::UnknownRoot)?;
        self.validate(uri, value, limits)
    }
    pub(crate) fn has_schema_type(&self, schema: &Digest) -> bool {
        self.schema_types.contains_key(schema)
    }
    pub(crate) fn projection_type(
        &self,
        schema: &Digest,
        path: &[crate::PathStep],
        limits: &Limits,
    ) -> Result<crate::ValueType, NativeSchemaError> {
        let uri = self
            .schema_types
            .get(schema)
            .ok_or(NativeSchemaError::UnknownRoot)?;
        crate::schema_hints::projection_type(&self.catalog, uri, path, limits)
    }
    pub(crate) fn projection_evaluation(
        &self,
        schema: &Digest,
        instance: &JsonDocument,
        output: &mut impl std::io::Write,
        limits: &Limits,
    ) -> Result<(), NativeSchemaError> {
        let uri = self
            .schema_types
            .get(schema)
            .ok_or(NativeSchemaError::UnknownRoot)?;
        let root = self.roots.get(uri).ok_or(NativeSchemaError::UnknownRoot)?;
        let value = parse(instance, limits)?;
        let evaluation = root.validator.evaluate(&value);
        if !evaluation.is_valid() {
            return Err(NativeSchemaError::EngineFailure);
        }
        serde_json::to_writer(output, &evaluation.hierarchical())
            .map_err(|_| NativeSchemaError::EngineFailure)
    }
    /// Native implementation/profile identity (not a hash of caller-supplied schemas).
    ///
    /// # Errors
    /// Returns codec bounds on the identity record.
    pub fn identity(limits: &Limits) -> Result<EngineIdentity, MetadataError> {
        EngineIdentity::new(
            "htlk.native-jsonschema".into(),
            "0.56.0/adapter-0.1".into(),
            "2020-12;format-annotation;ecma-linear".into(),
            hash_bytes(include_bytes!("native_schema.rs")),
            limits,
        )
    }
}

fn parse(document: &JsonDocument, limits: &Limits) -> Result<Json, NativeSchemaError> {
    JsonDocument::decode(document.as_bytes(), limits)?;
    let mut deserializer = serde_json::Deserializer::from_slice(document.as_bytes());
    // The HTLK decoder already enforced the supported finite JSON depth ceiling.
    deserializer.disable_recursion_limit();
    Json::deserialize(&mut deserializer).map_err(|_| NativeSchemaError::EngineFailure)
}
struct PlanSize<'a> {
    total: usize,
    document: usize,
    limits: &'a Limits,
}
impl std::io::Write for PlanSize<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("schema plan size overflow"))?;
        self.document = self
            .document
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("schema plan size overflow"))?;
        if self.total > self.limits.max_total_payload_bytes
            || self.document > self.limits.max_document_bytes
        {
            return Err(std::io::Error::other("schema plan byte limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn internal_uri(uri: &str) -> String {
    format!("urn:htlk:native-schema:{}", hash_bytes(uri.as_bytes()))
}
fn check_profile(value: &Value, options: NativeSchemaOptions) -> Result<(), NativeSchemaError> {
    let Value::Map(m) = value else {
        return Ok(());
    };
    if let Some(v) = m.get("$vocabulary") {
        let Value::Map(v) = v else {
            return Err(NativeSchemaError::InvalidSchema);
        };
        for (uri, required) in v.iter() {
            let parsed = iri_string::types::UriReferenceStr::new(uri)
                .map_err(|_| NativeSchemaError::InvalidSchema)?;
            if parsed.scheme_str().is_none() {
                return Err(NativeSchemaError::InvalidSchema);
            }
            let Value::Bool(required) = required else {
                return Err(NativeSchemaError::InvalidSchema);
            };
            if *required
                && ![
                    "core",
                    "applicator",
                    "unevaluated",
                    "validation",
                    "meta-data",
                    "format-annotation",
                    "content",
                ]
                .iter()
                .any(|v| {
                    uri.strip_suffix('#').unwrap_or(uri)
                        == format!("https://json-schema.org/draft/2020-12/vocab/{v}")
                })
            {
                return Err(NativeSchemaError::UnsupportedVocabulary);
            }
        }
    }
    if let Some(Value::Text(pattern)) = m.get("pattern") {
        pattern_limit(pattern, options)?;
    }
    if let Some(Value::Map(patterns)) = m.get("patternProperties") {
        for (pattern, _) in patterns.iter() {
            pattern_limit(pattern, options)?;
        }
    }
    Ok(())
}
fn pattern_limit(pattern: &str, options: NativeSchemaOptions) -> Result<(), NativeSchemaError> {
    if pattern.len() > options.max_pattern_bytes {
        return Err(NativeSchemaError::PatternLimit);
    }
    // Probe each declared pattern, including definitions not selected by the
    // current entry root. Unsupported syntax must fail compilation, not wait for
    // a future dynamic reference or become an ignored annotation.
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .offline()
        .with_pattern_options(
            jsonschema::PatternOptions::regex()
                .size_limit(options.max_regex_compiled_bytes)
                .dfa_size_limit(options.max_regex_compiled_bytes),
        )
        .build(&serde_json::json!({"pattern":pattern}))
        .map_err(|_| NativeSchemaError::InvalidSchema)?;
    Ok(())
}
fn standard_keyword(key: &str) -> bool {
    matches!(
        key,
        "$id"
            | "$schema"
            | "$vocabulary"
            | "$anchor"
            | "$dynamicAnchor"
            | "$ref"
            | "$dynamicRef"
            | "$defs"
            | "$comment"
            | "type"
            | "enum"
            | "const"
            | "multipleOf"
            | "minimum"
            | "maximum"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
            | "minLength"
            | "maxLength"
            | "pattern"
            | "minItems"
            | "maxItems"
            | "uniqueItems"
            | "minContains"
            | "maxContains"
            | "minProperties"
            | "maxProperties"
            | "required"
            | "dependentRequired"
            | "allOf"
            | "anyOf"
            | "oneOf"
            | "not"
            | "if"
            | "then"
            | "else"
            | "properties"
            | "patternProperties"
            | "additionalProperties"
            | "dependentSchemas"
            | "propertyNames"
            | "items"
            | "prefixItems"
            | "contains"
            | "unevaluatedItems"
            | "unevaluatedProperties"
            | "format"
            | "title"
            | "description"
            | "default"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
            | "examples"
            | "contentEncoding"
            | "contentMediaType"
            | "contentSchema"
    )
}
fn object_root(
    catalog: &SchemaCatalog,
    root_uri: &str,
    limits: &Limits,
    work: &mut usize,
    bytes: &mut usize,
) -> Result<bool, NativeSchemaError> {
    let empty = JsonPointer::new("", limits).map_err(SchemaResourceError::from)?;
    let mut uri = root_uri;
    let mut pointer = &empty;
    let mut value = catalog
        .document(uri)
        .ok_or(NativeSchemaError::UnknownRoot)?
        .value();
    let mut seen = BTreeSet::new();
    loop {
        *work = work
            .checked_add(1)
            .ok_or(NativeSchemaError::AdmissionLimit)?;
        if *work > limits.max_total_values || seen.len() >= limits.max_collection_entries {
            return Err(NativeSchemaError::AdmissionLimit);
        }
        for amount in std::iter::once(uri.len()).chain(pointer.tokens().iter().map(String::len)) {
            *bytes = bytes
                .checked_add(amount)
                .ok_or(NativeSchemaError::AdmissionLimit)?;
            if *bytes > limits.max_total_payload_bytes {
                return Err(NativeSchemaError::AdmissionLimit);
            }
        }
        if !seen.insert((uri, pointer.tokens())) {
            return Ok(false);
        }
        let Value::Map(m) = value else {
            return Ok(false);
        };
        if matches!(m.get("type"), Some(Value::Text(t)) if t == "object") {
            return Ok(true);
        }
        let Some(Value::Text(reference)) = m.get("$ref") else {
            return Ok(false);
        };
        let target = catalog.resolve(uri, pointer, reference, limits)?;
        uri = target.retrieval_uri();
        pointer = target.pointer();
        value = target.value();
    }
}
fn engine_error(error: &jsonschema::ValidationError<'_>) -> Result<(), NativeSchemaError> {
    use jsonschema::error::ValidationErrorKind as K;
    match error.kind() {
        K::BacktrackLimitExceeded { .. } | K::RegexEngineFailure { .. } | K::Referencing(_) => {
            Err(NativeSchemaError::EngineFailure)
        }
        K::AnyOf { context } | K::OneOfMultipleValid { context } | K::OneOfNotValid { context } => {
            for errors in context {
                for error in errors {
                    engine_error(error)?;
                }
            }
            Ok(())
        }
        K::PropertyNames { error } => engine_error(error),
        _ => Ok(()),
    }
}
struct NoRetrieval;
impl jsonschema::Retrieve for NoRetrieval {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> Result<Json, Box<dyn std::error::Error + Send + Sync>> {
        Err(Box::new(std::io::Error::other(
            "offline schema registry only",
        )))
    }
}

/// Native validation failures contain no submitted schema/instance contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaDiagnosticLocation {
    /// Original raw JCS document digest, never a private rewritten resource URI.
    pub document: Digest,
    /// Schema location within the original document.
    pub pointer: JsonPointer,
}
/// Redacted native schema admission failure with an original canonical location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSchemaDiagnostic {
    /// Location when the failure belongs to one known schema node/document.
    pub location: Option<SchemaDiagnosticLocation>,
    /// Underlying native schema failure.
    pub error: NativeSchemaError,
}
impl fmt::Display for NativeSchemaDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "schema at {:?}: {}", self.location, self.error)
    }
}
impl std::error::Error for NativeSchemaDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
/// Native validation failures contain no submitted schema/instance contents.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeSchemaError {
    /// A derived ordinary type exceeded its representation limits.
    Type(crate::TypeError),
    /// Private native compilation input exceeds document/aggregate byte ceilings.
    CompilationInputLimit,
    /// JSON input limit/profile failure.
    Json(JsonError),
    /// Schema catalog/reference failure.
    Resources(SchemaResourceError),
    /// Invalid native backend options.
    InvalidOptions,
    /// Invalid schema or unsupported regex syntax/compiled representation.
    InvalidSchema,
    /// Required vocabulary not implemented by this profile.
    UnsupportedVocabulary,
    /// Pattern text exceeds its configured bound.
    PatternLimit,
    /// Callable object-root constraint cannot be established.
    ObjectRootRequired,
    /// Admission traversal exceeds configured work/collection bounds.
    AdmissionLimit,
    /// No validator for the requested retrieval root.
    UnknownRoot,
    /// Backend failure, distinct from ordinary invalid instance data.
    EngineFailure,
}
impl From<JsonError> for NativeSchemaError {
    fn from(e: JsonError) -> Self {
        Self::Json(e)
    }
}
impl From<crate::TypeError> for NativeSchemaError {
    fn from(e: crate::TypeError) -> Self {
        Self::Type(e)
    }
}
impl From<SchemaResourceError> for NativeSchemaError {
    fn from(e: SchemaResourceError) -> Self {
        Self::Resources(e)
    }
}
impl fmt::Display for NativeSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native schema validation: {self:?}")
    }
}
impl std::error::Error for NativeSchemaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Resources(e) => Some(e),
            _ => None,
        }
    }
}
