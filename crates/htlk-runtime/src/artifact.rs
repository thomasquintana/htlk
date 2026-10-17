//! Immutable typed artifact preparation and integrity-checked metadata. A prepared
//! artifact is only a proposal; transactional metadata publication accepts it.
use crate::{
    EvaluationError, VerifiedExecutable,
    failure::{FailureBoundary, FailureCode},
    identity::{ArtifactId, IdentityError, InvocationId, RunId, ValueDigest},
    records::{RuntimeRecordError, SecurityLabels},
    validate_typed_value,
};
use htlk_executable::{
    Identifier, TypeContext, ValueType,
    cbor::{self, Limits, Map, Value},
    digest::{Digest, hash_bytes},
    record_accounting::RecordAccounting,
};
use std::fmt;

/// Accepted artifact provenance. A copied reference retains its existing identity;
/// a scope/use/loop that republishes a value records its own node-output producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactProducer {
    /// An inline public input at creation or additive installation.
    RunInput {
        /// Original host run identity.
        run_id: RunId,
        /// Zero at creation, otherwise the committed extension number.
        extension_number: u64,
        /// Public input name.
        port: Identifier,
    },
    /// A successful invocation's declared output.
    NodeOutput {
        /// Immutable producer invocation.
        invocation_id: InvocationId,
        /// Declared output name.
        port: Identifier,
    },
}
impl ArtifactProducer {
    fn identity(&self, content: ValueDigest, limits: &Limits) -> Result<ArtifactId, ArtifactError> {
        Ok(match self {
            Self::RunInput {
                run_id,
                extension_number,
                port,
            } => ArtifactId::run_input(run_id, *extension_number, port, content, limits)?,
            Self::NodeOutput {
                invocation_id,
                port,
            } => ArtifactId::node_output(*invocation_id, port, content, limits)?,
        })
    }
    fn to_value(&self, limits: &Limits) -> Result<Value, ArtifactError> {
        let fields = match self {
            Self::RunInput {
                run_id,
                extension_number,
                port,
            } => vec![
                ("kind".into(), Value::Text("run_input".into())),
                ("run_id".into(), Value::Text(run_id.to_string())),
                (
                    "extension_number".into(),
                    Value::Integer(
                        i64::try_from(*extension_number).map_err(|_| ArtifactError::Shape)?,
                    ),
                ),
                ("port".into(), Value::Text(port.to_string())),
            ],
            Self::NodeOutput {
                invocation_id,
                port,
            } => vec![
                ("kind".into(), Value::Text("node_output".into())),
                (
                    "invocation_id".into(),
                    Value::Text(invocation_id.to_string()),
                ),
                ("port".into(), Value::Text(port.to_string())),
            ],
        };
        let value = Value::Map(Map::try_from_entries(fields)?);
        RecordAccounting::new(limits)?
            .value(&value, 0)
            .map_err(|_| ArtifactError::Limit)?;
        Ok(value)
    }
    fn from_value(value: &Value, limits: &Limits) -> Result<Self, ArtifactError> {
        let Value::Map(fields) = value else {
            return Err(ArtifactError::Shape);
        };
        match text(field(fields, "kind")?)? {
            "run_input" if fields.len() == 4 => {
                let Value::Integer(extension) = field(fields, "extension_number")? else {
                    return Err(ArtifactError::Shape);
                };
                Ok(Self::RunInput {
                    run_id: RunId::new(text(field(fields, "run_id")?)?.into(), limits)?,
                    extension_number: u64::try_from(*extension)
                        .map_err(|_| ArtifactError::Shape)?,
                    port: text(field(fields, "port")?)?
                        .parse()
                        .map_err(|_| ArtifactError::Shape)?,
                })
            }
            "node_output" if fields.len() == 3 => Ok(Self::NodeOutput {
                invocation_id: text(field(fields, "invocation_id")?)?
                    .parse()
                    .map_err(|_| ArtifactError::Shape)?,
                port: text(field(fields, "port")?)?
                    .parse()
                    .map_err(|_| ArtifactError::Shape)?,
            }),
            _ => Err(ArtifactError::Shape),
        }
    }
}
/// The immutable accepted-value facts from runtime §8. Fields are read-only;
/// decoding is representation/integrity checking, not acceptance authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactMetadata {
    artifact_id: ArtifactId,
    value_type: ValueType,
    content_digest: ValueDigest,
    storage_reference: Digest,
    producer: ArtifactProducer,
    input_artifact_ids: Vec<ArtifactId>,
    security_labels: SecurityLabels,
    accepted_at: i64,
    validation_profile: Digest,
}
impl ArtifactMetadata {
    /// Stable producer/provenance identity.
    pub const fn artifact_id(&self) -> ArtifactId {
        self.artifact_id
    }
    /// Original declared source type; binding to a new type never rewrites it.
    pub fn value_type(&self) -> &ValueType {
        &self.value_type
    }
    /// Typed content digest, independent of storage and producer.
    pub const fn content_digest(&self) -> ValueDigest {
        self.content_digest
    }
    /// SHA-256 of the immutable canonical value bytes, shared across source types.
    pub const fn storage_reference(&self) -> Digest {
        self.storage_reference
    }
    /// Exact original producer.
    pub fn producer(&self) -> &ArtifactProducer {
        &self.producer
    }
    /// Sorted unique accepted lineage IDs. Full per-port manifests remain separate.
    pub fn input_artifact_ids(&self) -> &[ArtifactId] {
        &self.input_artifact_ids
    }
    /// Trusted value/control protection frozen at acceptance.
    pub fn security_labels(&self) -> &SecurityLabels {
        &self.security_labels
    }
    /// Authoritative coordinator timestamp, not part of content identity.
    pub const fn accepted_at(&self) -> i64 {
        self.accepted_at
    }
    /// Origin executable fingerprint identifying the exact validation profile and
    /// retained schema/library closure under which this value was checked.
    pub const fn validation_profile(&self) -> Digest {
        self.validation_profile
    }
    /// Produces the exact bounded private artifact record.
    ///
    /// # Errors
    /// Returns type/metadata representation or complete byte/depth failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, ArtifactError> {
        check_parents(&self.input_artifact_ids, limits)?;
        let value = Value::Map(Map::try_from_entries([
            (
                "artifact_id".into(),
                Value::Text(self.artifact_id.to_string()),
            ),
            (
                "type".into(),
                self.value_type.to_value(TypeContext::Value, limits)?,
            ),
            (
                "content_digest".into(),
                Value::Text(self.content_digest.to_string()),
            ),
            (
                "storage_reference".into(),
                Value::Text(self.storage_reference.to_string()),
            ),
            ("producer".into(), self.producer.to_value(limits)?),
            (
                "input_artifact_ids".into(),
                Value::Array(
                    self.input_artifact_ids
                        .iter()
                        .map(|id| Value::Text(id.to_string()))
                        .collect(),
                ),
            ),
            (
                "security_labels".into(),
                self.security_labels.to_value(limits)?,
            ),
            ("accepted_at".into(), Value::Integer(self.accepted_at)),
            (
                "validation_profile".into(),
                Value::Text(self.validation_profile.to_string()),
            ),
        ])?);
        RecordAccounting::new(limits)?
            .value(&value, 0)
            .map_err(|_| ArtifactError::Limit)?;
        Ok(value)
    }
    /// Checks closed metadata, exact producer identity and canonical label/lineage
    /// sets. Actual acceptance and authorization come from authoritative storage.
    ///
    /// # Errors
    /// Returns malformed type/metadata, mismatched identity or representation limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, ArtifactError> {
        RecordAccounting::new(limits)?
            .value(value, 0)
            .map_err(|_| ArtifactError::Limit)?;
        let Value::Map(fields) = value else {
            return Err(ArtifactError::Shape);
        };
        if fields.len() != 9 {
            return Err(ArtifactError::Shape);
        }
        let Value::Integer(accepted_at) = field(fields, "accepted_at")? else {
            return Err(ArtifactError::Shape);
        };
        if *accepted_at < 0 {
            return Err(ArtifactError::Shape);
        }
        let Value::Array(parents) = field(fields, "input_artifact_ids")? else {
            return Err(ArtifactError::Shape);
        };
        let mut input_artifact_ids = Vec::new();
        for value in parents {
            let id: ArtifactId = text(value)?.parse().map_err(|_| ArtifactError::Shape)?;
            if input_artifact_ids.last().is_some_and(|prior| *prior >= id) {
                return Err(ArtifactError::Shape);
            }
            input_artifact_ids.push(id);
        }
        let metadata = Self {
            artifact_id: text(field(fields, "artifact_id")?)?
                .parse()
                .map_err(|_| ArtifactError::Shape)?,
            value_type: ValueType::from_value(field(fields, "type")?, TypeContext::Value, limits)?,
            content_digest: text(field(fields, "content_digest")?)?
                .parse()
                .map_err(|_| ArtifactError::Shape)?,
            storage_reference: text(field(fields, "storage_reference")?)?
                .parse()
                .map_err(|_| ArtifactError::Shape)?,
            producer: ArtifactProducer::from_value(field(fields, "producer")?, limits)?,
            input_artifact_ids,
            security_labels: SecurityLabels::from_value(field(fields, "security_labels")?, limits)?,
            accepted_at: *accepted_at,
            validation_profile: text(field(fields, "validation_profile")?)?
                .parse()
                .map_err(|_| ArtifactError::Shape)?,
        };
        if metadata
            .producer
            .identity(metadata.content_digest, limits)?
            != metadata.artifact_id
        {
            return Err(ArtifactError::Integrity);
        }
        if matches!(&metadata.producer, ArtifactProducer::RunInput { .. })
            && !metadata.input_artifact_ids.is_empty()
        {
            return Err(ArtifactError::Shape);
        }
        Ok(metadata)
    }
    /// Verifies canonical immutable bytes, raw storage hash and typed content hash
    /// without converting numbers or substituting a destination type.
    ///
    /// # Errors
    /// Returns canonical representation failures or integrity mismatch.
    pub fn verify_value(&self, bytes: &[u8], limits: &Limits) -> Result<Value, ArtifactError> {
        if bytes.len() > limits.max_document_bytes {
            return Err(ArtifactError::Limit);
        }
        if hash_bytes(bytes) != self.storage_reference {
            return Err(ArtifactError::Integrity);
        }
        let value = cbor::decode(bytes, limits)?;
        if ValueDigest::new(&self.value_type, &value, limits)? != self.content_digest {
            return Err(ArtifactError::Integrity);
        }
        Ok(value)
    }
}
/// Trusted coordinator inputs to artifact preparation. Labels/lineage must include
/// all evaluated value, guard and boundary influences before publication.
pub struct ArtifactInput<'a> {
    /// Original producing invocation or inline run input.
    pub producer: ArtifactProducer,
    /// Declared source type selected from admitted executable records.
    pub value_type: &'a ValueType,
    /// Proposed native value; no handle or coercion semantics.
    pub value: &'a Value,
    /// Accepted contributing artifacts.
    pub input_artifact_ids: Vec<ArtifactId>,
    /// Trusted conservative protection union.
    pub security_labels: SecurityLabels,
    /// Authoritative acceptance proposal timestamp.
    pub accepted_at: i64,
}
/// A fully type-checked proposal. Staging its immutable bytes does not publish an
/// artifact; metadata and terminal outcome must be committed atomically.
pub struct PreparedArtifact {
    metadata: ArtifactMetadata,
    bytes: Vec<u8>,
}
impl PreparedArtifact {
    /// Validates a proposed value against the admitted native schema/profile,
    /// computes separate content/provenance/blob identities and bounds metadata.
    ///
    /// # Errors
    /// Returns type/schema/representation/limit failure. No accepted value is created.
    pub fn new(
        executable: &VerifiedExecutable<'_>,
        mut input: ArtifactInput<'_>,
        limits: &Limits,
    ) -> Result<Self, ArtifactError> {
        if input.accepted_at < 0 {
            return Err(ArtifactError::Shape);
        }
        if matches!(&input.producer, ArtifactProducer::RunInput { .. })
            && !input.input_artifact_ids.is_empty()
        {
            return Err(ArtifactError::Shape);
        }
        check_parents(&input.input_artifact_ids, limits)?;
        validate_typed_value(
            input.value,
            input.value_type,
            Some(executable.schemas()),
            limits,
            &executable.policy().fields().evaluator_limits,
        )?;
        let content_digest = ValueDigest::new(input.value_type, input.value, limits)?;
        let artifact_id = input.producer.identity(content_digest, limits)?;
        let bytes = cbor::encode(input.value, limits)?;
        input.input_artifact_ids.sort();
        input.input_artifact_ids.dedup();
        let metadata = ArtifactMetadata {
            artifact_id,
            value_type: input.value_type.clone(),
            content_digest,
            storage_reference: hash_bytes(&bytes),
            producer: input.producer,
            input_artifact_ids: input.input_artifact_ids,
            security_labels: input.security_labels,
            accepted_at: input.accepted_at,
            validation_profile: executable.fingerprint(),
        };
        metadata.to_value(limits)?;
        Ok(Self { metadata, bytes })
    }
    /// Borrows proposed immutable metadata; this does not establish acceptance.
    pub fn metadata(&self) -> &ArtifactMetadata {
        &self.metadata
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn metadata_at(&self, time: i64) -> Result<ArtifactMetadata, ArtifactError> {
        if time < 0 {
            return Err(ArtifactError::Shape);
        }
        let mut metadata = self.metadata.clone();
        metadata.accepted_at = time;
        Ok(metadata)
    }
}
/// An authorized snapshot returned after checking current policy and byte integrity.
#[derive(Clone, PartialEq)]
pub struct AuthorizedValue {
    /// Immutable accepted provenance and protection metadata.
    pub metadata: ArtifactMetadata,
    /// Original native value, not an ambient storage handle.
    pub value: Value,
}
impl fmt::Debug for AuthorizedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizedValue")
            .field("artifact_id", &self.metadata.artifact_id())
            .field("value", &"[redacted]")
            .finish()
    }
}
fn check_parents(parents: &[ArtifactId], limits: &Limits) -> Result<(), ArtifactError> {
    let mut accounting = RecordAccounting::new(limits)?;
    accounting
        .collection(parents.len(), 0)
        .map_err(|_| ArtifactError::Limit)?;
    for parent in parents {
        accounting
            .text(&parent.to_string(), 1)
            .map_err(|_| ArtifactError::Limit)?;
    }
    Ok(())
}
fn text(value: &Value) -> Result<&str, ArtifactError> {
    if let Value::Text(text) = value {
        Ok(text)
    } else {
        Err(ArtifactError::Shape)
    }
}
fn field<'a>(map: &'a Map, key: &str) -> Result<&'a Value, ArtifactError> {
    map.get(key).ok_or(ArtifactError::Shape)
}
/// Artifact proposal/record failure, retaining no submitted value bytes.
#[derive(Debug)]
#[non_exhaustive]
pub enum ArtifactError {
    /// Complete encoded value or metadata allowance is exhausted.
    Limit,
    /// Malformed stored metadata or impossible producer facts.
    Shape,
    /// Blob, typed-content or producer identity mismatch.
    Integrity,
    /// Actual proposed value fails its declared boundary or evaluator allowance.
    Value(EvaluationError),
    /// Canonical representation failed.
    Codec(cbor::Error),
    /// Content/provenance identity preparation failed.
    Identity(IdentityError),
    /// Trusted label metadata failed.
    Record(RuntimeRecordError),
    /// Source type representation failed.
    Type(htlk_executable::TypeError),
}
impl ArtifactError {
    /// Contextual classification for input/output preparation vs stored integrity.
    pub fn code(&self, boundary: FailureBoundary) -> FailureCode {
        match self {
            Self::Value(e) => FailureCode::for_evaluation(e, boundary),
            Self::Integrity | Self::Shape => FailureCode::Digest,
            Self::Limit => match boundary {
                FailureBoundary::Input => FailureCode::Input,
                FailureBoundary::Output => FailureCode::OutputSchema,
                FailureBoundary::Expression => FailureCode::ExpressionLimit,
            },
            Self::Codec(e)
                if matches!(
                    e.kind(),
                    cbor::ErrorKind::NonCanonicalEncoding
                        | cbor::ErrorKind::InvalidUtf8
                        | cbor::ErrorKind::UnexpectedEnd
                        | cbor::ErrorKind::TrailingData
                        | cbor::ErrorKind::UnsupportedType
                        | cbor::ErrorKind::NonFiniteFloat
                        | cbor::ErrorKind::IntegerOutOfRange
                        | cbor::ErrorKind::DuplicateMapKey
                        | cbor::ErrorKind::MapKeyOutOfOrder
                ) =>
            {
                FailureCode::Digest
            }
            _ => FailureCode::Input,
        }
    }
}
impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shape => "invalid artifact metadata",
            Self::Limit => "artifact representation allowance exhausted",
            Self::Integrity => "artifact integrity mismatch",
            Self::Value(_) => "artifact value violates its admitted boundary",
            Self::Codec(_) => "artifact encoding failed",
            Self::Identity(_) => "artifact identity preparation failed",
            Self::Record(_) => "artifact protection metadata failed",
            Self::Type(_) => "artifact source type is invalid",
        })
    }
}
impl std::error::Error for ArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shape | Self::Integrity | Self::Limit => None,
            Self::Value(e) => Some(e),
            Self::Codec(e) => Some(e),
            Self::Identity(e) => Some(e),
            Self::Record(e) => Some(e),
            Self::Type(e) => Some(e),
        }
    }
}
impl From<EvaluationError> for ArtifactError {
    fn from(e: EvaluationError) -> Self {
        Self::Value(e)
    }
}
impl From<cbor::Error> for ArtifactError {
    fn from(e: cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<IdentityError> for ArtifactError {
    fn from(e: IdentityError) -> Self {
        Self::Identity(e)
    }
}
impl From<RuntimeRecordError> for ArtifactError {
    fn from(e: RuntimeRecordError) -> Self {
        Self::Record(e)
    }
}
impl From<htlk_executable::TypeError> for ArtifactError {
    fn from(e: htlk_executable::TypeError) -> Self {
        Self::Type(e)
    }
}
