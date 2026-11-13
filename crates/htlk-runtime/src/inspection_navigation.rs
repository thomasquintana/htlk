//! Selected-record interpretation and bounded authored/recorded navigation.
use super::*;
use crate::artifact::ArtifactProducer;
use htlk_executable::{EdgeDestination, EdgeSource, Operation};

pub(super) const FAMILIES: &[RecordFamily] = &[
    RecordFamily::Scope,
    RecordFamily::Invocation,
    RecordFamily::Guard,
    RecordFamily::Binding,
    RecordFamily::Manifest,
    RecordFamily::Attempt,
    RecordFamily::Response,
    RecordFamily::Wait,
    RecordFamily::Reply,
    RecordFamily::Approval,
    RecordFamily::Reservation,
    RecordFamily::Usage,
    RecordFamily::Artifact,
    RecordFamily::Cancellation,
    RecordFamily::Extension,
    RecordFamily::Loop,
    RecordFamily::Timer,
    RecordFamily::Observation,
    RecordFamily::Tombstone,
];

fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Map(m) => m.get(name),
        _ => None,
    }
}
fn text<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    match field(v, name) {
        Some(Value::Text(s)) => Some(s),
        _ => None,
    }
}
fn required_text<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    text(v, name).ok_or(InspectionError::Corrupt)
}
fn digest(v: &Value, name: &str) -> Result<Digest> {
    required_text(v, name)?
        .parse()
        .map_err(|_| InspectionError::Corrupt)
}
fn number(v: &Value, name: &str) -> Option<u64> {
    match field(v, name) {
        Some(Value::Integer(n)) => u64::try_from(*n).ok(),
        _ => None,
    }
}
fn record(family: RecordFamily, id: impl ToString) -> EntityId {
    EntityId::Record(RecordKey {
        family,
        id: id.to_string(),
    })
}
fn status(record: &StoredRecord) -> Option<&'static str> {
    let v = &record.payload;
    match record.key.family {
        RecordFamily::Attempt => {
            return match text(v, "dispatch_state")? {
                "prepared" => Some("prepared"),
                "possibly_sent" => Some("possibly_sent"),
                "response_recorded" => Some("response_recorded"),
                _ => None,
            };
        }
        RecordFamily::Wait => {
            return match text(v, "state")? {
                "open" => Some("open"),
                "accepted" => Some("accepted"),
                "expired" => Some("expired"),
                "cancelled" => Some("cancelled"),
                _ => None,
            };
        }
        _ => {}
    }
    match text(v, "status").or_else(|| text(v, "state"))? {
        "pending" => Some("pending"),
        "active" => Some("active"),
        "running" => Some("running"),
        "succeeded" => Some("succeeded"),
        "failed" => Some("failed"),
        "skipped" => Some("skipped"),
        "cancelled" => Some("cancelled"),
        "open" => Some("open"),
        "closed" => Some("closed"),
        "expired" => Some("expired"),
        "value" => Some("value"),
        "absent" => Some("absent"),
        _ => None,
    }
}

impl Inspector {
    pub(super) async fn load(
        &self,
        snapshot: &Snapshot,
        reference: &Reference,
    ) -> Result<Selected> {
        let mut selected = Selected {
            record: None,
            event: None,
            definition: None,
            identity: None,
            protection: Protection::default(),
            expired_at_ms: None,
        };
        match &reference.entity {
            EntityId::Run => {}
            EntityId::Event(sequence) => {
                if *sequence <= 0 {
                    return Err(InspectionError::InvalidReference);
                }
                let mut events = self
                    .store
                    .inspection_journal(&snapshot.stored, sequence - 1, 1)
                    .await?;
                let event = events
                    .pop()
                    .filter(|e| e.sequence == *sequence)
                    .ok_or(InspectionError::Unknown)?;
                return self.selected_event(snapshot, event).await;
            }
            EntityId::Record(key) => {
                let stored = self.store.inspection_record(&snapshot.stored, key).await?;
                selected.protection.labels = self.record_labels(snapshot, &stored).await?;
                if key.family == RecordFamily::Artifact {
                    let id = key.id.parse().map_err(|_| InspectionError::Corrupt)?;
                    let state = self.store.inspection_artifact(&snapshot.stored, id).await?;
                    selected.expired_at_ms = state.expired_at_ms;
                    selected.protection.labels = state.metadata.security_labels().clone();
                    selected.protection.artifact = Some(state.metadata);
                }
                if matches!(key.family, RecordFamily::Scope | RecordFamily::Invocation) {
                    selected.identity = Some(self.instance_identity(snapshot, &stored).await?);
                }
                if key.family == RecordFamily::Scope && scope_use(&stored.payload).is_none() {
                    return Err(InspectionError::Corrupt);
                }
                selected.record = Some(stored);
            }
            entity => {
                let executable = match entity {
                    EntityId::Graph(id) => *id,
                    EntityId::ScopeDefinition { executable, .. }
                    | EntityId::NodeDefinition { executable, .. }
                    | EntityId::EdgeDefinition { executable, .. } => *executable,
                    _ => unreachable!(),
                };
                self.check_graph_membership(snapshot, reference, executable)
                    .await?;
                let doc = self.document(executable).await?;
                let (value, id) = self.definition_value(&doc, entity)?;
                selected.definition = Some(value);
                selected.identity = Some(DefinitionIdentity {
                    executable,
                    digest: id,
                    definition: entity.clone(),
                    canonical_path: self.canonical_path(&doc, entity)?,
                });
            }
        }
        Ok(selected)
    }

    async fn check_graph_membership(
        &self,
        snapshot: &Snapshot,
        reference: &Reference,
        executable: Digest,
    ) -> Result<()> {
        if executable == snapshot.stored.header().fingerprint {
            return Ok(());
        }
        let witness = reference
            .witness
            .as_ref()
            .ok_or(InspectionError::InvalidReference)?;
        let witness = self
            .store
            .inspection_record(&snapshot.stored, witness)
            .await?;
        let member = match witness.key.family {
            RecordFamily::Scope | RecordFamily::Invocation => {
                text(&witness.payload, "origin_executable") == Some(executable.to_string().as_str())
            }
            RecordFamily::Extension => ["previous_fingerprint", "active_fingerprint"]
                .into_iter()
                .any(|key| text(&witness.payload, key) == Some(executable.to_string().as_str())),
            _ => false,
        };
        if !member {
            return Err(InspectionError::InvalidReference);
        }
        Ok(())
    }

    fn definition_value(
        &self,
        doc: &CanonicalDocument,
        entity: &EntityId,
    ) -> Result<(Value, Option<Digest>)> {
        let invalid = || InspectionError::Corrupt;
        match entity {
            EntityId::Graph(id) => Ok((
                doc.to_value(self.limits()).map_err(|_| invalid())?,
                Some(*id),
            )),
            EntityId::ScopeDefinition { scope, .. } => {
                let definition = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                Ok((
                    definition.to_value(self.limits()).map_err(|_| invalid())?,
                    Some(*scope),
                ))
            }
            EntityId::NodeDefinition { scope, node, .. } => {
                let scope = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                let node = scope
                    .fields()
                    .nodes
                    .iter()
                    .find(|n| n.id() == node)
                    .ok_or(InspectionError::Unknown)?;
                Ok((
                    node.to_value(self.limits()).map_err(|_| invalid())?,
                    Some(node.digest(self.limits()).map_err(|_| invalid())?),
                ))
            }
            EntityId::EdgeDefinition { scope, edge, .. } => {
                let scope = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                let edge = scope
                    .fields()
                    .edges
                    .iter()
                    .find(|e| e.id() == edge)
                    .ok_or(InspectionError::Unknown)?;
                let value = edge.to_value(self.limits()).map_err(|_| invalid())?;
                Ok((value, None))
            }
            _ => Err(InspectionError::InvalidRequest),
        }
    }

    async fn instance_identity(
        &self,
        snapshot: &Snapshot,
        stored: &StoredRecord,
    ) -> Result<DefinitionIdentity> {
        let executable = digest(&stored.payload, "origin_executable")?;
        let id = digest(&stored.payload, "definition_digest")?;
        let entity = if stored.key.family == RecordFamily::Scope {
            EntityId::ScopeDefinition {
                executable,
                scope: id,
            }
        } else {
            let scope = self
                .store
                .inspection_record(
                    &snapshot.stored,
                    &RecordKey {
                        family: RecordFamily::Scope,
                        id: required_text(&stored.payload, "scope_instance_id")?.into(),
                    },
                )
                .await?;
            if digest(&scope.payload, "origin_executable")? != executable {
                return Err(InspectionError::Corrupt);
            }
            EntityId::NodeDefinition {
                executable,
                scope: digest(&scope.payload, "definition_digest")?,
                node: required_text(&stored.payload, "local_id")?
                    .parse()
                    .map_err(|_| InspectionError::Corrupt)?,
            }
        };
        // Canonical identity verification is independent of native implementations.
        let doc = self.document(executable).await?;
        let (_, actual) = self.definition_value(&doc, &entity)?;
        if actual != Some(id) {
            return Err(InspectionError::Corrupt);
        }
        Ok(DefinitionIdentity {
            executable,
            digest: Some(id),
            canonical_path: self.canonical_path(&doc, &entity)?,
            definition: entity,
        })
    }

    fn canonical_path(&self, doc: &CanonicalDocument, entity: &EntityId) -> Result<JsonPointer> {
        let path = match entity {
            EntityId::Graph(_) => String::new(),
            EntityId::ScopeDefinition { scope, .. } => format!("/scopes/{scope}"),
            EntityId::NodeDefinition { scope, node, .. } => {
                let definition = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                let index = definition
                    .fields()
                    .nodes
                    .iter()
                    .position(|n| n.id() == node)
                    .ok_or(InspectionError::Unknown)?;
                format!("/scopes/{scope}/nodes/{index}")
            }
            EntityId::EdgeDefinition { scope, edge, .. } => {
                let definition = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                let index = definition
                    .fields()
                    .edges
                    .iter()
                    .position(|e| e.id() == edge)
                    .ok_or(InspectionError::Unknown)?;
                format!("/scopes/{scope}/edges/{index}")
            }
            _ => return Err(InspectionError::InvalidRequest),
        };
        JsonPointer::new(&path, self.limits()).map_err(|_| InspectionError::Corrupt)
    }

    pub(super) async fn selected_event(
        &self,
        snapshot: &Snapshot,
        event: JournalEntry,
    ) -> Result<Selected> {
        let mut labels = Vec::new();
        for write in event_writes(&event)? {
            let (Value::Text(family), Value::Text(id)) = (&write[0], &write[1]) else {
                return Err(InspectionError::Corrupt);
            };
            let family = FAMILIES
                .iter()
                .copied()
                .find(|f| f.as_str() == family)
                .ok_or(InspectionError::Corrupt)?;
            // Owner protection is selected at the same snapshot, never from the
            // independently changing latest record or MCP adjunct state.
            let record = StoredRecord {
                key: RecordKey {
                    family,
                    id: id.clone(),
                },
                revision: 1,
                final_record: false,
                payload: write[4].clone(),
            };
            labels.push(self.record_labels(snapshot, &record).await?);
        }
        let labels = SecurityLabels::combine(&labels, self.limits())
            .map_err(|_| InspectionError::Corrupt)?;
        Ok(Selected {
            record: None,
            event: Some(event),
            definition: None,
            identity: None,
            protection: Protection {
                labels,
                artifact: None,
            },
            expired_at_ms: None,
        })
    }

    fn explicit_labels(&self, value: &Value) -> Result<SecurityLabels> {
        // Only coordinator-owned top-level fields. Application values may contain
        // identically named keys, but can never assign source protection.
        let mut labels = Vec::new();
        for name in ["influence_labels", "security_labels"] {
            if let Some(value) = field(value, name) {
                labels.push(
                    SecurityLabels::from_value(value, self.limits())
                        .map_err(|_| InspectionError::Corrupt)?,
                );
            }
        }
        SecurityLabels::combine(&labels, self.limits()).map_err(|_| InspectionError::Corrupt)
    }

    async fn record_labels(
        &self,
        snapshot: &Snapshot,
        record: &StoredRecord,
    ) -> Result<SecurityLabels> {
        let own = self.explicit_labels(&record.payload)?;
        if matches!(
            record.key.family,
            RecordFamily::Artifact | RecordFamily::Scope | RecordFamily::Invocation
        ) {
            return Ok(own);
        }
        let owner = text(&record.payload, "invocation_id")
            .map(|id| RecordKey {
                family: RecordFamily::Invocation,
                id: id.into(),
            })
            .or_else(|| {
                text(&record.payload, "scope_instance_id").map(|id| RecordKey {
                    family: RecordFamily::Scope,
                    id: id.into(),
                })
            });
        let Some(owner) = owner else {
            return Ok(own);
        };
        let owner = self
            .store
            .inspection_record(&snapshot.stored, &owner)
            .await
            .map_err(|error| {
                if error == StoreError::Missing {
                    InspectionError::Corrupt
                } else {
                    error.into()
                }
            })?;
        own.union(&self.explicit_labels(&owner.payload)?, self.limits())
            .map_err(|_| InspectionError::Corrupt)
    }

    pub(super) fn summary(
        &self,
        snapshot: &Snapshot,
        reference: Reference,
        selected: &Selected,
    ) -> EntityInspection {
        let payload = selected.record.as_ref().map(|r| &r.payload);
        EntityInspection {
            run_status: (reference.entity == EntityId::Run)
                .then_some(snapshot.stored.header().status),
            sequence: (reference.entity == EntityId::Run)
                .then_some(snapshot.stored.header().sequence),
            reference,
            protection: selected.protection.clone(),
            definition: selected.identity.clone(),
            revision: selected.record.as_ref().map(|r| r.revision),
            final_record: selected.record.as_ref().map(|r| r.final_record),
            status: selected.record.as_ref().and_then(status),
            iteration: payload.and_then(|v| number(v, "iteration")),
            extension_number: payload.and_then(|v| number(v, "extension_number")),
            scope_use: selected
                .record
                .as_ref()
                .filter(|r| r.key.family == RecordFamily::Scope)
                .and_then(|r| scope_use(&r.payload)),
            event: selected.event.as_ref().map(|e| e.event),
            expired_at_ms: selected.expired_at_ms,
            attempt_number: selected
                .record
                .as_ref()
                .filter(|r| r.key.family == RecordFamily::Attempt)
                .and_then(|r| number(&r.payload, "number")),
            generation: selected
                .record
                .as_ref()
                .filter(|r| r.key.family == RecordFamily::Wait)
                .and_then(|r| number(&r.payload, "generation")),
            deadline_ms: selected.record.as_ref().and_then(|r| match r.key.family {
                RecordFamily::Scope | RecordFamily::Invocation => number(&r.payload, "deadline_ms"),
                RecordFamily::Wait => number(&r.payload, "expires_at"),
                _ => None,
            }),
        }
    }

    pub(super) async fn candidates(
        &self,
        snapshot: &Snapshot,
        kind: &EntityKind,
        after: i64,
        limit: usize,
    ) -> Result<(Vec<Reference>, Option<i64>)> {
        match kind {
            EntityKind::Records(family) => {
                let rows = self
                    .store
                    .inspection_record_page(&snapshot.stored, *family, after, limit)
                    .await?;
                let next = rows.last().map(|r| r.cursor);
                Ok((
                    rows.into_iter()
                        .map(|r| snapshot.reference_for(EntityId::Record(r.record.key)))
                        .collect(),
                    next,
                ))
            }
            EntityKind::Events => {
                let rows = self
                    .store
                    .inspection_journal(&snapshot.stored, after, limit)
                    .await?;
                let next = rows.last().map(|r| r.sequence);
                Ok((
                    rows.into_iter()
                        .map(|r| snapshot.reference_for(EntityId::Event(r.sequence)))
                        .collect(),
                    next,
                ))
            }
            EntityKind::Run => Ok(slice(vec![snapshot.reference()], after, limit)),
            EntityKind::Graph => Ok(slice(
                vec![snapshot.reference_for(EntityId::Graph(snapshot.stored.header().fingerprint))],
                after,
                limit,
            )),
            EntityKind::Definitions => {
                let executable = snapshot.stored.header().fingerprint;
                let doc = self.document(executable).await?;
                let mut entities = Vec::new();
                for (scope, definition) in &doc.fields().scopes {
                    entities.push(snapshot.reference_for(EntityId::ScopeDefinition {
                        executable,
                        scope: *scope,
                    }));
                    for node in &definition.fields().nodes {
                        entities.push(snapshot.reference_for(EntityId::NodeDefinition {
                            executable,
                            scope: *scope,
                            node: node.id().clone(),
                        }));
                    }
                    for edge in &definition.fields().edges {
                        entities.push(snapshot.reference_for(EntityId::EdgeDefinition {
                            executable,
                            scope: *scope,
                            edge: edge.id().clone(),
                        }));
                    }
                }
                Ok(slice(entities, after, limit))
            }
        }
    }

    pub(super) async fn related(
        &self,
        snapshot: &Snapshot,
        source: &Reference,
        selected: &Selected,
        relation: RelationKind,
        after: i64,
        limit: usize,
    ) -> Result<(Vec<Reference>, Option<i64>)> {
        // Reverse containment is a bounded indexed family page. It does not load
        // a run-wide aggregate or scan until enough visible matches are found.
        let reverse = match (&source.entity, relation) {
            (EntityId::Record(k), RelationKind::ExecutionChild)
                if k.family == RecordFamily::Scope =>
            {
                Some((RecordFamily::Invocation, "scope_instance_id"))
            }
            (EntityId::Record(k), RelationKind::ExecutionChild)
                if k.family == RecordFamily::Invocation =>
            {
                Some((RecordFamily::Scope, "parent_invocation"))
            }
            (EntityId::Record(k), RelationKind::Attempts)
                if k.family == RecordFamily::Invocation =>
            {
                Some((RecordFamily::Attempt, "invocation_id"))
            }
            (EntityId::Record(k), RelationKind::Waits) if k.family == RecordFamily::Invocation => {
                Some((RecordFamily::Wait, "invocation_id"))
            }
            _ => None,
        };
        if relation == RelationKind::Bindings
            && let Some(owner) = &selected.record
            && owner.key.family == RecordFamily::Invocation
        {
            let prefix = format!(
                "{}:{}:",
                required_text(&owner.payload, "scope_instance_id")?,
                required_text(&owner.payload, "local_id")?
            );
            let candidates = self
                .store
                .inspection_record_page(&snapshot.stored, RecordFamily::Binding, after, limit)
                .await?;
            let next = candidates.last().map(|r| r.cursor);
            let targets = candidates
                .into_iter()
                .filter(|r| r.record.key.id.starts_with(&prefix))
                .map(|r| snapshot.reference_for(EntityId::Record(r.record.key)))
                .collect();
            return Ok((targets, next));
        }
        if let Some((family, field_name)) = reverse {
            let EntityId::Record(owner) = &source.entity else {
                unreachable!()
            };
            let candidates = self
                .store
                .inspection_record_page(&snapshot.stored, family, after, limit)
                .await?;
            let next = candidates.last().map(|r| r.cursor);
            let targets = candidates
                .into_iter()
                .filter(|r| text(&r.record.payload, field_name) == Some(owner.id.as_str()))
                .map(|r| snapshot.reference_for(EntityId::Record(r.record.key)))
                .collect();
            return Ok((targets, next));
        }
        let mut targets = Vec::new();
        let mut witness = source.witness.clone();
        match relation {
            RelationKind::AuthoredChild
            | RelationKind::AuthoredDataflow(_)
            | RelationKind::DefinitionUse => {
                targets = self.authored_targets(&source.entity, relation).await?;
            }
            RelationKind::ExecutionChild if source.entity == EntityId::Run => {
                targets.push(record(
                    RecordFamily::Scope,
                    snapshot.stored.header().root_scope,
                ));
            }
            RelationKind::InstanceOf => {
                if let Some(identity) = &selected.identity
                    && let EntityId::Record(key) = &source.entity
                {
                    targets.push(identity.definition.clone());
                    witness = Some(key.clone());
                }
            }
            RelationKind::OriginExecutable => {
                if let Some(identity) = &selected.identity {
                    targets.push(EntityId::Graph(identity.executable));
                    if let EntityId::Record(key) = &source.entity {
                        witness = Some(key.clone());
                    }
                }
            }
            RelationKind::ExecutionParent => {
                if let Some(stored) = &selected.record {
                    let parent = match stored.key.family {
                        RecordFamily::Scope => text(&stored.payload, "parent_invocation")
                            .map(|id| record(RecordFamily::Invocation, id)),
                        RecordFamily::Invocation => text(&stored.payload, "scope_instance_id")
                            .map(|id| record(RecordFamily::Scope, id)),
                        // Production responses share their attempt's key and do
                        // not duplicate invocation_id. The normal target lookup
                        // verifies and authorizes that attempt at this exact pin.
                        RecordFamily::Response => {
                            Some(record(RecordFamily::Attempt, &stored.key.id))
                        }
                        RecordFamily::Attempt
                        | RecordFamily::Wait
                        | RecordFamily::Reply
                        | RecordFamily::Approval => text(&stored.payload, "invocation_id")
                            .map(|id| record(RecordFamily::Invocation, id)),
                        _ => None,
                    };
                    targets.extend(parent);
                }
            }
            RelationKind::WaitRequest => {
                if let Some(stored) = &selected.record
                    && stored.key.family == RecordFamily::Wait
                    && let Some(id) = text(&stored.payload, "request_artifact_id")
                {
                    targets.push(record(RecordFamily::Artifact, id));
                }
            }
            RelationKind::Produces => {
                if let Some(stored) = &selected.record
                    && stored.key.family == RecordFamily::Invocation
                    && let Some(Value::Map(outputs)) = field(&stored.payload, "outputs")
                {
                    for (_, entry) in outputs.iter() {
                        if text(entry, "state") == Some("value") {
                            targets.push(record(
                                RecordFamily::Artifact,
                                required_text(entry, "artifact_id")?,
                            ));
                        }
                    }
                }
            }
            RelationKind::SelectedArtifact => {
                if let Some(stored) = &selected.record
                    && stored.key.family == RecordFamily::Binding
                    && text(&stored.payload, "state") == Some("value")
                {
                    targets.push(record(
                        RecordFamily::Artifact,
                        required_text(&stored.payload, "artifact_id")?,
                    ));
                }
            }
            RelationKind::SelectedEdge => {
                if let Some(stored) = &selected.record
                    && stored.key.family == RecordFamily::Binding
                    && let Some(edge) = text(&stored.payload, "selected_edge")
                {
                    // Runtime binding IDs are scope-instance:destination:port.
                    // Split from the right: digest spellings contain `sha256:`.
                    let scope_id = stored
                        .key
                        .id
                        .rsplitn(3, ':')
                        .nth(2)
                        .ok_or(InspectionError::Corrupt)?;
                    let scope_key = RecordKey {
                        family: RecordFamily::Scope,
                        id: scope_id.into(),
                    };
                    let scope = self
                        .store
                        .inspection_record(&snapshot.stored, &scope_key)
                        .await?;
                    targets.push(EntityId::EdgeDefinition {
                        executable: digest(&scope.payload, "origin_executable")?,
                        scope: digest(&scope.payload, "definition_digest")?,
                        edge: edge.parse().map_err(|_| InspectionError::Corrupt)?,
                    });
                    witness = Some(scope_key);
                }
            }
            RelationKind::Causes => {
                if let Some(stored) = &selected.record {
                    causes(&stored.payload, &mut targets);
                }
            }
            RelationKind::Provenance => {
                if let Some(metadata) = &selected.protection.artifact {
                    if let ArtifactProducer::NodeOutput { invocation_id, .. } = metadata.producer()
                    {
                        targets.push(record(RecordFamily::Invocation, invocation_id));
                    }
                    targets.extend(
                        metadata
                            .input_artifact_ids()
                            .iter()
                            .map(|id| record(RecordFamily::Artifact, id)),
                    );
                }
            }
            RelationKind::Reuses => {
                if let Some(stored) = &selected.record
                    && stored.key.family == RecordFamily::Extension
                {
                    targets.push(record(
                        RecordFamily::Scope,
                        required_text(&stored.payload, "preserved_scope")?,
                    ));
                    targets.push(record(
                        RecordFamily::Invocation,
                        required_text(&stored.payload, "preserved_wrapper")?,
                    ));
                    targets.push(EntityId::Graph(digest(
                        &stored.payload,
                        "previous_fingerprint",
                    )?));
                    witness = Some(stored.key.clone());
                }
            }
            _ => {}
        }
        // Targets are bounded by one selected record or one decoded canonical
        // document. Stable encounter ordering is independent of viewer grants.
        let mut seen = std::collections::BTreeSet::new();
        targets.retain(|entity| seen.insert(entity.clone()));
        let (targets, next) = slice(targets, after, limit);
        let mut references = Vec::new();
        for entity in targets {
            let mut reference = if let EntityId::Record(key) = &entity
                && key.family == RecordFamily::Artifact
            {
                let id = key.id.parse().map_err(|_| InspectionError::Corrupt)?;
                match self
                    .store
                    .inspection_artifact_origin(snapshot.position(), id)
                    .await
                {
                    Ok(stored) => Snapshot { stored }.reference_for(entity),
                    Err(StoreError::Missing) => continue,
                    Err(error) => return Err(error.into()),
                }
            } else {
                snapshot.reference_for(entity)
            };
            if matches!(
                reference.entity,
                EntityId::Graph(_)
                    | EntityId::ScopeDefinition { .. }
                    | EntityId::NodeDefinition { .. }
                    | EntityId::EdgeDefinition { .. }
            ) {
                reference.witness = witness.clone();
            }
            references.push(reference);
        }
        Ok((references, next))
    }

    async fn authored_targets(
        &self,
        entity: &EntityId,
        relation: RelationKind,
    ) -> Result<Vec<EntityId>> {
        let (executable, scope) = match entity {
            EntityId::Graph(id) => (*id, None),
            EntityId::ScopeDefinition { executable, scope }
            | EntityId::NodeDefinition {
                executable, scope, ..
            }
            | EntityId::EdgeDefinition {
                executable, scope, ..
            } => (*executable, Some(*scope)),
            _ => return Ok(Vec::new()),
        };
        let doc = self.document(executable).await?;
        let mut targets = Vec::new();
        match (entity, relation) {
            (EntityId::Graph(_), RelationKind::AuthoredChild) => {
                targets.extend(
                    doc.fields()
                        .scopes
                        .keys()
                        .map(|scope| EntityId::ScopeDefinition {
                            executable,
                            scope: *scope,
                        }),
                );
            }
            (EntityId::ScopeDefinition { scope, .. }, RelationKind::AuthoredChild) => {
                let definition = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                targets.extend(definition.fields().nodes.iter().map(|node| {
                    EntityId::NodeDefinition {
                        executable,
                        scope: *scope,
                        node: node.id().clone(),
                    }
                }));
                targets.extend(definition.fields().edges.iter().map(|edge| {
                    EntityId::EdgeDefinition {
                        executable,
                        scope: *scope,
                        edge: edge.id().clone(),
                    }
                }));
            }
            (EntityId::NodeDefinition { scope, node, .. }, RelationKind::DefinitionUse) => {
                let definition = doc
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or(InspectionError::Unknown)?;
                let node = definition
                    .fields()
                    .nodes
                    .iter()
                    .find(|n| n.id() == node)
                    .ok_or(InspectionError::Unknown)?;
                if let Operation::Scope(body) | Operation::Loop { body, .. } =
                    &node.fields().operation
                {
                    targets.push(EntityId::ScopeDefinition {
                        executable,
                        scope: *body,
                    });
                }
            }
            (EntityId::EdgeDefinition { edge, .. }, RelationKind::AuthoredDataflow(endpoint)) => {
                let scope = scope.expect("edge scope");
                let definition = doc
                    .fields()
                    .scopes
                    .get(&scope)
                    .ok_or(InspectionError::Unknown)?;
                let edge = definition
                    .fields()
                    .edges
                    .iter()
                    .find(|e| e.id() == edge)
                    .ok_or(InspectionError::Unknown)?;
                targets.push(match endpoint {
                    DataflowEnd::Source => match edge.source() {
                        EdgeSource::Output { node, .. } => EntityId::NodeDefinition {
                            executable,
                            scope,
                            node: node.clone(),
                        },
                        _ => EntityId::ScopeDefinition { executable, scope },
                    },
                    DataflowEnd::Destination => match edge.destination() {
                        EdgeDestination::Input { node, .. } => EntityId::NodeDefinition {
                            executable,
                            scope,
                            node: node.clone(),
                        },
                        _ => EntityId::ScopeDefinition { executable, scope },
                    },
                });
            }
            _ => {}
        }
        Ok(targets)
    }
}

fn slice<T>(items: Vec<T>, after: i64, limit: usize) -> (Vec<T>, Option<i64>) {
    let start = usize::try_from(after).unwrap_or(usize::MAX);
    let end = start.saturating_add(limit).min(items.len());
    let more = end < items.len();
    let result = items.into_iter().skip(start).take(limit).collect();
    (result, more.then_some(end as i64))
}
fn causes(payload: &Value, targets: &mut Vec<EntityId>) {
    // Only coordinator-owned cause arrays, not arbitrary nested application values.
    if let Some(Value::Array(entries)) = field(payload, "causes") {
        for entry in entries {
            if let Some(id) = text(entry, "invocation_id") {
                targets.push(record(RecordFamily::Invocation, id));
            }
        }
    }
    if let Some(Value::Array(failures)) = field(payload, "failures") {
        for failure in failures {
            causes(failure, targets);
        }
    }
}

fn scope_use(payload: &Value) -> Option<htlk_analyzer::ScopeUse> {
    match field(payload, "scope_use")? {
        Value::Array(a) if a.as_slice() == [Value::Text("root".into())] => {
            Some(htlk_analyzer::ScopeUse::Root)
        }
        Value::Array(a) if a.len() == 3 && a[0] == Value::Text("node".into()) => {
            let (Value::Text(scope), Value::Text(node)) = (&a[1], &a[2]) else {
                return None;
            };
            Some(htlk_analyzer::ScopeUse::Node {
                scope: scope.parse().ok()?,
                node: node.parse().ok()?,
            })
        }
        _ => None,
    }
}

fn event_writes(event: &JournalEntry) -> Result<Vec<&[Value]>> {
    let body = match event.event {
        JournalEvent::RecordsCommitted => match &event.payload {
            Value::Array(a) => a.get(2),
            _ => None,
        },
        JournalEvent::RunInitialized => match &event.payload {
            Value::Array(a) => a.first(),
            _ => None,
        },
        JournalEvent::TerminalReceipt | JournalEvent::ArtifactExpired => Some(&event.payload),
        _ => return Ok(Vec::new()),
    };
    let Some(Value::Array(body)) = body else {
        return Err(InspectionError::Corrupt);
    };
    let Some(Value::Array(writes)) = body.first() else {
        return Err(InspectionError::Corrupt);
    };
    writes
        .iter()
        .map(|w| match w {
            Value::Array(a) if a.len() == 5 => Ok(a.as_slice()),
            _ => Err(InspectionError::Corrupt),
        })
        .collect()
}
pub(super) fn history_matches(event: &JournalEntry, entity: Option<&EntityId>) -> Result<bool> {
    match entity {
        None | Some(EntityId::Run) => Ok(true),
        Some(EntityId::Event(sequence)) => Ok(event.sequence == *sequence),
        Some(EntityId::Record(key)) => Ok(event_writes(event)?.iter().any(|w| {
            w[0] == Value::Text(key.family.as_str().into()) && w[1] == Value::Text(key.id.clone())
        })),
        _ => Err(InspectionError::InvalidRequest),
    }
}
