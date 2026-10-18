//! Bounded recovery/scheduling scans derived from authoritative records.
use super::*;
impl SqliteStore {
    pub(crate) async fn record_protection(
        &self,
        run: &RunId,
    ) -> Result<crate::records::SecurityLabels, StoreError> {
        let run = run.clone();
        let limits = self.config.limits.clone();
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let mut labels = crate::records::SecurityLabels::default();
                let mut statement = db.prepare("SELECT payload FROM records WHERE run_id=?1")?;
                let mut rows = statement.query([run.as_str()])?;
                while let Some(row) = rows.next()? {
                    let value = cbor::decode(
                        row.get_ref(0)?.as_blob().map_err(|_| StoreError::Corrupt)?,
                        &limits,
                    )
                    .map_err(|_| StoreError::Corrupt)?;
                    if let Value::Map(fields) = value {
                        for name in ["influence_labels", "security_labels"] {
                            if let Some(value) = fields.get(name) {
                                let protected =
                                    crate::records::SecurityLabels::from_value(value, &limits)
                                        .map_err(|_| StoreError::Corrupt)?;
                                labels = labels
                                    .union(&protected, &limits)
                                    .map_err(|_| StoreError::Invalid)?;
                            }
                        }
                    }
                }
                Ok(labels)
            })
            .await
            .map_err(StoreError::from_worker)
    }
    pub(crate) async fn inspection_records(
        &self,
        run: &RunId,
        family: RecordFamily,
        fields: &[&'static str],
    ) -> Result<Vec<StoredRecord>, StoreError> {
        if fields.len() > 32 {
            return Err(StoreError::Invalid);
        }
        let fields = fields.to_vec();
        let run = run.clone();
        let limits = self.config.limits.clone();
        self.connection.call(move|db|->Result<_,StoreError>{
            let mut statement=db.prepare("SELECT record_id,revision,final,payload FROM records WHERE run_id=?1 AND family=?2 ORDER BY record_id")?;let mut rows=statement.query(params![run.as_str(),family.as_str()])?;let mut result=Vec::new();let mut accounting=htlk_executable::record_accounting::RecordAccounting::new(&limits)?;
            while let Some(row)=rows.next()?{
                let id:String=row.get(0)?;let value=cbor::decode(row.get_ref(3)?.as_blob().map_err(|_|StoreError::Corrupt)?,&limits).map_err(|_|StoreError::Corrupt)?;
                let Value::Map(value)=value else{return Err(StoreError::Corrupt)};
                let payload=Value::Map(cbor::Map::try_from_entries(value.iter().filter(|(k,_)|fields.contains(k)||matches!(*k,"influence_labels"|"security_labels")).map(|(k,v)|(k.to_string(),v.clone())))?);
                accounting.text(&id,1).and_then(|()|accounting.value(&payload,1)).map_err(|_|StoreError::Invalid)?;
                result.push(StoredRecord{key:RecordKey{family,id},revision:row.get(1)?,final_record:row.get(2)?,payload});
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
    // Terminal outcomes are immutable; recording a host no-op receipt needs no
    // execution owner and cannot revive work. Only a single final reply is allowed.
    pub(crate) async fn terminal_receipt(
        &self,
        run: &RunId,
        write: RecordWrite,
        observed: i64,
    ) -> Result<Value, StoreError> {
        if write.key.family != RecordFamily::Reply
            || !write.final_record
            || write.expected_revision.is_some()
        {
            return Err(StoreError::Invalid);
        }
        self.check_text(&write.key.id)?;
        timestamp(observed)?;
        let Value::Map(fields) = &write.payload else {
            return Err(StoreError::Invalid);
        };
        let (Some(Value::Text(actor)), Some(Value::Text(method)), Some(response)) = (
            fields.get("principal"),
            fields.get("method"),
            fields.get("response"),
        ) else {
            return Err(StoreError::Invalid);
        };
        let (actor, method, response) = (actor.clone(), method.clone(), response.clone());
        let limits = self.config.limits.clone();
        let mut transition = Transition {
            context: TransitionContext {
                command_id: write.key.id.clone(),
                actor: actor.clone(),
                causation: method.clone(),
                timestamp_ms: observed,
            },
            writes: vec![write.clone()],
            next_status: None,
            root: None,
            expected_fingerprint: None,
        };
        let event = cbor::encode(
            &transitions::transition_body(&mut transition, &limits)?,
            &limits,
        )?;
        let payload = cbor::encode(&write.payload, &limits)?;
        let run = run.clone();
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;let header=load_header(&tx,&run)?;
            if !header.status.is_terminal(){return Err(StoreError::Conflict)}
            let old:Option<Vec<u8>>=tx.query_row("SELECT payload FROM records WHERE run_id=?1 AND family='reply' AND record_id=?2",params![run.as_str(),write.key.id],|r|r.get(0)).optional()?;
            if let Some(old)=old{if old!=payload{return Err(StoreError::IdempotencyConflict)}return Ok(response)}
            let now=observed.max(header.last_time_ms);let sequence=header.sequence.checked_add(1).ok_or(StoreError::Invalid)?;
            tx.execute("INSERT INTO records(run_id,family,record_id,revision,final,payload) VALUES(?1,'reply',?2,1,1,?3)",params![run.as_str(),write.key.id,payload])?;
            tx.execute("UPDATE runs SET sequence=?2,last_time_ms=?3 WHERE run_id=?1",params![run.as_str(),sequence,now])?;
            append_event(&tx,run.as_str(),sequence,EventMeta{kind:"terminal_receipt",actor:&actor,timestamp:now,causation:&method,command:Some(&write.key.id)},&event,&limits)?;
            tx.commit()?;Ok(response)
        }).await.map_err(StoreError::from_worker)
    }
    pub(crate) async fn save_late_response(
        &self,
        run: &RunId,
        attempt: &str,
        response: &Value,
        labels: &crate::records::SecurityLabels,
    ) -> Result<(), StoreError> {
        let payload = cbor::encode(response, &self.config.limits)?;
        let digest = htlk_executable::digest::hash_bytes(&payload).to_string();
        let protection = cbor::encode(
            &labels
                .to_value(&self.config.limits)
                .map_err(|_| StoreError::Invalid)?,
            &self.config.limits,
        )?;
        let run = run.clone();
        let attempt = attempt.to_string();
        self.connection.call(move|db|db.execute("INSERT OR IGNORE INTO late_responses(run_id,attempt_id,content_digest,payload,protection) VALUES(?1,?2,?3,?4,?5)",params![run.as_str(),attempt,digest,payload,protection])).await.map_err(|_|StoreError::Database)?;
        Ok(())
    }
    pub(crate) async fn find_record(
        &self,
        family: RecordFamily,
        id: &str,
    ) -> Result<(RunId, StoredRecord), StoreError> {
        self.check_text(id)?;
        let id = id.to_string();
        let limits = self.config.limits.clone();
        self.connection.call(move|db|->Result<_,StoreError>{let mut statement=db.prepare("SELECT run_id,revision,final,payload FROM records WHERE family=?1 AND record_id=?2 LIMIT 2")?;let mut rows=statement.query(params![family.as_str(),id])?;let row=rows.next()?.ok_or(StoreError::Missing)?;let run=row.get_ref(0)?.as_str().map_err(|_|StoreError::Corrupt)?;let run=RunId::new(run.into(),&limits).map_err(|_|StoreError::Corrupt)?;let payload=cbor::decode(row.get_ref(3)?.as_blob().map_err(|_|StoreError::Corrupt)?,&limits)?;let record=StoredRecord{key:RecordKey{family,id},revision:row.get(1)?,final_record:row.get(2)?,payload};if rows.next()?.is_some(){return Err(StoreError::Corrupt)}Ok((run,record))}).await.map_err(StoreError::from_worker)
    }
    /// Lists one finite page of nonterminal run IDs after an exclusive opaque cursor.
    ///
    /// # Errors
    /// Returns invalid limits, corrupt IDs or database failure.
    pub async fn running_runs(
        &self,
        after: Option<&RunId>,
        page_size: usize,
    ) -> Result<Vec<RunId>, StoreError> {
        if page_size == 0 || page_size > self.config.max_page_size {
            return Err(StoreError::Invalid);
        }
        let after = after.map(ToString::to_string);
        let limits = self.config.limits.clone();
        self.connection.call(move|db|->Result<Vec<RunId>,StoreError>{
            let mut statement=db.prepare("SELECT run_id FROM runs WHERE status='running' AND (?1 IS NULL OR run_id>?1) ORDER BY run_id LIMIT ?2")?;
            let mut rows=statement.query(params![after,page_size as i64])?;let mut result=Vec::new();let mut bytes=0usize;
            while let Some(row)=rows.next()?{let id=row.get_ref(0)?.as_str().map_err(|_|StoreError::Corrupt)?;bytes=bytes.checked_add(id.len()+1).ok_or(StoreError::Invalid)?;if bytes>limits.max_document_bytes{return Err(StoreError::Invalid)}result.push(RunId::new(id.into(),&limits).map_err(|_|StoreError::Corrupt)?);}
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
    /// Lists a bounded page of a run's typed record family, with exclusive ID cursor.
    ///
    /// # Errors
    /// Returns invalid limits/cursor, corrupt records or database failure.
    pub async fn records(
        &self,
        run: &RunId,
        family: RecordFamily,
        after: Option<&str>,
        page_size: usize,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        if page_size == 0 || page_size > self.config.max_page_size {
            return Err(StoreError::Invalid);
        }
        let (run, after, limits) = (
            run.clone(),
            after.map(str::to_owned),
            self.config.limits.clone(),
        );
        self.connection.call(move|db|->Result<Vec<StoredRecord>,StoreError>{
            let mut statement=db.prepare("SELECT record_id,revision,final,payload FROM records WHERE run_id=?1 AND family=?2 AND (?3 IS NULL OR record_id>?3) ORDER BY record_id LIMIT ?4")?;
            let mut rows=statement.query(params![run.as_str(),family.as_str(),after,page_size as i64])?;let mut result=Vec::new();let mut bytes=0usize;
            while let Some(row)=rows.next()?{
                let id=row.get_ref(0)?.as_str().map_err(|_|StoreError::Corrupt)?;let raw=row.get_ref(3)?.as_blob().map_err(|_|StoreError::Corrupt)?;
                let size=raw.len().checked_add(id.len()).ok_or(StoreError::Invalid)?;
                if bytes.checked_add(size).is_none_or(|n|n>limits.max_document_bytes){if result.is_empty(){return Err(StoreError::Invalid)}break}
                bytes+=size;let payload=cbor::decode(raw,&limits).map_err(|_|StoreError::Corrupt)?;
                result.push(StoredRecord{key:RecordKey{family,id:id.into()},revision:row.get(1)?,final_record:row.get(2)?,payload});
            }Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
}
