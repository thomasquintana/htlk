//! Value bytes may be staged first; only journaled final metadata accepts a value.
use super::*;
use crate::artifact::{ArtifactError, ArtifactMetadata, ArtifactProducer, PreparedArtifact};
use crate::identity::ArtifactId;

impl SqliteStore {
    /// Stages a type-checked proposal's immutable canonical bytes under their raw
    /// content address. This publishes no artifact or successful outcome. Unused
    /// staged data can be collected after retention/reference checks.
    ///
    /// # Errors
    /// Returns representation or integrity mismatch and database failures.
    pub async fn stage_artifact(&self, artifact: &PreparedArtifact) -> Result<Digest, StoreError> {
        if artifact.bytes().len() > self.config.limits.max_document_bytes {
            return Err(StoreError::Invalid);
        }
        artifact
            .metadata()
            .verify_value(artifact.bytes(), &self.config.limits)
            .map_err(artifact_error)?;
        let reference = artifact.metadata().storage_reference();
        let bytes = artifact.bytes().to_vec();
        self.connection
            .call(move |db| -> Result<Digest, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let old = tx
                    .query_row(
                        "SELECT value_bytes=?2 FROM value_blobs WHERE blob_digest=?1",
                        params![reference.to_string(), bytes],
                        |r| r.get::<_, bool>(0),
                    )
                    .optional()?;
                if old == Some(false) {
                    return Err(StoreError::Corrupt);
                }
                tx.execute(
                    "INSERT OR IGNORE INTO value_blobs(blob_digest,value_bytes) VALUES(?1,?2)",
                    params![reference.to_string(), bytes],
                )?;
                tx.commit()?;
                Ok(reference)
            })
            .await
            .map_err(StoreError::from_worker)
    }
    pub(crate) async fn artifact_metadata(
        &self,
        id: ArtifactId,
    ) -> Result<ArtifactMetadata, StoreError> {
        let limits = self.config.limits.clone();
        self.connection
            .call(move |db| -> Result<ArtifactMetadata, StoreError> {
                let mut statement = db.prepare(
                    "SELECT payload,final FROM records WHERE family='artifact' AND record_id=?1",
                )?;
                let mut rows = statement.query([id.to_string()])?;
                let row = rows.next()?.ok_or(StoreError::Missing)?;
                if !row.get::<_, bool>(1)? {
                    return Err(StoreError::Corrupt);
                }
                let bytes = row.get_ref(0)?.as_blob().map_err(|_| StoreError::Corrupt)?;
                let value = cbor::decode(bytes, &limits)
                    .map_err(ArtifactError::from)
                    .map_err(artifact_error)?;
                let metadata =
                    ArtifactMetadata::from_value(&value, &limits).map_err(artifact_error)?;
                if metadata.artifact_id() != id {
                    return Err(StoreError::Corrupt);
                }
                Ok(metadata)
            })
            .await
            .map_err(StoreError::from_worker)
    }
    pub(crate) async fn artifact_bytes(&self, reference: Digest) -> Result<Vec<u8>, StoreError> {
        let limits = self.config.limits.clone();
        self.connection
            .call(move |db| -> Result<Vec<u8>, StoreError> {
                let mut statement =
                    db.prepare("SELECT value_bytes FROM value_blobs WHERE blob_digest=?1")?;
                let mut rows = statement.query([reference.to_string()])?;
                let row = rows.next()?.ok_or(StoreError::Corrupt)?;
                let bytes = row.get_ref(0)?.as_blob().map_err(|_| StoreError::Corrupt)?;
                if bytes.len() > limits.max_document_bytes {
                    return Err(StoreError::Invalid);
                }
                if htlk_executable::digest::hash_bytes(bytes) != reference {
                    return Err(StoreError::Corrupt);
                }
                Ok(bytes.to_vec())
            })
            .await
            .map_err(StoreError::from_worker)
    }
}

pub(super) fn check_artifact_write(
    tx: &tokio_rusqlite::Transaction<'_>,
    run: &RunId,
    write: &RecordWrite,
    limits: &Limits,
) -> Result<(), StoreError> {
    if write.key.family != RecordFamily::Artifact {
        return Ok(());
    }
    if !write.final_record || write.expected_revision.is_some() {
        return Err(StoreError::Terminal);
    }
    let metadata =
        ArtifactMetadata::from_value(&write.payload, limits).map_err(|_| StoreError::Corrupt)?;
    if metadata.artifact_id().to_string() != write.key.id {
        return Err(StoreError::Corrupt);
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM graphs WHERE fingerprint=?1)",
        [metadata.validation_profile().to_string()],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(StoreError::Missing);
    }
    let staged: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM value_blobs WHERE blob_digest=?1)",
        [metadata.storage_reference().to_string()],
        |r| r.get(0),
    )?;
    if !staged {
        return Err(StoreError::Missing);
    }
    match metadata.producer() {
        ArtifactProducer::RunInput { run_id, .. } if run_id != run => {
            return Err(StoreError::Corrupt);
        }
        ArtifactProducer::RunInput { .. } if !metadata.input_artifact_ids().is_empty() => {
            return Err(StoreError::Corrupt);
        }
        ArtifactProducer::NodeOutput {
            invocation_id,
            port,
        } => {
            let mut statement=tx.prepare("SELECT final,payload FROM records WHERE run_id=?1 AND family='invocation' AND record_id=?2")?;
            let mut rows = statement.query(params![run.as_str(), invocation_id.to_string()])?;
            let row = rows.next()?.ok_or(StoreError::Missing)?;
            if !row.get::<_, bool>(0)? {
                return Err(StoreError::Corrupt);
            }
            let bytes = row.get_ref(1)?.as_blob().map_err(|_| StoreError::Corrupt)?;
            let Value::Map(invocation) =
                cbor::decode(bytes, limits).map_err(|_| StoreError::Corrupt)?
            else {
                return Err(StoreError::Corrupt);
            };
            if invocation.get("status") != Some(&Value::Text("succeeded".into())) {
                return Err(StoreError::Corrupt);
            }
            let Some(Value::Map(outputs)) = invocation.get("outputs") else {
                return Err(StoreError::Corrupt);
            };
            let Some(Value::Map(output)) = outputs.get(port.as_str()) else {
                return Err(StoreError::Corrupt);
            };
            if output.len() != 2
                || output.get("state") != Some(&Value::Text("value".into()))
                || output.get("artifact_id")
                    != Some(&Value::Text(metadata.artifact_id().to_string()))
            {
                return Err(StoreError::Corrupt);
            }
        }
        _ => {}
    }
    for parent in metadata.input_artifact_ids() {
        let accepted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM records WHERE family='artifact' AND record_id=?1 AND final=1)",[parent.to_string()],|r|r.get(0))?;
        if !accepted {
            return Err(StoreError::Missing);
        }
    }
    let (kind, producer, extension, port) = match metadata.producer() {
        ArtifactProducer::RunInput {
            run_id,
            extension_number,
            port,
        } => (
            "run_input",
            run_id.to_string(),
            i64::try_from(*extension_number).map_err(|_| StoreError::Corrupt)?,
            port,
        ),
        ArtifactProducer::NodeOutput {
            invocation_id,
            port,
        } => ("node_output", invocation_id.to_string(), 0, port),
    };
    let old=tx.query_row("SELECT artifact_id FROM artifact_producers WHERE producer_kind=?1 AND producer_id=?2 AND extension_number=?3 AND port=?4",params![kind,producer,extension,port.as_str()],|r|r.get::<_,String>(0)).optional()?;
    if let Some(old) = old {
        return Err(if old == metadata.artifact_id().to_string() {
            StoreError::Terminal
        } else {
            StoreError::Corrupt
        });
    }
    tx.execute("INSERT INTO artifact_producers(producer_kind,producer_id,extension_number,port,artifact_id,run_id) VALUES(?1,?2,?3,?4,?5,?6)",params![kind,producer,extension,port.as_str(),metadata.artifact_id().to_string(),run.as_str()])?;
    Ok(())
}

pub(super) fn check_output_publication(
    tx: &tokio_rusqlite::Transaction<'_>,
    run: &RunId,
    writes: &[RecordWrite],
    limits: &Limits,
) -> Result<(), StoreError> {
    for write in writes
        .iter()
        .filter(|w| w.key.family == RecordFamily::Invocation && w.final_record)
    {
        let Value::Map(invocation) = &write.payload else {
            continue;
        };
        if invocation.get("status") != Some(&Value::Text("succeeded".into())) {
            continue;
        }
        let invocation_id: crate::identity::InvocationId =
            write.key.id.parse().map_err(|_| StoreError::Corrupt)?;
        let Some(Value::Map(outputs)) = invocation.get("outputs") else {
            return Err(StoreError::Corrupt);
        };
        for (port, entry) in outputs.iter() {
            let Value::Map(entry) = entry else {
                return Err(StoreError::Corrupt);
            };
            if entry.get("state") == Some(&Value::Text("absent".into())) && entry.len() == 1 {
                continue;
            }
            let Some(Value::Text(id)) = entry.get("artifact_id") else {
                return Err(StoreError::Corrupt);
            };
            if entry.len() != 2 || entry.get("state") != Some(&Value::Text("value".into())) {
                return Err(StoreError::Corrupt);
            }
            let mut statement=tx.prepare("SELECT payload,final FROM records WHERE run_id=?1 AND family='artifact' AND record_id=?2")?;
            let mut rows = statement.query(params![run.as_str(), id])?;
            let row = rows.next()?.ok_or(StoreError::Missing)?;
            if !row.get::<_, bool>(1)? {
                return Err(StoreError::Corrupt);
            }
            let value = cbor::decode(
                row.get_ref(0)?.as_blob().map_err(|_| StoreError::Corrupt)?,
                limits,
            )
            .map_err(|_| StoreError::Corrupt)?;
            let metadata = ArtifactMetadata::from_value(&value, limits).map_err(artifact_error)?;
            if !matches!(metadata.producer(),ArtifactProducer::NodeOutput{invocation_id:owner,port:output} if *owner==invocation_id&&output.as_str()==port)
            {
                return Err(StoreError::Corrupt);
            }
            if metadata.artifact_id().to_string() != *id {
                return Err(StoreError::Corrupt);
            }
        }
    }
    Ok(())
}

fn artifact_error(error: ArtifactError) -> StoreError {
    match error {
        ArtifactError::Limit => StoreError::Invalid,
        ArtifactError::Codec(error)
            if matches!(
                error.kind(),
                cbor::ErrorKind::LimitExceeded { .. } | cbor::ErrorKind::InvalidLimits
            ) =>
        {
            StoreError::Invalid
        }
        ArtifactError::Identity(crate::identity::IdentityError::Encoding(_)) => StoreError::Invalid,
        ArtifactError::Record(crate::records::RuntimeRecordError::Encoding(_)) => {
            StoreError::Invalid
        }
        _ => StoreError::Corrupt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
        artifact::ArtifactInput, failure::RuntimeFailure, identity::InvocationId,
        records::SecurityLabels,
    };
    use htlk_executable::{BuiltinType, Identifier, ValueType, cbor::Map};
    use std::sync::Arc;

    async fn setup() -> (SqliteStore, PreparedArtifact, RunLease) {
        let (admitted, bytes) = registration::tests::executable();
        let l = Limits::default();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        store
            .store_graph(&admitted, &bytes, &Value::Null)
            .await
            .unwrap();
        let run = RunId::new("input-run".into(), &l).unwrap();
        store
            .create_run(RunSeed {
                root_scope: ScopeInstanceId::root(&run, &l).unwrap(),
                run_id: run.clone(),
                fingerprint: admitted.fingerprint(),
                actor: "host".into(),
                causation: "input".into(),
                timestamp_ms: 0,
            })
            .await
            .unwrap();
        let lease = store.acquire(&run, "owner".into(), 0, 100).await.unwrap();
        let artifact = PreparedArtifact::new(
            &admitted,
            ArtifactInput {
                producer: ArtifactProducer::RunInput {
                    run_id: run,
                    extension_number: 0,
                    port: "seed".parse().unwrap(),
                },
                value_type: &ValueType::builtin(BuiltinType::String),
                value: &Value::Text("protected".into()),
                input_artifact_ids: vec![],
                security_labels: SecurityLabels::new(vec!["protected".into()], &l).unwrap(),
                accepted_at: 1,
            },
            &l,
        )
        .unwrap();
        (store, artifact, lease)
    }
    fn write(artifact: &PreparedArtifact) -> RecordWrite {
        RecordWrite {
            key: RecordKey {
                family: RecordFamily::Artifact,
                id: artifact.metadata().artifact_id().to_string(),
            },
            expected_revision: None,
            final_record: true,
            payload: artifact.metadata().to_value(&Limits::default()).unwrap(),
        }
    }
    fn transition(command: &str, writes: Vec<RecordWrite>) -> Transition {
        Transition {
            context: TransitionContext {
                command_id: command.into(),
                actor: "host".into(),
                causation: "accept".into(),
                timestamp_ms: 1,
            },
            writes,
            next_status: None,
            root: None,
            expected_fingerprint: None,
        }
    }
    #[test]
    fn metadata_requires_staged_bytes_and_is_immutable_after_atomic_publication() {
        actix::System::new().block_on(async {
            let (store, artifact, lease) = setup().await;
            assert_eq!(
                store
                    .commit(&lease, transition("accept", vec![write(&artifact)]))
                    .await,
                Err(StoreError::Missing)
            );
            store.stage_artifact(&artifact).await.unwrap();
            assert!(matches!(
                store
                    .artifact_metadata(artifact.metadata().artifact_id())
                    .await,
                Err(StoreError::Missing)
            ));
            let mut mutable = write(&artifact);
            mutable.final_record = false;
            assert_eq!(
                store
                    .commit(&lease, transition("mutable", vec![mutable]))
                    .await,
                Err(StoreError::Terminal)
            );
            store
                .commit(&lease, transition("accept", vec![write(&artifact)]))
                .await
                .unwrap();
            assert_eq!(
                store
                    .artifact_metadata(artifact.metadata().artifact_id())
                    .await
                    .unwrap(),
                *artifact.metadata()
            );
            assert_eq!(
                store
                    .commit(&lease, transition("overwrite", vec![write(&artifact)]))
                    .await,
                Err(StoreError::Terminal)
            );
        });
    }
    struct Policy(bool);
    impl RuntimePolicy for Policy {
        fn authorize(&self, _: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
            if self.0 {
                Ok(())
            } else {
                Err(RuntimeFailure::new(FailureCode::Policy))
            }
        }
        fn input_labels(
            &self,
            _: &HostContext,
            _: Digest,
            _: &Identifier,
            _: &Value,
        ) -> Result<SecurityLabels, RuntimeFailure> {
            Ok(SecurityLabels::default())
        }
    }
    #[test]
    fn corruption_is_detected_but_denied_reads_do_not_load_blob_bytes() {
        actix::System::new().block_on(async {
            let (store, artifact, lease) = setup().await;
            store.stage_artifact(&artifact).await.unwrap();
            store
                .commit(&lease, transition("accept", vec![write(&artifact)]))
                .await
                .unwrap();
            store
                .connection
                .call(|db| db.execute("UPDATE value_blobs SET value_bytes=x'00'", []))
                .await
                .unwrap();
            let registry = Arc::new(NativeRegistry::new(&Limits::default()).unwrap());
            let context = HostContext::new("reader".into(), &Limits::default()).unwrap();
            let denied = Runtime::new(
                store.clone(),
                registry.clone(),
                Arc::new(Policy(false)),
                RuntimeConfig::default(),
            )
            .unwrap();
            assert_eq!(
                denied
                    .read_artifact(&context, artifact.metadata().artifact_id())
                    .await
                    .err()
                    .unwrap()
                    .code(),
                FailureCode::Policy
            );
            let allowed = Runtime::new(
                store.clone(),
                registry,
                Arc::new(Policy(true)),
                RuntimeConfig::default(),
            )
            .unwrap();
            assert_eq!(
                allowed
                    .read_artifact(&context, artifact.metadata().artifact_id())
                    .await
                    .err()
                    .unwrap()
                    .code(),
                FailureCode::Digest
            );
            assert_eq!(
                store.stage_artifact(&artifact).await,
                Err(StoreError::Corrupt)
            );
        });
    }
    #[test]
    fn failed_invocation_cannot_publish_outputs_and_success_publishes_metadata_together() {
        actix::System::new().block_on(async {
            let (store, input, lease) = setup().await;
            store.stage_artifact(&input).await.unwrap();
            store
                .commit(&lease, transition("input", vec![write(&input)]))
                .await
                .unwrap();
            let (admitted, _) = registration::tests::executable();
            let invocation = InvocationId::new(
                ScopeInstanceId::root(lease.run_id(), &Limits::default()).unwrap(),
                &"work".parse().unwrap(),
                &Limits::default(),
            )
            .unwrap();
            let output = PreparedArtifact::new(
                &admitted,
                ArtifactInput {
                    producer: ArtifactProducer::NodeOutput {
                        invocation_id: invocation,
                        port: "value".parse().unwrap(),
                    },
                    value_type: input.metadata().value_type(),
                    value: &Value::Text("protected".into()),
                    input_artifact_ids: vec![input.metadata().artifact_id()],
                    security_labels: input.metadata().security_labels().clone(),
                    accepted_at: 1,
                },
                &Limits::default(),
            )
            .unwrap();
            store.stage_artifact(&output).await.unwrap();
            let mut node = RecordWrite {
                key: RecordKey {
                    family: RecordFamily::Invocation,
                    id: invocation.to_string(),
                },
                expected_revision: None,
                final_record: true,
                payload: Value::Map(
                    Map::try_from_entries([("status".into(), Value::Text("failed".into()))])
                        .unwrap(),
                ),
            };
            assert_eq!(
                store
                    .commit(
                        &lease,
                        transition("node", vec![node.clone(), write(&output)])
                    )
                    .await,
                Err(StoreError::Corrupt)
            );
            assert_eq!(
                store.record(lease.run_id(), &node.key).await,
                Err(StoreError::Missing)
            );
            assert!(matches!(
                store
                    .artifact_metadata(output.metadata().artifact_id())
                    .await,
                Err(StoreError::Missing)
            ));
            let output_manifest = Value::Map(
                Map::try_from_entries([(
                    "value".into(),
                    Value::Map(
                        Map::try_from_entries([
                            ("state".into(), Value::Text("value".into())),
                            (
                                "artifact_id".into(),
                                Value::Text(output.metadata().artifact_id().to_string()),
                            ),
                        ])
                        .unwrap(),
                    ),
                )])
                .unwrap(),
            );
            node.payload = Value::Map(
                Map::try_from_entries([
                    ("status".into(), Value::Text("succeeded".into())),
                    ("outputs".into(), output_manifest),
                ])
                .unwrap(),
            );
            assert_eq!(
                store
                    .commit(&lease, transition("incomplete", vec![node.clone()]))
                    .await,
                Err(StoreError::Missing)
            );
            assert_eq!(
                store.record(lease.run_id(), &node.key).await,
                Err(StoreError::Missing)
            );
            store
                .commit(
                    &lease,
                    transition("node", vec![node.clone(), write(&output)]),
                )
                .await
                .unwrap();
            assert_eq!(
                store
                    .record(lease.run_id(), &node.key)
                    .await
                    .unwrap()
                    .payload,
                node.payload
            );
            let extra = PreparedArtifact::new(
                &admitted,
                ArtifactInput {
                    producer: ArtifactProducer::NodeOutput {
                        invocation_id: invocation,
                        port: "extra".parse().unwrap(),
                    },
                    value_type: input.metadata().value_type(),
                    value: &Value::Text("protected".into()),
                    input_artifact_ids: vec![input.metadata().artifact_id()],
                    security_labels: input.metadata().security_labels().clone(),
                    accepted_at: 1,
                },
                &Limits::default(),
            )
            .unwrap();
            store.stage_artifact(&extra).await.unwrap();
            assert_eq!(
                store
                    .commit(&lease, transition("extra-output", vec![write(&extra)]))
                    .await,
                Err(StoreError::Corrupt)
            );
            assert_eq!(
                store
                    .artifact_metadata(output.metadata().artifact_id())
                    .await
                    .unwrap()
                    .input_artifact_ids(),
                &[input.metadata().artifact_id()]
            );
        });
    }

    #[test]
    fn producer_slot_cannot_be_reused_for_different_content() {
        actix::System::new().block_on(async {
            let (store, original, lease) = setup().await;
            store.stage_artifact(&original).await.unwrap();
            store
                .commit(&lease, transition("first", vec![write(&original)]))
                .await
                .unwrap();
            let (admitted, _) = registration::tests::executable();
            let changed = PreparedArtifact::new(
                &admitted,
                ArtifactInput {
                    producer: original.metadata().producer().clone(),
                    value_type: original.metadata().value_type(),
                    value: &Value::Text("changed".into()),
                    input_artifact_ids: vec![],
                    security_labels: original.metadata().security_labels().clone(),
                    accepted_at: 1,
                },
                &Limits::default(),
            )
            .unwrap();
            store.stage_artifact(&changed).await.unwrap();
            assert_eq!(
                store
                    .commit(&lease, transition("replacement", vec![write(&changed)]))
                    .await,
                Err(StoreError::Corrupt)
            );
            assert!(matches!(
                store
                    .artifact_metadata(changed.metadata().artifact_id())
                    .await,
                Err(StoreError::Missing)
            ));
            assert_eq!(
                store
                    .artifact_metadata(original.metadata().artifact_id())
                    .await
                    .unwrap(),
                *original.metadata()
            );
        });
    }
}
