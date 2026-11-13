//! Bounded, versioned identity conversion. Tokens are deliberately not grants or
//! signatures; all selectors and current grants are checked by the service.
use super::*;

const TOKEN_BYTES: usize = 16_384;
fn limits(limits: &Limits) -> Limits {
    let mut limits = limits.clone();
    limits.max_document_bytes = limits.max_document_bytes.min(TOKEN_BYTES);
    limits
}
fn encode(value: &Value, l: &Limits) -> Result<Vec<u8>> {
    cbor::encode(value, &limits(l)).map_err(|_| InspectionError::InvalidReference)
}
fn decode(bytes: &[u8], l: &Limits) -> Result<Value> {
    cbor::decode(bytes, &limits(l)).map_err(|_| InspectionError::InvalidReference)
}
fn text(s: impl ToString) -> Value {
    Value::Text(s.to_string())
}
fn array(v: Vec<Value>) -> Value {
    Value::Array(v)
}
fn string(v: &Value) -> Result<&str> {
    if let Value::Text(s) = v {
        Ok(s)
    } else {
        Err(InspectionError::InvalidReference)
    }
}
fn number(v: &Value) -> Result<i64> {
    if let Value::Integer(n) = v
        && *n >= 0
    {
        Ok(*n)
    } else {
        Err(InspectionError::InvalidReference)
    }
}
fn fields(v: &Value, n: usize) -> Result<&[Value]> {
    if let Value::Array(a) = v
        && a.len() == n
    {
        Ok(a)
    } else {
        Err(InspectionError::InvalidReference)
    }
}
fn digest(v: &Value) -> Result<Digest> {
    string(v)?
        .parse()
        .map_err(|_| InspectionError::InvalidReference)
}
fn family(v: &Value) -> Result<RecordFamily> {
    let name = string(v)?;
    navigation::FAMILIES
        .iter()
        .copied()
        .find(|f| f.as_str() == name)
        .ok_or(InspectionError::InvalidReference)
}
fn key(v: &Value) -> Result<RecordKey> {
    let a = fields(v, 2)?;
    Ok(RecordKey {
        family: family(&a[0])?,
        id: string(&a[1])?.into(),
    })
}
fn key_value(key: &RecordKey) -> Value {
    array(vec![text(key.family.as_str()), text(&key.id)])
}

fn entity_value(entity: &EntityId) -> Value {
    match entity {
        EntityId::Run => array(vec![text("run")]),
        EntityId::Graph(id) => array(vec![text("graph"), text(id)]),
        EntityId::ScopeDefinition { executable, scope } => array(vec![
            text("scope_definition"),
            text(executable),
            text(scope),
        ]),
        EntityId::NodeDefinition {
            executable,
            scope,
            node,
        } => array(vec![
            text("node_definition"),
            text(executable),
            text(scope),
            text(node),
        ]),
        EntityId::EdgeDefinition {
            executable,
            scope,
            edge,
        } => array(vec![
            text("edge_definition"),
            text(executable),
            text(scope),
            text(edge),
        ]),
        EntityId::Record(key) => array(vec![text("record"), key_value(key)]),
        EntityId::Event(n) => array(vec![text("event"), Value::Integer(*n)]),
    }
}
fn entity(v: &Value) -> Result<EntityId> {
    let Value::Array(a) = v else {
        return Err(InspectionError::InvalidReference);
    };
    let tag = a
        .first()
        .ok_or(InspectionError::InvalidReference)
        .and_then(string)?;
    let invalid = || InspectionError::InvalidReference;
    Ok(match (tag, a.len()) {
        ("run", 1) => EntityId::Run,
        ("graph", 2) => EntityId::Graph(digest(&a[1])?),
        ("scope_definition", 3) => EntityId::ScopeDefinition {
            executable: digest(&a[1])?,
            scope: digest(&a[2])?,
        },
        ("node_definition", 4) => EntityId::NodeDefinition {
            executable: digest(&a[1])?,
            scope: digest(&a[2])?,
            node: string(&a[3])?.parse().map_err(|_| invalid())?,
        },
        ("edge_definition", 4) => EntityId::EdgeDefinition {
            executable: digest(&a[1])?,
            scope: digest(&a[2])?,
            edge: string(&a[3])?.parse().map_err(|_| invalid())?,
        },
        ("record", 2) => EntityId::Record(key(&a[1])?),
        ("event", 2) if number(&a[1])? > 0 => EntityId::Event(number(&a[1])?),
        _ => return Err(invalid()),
    })
}
pub(super) fn reference_value(r: &Reference) -> Value {
    array(vec![
        text("htlk.inspection.reference.1"),
        text(&r.namespace),
        text(&r.run),
        Value::Integer(r.position),
        entity_value(&r.entity),
        r.witness.as_ref().map_or(Value::Null, key_value),
    ])
}
impl Reference {
    /// Encode a versioned canonical identity, capped at 16 KiB and caller limits.
    /// This is not a grant, signature, or encryption mechanism.
    ///
    /// # Errors
    /// Returns representation/codec limits.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>> {
        encode(&reference_value(self), limits)
    }
    /// Decode bounded identity fields without granting access or trusting their
    /// assertions. Resolve through an Inspector before disclosure.
    ///
    /// # Errors
    /// Returns malformed/version/identifier/codec failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self> {
        let value = decode(bytes, limits)?;
        let a = fields(&value, 6)?;
        if string(&a[0])? != "htlk.inspection.reference.1" || string(&a[1])?.is_empty() {
            return Err(InspectionError::InvalidReference);
        }
        Ok(Self {
            namespace: string(&a[1])?.into(),
            run: RunId::new(string(&a[2])?.into(), limits)
                .map_err(|_| InspectionError::InvalidReference)?,
            position: number(&a[3])?,
            entity: entity(&a[4])?,
            witness: if a[5] == Value::Null {
                None
            } else {
                Some(key(&a[5])?)
            },
        })
    }
}
impl Cursor {
    /// Encode a bounded versioned continuation. Possession grants no authority.
    ///
    /// # Errors
    /// Returns codec/representation limits.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>> {
        encode(
            &array(vec![
                text("htlk.inspection.cursor.1"),
                text(&self.namespace),
                Value::Integer(self.position),
                text(self.query),
                Value::Integer(self.after),
            ]),
            limits,
        )
    }
    /// Decode bounded continuation assertions; each operation checks their scope.
    ///
    /// # Errors
    /// Returns invalid shape/version/cursor/codec limits.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self> {
        let parse = || {
            let value = decode(bytes, limits)?;
            let a = fields(&value, 5)?;
            if string(&a[0])? != "htlk.inspection.cursor.1" {
                return Err(InspectionError::InvalidCursor);
            }
            Ok(Self {
                namespace: string(&a[1])?.into(),
                position: number(&a[2])?,
                query: digest(&a[3])?,
                after: number(&a[4])?,
            })
        };
        parse().map_err(|_| InspectionError::InvalidCursor)
    }
}
impl ChangeCursor {
    /// Encode a bounded independent consumer offset.
    ///
    /// # Errors
    /// Returns codec/representation limits.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>> {
        encode(
            &array(vec![
                text("htlk.inspection.changes.1"),
                text(&self.namespace),
                Value::Integer(self.after),
            ]),
            limits,
        )
    }
    /// Decode bounded consumer assertions; the service checks the store namespace.
    ///
    /// # Errors
    /// Returns invalid shape/version/offset/codec limits.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self> {
        let parse = || {
            let value = decode(bytes, limits)?;
            let a = fields(&value, 3)?;
            if string(&a[0])? != "htlk.inspection.changes.1" {
                return Err(InspectionError::InvalidCursor);
            }
            Ok(Self {
                namespace: string(&a[1])?.into(),
                after: number(&a[2])?,
            })
        };
        parse().map_err(|_| InspectionError::InvalidCursor)
    }
}

pub(super) fn run_query(filter: &InspectionRunFilter, limits: &Limits) -> Result<Digest> {
    let value = array(vec![
        text("runs.1"),
        filter.run_id.as_ref().map_or(Value::Null, text),
        filter.status.map_or(Value::Null, |s| text(s.as_str())),
        filter.fingerprint.map_or(Value::Null, text),
        filter.since_ms.map_or(Value::Null, Value::Integer),
        filter.until_ms.map_or(Value::Null, Value::Integer),
    ]);
    Ok(hash_bytes(
        &cbor::encode(&value, limits).map_err(|_| InspectionError::InvalidRequest)?,
    ))
}
pub(super) fn entity_query(
    reference: &Reference,
    kind: &EntityKind,
    limits: &Limits,
) -> Result<Digest> {
    let kind = match kind {
        EntityKind::Run => text("run"),
        EntityKind::Graph => text("graph"),
        EntityKind::Definitions => text("definitions"),
        EntityKind::Events => text("events"),
        EntityKind::Records(f) => array(vec![text("records"), text(f.as_str())]),
    };
    query(
        array(vec![text("entities.1"), reference_value(reference), kind]),
        limits,
    )
}
pub(super) fn relation_query(
    reference: &Reference,
    kind: RelationKind,
    limits: &Limits,
) -> Result<Digest> {
    query(
        array(vec![
            text("relations.1"),
            reference_value(reference),
            text(relation_name(kind)),
        ]),
        limits,
    )
}
fn relation_name(kind: RelationKind) -> &'static str {
    // Explicit versioned spellings, independent of Rust discriminant assignment.
    match kind {
        RelationKind::AuthoredChild => "authored_child",
        RelationKind::AuthoredDataflow(DataflowEnd::Source) => "authored_source",
        RelationKind::AuthoredDataflow(DataflowEnd::Destination) => "authored_destination",
        RelationKind::ExecutionParent => "execution_parent",
        RelationKind::WaitRequest => "wait_request",
        RelationKind::Bindings => "bindings",
        RelationKind::OriginExecutable => "origin_executable",
        RelationKind::DefinitionUse => "definition_use",
        RelationKind::ExecutionChild => "execution_child",
        RelationKind::InstanceOf => "instance_of",
        RelationKind::Attempts => "attempts",
        RelationKind::Waits => "waits",
        RelationKind::Produces => "produces",
        RelationKind::SelectedArtifact => "selected_artifact",
        RelationKind::SelectedEdge => "selected_edge",
        RelationKind::Causes => "causes",
        RelationKind::Provenance => "provenance",
        RelationKind::Reuses => "reuses",
    }
}
pub(super) fn history_query(
    reference: &Reference,
    entity: Option<&Reference>,
    limits: &Limits,
) -> Result<Digest> {
    query(
        array(vec![
            text("history.1"),
            reference_value(reference),
            entity.map_or(Value::Null, reference_value),
        ]),
        limits,
    )
}
fn query(value: Value, limits: &Limits) -> Result<Digest> {
    Ok(hash_bytes(
        &cbor::encode(&value, limits).map_err(|_| InspectionError::InvalidRequest)?,
    ))
}
pub(super) fn record_value(r: &StoredRecord) -> Value {
    array(vec![
        key_value(&r.key),
        Value::Integer(r.revision),
        Value::Bool(r.final_record),
        r.payload.clone(),
    ])
}
pub(super) fn event_value(e: &JournalEntry) -> Value {
    array(vec![
        text(&e.run_id),
        Value::Integer(e.sequence),
        text(event_name(e.event)),
        text(e.record_version),
        text(&e.actor),
        Value::Integer(e.timestamp_ms),
        text(&e.causation),
        e.command_id.as_ref().map_or(Value::Null, text),
        e.payload.clone(),
    ])
}
fn event_name(e: JournalEvent) -> &'static str {
    match e {
        JournalEvent::ArtifactExpired => "artifact_expired",
        JournalEvent::TerminalReceipt => "terminal_receipt",
        JournalEvent::RunInitialized => "run_initialized",
        JournalEvent::RunCreated => "run_created",
        JournalEvent::OwnershipAcquired => "ownership_acquired",
        JournalEvent::OwnershipRenewed => "ownership_renewed",
        JournalEvent::RecordsCommitted => "records_committed",
    }
}
pub(super) fn header_value(h: &RunHeader) -> Value {
    array(vec![
        text(&h.run_id),
        text(h.fingerprint),
        text(h.root_scope),
        text(h.status.as_str()),
        Value::Integer(h.sequence),
        h.owner.as_ref().map_or(Value::Null, text),
        Value::Integer(h.fence),
        h.expires_at_ms.map_or(Value::Null, Value::Integer),
        Value::Integer(h.last_time_ms),
    ])
}

// Internal byte accounting representations, not public serialization contracts.
pub(super) fn run_summary_value(reference: &Reference, status: RunStatus) -> Value {
    array(vec![reference_value(reference), text(status.as_str())])
}
fn protection_value(p: &Protection, limits: &Limits) -> Result<Value> {
    Ok(array(vec![
        p.labels
            .to_value(limits)
            .map_err(|_| InspectionError::Limit)?,
        p.artifact
            .as_ref()
            .map(|m| m.to_value(limits).map_err(|_| InspectionError::Limit))
            .transpose()?
            .unwrap_or(Value::Null),
    ]))
}
pub(super) fn inspection_value(i: &EntityInspection, limits: &Limits) -> Result<Value> {
    let definition = i.definition.as_ref().map_or(Value::Null, |d| {
        array(vec![
            text(d.executable),
            d.digest.map_or(Value::Null, text),
            entity_value(&d.definition),
            text(&d.canonical_path),
        ])
    });
    let scope_use = match &i.scope_use {
        Some(htlk_analyzer::ScopeUse::Root) => array(vec![text("root")]),
        Some(htlk_analyzer::ScopeUse::Node { scope, node }) => {
            array(vec![text("node"), text(scope), text(node)])
        }
        None => Value::Null,
    };
    Ok(array(vec![
        reference_value(&i.reference),
        protection_value(&i.protection, limits)?,
        definition,
        i.run_status.map_or(Value::Null, |s| text(s.as_str())),
        i.sequence.map_or(Value::Null, Value::Integer),
        i.revision.map_or(Value::Null, Value::Integer),
        i.final_record.map_or(Value::Null, Value::Bool),
        i.status.map_or(Value::Null, text),
        i.iteration
            .map_or(Value::Null, |n| Value::Integer(n as i64)),
        i.extension_number
            .map_or(Value::Null, |n| Value::Integer(n as i64)),
        scope_use,
        i.event.map_or(Value::Null, |e| text(event_name(e))),
        i.expired_at_ms.map_or(Value::Null, Value::Integer),
        i.attempt_number
            .map_or(Value::Null, |n| Value::Integer(n as i64)),
        i.generation
            .map_or(Value::Null, |n| Value::Integer(n as i64)),
        i.deadline_ms
            .map_or(Value::Null, |n| Value::Integer(n as i64)),
    ]))
}
pub(super) fn relation_value(
    source: &Reference,
    target: &Reference,
    relation: RelationKind,
) -> Value {
    array(vec![
        reference_value(source),
        reference_value(target),
        text(relation_name(relation)),
    ])
}
pub(super) fn history_value(
    reference: &Reference,
    event: &JournalEntry,
    protection: &Protection,
    limits: &Limits,
) -> Result<Value> {
    Ok(array(vec![
        reference_value(reference),
        Value::Integer(event.sequence),
        text(event_name(event.event)),
        Value::Integer(event.timestamp_ms),
        protection_value(protection, limits)?,
    ]))
}
pub(super) fn change_value(
    reference: &Reference,
    position: &StorePosition,
    cursor: i64,
    sequence: i64,
) -> Value {
    array(vec![
        reference_value(reference),
        text(position.namespace()),
        Value::Integer(position.number()),
        Value::Integer(cursor),
        Value::Integer(sequence),
    ])
}
