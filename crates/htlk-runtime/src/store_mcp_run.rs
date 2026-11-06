//! Client work shares the existing run journal, authority and ancestor reservations.
use super::*;
use crate::identity::{AttemptId, InvocationId};
use crate::mcp::client::McpOperationOrigin;
use htlk_executable::ExecutionLimits;
use std::collections::BTreeSet;

fn record(
    db: &Db,
    run: &RunId,
    family: RecordFamily,
    id: &str,
    limits: &Limits,
) -> Result<StoredRecord, StoreError> {
    let mut statement = db.prepare(
        "SELECT revision,final,payload FROM records WHERE run_id=?1 AND family=?2 AND record_id=?3",
    )?;
    let mut rows = statement.query(params![run.as_str(), family.as_str(), id])?;
    let row = rows.next()?.ok_or(StoreError::Missing)?;
    Ok(StoredRecord {
        key: RecordKey {
            family,
            id: id.into(),
        },
        revision: row.get(0)?,
        final_record: row.get(1)?,
        payload: cbor::decode(
            row.get_ref(2)?.as_blob().map_err(|_| StoreError::Corrupt)?,
            limits,
        )?,
    })
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, StoreError> {
    if let Value::Text(text) = field(map(value)?, key)? {
        Ok(text)
    } else {
        Err(StoreError::Corrupt)
    }
}
fn number(value: &Value, key: &str) -> Result<i64, StoreError> {
    if let Value::Integer(value) = field(map(value)?, key)? {
        Ok(*value)
    } else {
        Err(StoreError::Corrupt)
    }
}
fn protection(value: &Value, limits: &Limits) -> Result<SecurityLabels, StoreError> {
    match map(value)?.get("influence_labels") {
        Some(value) => SecurityLabels::from_value(value, limits).map_err(|_| StoreError::Corrupt),
        None => Ok(SecurityLabels::default()),
    }
}
fn replace(value: &Value, updates: &[(&str, Value)]) -> Result<Value, StoreError> {
    let mut fields: std::collections::BTreeMap<_, _> = map(value)?
        .iter()
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect();
    for (key, value) in updates {
        fields.insert((*key).into(), value.clone());
    }
    Ok(Value::Map(cbor::Map::try_from_entries(fields)?))
}
fn write(record: &StoredRecord, payload: Value, final_record: bool) -> RecordWrite {
    RecordWrite {
        key: record.key.clone(),
        expected_revision: Some(record.revision),
        final_record,
        payload,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn admit(
    db: &Db,
    owner: &McpOwner,
    session: Digest,
    origin: Option<&McpOperationOrigin>,
    method: &str,
    mut labels: SecurityLabels,
    mut deadline: i64,
    now: i64,
    limits: &Limits,
) -> Result<(SecurityLabels, i64), StoreError> {
    let McpOwnerScope::Run(run) = owner.scope() else {
        if origin.is_some() {
            return Err(StoreError::Invalid);
        }
        return Ok((labels, deadline));
    };
    let Some(origin) = origin else {
        if method != "roots/list" {
            return Err(StoreError::Invalid);
        }
        let header = load_header(db, run)?;
        let scope = record(
            db,
            run,
            RecordFamily::Scope,
            &header.root_scope.to_string(),
            limits,
        )?;
        labels = labels
            .union(&protection(&scope.payload, limits)?, limits)
            .map_err(|_| StoreError::Invalid)?;
        deadline = deadline.min(number(&scope.payload, "deadline_ms")?);
        if now >= deadline {
            return Err(StoreError::Deadline(FailureCode::Timeout));
        }
        return Ok((labels, deadline));
    };
    if &origin.run_id != run {
        return Err(StoreError::Fenced);
    }
    let attempt = record(
        db,
        run,
        RecordFamily::Attempt,
        &origin.attempt_id.to_string(),
        limits,
    )?;
    let node = record(
        db,
        run,
        RecordFamily::Invocation,
        &origin.invocation_id.to_string(),
        limits,
    )?;
    let invocation: InvocationId = text(&attempt.payload, "invocation_id")?
        .parse()
        .map_err(|_| StoreError::Corrupt)?;
    let number =
        u64::try_from(number(&attempt.payload, "number")?).map_err(|_| StoreError::Corrupt)?;
    if attempt.final_record
        || node.final_record
        || invocation != origin.invocation_id
        || AttemptId::new(invocation, number, limits).map_err(|_| StoreError::Corrupt)?
            != origin.attempt_id
        || text(&attempt.payload, "dispatch_state")? != "possibly_sent"
        || text(&node.payload, "status")? != "active"
        || self::number(&attempt.payload, "owner_fence")? != owner.generation()
    {
        return Err(StoreError::Fenced);
    }
    let server: String = db.query_row(
        "SELECT server_digest FROM mcp_epochs WHERE session_id=?1",
        [session.to_string()],
        |row| row.get(0),
    )?;
    if text(&attempt.payload, "server_digest")? != server {
        return Err(StoreError::Corrupt);
    }
    deadline = deadline.min(self::number(&node.payload, "deadline_ms")?);
    labels = labels
        .union(&protection(&node.payload, limits)?, limits)
        .map_err(|_| StoreError::Invalid)?;
    if let Some(protected) = map(&attempt.payload)?.get("result_labels") {
        labels = labels
            .union(
                &SecurityLabels::from_value(protected, limits).map_err(|_| StoreError::Corrupt)?,
                limits,
            )
            .map_err(|_| StoreError::Invalid)?;
    }
    let mut next = Some(node);
    let mut seen = BTreeSet::new();
    while let Some(node) = next {
        if seen.len() >= 1024 || !seen.insert(node.key.id.clone()) || node.final_record {
            return Err(StoreError::Corrupt);
        }
        if text(&node.payload, "status")? != "active" {
            return Err(StoreError::Fenced);
        }
        deadline = deadline.min(self::number(&node.payload, "deadline_ms")?);
        labels = labels
            .union(&protection(&node.payload, limits)?, limits)
            .map_err(|_| StoreError::Invalid)?;
        let scope = record(
            db,
            run,
            RecordFamily::Scope,
            text(&node.payload, "scope_instance_id")?,
            limits,
        )?;
        if scope.final_record || text(&scope.payload, "status")? != "active" {
            return Err(StoreError::Fenced);
        }
        if text(&scope.payload, "principal")? != owner.host().principal() {
            return Err(StoreError::Fenced);
        }
        deadline = deadline.min(self::number(&scope.payload, "deadline_ms")?);
        labels = labels
            .union(&protection(&scope.payload, limits)?, limits)
            .map_err(|_| StoreError::Invalid)?;
        next = match map(&scope.payload)?.get("parent_invocation") {
            Some(Value::Text(parent)) => {
                Some(record(db, run, RecordFamily::Invocation, parent, limits)?)
            }
            None | Some(Value::Null) => None,
            _ => return Err(StoreError::Corrupt),
        };
    }
    if now >= deadline {
        return Err(StoreError::Deadline(FailureCode::Timeout));
    }
    Ok((labels, deadline))
}

pub(super) fn reserve(
    tx: &tokio_rusqlite::Transaction<'_>,
    owner: &McpOwner,
    interaction: &McpStoredInteraction,
    now: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    let Some(origin) = &interaction.origin else {
        return Ok(());
    };
    let request =
        JsonDocument::new(&interaction.request, limits).map_err(|_| StoreError::Corrupt)?;
    if text(request.value(), "method")? == "roots/list" {
        return Ok(());
    }
    let (labels, deadline) = admit(
        tx,
        owner,
        interaction.session,
        Some(origin),
        text(request.value(), "method")?,
        interaction.labels.clone(),
        interaction.deadline_ms,
        now,
        limits,
    )?;
    if labels != interaction.labels || deadline != interaction.deadline_ms {
        return Err(StoreError::Conflict);
    }
    let parent = record(
        tx,
        &origin.run_id,
        RecordFamily::Reservation,
        &origin.attempt_id.to_string(),
        limits,
    )?;
    if parent.final_record
        || text(&parent.payload, "invocation_id")? != origin.invocation_id.to_string()
    {
        return Err(StoreError::Fenced);
    }
    let Value::Array(keys) = field(map(&parent.payload)?, "usage_keys")? else {
        return Err(StoreError::Corrupt);
    };
    let mut writes = Vec::new();
    let mut seen = BTreeSet::new();
    for key in keys {
        let Value::Text(key) = key else {
            return Err(StoreError::Corrupt);
        };
        if !seen.insert(key.clone()) {
            return Err(StoreError::Corrupt);
        }
        let usage = record(tx, &origin.run_id, RecordFamily::Usage, key, limits)?;
        let bounds = ExecutionLimits::from_value(field(map(&usage.payload)?, "limits")?, limits)
            .map_err(|_| StoreError::Corrupt)?;
        let spent = number(&usage.payload, "calls_spent")?;
        let concurrent = number(&usage.payload, "concurrency_reserved")?;
        if spent < 0 || concurrent < 0 {
            return Err(StoreError::Corrupt);
        }
        if bounds
            .max_mcp_calls()
            .is_some_and(|limit| spent as u64 >= limit)
            || bounds
                .max_concurrency()
                .is_some_and(|limit| concurrent as u64 >= limit)
        {
            return Err(StoreError::Budget);
        }
        writes.push(write(
            &usage,
            replace(
                &usage.payload,
                &[
                    (
                        "calls_spent",
                        Value::Integer(spent.checked_add(1).ok_or(StoreError::Invalid)?),
                    ),
                    (
                        "concurrency_reserved",
                        Value::Integer(concurrent.checked_add(1).ok_or(StoreError::Invalid)?),
                    ),
                ],
            )?,
            false,
        ));
    }
    if seen != usage_keys(tx, origin, limits)? {
        return Err(StoreError::Corrupt);
    }
    let payload = Value::Map(cbor::Map::try_from_entries([
        ("state".into(), Value::Text("outstanding".into())),
        ("usage_keys".into(), Value::Array(keys.clone())),
        (
            "invocation_id".into(),
            Value::Text(origin.invocation_id.to_string()),
        ),
        ("calls_spent".into(), Value::Integer(1)),
    ])?);
    writes.push(RecordWrite {
        key: RecordKey {
            family: RecordFamily::Reservation,
            id: format!("client:{}", interaction.id),
        },
        expected_revision: None,
        final_record: false,
        payload,
    });
    commit(
        tx,
        owner,
        interaction.id,
        "client_intent",
        writes,
        now,
        limits,
    )
}

pub(super) fn release(
    tx: &tokio_rusqlite::Transaction<'_>,
    owner: &McpOwner,
    interaction: &McpStoredInteraction,
    now: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    let Some(origin) = &interaction.origin else {
        return Ok(());
    };
    let request =
        JsonDocument::new(&interaction.request, limits).map_err(|_| StoreError::Corrupt)?;
    if text(request.value(), "method")? == "roots/list" {
        return Ok(());
    }
    let reservation = record(
        tx,
        &origin.run_id,
        RecordFamily::Reservation,
        &format!("client:{}", interaction.id),
        limits,
    )?;
    if reservation.final_record {
        return Ok(());
    }
    let Value::Array(keys) = field(map(&reservation.payload)?, "usage_keys")? else {
        return Err(StoreError::Corrupt);
    };
    let mut writes = Vec::new();
    let mut seen = BTreeSet::new();
    for key in keys {
        let Value::Text(key) = key else {
            return Err(StoreError::Corrupt);
        };
        if !seen.insert(key) {
            return Err(StoreError::Corrupt);
        }
        let usage = record(tx, &origin.run_id, RecordFamily::Usage, key, limits)?;
        let concurrent = number(&usage.payload, "concurrency_reserved")?;
        if concurrent <= 0 {
            return Err(StoreError::Corrupt);
        }
        writes.push(write(
            &usage,
            replace(
                &usage.payload,
                &[("concurrency_reserved", Value::Integer(concurrent - 1))],
            )?,
            false,
        ));
    }
    writes.push(write(
        &reservation,
        replace(
            &reservation.payload,
            &[("state", Value::Text("released".into()))],
        )?,
        true,
    ));
    commit(
        tx,
        owner,
        interaction.id,
        "client_release",
        writes,
        now,
        limits,
    )
}

fn commit(
    tx: &tokio_rusqlite::Transaction<'_>,
    owner: &McpOwner,
    id: Digest,
    action: &str,
    writes: Vec<RecordWrite>,
    now: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    let McpOwnerScope::Run(run) = owner.scope() else {
        return Err(StoreError::Invalid);
    };
    let header = load_header(tx, run)?;
    let lease = RunLease {
        run_id: run.clone(),
        owner: header.owner.ok_or(StoreError::Fenced)?,
        fence: owner.generation(),
    };
    let transition = Transition {
        context: TransitionContext {
            command_id: format!("{action}:{id}"),
            actor: lease.owner.clone(),
            causation: action.into(),
            timestamp_ms: now,
        },
        writes,
        next_status: None,
        root: None,
        expected_fingerprint: Some(header.fingerprint),
    };
    transitions::PreparedTransition::new(&lease, transition, limits)?.apply(tx, &lease, limits)?;
    Ok(())
}

fn usage_keys(
    db: &Db,
    origin: &McpOperationOrigin,
    limits: &Limits,
) -> Result<BTreeSet<String>, StoreError> {
    let mut keys = BTreeSet::from(["run".into()]);
    let mut node = record(
        db,
        &origin.run_id,
        RecordFamily::Invocation,
        &origin.invocation_id.to_string(),
        limits,
    )?;
    for _ in 0..1024 {
        if !keys.insert(format!("node:{}", node.key.id)) {
            return Err(StoreError::Corrupt);
        }
        let scope = record(
            db,
            &origin.run_id,
            RecordFamily::Scope,
            text(&node.payload, "scope_instance_id")?,
            limits,
        )?;
        if !keys.insert(format!("scope:{}", scope.key.id)) {
            return Err(StoreError::Corrupt);
        }
        match map(&scope.payload)?.get("parent_invocation") {
            Some(Value::Text(parent)) => {
                node = record(db, &origin.run_id, RecordFamily::Invocation, parent, limits)?
            }
            None | Some(Value::Null) => return Ok(keys),
            _ => return Err(StoreError::Corrupt),
        }
    }
    Err(StoreError::Corrupt)
}

pub(super) fn validate(
    db: &Db,
    owner: &McpOwner,
    interaction: &McpStoredInteraction,
    limits: &Limits,
) -> Result<(), StoreError> {
    let McpOwnerScope::Run(run) = owner.scope() else {
        return Err(StoreError::Invalid);
    };
    let request =
        JsonDocument::new(&interaction.request, limits).map_err(|_| StoreError::Corrupt)?;
    let method = text(request.value(), "method")?;
    let Some(origin) = &interaction.origin else {
        return if method == "roots/list" {
            Ok(())
        } else {
            Err(StoreError::Corrupt)
        };
    };
    let attempt = record(
        db,
        run,
        RecordFamily::Attempt,
        &origin.attempt_id.to_string(),
        limits,
    )?;
    if text(&attempt.payload, "invocation_id")? != origin.invocation_id.to_string()
        || AttemptId::new(
            origin.invocation_id,
            u64::try_from(number(&attempt.payload, "number")?).map_err(|_| StoreError::Corrupt)?,
            limits,
        )
        .map_err(|_| StoreError::Corrupt)?
            != origin.attempt_id
        || interaction.generation > load_header(db, run)?.fence
    {
        return Err(StoreError::Corrupt);
    }
    let server: String = db.query_row(
        "SELECT server_digest FROM mcp_epochs WHERE session_id=?1",
        [interaction.session.to_string()],
        |row| row.get(0),
    )?;
    if text(&attempt.payload, "server_digest")? != server {
        return Err(StoreError::Corrupt);
    }
    if interaction.call_charged && method != "roots/list" {
        let reservation = record(
            db,
            run,
            RecordFamily::Reservation,
            &format!("client:{}", interaction.id),
            limits,
        )?;
        if reservation.final_record == interaction.outstanding
            || number(&reservation.payload, "calls_spent")? != 1
            || text(&reservation.payload, "invocation_id")? != origin.invocation_id.to_string()
        {
            return Err(StoreError::Corrupt);
        }
    }
    Ok(())
}
