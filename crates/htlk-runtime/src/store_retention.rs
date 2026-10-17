//! Terminal retention decisions share SQLite authority with new input references.
use super::*;
use crate::{
    artifact::ArtifactMetadata,
    identity::ArtifactId,
    retention::{GarbageCollection, RetentionDisposition},
};
use std::collections::BTreeSet;
impl SqliteStore {
    pub(crate) async fn expire_artifact(
        &self,
        metadata: &ArtifactMetadata,
        actor: &str,
        observed: i64,
        audit: i64,
    ) -> Result<RetentionDisposition, StoreError> {
        timestamp(observed)?;
        self.check_text(actor)?;
        let metadata = metadata.clone();
        let actor = actor.to_string();
        let limits = self.config.limits.clone();
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;let id=metadata.artifact_id();
            if let Some(at_ms)=expired(&tx,id,&limits)?{return Ok(RetentionDisposition::Expired{at_ms})}
            let source:String=tx.query_row("SELECT run_id FROM records WHERE family='artifact' AND record_id=?1",[id.to_string()],|r|r.get(0))?;
            let run=RunId::new(source,&limits).map_err(|_|StoreError::Corrupt)?;let header=load_header(&tx,&run)?;
            if !header.status.is_terminal(){return Ok(RetentionDisposition::Protected)}
            let now=observed.max(header.last_time_ms);
            if now<protected_until(&tx,&run,audit,&limits)?{return Ok(RetentionDisposition::Protected)}
            {
                let mut statement=tx.prepare("SELECT records.run_id,runs.status,records.payload FROM records JOIN runs USING(run_id) ORDER BY records.run_id")?;
                let mut rows=statement.query([])?;
                let mut last_checked=run.to_string();
                while let Some(row)=rows.next()?{
                    let source:String=row.get(0)?;
                    if source==run.as_str()||source==last_checked{continue}
                    let raw=row.get_ref(2)?.as_blob().map_err(|_|StoreError::Corrupt)?;
                    let value=cbor::decode(raw,&limits).map_err(|_|StoreError::Corrupt)?;
                    if references(&value,&id.to_string()){
                        if row.get_ref(1)?.as_str().map_err(|_|StoreError::Corrupt)?=="running"{return Ok(RetentionDisposition::Protected)}
                        let referenced=RunId::new(source.clone(),&limits).map_err(|_|StoreError::Corrupt)?;
                        if now<protected_until(&tx,&referenced,audit,&limits)?{return Ok(RetentionDisposition::Protected)}
                        last_checked=source;
                    }
                }
            }
            let payload=Value::Map(cbor::Map::try_from_entries([
                ("artifact_id".into(),Value::Text(id.to_string())),("expired_at_ms".into(),Value::Integer(now)),("reason".into(),Value::Text("retention_expired".into())),
                ("influence_labels".into(),metadata.security_labels().to_value(&limits).map_err(|_|StoreError::Invalid)?),
            ])?);
            let write=RecordWrite{key:RecordKey{family:RecordFamily::Tombstone,id:id.to_string()},expected_revision:None,final_record:true,payload:payload.clone()};
            let mut transition=Transition{context:TransitionContext{command_id:id.to_string(),actor:actor.clone(),causation:"expire_artifact".into(),timestamp_ms:now},writes:vec![write],next_status:None,root:None,expected_fingerprint:None};
            let event=cbor::encode(&transitions::transition_body(&mut transition,&limits)?,&limits)?;
            let sequence=header.sequence.checked_add(1).ok_or(StoreError::Invalid)?;
            tx.execute("INSERT INTO records(run_id,family,record_id,revision,final,payload) VALUES(?1,'tombstone',?2,1,1,?3)",params![run.as_str(),id.to_string(),cbor::encode(&payload,&limits)?])?;
            tx.execute("UPDATE runs SET sequence=?2,last_time_ms=?3 WHERE run_id=?1",params![run.as_str(),sequence,now])?;
            append_event(&tx,run.as_str(),sequence,EventMeta{kind:"artifact_expired",actor:&actor,timestamp:now,causation:"expire_artifact",command:Some(&id.to_string())},&event,&limits)?;
            tx.commit()?;Ok(RetentionDisposition::Expired{at_ms:now})
        }).await.map_err(StoreError::from_worker)
    }
    pub(crate) async fn collect_garbage(
        &self,
        after: Option<Digest>,
        page_size: usize,
    ) -> Result<GarbageCollection, StoreError> {
        if page_size == 0 || page_size > self.config.max_page_size {
            return Err(StoreError::Invalid);
        }
        let limits = self.config.limits.clone();
        let after = after.map(|d| d.to_string());
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE status='running')",[],|r|r.get(0))?;
            let mut candidates=BTreeSet::new();let mut found=BTreeSet::new();
            {let mut statement=tx.prepare("SELECT blob_digest FROM value_blobs WHERE (?1 IS NULL OR blob_digest>?1) ORDER BY blob_digest LIMIT ?2")?;let mut rows=statement.query(params![after,page_size as i64])?;
                while let Some(row)=rows.next()?{let value:String=row.get(0)?;let id:Digest=value.parse().map_err(|_|StoreError::Corrupt)?;candidates.insert(id);}}
            let scanned=candidates.len();let next=if scanned==page_size{candidates.last().copied()}else{None};
            let mut retained=BTreeSet::new();
            {let mut statement=tx.prepare("SELECT payload FROM records WHERE family='artifact'")?;let mut rows=statement.query([])?;
                while let Some(row)=rows.next()?{
                    let value=cbor::decode(row.get_ref(0)?.as_blob().map_err(|_|StoreError::Corrupt)?,&limits).map_err(|_|StoreError::Corrupt)?;
                    let metadata=ArtifactMetadata::from_value(&value,&limits).map_err(|_|StoreError::Corrupt)?;let blob=metadata.storage_reference();
                    if candidates.contains(&blob){found.insert(blob);if expired(&tx,metadata.artifact_id(),&limits)?.is_none(){retained.insert(blob);}}
                }
            }
            let mut removed=0;
            for digest in candidates{
                if retained.contains(&digest)||(active&&!found.contains(&digest)){continue}
                removed+=tx.execute("DELETE FROM value_blobs WHERE blob_digest=?1",[digest.to_string()])?;
            }
            tx.commit()?;Ok(GarbageCollection{scanned,removed,next})
        }).await.map_err(StoreError::from_worker)
    }
}
pub(super) fn expired(
    db: &tokio_rusqlite::rusqlite::Connection,
    id: ArtifactId,
    limits: &Limits,
) -> Result<Option<i64>, StoreError> {
    let mut statement =
        db.prepare("SELECT final,payload FROM records WHERE family='tombstone' AND record_id=?1")?;
    let mut rows = statement.query([id.to_string()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    if !row.get::<_, bool>(0)? {
        return Err(StoreError::Corrupt);
    }
    let value = cbor::decode(
        row.get_ref(1)?.as_blob().map_err(|_| StoreError::Corrupt)?,
        limits,
    )
    .map_err(|_| StoreError::Corrupt)?;
    let Value::Map(value) = value else {
        return Err(StoreError::Corrupt);
    };
    if value.get("artifact_id") != Some(&Value::Text(id.to_string()))
        || value.get("reason") != Some(&Value::Text("retention_expired".into()))
        || rows.next()?.is_some()
    {
        return Err(StoreError::Corrupt);
    }
    let Some(Value::Integer(time)) = value.get("expired_at_ms") else {
        return Err(StoreError::Corrupt);
    };
    timestamp(*time).map_err(|_| StoreError::Corrupt)?;
    Ok(Some(*time))
}
fn terminal_time(
    tx: &tokio_rusqlite::Transaction<'_>,
    run: &RunId,
    limits: &Limits,
) -> Result<i64, StoreError> {
    let mut statement=tx.prepare("SELECT event_type,timestamp_ms,payload FROM journal WHERE run_id=?1 AND event_type IN ('run_initialized','records_committed') ORDER BY sequence")?;
    let mut rows = statement.query([run.as_str()])?;
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let value = cbor::decode(
            row.get_ref(2)?.as_blob().map_err(|_| StoreError::Corrupt)?,
            limits,
        )
        .map_err(|_| StoreError::Corrupt)?;
        let Value::Array(parts) = value else {
            return Err(StoreError::Corrupt);
        };
        let body = parts
            .get(if kind == "run_initialized" { 0 } else { 2 })
            .ok_or(StoreError::Corrupt)?;
        let Value::Array(body) = body else {
            return Err(StoreError::Corrupt);
        };
        if matches!(body.get(1), Some(Value::Text(_))) {
            let time: i64 = row.get(1)?;
            timestamp(time).map_err(|_| StoreError::Corrupt)?;
            return Ok(time);
        }
    }
    Err(StoreError::Corrupt)
}
fn protected_until(
    tx: &tokio_rusqlite::Transaction<'_>,
    run: &RunId,
    audit: i64,
    limits: &Limits,
) -> Result<i64, StoreError> {
    let stored: Option<Vec<u8>> = tx
        .query_row(
            "SELECT payload FROM records WHERE run_id=?1 AND family='usage' AND record_id='run'",
            [run.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let audit = if let Some(raw) = stored {
        let Value::Map(usage) = cbor::decode(&raw, limits).map_err(|_| StoreError::Corrupt)? else {
            return Err(StoreError::Corrupt);
        };
        match usage.get("audit_retention_ms") {
            Some(Value::Integer(saved)) if *saved > 0 => audit.max(*saved),
            None => audit,
            _ => return Err(StoreError::Corrupt),
        }
    } else {
        audit
    };
    Ok(terminal_time(tx, run, limits)?.saturating_add(audit))
}
fn references(value: &Value, id: &str) -> bool {
    match value {
        Value::Text(s) => s == id,
        Value::Array(a) => a.iter().any(|v| references(v, id)),
        Value::Map(m) => m.iter().any(|(_, v)| references(v, id)),
        _ => false,
    }
}

pub(super) fn check_references(
    tx: &tokio_rusqlite::Transaction<'_>,
    writes: &[RecordWrite],
    limits: &Limits,
) -> Result<(), StoreError> {
    for write in writes
        .iter()
        .filter(|w| matches!(w.key.family, RecordFamily::Scope | RecordFamily::Invocation))
    {
        let Value::Map(payload) = &write.payload else {
            continue;
        };
        for name in ["inputs", "outputs", "carried", "next"] {
            let Some(Value::Map(manifest)) = payload.get(name) else {
                continue;
            };
            for (_, entry) in manifest.iter() {
                let Value::Map(entry) = entry else { continue };
                if let Some(Value::Text(id)) = entry.get("artifact_id") {
                    let id: ArtifactId = id.parse().map_err(|_| StoreError::Corrupt)?;
                    if expired(tx, id, limits)?.is_some() {
                        return Err(StoreError::Expired);
                    }
                }
            }
        }
    }
    Ok(())
}
