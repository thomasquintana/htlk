//! SQL state transitions shared by the fenced store facade and independent SQLite
//! fault tests. Every mutating function requires an already-authorized transaction;
//! the store facade owns BEGIN IMMEDIATE and parent owner/session checks.
use crate::{
    mcp::client::{
        McpOperationOrigin, McpOwner, McpRequestId, McpRpcError, McpStoredUrlInteraction,
        McpStoredUrlRequiredError, McpUrlCompletion, McpUrlConsent, McpUrlRequest,
        McpUrlRequiredError,
    },
    records::SecurityLabels,
    store::StoreError,
};
use htlk_executable::{
    JsonDocument,
    cbor::{self, Limits, Value},
    digest::{Digest, hash_bytes},
};
use rusqlite::{Connection as Db, OptionalExtension, params};

/// Exact scope supplied only after the parent transaction checks authority.
#[derive(Clone, Copy)]
pub(super) struct Scope<'a> {
    pub owner: &'a McpOwner,
    pub server: Digest,
    pub session: Digest,
}
fn digest(
    domain: &str,
    scope: Scope<'_>,
    key: Value,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    Ok(hash_bytes(&cbor::encode(
        &Value::Array(vec![
            Value::Text(format!("htlk.mcp-url-{domain}/1")),
            Value::Text(scope.owner.key().to_string()),
            Value::Text(scope.server.to_string()),
            Value::Text(scope.session.to_string()),
            key,
        ]),
        limits,
    )?))
}
fn url_id(scope: Scope<'_>, id: &str, limits: &Limits) -> Result<Digest, StoreError> {
    digest("interaction", scope, Value::Text(id.into()), limits)
}
fn error_id(scope: Scope<'_>, id: &McpRequestId, limits: &Limits) -> Result<Digest, StoreError> {
    digest("required-error", scope, id.to_value(), limits)
}
fn protection(labels: &SecurityLabels, limits: &Limits) -> Result<Vec<u8>, StoreError> {
    let bytes = cbor::encode(
        &labels.to_value(limits).map_err(|_| StoreError::Invalid)?,
        limits,
    )?;
    if bytes.len() > limits.max_document_bytes / 8 {
        return Err(StoreError::Invalid);
    }
    Ok(bytes)
}
fn labels(bytes: &[u8], limits: &Limits) -> Result<SecurityLabels, StoreError> {
    if bytes.len() > limits.max_document_bytes / 8 {
        return Err(StoreError::Corrupt);
    }
    SecurityLabels::from_value(&cbor::decode(bytes, limits)?, limits)
        .map_err(|_| StoreError::Corrupt)
}
fn bounded(bytes: &[u8], limits: &Limits) -> Result<(), StoreError> {
    if bytes.len() > limits.max_document_bytes / 8 {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
fn blob(
    row: &rusqlite::Row<'_>,
    column: usize,
    limits: &Limits,
) -> Result<Option<Vec<u8>>, StoreError> {
    use rusqlite::types::ValueRef;
    match row.get_ref(column)? {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(bytes) if bytes.len() <= limits.max_document_bytes / 8 => {
            Ok(Some(bytes.to_vec()))
        }
        _ => Err(StoreError::Corrupt),
    }
}
fn capacity(db: &Db, table: &str, owner: &McpOwner, maximum: usize) -> Result<(), StoreError> {
    if maximum == 0 || maximum > i64::MAX as usize {
        return Err(StoreError::Invalid);
    }
    // Table is a private implementation constant, never remote input.
    let count: i64 = db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE owner_key=?1"),
        [owner.key().to_string()],
        |r| r.get(0),
    )?;
    if count >= maximum as i64 {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register(
    db: &Db,
    scope: Scope<'_>,
    request: &McpUrlRequest,
    callback: Option<Digest>,
    request_labels: &SecurityLabels,
    deadline: i64,
    maximum: usize,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    bounded(request.params().as_bytes(), limits)?;
    if deadline <= 0 {
        return Err(StoreError::Invalid);
    }
    let id = url_id(scope, request.elicitation_id(), limits)?;
    let previous: Option<(String, Option<String>, Vec<u8>, String)> = db
        .query_row(
            "SELECT params_digest,callback_id,protection,consent FROM mcp_url_interactions WHERE url_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let merged = if let Some((previous, old_callback, old_protection, old_consent)) = previous {
        if previous != request.params().digest().to_string()
            || (callback.is_some()
                && old_callback.is_some()
                && old_callback != callback.map(|id| id.to_string()))
            || (callback.is_some() && old_callback.is_none() && old_consent != "pending")
        {
            return Err(StoreError::IdempotencyConflict);
        }
        let merged = labels(&old_protection, limits)?
            .union(request_labels, limits)
            .map_err(|_| StoreError::Invalid)?;
        db.execute(
            "UPDATE mcp_url_interactions SET protection=?2,callback_id=coalesce(callback_id,?3) WHERE url_id=?1",
            params![id.to_string(), protection(&merged, limits)?, callback.map(|id| id.to_string())],
        )?;
        // Replays recheck the selected saved evidence, not just its index keys.
        inspect(db, scope, id, limits)?;
        return Ok(id);
    } else {
        capacity(db, "mcp_url_interactions", scope.owner, maximum)?;
        protection(request_labels, limits)?
    };
    db.execute("INSERT INTO mcp_url_interactions(url_id,owner_key,server_digest,session_id,callback_id,params,params_digest,protection,deadline_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![id.to_string(), scope.owner.key().to_string(), scope.server.to_string(), scope.session.to_string(), callback.map(|id| id.to_string()), request.params().as_bytes(), request.params().digest().to_string(), merged, deadline])?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn required(
    db: &Db,
    scope: Scope<'_>,
    rpc: &McpRequestId,
    required: &McpUrlRequiredError,
    request_labels: &SecurityLabels,
    deadline: i64,
    maximum: usize,
    limits: &Limits,
) -> Result<Vec<Digest>, StoreError> {
    bounded(required.error().as_bytes(), limits)?;
    let request_id = cbor::encode(&rpc.to_value(), limits)?;
    bounded(&request_id, limits)?;
    let id = error_id(scope, rpc, limits)?;
    let raw_digest = hash_bytes(required.error().as_bytes());
    let previous: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT error_digest,protection FROM mcp_url_required_errors WHERE error_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((previous, previous_labels)) = previous {
        original_error(db, scope, rpc, limits)?;
        if previous != raw_digest.to_string() {
            return Err(StoreError::IdempotencyConflict);
        }
        let merged = labels(&previous_labels, limits)?
            .union(request_labels, limits)
            .map_err(|_| StoreError::Invalid)?;
        db.execute(
            "UPDATE mcp_url_required_errors SET protection=?2 WHERE error_id=?1",
            params![id.to_string(), protection(&merged, limits)?],
        )?;
    } else {
        capacity(db, "mcp_url_required_errors", scope.owner, maximum)?;
        db.execute(
            "INSERT INTO mcp_url_required_errors(error_id,owner_key,server_digest,session_id,request_id,error,error_digest,protection) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id.to_string(),
                scope.owner.key().to_string(),
                scope.server.to_string(),
                scope.session.to_string(),
                request_id,
                required.error().as_bytes(),
                raw_digest.to_string(),
                protection(request_labels, limits)?
            ],
        )?;
    }
    let mut ids = Vec::new();
    for (ordinal, request) in required.interactions().iter().enumerate() {
        let url = register(
            db,
            scope,
            request,
            None,
            request_labels,
            deadline,
            maximum,
            limits,
        )?;
        db.execute("INSERT INTO mcp_url_required_links VALUES(?1,?2,?3) ON CONFLICT(error_id,ordinal) DO NOTHING", params![id.to_string(), ordinal as i64, url.to_string()])?;
        ids.push(url);
    }
    Ok(ids)
}

pub(super) fn complete(
    db: &Db,
    scope: Scope<'_>,
    elicitation: &str,
    now: i64,
    limits: &Limits,
) -> Result<McpUrlCompletion, StoreError> {
    let id = url_id(scope, elicitation, limits)?;
    let changed = db.execute(
        "UPDATE mcp_url_interactions SET completed_ms=?2 WHERE url_id=?1 AND completed_ms IS NULL",
        params![id.to_string(), now],
    )?;
    Ok(if changed == 0 {
        McpUrlCompletion::Ignored
    } else {
        McpUrlCompletion::Recorded { interaction_id: id }
    })
}

pub(super) fn inspect(
    db: &Db,
    scope: Scope<'_>,
    id: Digest,
    limits: &Limits,
) -> Result<McpStoredUrlInteraction, StoreError> {
    let mut statement = db.prepare("SELECT u.callback_id,u.params,u.params_digest,u.protection,u.deadline_ms,u.consent,u.result,u.result_digest,u.completed_ms,e.closed,u.origin FROM mcp_url_interactions u JOIN mcp_epochs e ON e.session_id=u.session_id WHERE u.url_id=?1 AND u.owner_key=?2 AND u.server_digest=?3 AND u.session_id=?4 AND e.server_digest=u.server_digest AND e.owner_key=u.owner_key")?;
    let mut rows = statement.query(params![
        id.to_string(),
        scope.owner.key().to_string(),
        scope.server.to_string(),
        scope.session.to_string()
    ])?;
    let row = rows.next()?.ok_or(StoreError::Missing)?;
    let params = blob(row, 1, limits)?.ok_or(StoreError::Corrupt)?;
    let params = JsonDocument::new(&params, limits).map_err(|_| StoreError::Corrupt)?;
    let request =
        McpUrlRequest::from_params(params.value(), limits).map_err(|_| StoreError::Corrupt)?;
    if request.params().digest().to_string() != row.get::<_, String>(2)?
        || url_id(scope, request.elicitation_id(), limits)? != id
    {
        return Err(StoreError::Corrupt);
    }
    let mut labels = labels(&blob(row, 3, limits)?.ok_or(StoreError::Corrupt)?, limits)?;
    let mut consent = McpUrlConsent::parse(&row.get::<_, String>(5)?).ok_or(StoreError::Corrupt)?;
    let result = blob(row, 6, limits)?;
    let result_digest: Option<String> = row.get(7)?;
    if let Some(result) = result {
        bounded(&result, limits).map_err(|_| StoreError::Corrupt)?;
        if Some(hash_bytes(&result).to_string()) != result_digest
            || McpUrlConsent::from_result(
                &JsonDocument::new(&result, limits).map_err(|_| StoreError::Corrupt)?,
            )
            .map_err(|_| StoreError::Corrupt)?
                != consent
        {
            return Err(StoreError::Corrupt);
        }
    } else if result_digest.is_some() || consent != McpUrlConsent::Pending {
        return Err(StoreError::Corrupt);
    }
    let callback_id: Option<Digest> = row
        .get::<_, Option<String>>(0)?
        .map(|id| id.parse().map_err(|_| StoreError::Corrupt))
        .transpose()?;
    let origin = decode_origin(blob(row, 10, limits)?.as_deref(), limits)?;
    if let Some(callback) = callback_id {
        // The callback's existing durable reply is the sole consent authority. No
        // dual write or completion/response race can manufacture an acceptance.
        let mut statement = db.prepare("SELECT reply,protection,reply_digest,request,request_digest,raw_digest,origin_run,origin_invocation,origin_attempt FROM mcp_interactions WHERE interaction_id=?1 AND owner_key=?2 AND session_id=?3")?;
        let mut callbacks = statement.query(params![
            callback.to_string(),
            scope.owner.key().to_string(),
            scope.session.to_string()
        ])?;
        let callback = callbacks.next()?.ok_or(StoreError::Corrupt)?;
        let callback_origin = match (
            callback.get::<_, Option<String>>(6)?,
            callback.get::<_, Option<String>>(7)?,
            callback.get::<_, Option<String>>(8)?,
        ) {
            (None, None, None) => None,
            (Some(run), Some(invocation), Some(attempt)) => Some(McpOperationOrigin {
                run_id: crate::identity::RunId::new(run, limits)
                    .map_err(|_| StoreError::Corrupt)?,
                invocation_id: invocation.parse().map_err(|_| StoreError::Corrupt)?,
                attempt_id: attempt.parse().map_err(|_| StoreError::Corrupt)?,
            }),
            _ => return Err(StoreError::Corrupt),
        };
        if callback_origin != origin {
            return Err(StoreError::Corrupt);
        }
        let reply = blob(callback, 0, limits)?;
        let protection = blob(callback, 1, limits)?.ok_or(StoreError::Corrupt)?;
        let original = blob(callback, 3, limits)?.ok_or(StoreError::Corrupt)?;
        if hash_bytes(&original).to_string() != callback.get::<_, String>(5)? {
            return Err(StoreError::Corrupt);
        }
        let original = JsonDocument::new(&original, limits).map_err(|_| StoreError::Corrupt)?;
        if original.digest().to_string() != callback.get::<_, String>(4)? {
            return Err(StoreError::Corrupt);
        }
        let Value::Map(original) = original.value() else {
            return Err(StoreError::Corrupt);
        };
        if original.get("method") != Some(&Value::Text("elicitation/create".into())) {
            return Err(StoreError::Corrupt);
        }
        let original_params =
            McpUrlRequest::from_params(original.get("params").ok_or(StoreError::Corrupt)?, limits)
                .map_err(|_| StoreError::Corrupt)?;
        if original_params.params().digest() != request.params().digest() {
            return Err(StoreError::Corrupt);
        }
        labels = labels
            .union(&self::labels(&protection, limits)?, limits)
            .map_err(|_| StoreError::Corrupt)?;
        if let Some(reply) = reply {
            if Some(hash_bytes(&reply).to_string()) != callback.get::<_, Option<String>>(2)? {
                return Err(StoreError::Corrupt);
            }
            let reply = JsonDocument::new(&reply, limits).map_err(|_| StoreError::Corrupt)?;
            let Value::Map(fields) = reply.value() else {
                return Err(StoreError::Corrupt);
            };
            if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
                || fields.get("id") != original.get("id")
                || fields.get("method").is_some()
                || fields.get("result").is_some() == fields.get("error").is_some()
            {
                return Err(StoreError::Corrupt);
            }
            if let Some(result) = fields.get("result") {
                let observed = McpUrlConsent::from_result(
                    &JsonDocument::from_value(result, limits).map_err(|_| StoreError::Corrupt)?,
                )
                .map_err(|_| StoreError::Corrupt)?;
                if consent != McpUrlConsent::Pending && consent != observed {
                    return Err(StoreError::Corrupt);
                }
                consent = observed;
            }
        } else if callback.get::<_, Option<String>>(2)?.is_some() {
            return Err(StoreError::Corrupt);
        }
    }
    Ok(McpStoredUrlInteraction {
        origin,
        id,
        server: scope.server,
        session: scope.session,
        callback_id,
        request,
        labels,
        consent,
        completed_ms: row.get(8)?,
        deadline_ms: row.get(4)?,
        session_closed: row.get(9)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn receipt(
    db: &Db,
    scope: Scope<'_>,
    id: Digest,
    request_digest: Digest,
    actor: &str,
    key: &str,
    result: &JsonDocument,
    limits: &Limits,
) -> Result<Option<Digest>, StoreError> {
    let previous: Option<(String,String)> = db.query_row("SELECT url_id,result_digest FROM mcp_url_receipts WHERE owner_key=?1 AND principal=?2 AND idempotency_key=?3", params![scope.owner.key().to_string(), actor, key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some(previous) = previous else {
        return Ok(None);
    };
    let old = inspect(db, scope, id, limits)?;
    if old.request.params().digest() != request_digest {
        return Err(StoreError::IdempotencyConflict);
    }
    let result_digest = hash_bytes(result.as_bytes());
    if previous != (id.to_string(), result_digest.to_string()) {
        return Err(StoreError::IdempotencyConflict);
    }
    let saved_digest: Option<String> = db.query_row(
        "SELECT result_digest FROM mcp_url_interactions WHERE url_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if saved_digest != Some(result_digest.to_string()) {
        return Err(StoreError::Corrupt);
    }
    Ok(Some(result_digest))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn respond(
    db: &Db,
    scope: Scope<'_>,
    id: Digest,
    request_digest: Digest,
    actor: &str,
    key: &str,
    result: &JsonDocument,
    result_labels: &SecurityLabels,
    now: i64,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    bounded(result.as_bytes(), limits)?;
    let consent = McpUrlConsent::from_result(result).map_err(|_| StoreError::Invalid)?;
    let old = inspect(db, scope, id, limits)?;
    if old.callback_id.is_some() {
        return Err(StoreError::Invalid);
    }
    if old.request.params().digest() != request_digest {
        return Err(StoreError::IdempotencyConflict);
    }
    let result_digest = hash_bytes(result.as_bytes());
    if let Some(saved) = receipt(db, scope, id, request_digest, actor, key, result, limits)? {
        return Ok(saved);
    }
    if old.consent != McpUrlConsent::Pending {
        return Err(StoreError::IdempotencyConflict);
    }
    if now >= old.deadline_ms {
        return Err(StoreError::Deadline(crate::failure::FailureCode::Timeout));
    }
    let merged = old
        .labels
        .union(result_labels, limits)
        .map_err(|_| StoreError::Invalid)?;
    db.execute("UPDATE mcp_url_interactions SET consent=?2,result=?3,result_digest=?4,protection=?5 WHERE url_id=?1", params![id.to_string(), consent.as_str(), result.as_bytes(), result_digest.to_string(), protection(&merged, limits)?])?;
    db.execute(
        "INSERT INTO mcp_url_receipts VALUES(?1,?2,?3,?4,?5)",
        params![
            scope.owner.key().to_string(),
            actor,
            key,
            id.to_string(),
            result_digest.to_string()
        ],
    )?;
    Ok(result_digest)
}

pub(super) fn original_error(
    db: &Db,
    scope: Scope<'_>,
    rpc: &McpRequestId,
    limits: &Limits,
) -> Result<McpStoredUrlRequiredError, StoreError> {
    let id = error_id(scope, rpc, limits)?;
    let mut statement = db.prepare("SELECT request_id,error,error_digest,protection,origin FROM mcp_url_required_errors WHERE error_id=?1")?;
    let mut errors = statement.query([id.to_string()])?;
    let row = errors.next()?.ok_or(StoreError::Missing)?;
    let request_id = blob(row, 0, limits)?.ok_or(StoreError::Corrupt)?;
    let error = blob(row, 1, limits)?.ok_or(StoreError::Corrupt)?;
    let raw_digest: String = row.get(2)?;
    let protection = blob(row, 3, limits)?.ok_or(StoreError::Corrupt)?;
    let origin = decode_origin(blob(row, 4, limits)?.as_deref(), limits)?;
    if hash_bytes(&error).to_string() != raw_digest
        || McpRequestId::from_value(&cbor::decode(&request_id, limits)?)
            .map_err(|_| StoreError::Corrupt)?
            != *rpc
    {
        return Err(StoreError::Corrupt);
    }
    let error = McpRpcError::from_bytes(&error, limits).map_err(|_| StoreError::Corrupt)?;
    let required = McpUrlRequiredError::parse(&error, limits.max_document_bytes, limits)
        .map_err(|_| StoreError::Corrupt)?
        .ok_or(StoreError::Corrupt)?;
    let mut statement = db.prepare(
        "SELECT ordinal,url_id FROM mcp_url_required_links WHERE error_id=?1 ORDER BY ordinal",
    )?;
    let mut rows = statement.query([id.to_string()])?;
    let mut interactions = Vec::new();
    while let Some(row) = rows.next()? {
        let ordinal = row.get::<_, usize>(0)?;
        let url: Digest = row
            .get::<_, String>(1)?
            .parse()
            .map_err(|_| StoreError::Corrupt)?;
        if ordinal != interactions.len()
            || required
                .interactions()
                .get(ordinal)
                .map(|request| url_id(scope, request.elicitation_id(), limits))
                .transpose()?
                != Some(url)
        {
            return Err(StoreError::Corrupt);
        }
        interactions.push(url);
    }
    if interactions.len() != required.interactions().len() {
        return Err(StoreError::Corrupt);
    }
    Ok(McpStoredUrlRequiredError {
        origin,
        request_id: rpc.clone(),
        error,
        labels: labels(&protection, limits)?,
        interactions,
    })
}

fn decode_origin(
    bytes: Option<&[u8]>,
    limits: &Limits,
) -> Result<Option<McpOperationOrigin>, StoreError> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let Value::Array(values) = cbor::decode(bytes, limits)? else {
        return Err(StoreError::Corrupt);
    };
    let [
        Value::Text(run),
        Value::Text(invocation),
        Value::Text(attempt),
    ] = values.as_slice()
    else {
        return Err(StoreError::Corrupt);
    };
    Ok(Some(McpOperationOrigin {
        run_id: crate::identity::RunId::new(run.clone(), limits)
            .map_err(|_| StoreError::Corrupt)?,
        invocation_id: invocation.parse().map_err(|_| StoreError::Corrupt)?,
        attempt_id: attempt.parse().map_err(|_| StoreError::Corrupt)?,
    }))
}

pub(super) fn set_origin(
    db: &Db,
    id: Digest,
    origin: Option<&McpOperationOrigin>,
    required_error: bool,
    limits: &Limits,
) -> Result<(), StoreError> {
    let (table, column) = if required_error {
        ("mcp_url_required_errors", "error_id")
    } else {
        ("mcp_url_interactions", "url_id")
    };
    let previous: Option<Vec<u8>> = db.query_row(
        &format!("SELECT origin FROM {table} WHERE {column}=?1"),
        [id.to_string()],
        |row| row.get(0),
    )?;
    if previous.is_some() && decode_origin(previous.as_deref(), limits)?.as_ref() != origin {
        return Err(StoreError::IdempotencyConflict);
    }
    let encoded = origin
        .map(|origin| {
            cbor::encode(
                &Value::Array(vec![
                    Value::Text(origin.run_id.as_str().into()),
                    Value::Text(origin.invocation_id.to_string()),
                    Value::Text(origin.attempt_id.to_string()),
                ]),
                limits,
            )
        })
        .transpose()?;
    db.execute(
        &format!("UPDATE {table} SET origin=?2 WHERE {column}=?1"),
        params![id.to_string(), encoded],
    )?;
    Ok(())
}

pub(super) fn required_origin(
    db: &Db,
    scope: Scope<'_>,
    rpc: &McpRequestId,
    origin: Option<&McpOperationOrigin>,
    limits: &Limits,
) -> Result<(), StoreError> {
    set_origin(db, error_id(scope, rpc, limits)?, origin, true, limits)
}
