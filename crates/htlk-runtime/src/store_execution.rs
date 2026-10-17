//! Bounded recovery/scheduling scans derived from authoritative records.
use super::*;
impl SqliteStore {
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
