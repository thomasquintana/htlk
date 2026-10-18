//! Immutable exact-byte graph storage, separate detached provenance.
use super::*;
use crate::VerifiedExecutable;
impl SqliteStore {
    pub(crate) async fn attach_provenance(
        &self,
        fingerprint: Digest,
        id: Digest,
        value: &Value,
    ) -> Result<(), StoreError> {
        let mut accounting =
            htlk_executable::record_accounting::RecordAccounting::new(&self.config.limits)?;
        accounting
            .collection(3, 0)
            .and_then(|()| accounting.text(&fingerprint.to_string(), 1))
            .and_then(|()| accounting.text(&id.to_string(), 1))
            .and_then(|()| accounting.value(value, 1))
            .map_err(|_| StoreError::Invalid)?;
        let payload = cbor::encode(value, &self.config.limits)?;
        if htlk_executable::digest::hash_bytes(&payload) != id {
            return Err(StoreError::Corrupt);
        }
        self.connection.call(move|db|->Result<(),StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM graphs WHERE fingerprint=?1)",[fingerprint.to_string()],|r|r.get(0))?;if !exists{return Err(StoreError::Missing)}
            let old:Option<bool>=tx.query_row("SELECT payload=?3 FROM graph_provenance WHERE fingerprint=?1 AND provenance_digest=?2",params![fingerprint.to_string(),id.to_string(),payload],|r|r.get(0)).optional()?;
            if old==Some(false){return Err(StoreError::Corrupt)}
            tx.execute("INSERT OR IGNORE INTO graph_provenance(fingerprint,provenance_digest,payload) VALUES(?1,?2,?3)",params![fingerprint.to_string(),id.to_string(),payload])?;tx.commit()?;Ok(())
        }).await.map_err(StoreError::from_worker)
    }
    pub(crate) async fn provenance(
        &self,
        fingerprint: Digest,
        after: Option<Digest>,
        page_size: usize,
    ) -> Result<Vec<(Digest, Value)>, StoreError> {
        if page_size == 0 || page_size > self.config.max_page_size {
            return Err(StoreError::Invalid);
        }
        let limits = self.config.limits.clone();
        let after = after.map(|id| id.to_string());
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM graphs WHERE fingerprint=?1)",[fingerprint.to_string()],|r|r.get(0))?;if !exists{return Err(StoreError::Missing)}
            let mut statement=tx.prepare("SELECT provenance_digest,payload FROM graph_provenance WHERE fingerprint=?1 AND (?2 IS NULL OR provenance_digest>?2) ORDER BY provenance_digest LIMIT ?3")?;let mut rows=statement.query(params![fingerprint.to_string(),after,page_size as i64])?;
            let mut result=Vec::new();let mut accounting=htlk_executable::record_accounting::RecordAccounting::new(&limits)?;
            while let Some(row)=rows.next()?{
                let id:String=row.get(0)?;let raw=row.get_ref(1)?.as_blob().map_err(|_|StoreError::Corrupt)?;let digest=htlk_executable::digest::hash_bytes(raw);if digest.to_string()!=id{return Err(StoreError::Corrupt)}
                let value=cbor::decode(raw,&limits).map_err(|_|StoreError::Corrupt)?;
                if accounting.text(&id,1).and_then(|()|accounting.value(&value,1)).is_err(){if result.is_empty(){return Err(StoreError::Invalid)}break}
                result.push((digest,value));
            }
            Ok(result)
        }).await.map_err(StoreError::from_worker)
    }
    pub(crate) async fn store_graph(
        &self,
        admitted: &VerifiedExecutable<'_>,
        bytes: &[u8],
        provenance: &Value,
    ) -> Result<(), StoreError> {
        let fingerprint = admitted.fingerprint();
        let graph_id = admitted.document().fields().graph_id.clone();
        if admitted
            .document()
            .envelope(&self.config.limits)
            .map_err(|_| StoreError::Invalid)?
            .encode(&self.config.limits)
            .map_err(|_| StoreError::Invalid)?
            != bytes
        {
            return Err(StoreError::Corrupt);
        }
        let provenance = cbor::encode(provenance, &self.config.limits)?;
        let provenance_digest = htlk_executable::digest::hash_bytes(&provenance);
        let bytes = bytes.to_vec();
        self.connection.call(move |db|->Result<(),StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old=tx.query_row("SELECT graph_id=?2 AND envelope=?3 FROM graphs WHERE fingerprint=?1",params![fingerprint.to_string(),graph_id,bytes],|r|r.get::<_,bool>(0)).optional()?;
            match old {
                Some(false)=>return Err(StoreError::Corrupt),
                Some(_)=>{},
                None=>{tx.execute("INSERT INTO graphs(fingerprint,graph_id,envelope) VALUES(?1,?2,?3)",params![fingerprint.to_string(),graph_id,bytes])?;},
            }
            let old=tx.query_row("SELECT payload=?3 FROM graph_provenance WHERE fingerprint=?1 AND provenance_digest=?2",params![fingerprint.to_string(),provenance_digest.to_string(),provenance],|r|r.get::<_,bool>(0)).optional()?;
            if old==Some(false){return Err(StoreError::Corrupt)}
            tx.execute("INSERT OR IGNORE INTO graph_provenance(fingerprint,provenance_digest,payload) VALUES(?1,?2,?3)",params![fingerprint.to_string(),provenance_digest.to_string(),provenance])?;
            tx.commit()?;
            Ok(())
        }).await.map_err(StoreError::from_worker)
    }
    pub(crate) async fn graph_bytes(&self, fingerprint: Digest) -> Result<Vec<u8>, StoreError> {
        let limits = self.config.limits.clone();
        self.connection
            .call(move |db| -> Result<Vec<u8>, StoreError> {
                let mut statement =
                    db.prepare("SELECT envelope FROM graphs WHERE fingerprint=?1")?;
                let mut rows = statement.query([fingerprint.to_string()])?;
                let row = rows.next()?.ok_or(StoreError::Missing)?;
                let bytes = row.get_ref(0)?.as_blob().map_err(|_| StoreError::Corrupt)?;
                if bytes.len() > limits.max_document_bytes {
                    return Err(StoreError::Invalid);
                }
                Ok(bytes.to_vec())
            })
            .await
            .map_err(StoreError::from_worker)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{NativeRegistry, verify_executable_owned};
    use htlk_executable::{CanonicalDocument, ExecutableEnvelope, PolicyDocument};
    use std::sync::Arc;

    pub(in crate::store) fn executable() -> (VerifiedExecutable<'static>, Vec<u8>) {
        let l = Limits::default();
        let fixture = include_str!("../tests/fixtures/native-empty.hex")
            .lines()
            .skip(2)
            .collect::<String>();
        let bytes = fixture
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let document =
            CanonicalDocument::from_envelope(&ExecutableEnvelope::decode(&bytes, &l).unwrap(), &l)
                .unwrap();
        let mut fields = document.fields().clone();
        let policy = PolicyDocument::from_document(
            fields.documents[&fields.profile.policy_document()].clone(),
            &l,
        )
        .unwrap();
        let mut registry = NativeRegistry::new(&l).unwrap();
        fields.profile = registry.link_policy(&policy).unwrap();
        let bytes = CanonicalDocument::new(fields, &l)
            .unwrap()
            .envelope(&l)
            .unwrap()
            .encode(&l)
            .unwrap();
        (
            verify_executable_owned(&bytes, Arc::new(registry), &l).unwrap(),
            bytes,
        )
    }
    #[test]
    fn provenance_failure_does_not_leave_a_registration_and_corrupt_bytes_are_never_overwritten() {
        actix::System::new().block_on(async{
            let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
            let (admitted,bytes)=executable();
            let evidence=Value::Array(vec![Value::Text("alice".into()),Value::Array(vec![])]);
            store.connection.call(|db|db.execute_batch("CREATE TRIGGER fail_provenance BEFORE INSERT ON graph_provenance BEGIN SELECT RAISE(ABORT,'injected'); END;")).await.unwrap();
            assert_eq!(store.store_graph(&admitted,&bytes,&evidence).await,Err(StoreError::Database));
            assert_eq!(store.graph_bytes(admitted.fingerprint()).await,Err(StoreError::Missing));
            store.connection.call(|db|db.execute_batch("DROP TRIGGER fail_provenance;")).await.unwrap();
            store.store_graph(&admitted,&bytes,&evidence).await.unwrap();
            assert_eq!(store.graph_bytes(admitted.fingerprint()).await.unwrap(),bytes);
            store.connection.call(|db|db.execute("UPDATE graphs SET envelope=x'00'",[])).await.unwrap();
            assert_eq!(store.store_graph(&admitted,&bytes,&evidence).await,Err(StoreError::Corrupt));
            assert_eq!(store.graph_bytes(admitted.fingerprint()).await.unwrap(),vec![0]);
        });
    }
    #[test]
    fn exact_duplicate_registration_keeps_one_graph_and_distinct_provenance() {
        actix::System::new().block_on(async {
            let store = SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap();
            let (admitted, bytes) = executable();
            for source in ["first", "first", "second"] {
                store
                    .store_graph(&admitted, &bytes, &Value::Text(source.into()))
                    .await
                    .unwrap();
            }
            let counts = store
                .connection
                .call(|db| -> Result<_, StoreError> {
                    Ok((
                        db.query_row("SELECT count(*) FROM graphs", [], |r| r.get::<_, i64>(0))?,
                        db.query_row("SELECT count(*) FROM graph_provenance", [], |r| {
                            r.get::<_, i64>(0)
                        })?,
                    ))
                })
                .await
                .unwrap();
            assert_eq!(counts, (1, 2));
            assert_eq!(
                store.store_graph(&admitted, &[0], &Value::Null).await,
                Err(StoreError::Corrupt)
            );
        });
    }
}
