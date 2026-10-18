//! Detached, protected source locations and descriptions, outside execution identity.
use crate::{
    Authorization, HostContext, Runtime, RuntimeError, failure::FailureCode,
    records::SecurityLabels,
};
use htlk_executable::{
    CanonicalDocument, ExecutableEnvelope, JsonPointer,
    cbor::{self, Limits, Map, Value},
    digest::{Digest, hash_bytes},
    record_accounting::RecordAccounting,
};
/// Authored source identity or an explicitly generated record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceOrigin {
    /// Standalone source, identified by the sidecar provenance digest.
    Inline,
    /// Exact source-package module/file mapping.
    Module {
        /// Immutable source package identity.
        package_digest: Digest,
        /// Logical module key.
        module_path: String,
        /// Manifest-mapped source file key.
        source_path: String,
    },
    /// Generated wrapper/binding, with no invented authored byte span.
    Synthetic {
        /// Human-readable generation reason, never executable instructions.
        reason: String,
    },
}
/// One canonical-record-to-source association. Several origins may share a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEntry {
    /// RFC 6901 pointer into the canonical executable document.
    pub canonical_path: JsonPointer,
    /// Immutable authored location or generated marker.
    pub origin: SourceOrigin,
    /// Exclusive UTF-8 byte range for authored locations; None for synthetic ones.
    pub span: Option<(u64, u64)>,
    /// Original declaration, when available.
    pub declaration_name: Option<String>,
    /// Nonsemantic description, separate from graph code and failure messages.
    pub description: Option<String>,
}
/// Immutable host source report, independently keyed by provenance and graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSidecar {
    /// Source package/report identity, independent of executable fingerprint.
    pub provenance: Digest,
    /// Bounded location/description inventory; multiple source origins are retained.
    pub entries: Vec<SourceEntry>,
}
/// Authorized detached source attachment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAttachment {
    /// Immutable digest of the protected stored attachment.
    pub id: Digest,
    /// Actual authenticated publisher, not a source-authored claim.
    pub attached_by: String,
    /// Frozen trusted source metadata protection.
    pub labels: SecurityLabels,
    /// Original source report.
    pub sidecar: SourceSidecar,
}
/// A bounded provenance cursor page; denied source metadata is omitted.
#[derive(Clone, Debug)]
pub struct SourcePage {
    /// Authorized source reports from this page.
    pub sources: Vec<SourceAttachment>,
    /// Exclusive opaque provenance cursor; None ends this pass.
    pub next: Option<Digest>,
}
impl Runtime {
    /// Attaches source diagnostics to an already registered graph without changing
    /// its envelope/fingerprint, producer trust, execution records or error values.
    ///
    /// # Errors
    /// Returns policy, invalid pointer/span/report, unknown graph or integrity errors.
    pub async fn attach_source(
        &self,
        context: &HostContext,
        fingerprint: Digest,
        source: &SourceSidecar,
    ) -> Result<Digest, RuntimeError> {
        self.attach_source_with_labels(context, fingerprint, source, &SecurityLabels::default())
            .await
    }
    async fn attach_source_with_labels(
        &self,
        context: &HostContext,
        fingerprint: Digest,
        source: &SourceSidecar,
        inherited: &SecurityLabels,
    ) -> Result<Digest, RuntimeError> {
        let encoded = source.to_value(&self.config.limits)?;
        let labels = self
            .policy
            .source_labels(context, fingerprint, source)?
            .union(inherited, &self.config.limits)?;
        self.policy.authorize(
            context,
            Authorization::AttachSource {
                fingerprint,
                source,
                labels: &labels,
            },
        )?;
        let (_, document) = self.source_document(fingerprint).await?;
        let wire = document
            .to_value(&self.config.limits)
            .map_err(|_| invalid())?;
        for entry in &source.entries {
            entry
                .canonical_path
                .resolve_value(&wire)
                .map_err(|_| invalid())?;
        }
        let payload = object([
            ("kind", Value::Text("htlk.source-map".into())),
            ("version", Value::Text("0.1".into())),
            ("fingerprint", Value::Text(fingerprint.to_string())),
            ("attached_by", Value::Text(context.principal().into())),
            ("labels", labels.to_value(&self.config.limits)?),
            ("source", encoded),
        ])?;
        let id = hash_bytes(&cbor::encode(&payload, &self.config.limits)?);
        self.store
            .attach_provenance(fingerprint, id, &payload)
            .await?;
        Ok(id)
    }
    /// Inspects a bounded source page under current graph/source grants. Historical
    /// source reports do not require loading their original native implementations.
    ///
    /// # Errors
    /// Returns graph denial, invalid bounds/cursor, malformed metadata or storage errors.
    pub async fn inspect_sources(
        &self,
        context: &HostContext,
        fingerprint: Digest,
        after: Option<Digest>,
        page_size: usize,
    ) -> Result<SourcePage, RuntimeError> {
        self.policy
            .authorize(context, Authorization::InspectGraph { fingerprint })?;
        let records = self.store.provenance(fingerprint, after, page_size).await?;
        let next = records.last().map(|(id, _)| *id);
        let mut sources = Vec::new();
        for (id, value) in records {
            let Value::Map(fields) = &value else { continue };
            if fields.get("kind") != Some(&Value::Text("htlk.source-map".into())) {
                continue;
            }
            if fields.get("version") != Some(&Value::Text("0.1".into()))
                || fields.get("fingerprint") != Some(&Value::Text(fingerprint.to_string()))
            {
                return Err(corrupt());
            }
            let labels = SecurityLabels::from_value(field(fields, "labels")?, &self.config.limits)?;
            if self
                .policy
                .authorize(
                    context,
                    Authorization::ReadSource {
                        fingerprint,
                        provenance: id,
                        labels: &labels,
                    },
                )
                .is_err()
            {
                continue;
            }
            sources.push(SourceAttachment {
                id,
                attached_by: text(field(fields, "attached_by")?)?.into(),
                labels,
                sidecar: SourceSidecar::from_value(field(fields, "source")?, &self.config.limits)?,
            });
        }
        Ok(SourcePage { sources, next })
    }
    /// Carries authorized source annotations for byte-identical canonical targets
    /// into another registered graph, preserving their original protection. Changed
    /// roots/ports are excluded; hosts explicitly attach generated wrapper markers.
    ///
    /// # Errors
    /// Returns graph/source denial, target mismatch, bounds or storage failures.
    pub async fn inherit_sources(
        &self,
        context: &HostContext,
        from: Digest,
        to: Digest,
    ) -> Result<Vec<Digest>, RuntimeError> {
        self.policy
            .authorize(context, Authorization::InspectGraph { fingerprint: from })?;
        self.policy
            .authorize(context, Authorization::InspectGraph { fingerprint: to })?;
        if from == to {
            self.source_document(from).await?;
            return Ok(vec![]);
        }
        let (_, old) = self.source_document(from).await?;
        let (_, new) = self.source_document(to).await?;
        let old = old.to_value(&self.config.limits).map_err(|_| corrupt())?;
        let new = new.to_value(&self.config.limits).map_err(|_| corrupt())?;
        let mut after = None;
        let mut ids = Vec::new();
        let mut accounting = RecordAccounting::new(&self.config.limits)?;
        loop {
            let page = self
                .inspect_sources(context, from, after, self.store.config().max_page_size)
                .await?;
            for attachment in page.sources {
                let mut sidecar = attachment.sidecar;
                sidecar.entries.retain(|e| {
                    match (
                        e.canonical_path.resolve_value(&old),
                        e.canonical_path.resolve_value(&new),
                    ) {
                        (Ok(a), Ok(b)) => a == b,
                        _ => false,
                    }
                });
                if !sidecar.entries.is_empty() {
                    let id = self
                        .attach_source_with_labels(context, to, &sidecar, &attachment.labels)
                        .await?;
                    accounting.text(&id.to_string(), 0).map_err(|_| invalid())?;
                    ids.push(id);
                }
            }
            if page.next.is_none() {
                break;
            }
            after = page.next;
        }
        Ok(ids)
    }
    pub(crate) async fn source_document(
        &self,
        fingerprint: Digest,
    ) -> Result<(Vec<u8>, CanonicalDocument), RuntimeError> {
        let bytes = self.store.graph_bytes(fingerprint).await?;
        let envelope =
            ExecutableEnvelope::decode(&bytes, &self.config.limits).map_err(|_| corrupt())?;
        if envelope.fingerprint() != fingerprint {
            return Err(corrupt());
        }
        let document = CanonicalDocument::decode(envelope.payload(), &self.config.limits)
            .map_err(|_| corrupt())?;
        Ok((bytes, document))
    }
}
impl SourceSidecar {
    /// Encodes a complete bounded detached report; it is never executable data.
    ///
    /// # Errors
    /// Returns invalid authored/synthetic spans or complete representation limits.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, RuntimeError> {
        let mut preflight = RecordAccounting::new(limits)?;
        preflight
            .collection(self.entries.len(), 0)
            .map_err(|_| invalid())?;
        for entry in &self.entries {
            for token in entry.canonical_path.tokens() {
                preflight.text(token, 1).map_err(|_| invalid())?;
            }
            match &entry.origin {
                SourceOrigin::Module {
                    module_path,
                    source_path,
                    ..
                } => preflight
                    .text(module_path, 1)
                    .and_then(|()| preflight.text(source_path, 1))
                    .map_err(|_| invalid())?,
                SourceOrigin::Synthetic { reason } => {
                    preflight.text(reason, 1).map_err(|_| invalid())?
                }
                SourceOrigin::Inline => {}
            }
            for text in [entry.declaration_name.as_ref(), entry.description.as_ref()]
                .into_iter()
                .flatten()
            {
                preflight.text(text, 1).map_err(|_| invalid())?;
            }
        }
        let mut accounting = RecordAccounting::new(limits)?;
        accounting
            .collection(self.entries.len(), 1)
            .map_err(|_| invalid())?;
        let mut entries = Vec::new();
        for entry in &self.entries {
            let origin = match &entry.origin {
                SourceOrigin::Inline => Value::Array(vec![Value::Text("inline".into())]),
                SourceOrigin::Module {
                    package_digest,
                    module_path,
                    source_path,
                } => Value::Array(vec![
                    Value::Text("module".into()),
                    Value::Text(package_digest.to_string()),
                    Value::Text(module_path.clone()),
                    Value::Text(source_path.clone()),
                ]),
                SourceOrigin::Synthetic { reason } => Value::Array(vec![
                    Value::Text("synthetic".into()),
                    Value::Text(reason.clone()),
                ]),
            };
            let span = match (entry.span, &entry.origin) {
                (None, SourceOrigin::Synthetic { .. }) => Value::Null,
                (Some((start, end)), origin)
                    if !matches!(origin, SourceOrigin::Synthetic { .. })
                        && start <= end
                        && end <= i64::MAX as u64 =>
                {
                    Value::Array(vec![
                        Value::Integer(start as i64),
                        Value::Integer(end as i64),
                    ])
                }
                _ => return Err(invalid()),
            };
            let value = object([
                (
                    "canonical_path",
                    Value::Text(entry.canonical_path.to_string()),
                ),
                ("origin", origin),
                ("span", span),
                (
                    "declaration_name",
                    entry
                        .declaration_name
                        .clone()
                        .map_or(Value::Null, Value::Text),
                ),
                (
                    "description",
                    entry.description.clone().map_or(Value::Null, Value::Text),
                ),
            ])?;
            accounting.value(&value, 2).map_err(|_| invalid())?;
            entries.push(value);
        }
        let value = object([
            ("provenance", Value::Text(self.provenance.to_string())),
            ("entries", Value::Array(entries)),
        ])?;
        cbor::encode(&value, limits)?;
        Ok(value)
    }
    /// Decodes an authoritative report, retaining exact source identities and spans.
    ///
    /// # Errors
    /// Returns malformed metadata/version/identity or complete limits.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, RuntimeError> {
        cbor::encode(value, limits)?;
        let Value::Map(fields) = value else {
            return Err(corrupt());
        };
        if fields.len() != 2 {
            return Err(corrupt());
        }
        let provenance = text(field(fields, "provenance")?)?
            .parse()
            .map_err(|_| corrupt())?;
        let Value::Array(values) = field(fields, "entries")? else {
            return Err(corrupt());
        };
        let mut entries = Vec::new();
        for value in values {
            let Value::Map(f) = value else {
                return Err(corrupt());
            };
            if f.len() != 5 {
                return Err(corrupt());
            }
            let Value::Array(origin) = field(f, "origin")? else {
                return Err(corrupt());
            };
            let origin = match origin.as_slice() {
                [Value::Text(kind)] if kind == "inline" => SourceOrigin::Inline,
                [Value::Text(kind), package, module, path] if kind == "module" => {
                    SourceOrigin::Module {
                        package_digest: text(package)?.parse().map_err(|_| corrupt())?,
                        module_path: text(module)?.into(),
                        source_path: text(path)?.into(),
                    }
                }
                [Value::Text(kind), reason] if kind == "synthetic" => SourceOrigin::Synthetic {
                    reason: text(reason)?.into(),
                },
                _ => return Err(corrupt()),
            };
            let span = match field(f, "span")? {
                Value::Null => None,
                Value::Array(a) => match a.as_slice() {
                    [Value::Integer(start), Value::Integer(end)] if *start >= 0 && end >= start => {
                        Some((*start as u64, *end as u64))
                    }
                    _ => return Err(corrupt()),
                },
                _ => return Err(corrupt()),
            };
            let optional = |name| -> Result<Option<String>, RuntimeError> {
                Ok(match field(f, name)? {
                    Value::Null => None,
                    value => Some(text(value)?.into()),
                })
            };
            entries.push(SourceEntry {
                canonical_path: JsonPointer::new(text(field(f, "canonical_path")?)?, limits)
                    .map_err(|_| corrupt())?,
                origin,
                span,
                declaration_name: optional("declaration_name")?,
                description: optional("description")?,
            });
        }
        let source = Self {
            provenance,
            entries,
        };
        source.to_value(limits).map_err(|_| corrupt())?;
        Ok(source)
    }
}
fn invalid() -> RuntimeError {
    RuntimeError::failure(FailureCode::Input)
}
fn corrupt() -> RuntimeError {
    RuntimeError::failure(FailureCode::Digest)
}
fn field<'a>(map: &'a Map, name: &str) -> Result<&'a Value, RuntimeError> {
    map.get(name).ok_or_else(corrupt)
}
fn text(value: &Value) -> Result<&str, RuntimeError> {
    if let Value::Text(value) = value {
        Ok(value)
    } else {
        Err(corrupt())
    }
}
fn object<const N: usize>(entries: [(&str, Value); N]) -> Result<Value, RuntimeError> {
    Ok(Value::Map(Map::try_from_entries(
        entries.into_iter().map(|(k, v)| (k.into(), v)),
    )?))
}
