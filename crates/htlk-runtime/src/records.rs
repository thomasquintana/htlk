//! Immutable runtime manifests and lifecycle vocabulary. Record construction
//! establishes representation only; the coordinator supplies authorization,
//! accepted-artifact integrity and transactional authority.

use crate::{
    failure::RuntimeFailure,
    identity::{self, ArtifactId, IdentityError, ValueDigest},
};
use htlk_executable::{
    Identifier, PortTable,
    cbor::{self, Limits, Map, Value},
    digest::Digest,
    record_accounting::{EncodingLimitError, RecordAccounting},
};
use std::fmt;

/// The six permitted invocation lifecycle states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationStatus {
    /// Waiting for guard and complete input admission.
    Pending,
    /// Admitted; may include retries, approvals and external waiting.
    Active,
    /// Complete outputs accepted atomically.
    Succeeded,
    /// Immutable terminal failure.
    Failed,
    /// False guard or required-input absence.
    Skipped,
    /// Intentionally stopped.
    Cancelled,
}
impl InvocationStatus {
    /// Whether the outcome can no longer change.
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Active)
    }
    /// Stable stored and inspection spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }
    /// Checks the specification lifecycle. Retrying occurs inside active; duplicate
    /// accepted commands return receipts rather than applying another transition.
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Pending,
                Self::Active | Self::Skipped | Self::Failed | Self::Cancelled
            ) | (
                Self::Active,
                Self::Succeeded | Self::Failed | Self::Cancelled
            )
        )
    }
}

/// Run outcomes; blocked is a derived observation, not a lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// The active root is unfinished.
    Running,
    /// The active root committed a successful boundary.
    Succeeded,
    /// The active root failed.
    Failed,
    /// The run was cancelled.
    Cancelled,
}
impl RunStatus {
    /// Whether the active-root result is immutable.
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
    /// Stable stored and inspection spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    /// Only a running run may acquire a terminal result.
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(self, Self::Running) && next.is_terminal()
    }
}

/// Trusted policy-assigned sorted unique opaque label strings. Merely constructing
/// this set grants no authority; untrusted data fields cannot assign labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityLabels(Vec<String>);
impl SecurityLabels {
    /// Bounds supplied labels before sorting/deduplicating without Unicode changes.
    ///
    /// # Errors
    /// Returns codec or complete label-array limits.
    pub fn new(mut labels: Vec<String>, limits: &Limits) -> Result<Self, RuntimeRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(labels.len(), 0)?;
        for label in &labels {
            accounting.text(label, 1)?;
        }
        labels.sort();
        labels.dedup();
        Ok(Self(labels))
    }
    /// Borrows canonical decoded UTF-8 order.
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
    /// Produces the canonical label array.
    ///
    /// # Errors
    /// Returns codec or complete label-array limits.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, RuntimeRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(self.0.len(), 0)?;
        for label in &self.0 {
            accounting.text(label, 1)?;
        }
        Ok(Value::Array(
            self.0.iter().cloned().map(Value::Text).collect(),
        ))
    }
    /// Reads canonical persisted labels, rejecting duplicate or unsorted spelling.
    ///
    /// # Errors
    /// Returns malformed labels or complete representation limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, RuntimeRecordError> {
        RecordAccounting::new(limits)?.value(value, 0)?;
        let Value::Array(values) = value else {
            return Err(RuntimeRecordError::Manifest);
        };
        let mut labels = Vec::new();
        let mut previous: Option<&str> = None;
        for value in values {
            let Value::Text(label) = value else {
                return Err(RuntimeRecordError::Manifest);
            };
            if previous.is_some_and(|p| p >= label.as_str()) {
                return Err(RuntimeRecordError::Manifest);
            }
            previous = Some(label);
            labels.push(label.clone());
        }
        Ok(Self(labels))
    }
    /// Hashes the label set under the private `htlk.security_labels` 0.1 domain.
    ///
    /// # Errors
    /// Returns complete preimage limits or codec failures.
    pub fn digest(&self, limits: &Limits) -> Result<Digest, RuntimeRecordError> {
        Ok(identity::hash(
            "security_labels",
            &[&self.to_value(limits)?],
            limits,
        )?)
    }
    /// Conservatively unions trusted influence sets, preserving exact spelling.
    ///
    /// # Errors
    /// Returns limits before retaining an oversized union.
    pub fn union(&self, other: &Self, limits: &Limits) -> Result<Self, RuntimeRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        let mut result = Vec::new();
        let (mut left, mut right) = (self.0.iter().peekable(), other.0.iter().peekable());
        // Charge each distinct item before copying; final conversion charges the
        // exact array header (whose width may grow with the union).
        while left.peek().is_some() || right.peek().is_some() {
            let value = match (left.peek(), right.peek()) {
                (Some(a), Some(b)) => match a.cmp(b) {
                    std::cmp::Ordering::Less => left.next(),
                    std::cmp::Ordering::Greater => right.next(),
                    std::cmp::Ordering::Equal => {
                        right.next();
                        left.next()
                    }
                },
                (Some(_), None) => left.next(),
                _ => right.next(),
            }
            .expect("one input remains");
            accounting.text(value, 1)?;
            result.push(value.clone());
        }
        Self::new(result, limits)
    }
}

/// Frozen declared-port entry. Pending is deliberately unrepresentable here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestEntry {
    /// Accepted artifact plus separately retained content/label facts.
    Value {
        /// Accepted provenance identity, retained in the manifest but not its hash.
        artifact_id: ArtifactId,
        /// Typed source content, independent of artifact provenance.
        content_digest: ValueDigest,
        /// Trusted artifact labels; control influence is retained separately.
        security_labels: SecurityLabels,
    },
    /// Settled legitimate absence, distinct from a present null artifact.
    Absent,
}

/// An immutable complete admitted input inventory. The coordinator must establish
/// that referenced artifacts are accepted, authorized, unchanged and type-valid
/// before constructing this representation. No API mutates/refreezes its entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputManifest {
    entries: Vec<(Identifier, ManifestEntry)>,
    influence: SecurityLabels,
    input_hash: Digest,
}
impl InputManifest {
    /// Checks exact declared coverage and presence and freezes the content hash.
    /// Optional absence must be explicitly supplied. Required absence belongs to
    /// admission's skipped result and cannot create an admitted manifest.
    ///
    /// # Errors
    /// Returns duplicate/unknown/missing ports, required absence, codec/type limits.
    pub fn freeze(
        ports: &PortTable,
        mut entries: Vec<(Identifier, ManifestEntry)>,
        mut influence: SecurityLabels,
        limits: &Limits,
    ) -> Result<Self, RuntimeRecordError> {
        // Bound retained metadata before sorting, including artifact IDs and labels.
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(entries.len(), 0)?;
        for (name, entry) in &entries {
            accounting.collection(2, 1)?;
            accounting.text(name.as_str(), 2)?;
            accounting.value(&entry_value(entry, true, limits)?, 2)?;
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.len() != ports.len() {
            return Err(RuntimeRecordError::Manifest);
        }
        let mut inventory = Vec::new();
        for ((name, entry), (declared, port)) in entries.iter().zip(ports.iter()) {
            if name != declared || (port.required() && matches!(entry, ManifestEntry::Absent)) {
                return Err(RuntimeRecordError::Manifest);
            }
            let mut fields = vec![
                ("name".into(), Value::Text(name.to_string())),
                (
                    "port".into(),
                    port.to_value(htlk_executable::TypeContext::Value, limits)?,
                ),
            ];
            if let ManifestEntry::Value {
                content_digest,
                security_labels,
                ..
            } = entry
            {
                influence = influence.union(security_labels, limits)?;
                fields.push(("state".into(), Value::Text("value".into())));
                fields.push((
                    "content_digest".into(),
                    Value::Text(content_digest.to_string()),
                ));
                fields.push((
                    "label_digest".into(),
                    Value::Text(security_labels.digest(limits)?.to_string()),
                ));
            } else {
                fields.push(("state".into(), Value::Text("absent".into())));
            }
            let value = Value::Map(Map::try_from_entries(fields)?);
            // Bound the aggregate hash inventory as it is built.
            accounting.value(&value, 1)?;
            inventory.push(value);
        }
        let influence_digest = Value::Text(influence.digest(limits)?.to_string());
        let input_hash = identity::hash(
            "input",
            &[&Value::Array(inventory), &influence_digest],
            limits,
        )?;
        Ok(Self {
            entries,
            influence,
            input_hash,
        })
    }
    /// Iterates every declared port in decoded UTF-8 order.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (&Identifier, &ManifestEntry)> {
        self.entries.iter().map(|(name, entry)| (name, entry))
    }
    /// Frozen input and guard control influence, including absence decisions.
    pub fn influence_labels(&self) -> &SecurityLabels {
        &self.influence
    }
    /// Content-equivalence hash; it grants no authorization or cache reuse.
    pub const fn input_hash(&self) -> Digest {
        self.input_hash
    }
    /// Encodes the specified port-to-value/absence inventory. Content and label
    /// metadata are deliberately separate from the logical manifest entry shape.
    ///
    /// # Errors
    /// Returns complete manifest representation limits.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, RuntimeRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(self.entries.len(), 0)?;
        let mut entries = Vec::new();
        for (name, entry) in &self.entries {
            accounting.text(name.as_str(), 1)?;
            let value = entry_value(entry, false, limits)?;
            accounting.value(&value, 1)?;
            entries.push((name.to_string(), value));
        }
        Ok(Value::Map(Map::try_from_entries(entries)?))
    }
    /// Encodes a private version-0.1 input checkpoint with content/label facts and
    /// the frozen hash. Logical manifest entries remain available via `to_value`.
    ///
    /// # Errors
    /// Returns complete checkpoint limits or codec failures.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, RuntimeRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(4, 0)?;
        for text in [
            "record_version",
            "0.1",
            "input_hash",
            &self.input_hash.to_string(),
        ] {
            accounting.text(text, 1)?;
        }
        accounting.text("influence_labels", 1)?;
        let influence = self.influence.to_value(limits)?;
        accounting.value(&influence, 1)?;
        accounting.text("entries", 1)?;
        accounting.collection(self.entries.len(), 1)?;
        let mut entries = Vec::new();
        for (name, entry) in &self.entries {
            accounting.text(name.as_str(), 2)?;
            let value = entry_value(entry, true, limits)?;
            accounting.value(&value, 2)?;
            entries.push((name.to_string(), value));
        }
        let record = Value::Map(Map::try_from_entries([
            ("record_version".into(), Value::Text("0.1".into())),
            (
                "input_hash".into(),
                Value::Text(self.input_hash.to_string()),
            ),
            ("influence_labels".into(), influence),
            (
                "entries".into(),
                Value::Map(Map::try_from_entries(entries)?),
            ),
        ])?);
        Ok(cbor::encode(&record, limits)?)
    }
    /// Restores a checkpoint against authoritative port declarations and recomputes
    /// its hash. Persisted bytes do not prove artifact authorization or integrity.
    ///
    /// # Errors
    /// Returns malformed/noncanonical/wrong-version records, changed hash or limits.
    pub fn decode(
        bytes: &[u8],
        ports: &PortTable,
        limits: &Limits,
    ) -> Result<Self, RuntimeRecordError> {
        let Value::Map(record) = cbor::decode(bytes, limits)? else {
            return Err(RuntimeRecordError::Manifest);
        };
        if record.len() != 4 || record.get("record_version") != Some(&Value::Text("0.1".into())) {
            return Err(RuntimeRecordError::Manifest);
        }
        let (Some(Value::Map(entries)), Some(influence), Some(Value::Text(expected))) = (
            record.get("entries"),
            record.get("influence_labels"),
            record.get("input_hash"),
        ) else {
            return Err(RuntimeRecordError::Manifest);
        };
        let influence = SecurityLabels::from_value(influence, limits)?;
        let mut parsed = Vec::new();
        for (name, value) in entries.iter() {
            let Value::Map(fields) = value else {
                return Err(RuntimeRecordError::Manifest);
            };
            let entry = match fields.get("state") {
                Some(Value::Text(state)) if state == "absent" && fields.len() == 1 => {
                    ManifestEntry::Absent
                }
                Some(Value::Text(state)) if state == "value" && fields.len() == 4 => {
                    let (Some(Value::Text(artifact)), Some(Value::Text(content)), Some(labels)) = (
                        fields.get("artifact_id"),
                        fields.get("content_digest"),
                        fields.get("security_labels"),
                    ) else {
                        return Err(RuntimeRecordError::Manifest);
                    };
                    ManifestEntry::Value {
                        artifact_id: artifact.parse().map_err(|_| RuntimeRecordError::Manifest)?,
                        content_digest: content
                            .parse()
                            .map_err(|_| RuntimeRecordError::Manifest)?,
                        security_labels: SecurityLabels::from_value(labels, limits)?,
                    }
                }
                _ => return Err(RuntimeRecordError::Manifest),
            };
            parsed.push((
                name.parse().map_err(|_| RuntimeRecordError::Manifest)?,
                entry,
            ));
        }
        let expected: Digest = expected.parse().map_err(|_| RuntimeRecordError::Manifest)?;
        let expected_influence = influence.clone();
        let restored = Self::freeze(ports, parsed, influence, limits)?;
        if restored.input_hash != expected || restored.influence != expected_influence {
            return Err(RuntimeRecordError::Integrity);
        }
        Ok(restored)
    }
}
fn entry_value(
    entry: &ManifestEntry,
    metadata: bool,
    limits: &Limits,
) -> Result<Value, RuntimeRecordError> {
    let fields = match entry {
        ManifestEntry::Value {
            artifact_id,
            content_digest,
            security_labels,
        } => {
            let mut fields = vec![
                ("state".into(), Value::Text("value".into())),
                ("artifact_id".into(), Value::Text(artifact_id.to_string())),
            ];
            if metadata {
                fields.push((
                    "content_digest".into(),
                    Value::Text(content_digest.to_string()),
                ));
                fields.push(("security_labels".into(), security_labels.to_value(limits)?));
            }
            fields
        }
        ManifestEntry::Absent => vec![("state".into(), Value::Text("absent".into()))],
    };
    let value = Value::Map(Map::try_from_entries(fields)?);
    RecordAccounting::new(limits)?.value(&value, 0)?;
    Ok(value)
}

/// Source availability for reduction; unavailable is never optional absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// Producer has not committed an outcome.
    Pending,
    /// Producer committed an accepted artifact.
    Value(ArtifactId),
    /// Optional omission or skipped producer.
    Absent,
    /// Failed/cancelled producer, with immutable causal outcome.
    Unavailable(RuntimeFailure),
}

/// Tagged host input. Artifact references do not become application handles.
#[derive(Clone, PartialEq)]
pub enum InputValue {
    /// Direct native value, including ordinary JSON objects and present null.
    Inline(Value),
    /// Existing accepted artifact requiring authorization/integrity/type checks.
    Artifact(ArtifactId),
}
impl fmt::Debug for InputValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline(_) => f.write_str("InputValue::Inline([redacted])"),
            Self::Artifact(id) => f.debug_tuple("InputValue::Artifact").field(id).finish(),
        }
    }
}
impl InputValue {
    /// Decodes the closed `kind` variant without interpreting ordinary object data
    /// as an artifact handle or granting the referenced artifact any authority.
    ///
    /// # Errors
    /// Returns unknown/extra fields, invalid digest or complete value limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, RuntimeRecordError> {
        RecordAccounting::new(limits)?.value(value, 0)?;
        let Value::Map(fields) = value else {
            return Err(RuntimeRecordError::Input);
        };
        if fields.len() != 2 {
            return Err(RuntimeRecordError::Input);
        }
        match fields.get("kind") {
            Some(Value::Text(kind)) if kind == "inline" => fields
                .get("value")
                .cloned()
                .map(Self::Inline)
                .ok_or(RuntimeRecordError::Input),
            Some(Value::Text(kind)) if kind == "artifact" => match fields.get("artifact_id") {
                Some(Value::Text(id)) => id
                    .parse()
                    .map(Self::Artifact)
                    .map_err(|_| RuntimeRecordError::Input),
                _ => Err(RuntimeRecordError::Input),
            },
            _ => Err(RuntimeRecordError::Input),
        }
    }
    /// Encodes the explicit host variant under complete record limits.
    ///
    /// # Errors
    /// Returns byte/depth or codec failures before copying inline value data.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, RuntimeRecordError> {
        let mut accounting = RecordAccounting::new(limits)?;
        accounting.collection(2, 0)?;
        accounting.text("kind", 1)?;
        let fields = match self {
            Self::Inline(value) => {
                accounting.text("inline", 1)?;
                accounting.text("value", 1)?;
                accounting.value(value, 1)?;
                vec![
                    ("kind".into(), Value::Text("inline".into())),
                    ("value".into(), value.clone()),
                ]
            }
            Self::Artifact(id) => {
                accounting.text("artifact", 1)?;
                accounting.text("artifact_id", 1)?;
                accounting.text(&id.to_string(), 1)?;
                vec![
                    ("kind".into(), Value::Text("artifact".into())),
                    ("artifact_id".into(), Value::Text(id.to_string())),
                ]
            }
        };
        Ok(Value::Map(Map::try_from_entries(fields)?))
    }
}

/// Runtime metadata conversion/admission failure, retaining no submitted values.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeRecordError {
    /// Input manifest is incomplete, duplicated, unknown or missing a required value.
    Manifest,
    /// Host input variant is malformed.
    Input,
    /// Stored content-equivalence metadata differs from its frozen hash.
    Integrity,
    /// Codec configuration/allocation failed.
    Codec(cbor::Error),
    /// Aggregate encoded metadata byte/depth limit failed.
    Encoding(EncodingLimitError),
    /// Canonical identity preimage failed.
    Identity(IdentityError),
    /// Port/type metadata representation failed.
    Type(htlk_executable::TypeError),
}
impl fmt::Display for RuntimeRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest => f.write_str("input manifest does not exactly match admitted ports"),
            Self::Input => f.write_str("invalid tagged runtime input"),
            Self::Integrity => f.write_str("runtime checkpoint hash mismatch"),
            Self::Codec(e) => write!(f, "runtime record encoding: {e}"),
            Self::Encoding(e) => write!(f, "runtime record: {e}"),
            Self::Identity(e) => write!(f, "runtime record identity: {e}"),
            Self::Type(e) => write!(f, "runtime record type: {e}"),
        }
    }
}
impl std::error::Error for RuntimeRecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest | Self::Input | Self::Integrity => None,
            Self::Codec(e) => Some(e),
            Self::Encoding(e) => Some(e),
            Self::Identity(e) => Some(e),
            Self::Type(e) => Some(e),
        }
    }
}
impl From<cbor::Error> for RuntimeRecordError {
    fn from(e: cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<EncodingLimitError> for RuntimeRecordError {
    fn from(e: EncodingLimitError) -> Self {
        Self::Encoding(e)
    }
}
impl From<IdentityError> for RuntimeRecordError {
    fn from(e: IdentityError) -> Self {
        Self::Identity(e)
    }
}
impl From<htlk_executable::TypeError> for RuntimeRecordError {
    fn from(e: htlk_executable::TypeError) -> Self {
        Self::Type(e)
    }
}
