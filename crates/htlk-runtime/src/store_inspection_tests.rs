use super::*;

fn seed(name: &str) -> RunSeed {
    let limits = Limits::default();
    let run_id = RunId::new(name.into(), &limits).unwrap();
    RunSeed {
        root_scope: ScopeInstanceId::root(&run_id, &limits).unwrap(),
        run_id,
        fingerprint: Digest::from_bytes([1; 32]),
        actor: "host".into(),
        causation: "start".into(),
        timestamp_ms: 0,
    }
}
fn write(id: &str, revision: Option<i64>, value: i64) -> RecordWrite {
    RecordWrite {
        key: RecordKey {
            family: RecordFamily::Observation,
            id: id.into(),
        },
        expected_revision: revision,
        final_record: false,
        payload: Value::Integer(value),
    }
}
fn transition(command: &str, writes: Vec<RecordWrite>, time: i64) -> Transition {
    Transition {
        context: TransitionContext {
            command_id: command.into(),
            actor: "host".into(),
            causation: "test".into(),
            timestamp_ms: time,
        },
        writes,
        next_status: None,
        root: None,
        expected_fingerprint: None,
    }
}

// Same observable contract on both drivers, including arbitrary UTF-8 identities.
async fn frozen_contract(store: &Store) {
    let empty = store.inspection_position().await.unwrap();
    assert_eq!(empty.number(), 0);
    let run = seed(&format!("run\0é{}", "x".repeat(8192)));
    store.create_run(run.clone()).await.unwrap();
    let creation = store
        .open_inspection_snapshot(&run.run_id, None)
        .await
        .unwrap();
    assert_eq!(creation.header().sequence, 1);
    assert_eq!(
        store
            .open_inspection_snapshot(&run.run_id, Some(&empty))
            .await,
        Err(StoreError::Missing)
    );
    let lease = store
        .acquire(&run.run_id, "owner\0é".into(), 0, 100)
        .await
        .unwrap();
    let initial = transition(
        "initial",
        vec![write("a\0é", None, 1), write("b", None, 2)],
        1,
    );
    store.commit(&lease, initial.clone()).await.unwrap();
    let frozen = store
        .open_inspection_snapshot(&run.run_id, None)
        .await
        .unwrap();
    let first_page = store
        .inspection_record_page(&frozen, RecordFamily::Observation, 0, 1)
        .await
        .unwrap();
    assert_eq!(first_page.len(), 1);
    assert_eq!(first_page[0].record.payload, Value::Integer(1));
    let old_changes = store
        .inspection_changes(0, Some(frozen.position()), 100)
        .await
        .unwrap();
    // Idempotent retries cannot invent a new commit or feed notice.
    store.commit(&lease, initial).await.unwrap();
    assert_eq!(
        store.inspection_position().await.unwrap(),
        *frozen.position()
    );
    assert_eq!(
        store.inspection_changes(0, None, 100).await.unwrap(),
        old_changes
    );
    let mut finish = transition(
        "finish",
        vec![write("a\0é", Some(1), 99), write("c", None, 3)],
        2,
    );
    finish.next_status = Some(RunStatus::Succeeded);
    finish.root = Some(RootReplacement {
        fingerprint: Digest::from_bytes([2; 32]),
        root_scope: ScopeInstanceId::extended_root(&run.run_id, 1, &Limits::default()).unwrap(),
    });
    store.commit(&lease, finish).await.unwrap();
    let terminal = store
        .open_inspection_snapshot(&run.run_id, None)
        .await
        .unwrap();
    assert_eq!(terminal.header().status, RunStatus::Succeeded);
    assert_eq!(frozen.header().status, RunStatus::Running);
    assert_ne!(terminal.header().root_scope, frozen.header().root_scope);
    assert_eq!(
        store
            .open_inspection_snapshot(&run.run_id, Some(frozen.position()))
            .await
            .unwrap(),
        frozen
    );
    assert_eq!(
        store
            .inspection_record(&frozen, &write("a\0é", None, 0).key)
            .await
            .unwrap()
            .payload,
        Value::Integer(1)
    );
    assert_eq!(
        store
            .inspection_record(&terminal, &write("a\0é", None, 0).key)
            .await
            .unwrap()
            .payload,
        Value::Integer(99)
    );
    assert_eq!(
        store
            .inspection_record(&frozen, &write("c", None, 0).key)
            .await,
        Err(StoreError::Missing)
    );
    let rest = store
        .inspection_record_page(
            &frozen,
            RecordFamily::Observation,
            first_page[0].cursor,
            100,
        )
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].record.key.id, "b");
    assert!(
        store
            .inspection_record_page(&frozen, RecordFamily::Observation, rest[0].cursor, 100)
            .await
            .unwrap()
            .is_empty()
    );
    // Independent reconstruction from retained journal facts is the oracle.
    let history = store.inspection_journal(&frozen, 0, 100).await.unwrap();
    assert_eq!(history.len(), frozen.header().sequence as usize);
    let mut replay = ReplayState::begin(&history[0], &Limits::default()).unwrap();
    for entry in &history[1..] {
        replay.apply(entry, &Limits::default()).unwrap();
    }
    assert_eq!(replay.header(), frozen.header());
    assert_eq!(
        replay.record(&first_page[0].record.key).unwrap(),
        &first_page[0].record
    );
    assert!(
        store
            .inspection_journal(&frozen, frozen.header().sequence, 1)
            .await
            .unwrap()
            .is_empty()
    );
    for notice in store.pending_outbox(100).await.unwrap() {
        store.acknowledge(&notice).await.unwrap();
    }
    assert!(store.pending_outbox(100).await.unwrap().is_empty());
    assert_eq!(
        store
            .inspection_changes(0, Some(frozen.position()), 100)
            .await
            .unwrap(),
        old_changes
    );
    let mut consumer_a = Vec::new();
    let mut after = 0;
    loop {
        let page = store
            .inspection_changes(after, Some(terminal.position()), 1)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        after = page[0].cursor;
        consumer_a.extend(page);
    }
    let consumer_b = store
        .inspection_changes(0, Some(terminal.position()), 100)
        .await
        .unwrap();
    assert_eq!(consumer_a, consumer_b);
    let last = consumer_b.last().unwrap();
    assert_eq!(last.position, *terminal.position());
    assert_eq!(last.sequence, terminal.header().sequence);
    assert!(
        consumer_b
            .iter()
            .any(|change| change.key.as_ref().is_some_and(|key| key.id == "a\0é"))
    );
    assert!(
        store
            .inspection_changes(last.cursor, Some(frozen.position()), 100)
            .await
            .unwrap()
            .is_empty()
    );
    // Candidate bounds, rather than an unbounded search for matching headers.
    let second = seed("second");
    store.create_run(second.clone()).await.unwrap();
    let filter = InspectionRunFilter {
        status: Some(RunStatus::Running),
        ..Default::default()
    };
    let page = store
        .inspection_runs(None, filter.clone(), 0, 1)
        .await
        .unwrap();
    assert!(page.runs.is_empty());
    let next = page.next.expect("filtered empty page still advances");
    let page2 = store
        .inspection_runs(Some(&page.position), filter, next, 1)
        .await
        .unwrap();
    assert_eq!(page2.runs[0].run_id, second.run_id);
    let historical_runs = store
        .inspection_runs(
            Some(frozen.position()),
            InspectionRunFilter::default(),
            0,
            100,
        )
        .await
        .unwrap();
    assert_eq!(historical_runs.runs, vec![frozen.header().clone()]);
    assert!(
        store
            .inspection_runs(
                Some(frozen.position()),
                InspectionRunFilter::default(),
                next,
                100
            )
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert_eq!(
        store.inspection_position_at(-1).await,
        Err(StoreError::Invalid)
    );
    assert_eq!(
        store.inspection_position_at(i64::MAX).await,
        Err(StoreError::Invalid)
    );
    assert_eq!(
        store
            .inspection_record_page(&frozen, RecordFamily::Observation, -1, 1)
            .await,
        Err(StoreError::Invalid)
    );
    assert_eq!(
        store
            .inspection_record_page(&frozen, RecordFamily::Observation, 0, 0)
            .await,
        Err(StoreError::Invalid)
    );
    assert_eq!(
        store
            .inspection_changes(0, None, store.config.max_page_size + 1)
            .await,
        Err(StoreError::Invalid)
    );
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_frozen_snapshots_paging_filters_and_independent_changes() {
    actix::System::new().block_on(async {
        let store = Store::in_memory(StoreConfig::default()).await.unwrap();
        frozen_contract(&store).await;
        let other = Store::in_memory(StoreConfig::default()).await.unwrap();
        let pin = store.inspection_position().await.unwrap();
        assert_eq!(
            other
                .inspection_runs(Some(&pin), InspectionRunFilter::default(), 0, 1)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
    });
}

#[cfg(feature = "sqlite")]
async fn initialization_and_expiry(store: &Store) {
    use crate::{
        artifact::{ArtifactInput, ArtifactProducer, PreparedArtifact},
        records::{InputManifest, SecurityLabels},
        run::PreparedRun,
    };
    use htlk_executable::{BuiltinType, PortTable, ValueType};
    let limits = Limits::default();
    let (admitted, bytes) = registration::tests::executable();
    store
        .store_graph(&admitted, &bytes, &Value::Null)
        .await
        .unwrap();
    let before = store.inspection_position().await.unwrap();
    let run = store.reserve_run_id().await.unwrap();
    let prepared = PreparedRun {
        audit_retention_ms: 10,
        run_id: run.clone(),
        fingerprint: admitted.fingerprint(),
        definition: admitted.document().fields().root_scope,
        principal: "host".into(),
        inputs: InputManifest::freeze(
            &PortTable::default(),
            vec![],
            SecurityLabels::default(),
            &limits,
        )
        .unwrap(),
        artifacts: vec![],
        failure: None,
        run_limits: admitted.policy().fields().defaults.clone(),
        scope_limits: admitted.policy().fields().defaults.clone(),
        requested_limits: Default::default(),
        limits: limits.clone(),
    };
    let request = runs::HostRequest {
        principal: "host".into(),
        method: "start_run".into(),
        target: admitted.fingerprint().to_string(),
        key: "start".into(),
        digest: Digest::from_bytes([3; 32]),
    };
    store
        .initialize_run(prepared, request, 0, 100)
        .await
        .unwrap();
    let initialized = store.open_inspection_snapshot(&run, None).await.unwrap();
    assert_eq!(initialized.position().number(), before.number() + 1);
    assert_eq!(initialized.header().sequence, 2);
    assert_eq!(
        store.open_inspection_snapshot(&run, Some(&before)).await,
        Err(StoreError::Missing)
    );
    assert!(
        !store
            .inspection_record_page(&initialized, RecordFamily::Scope, 0, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let changes = store
        .inspection_changes(0, Some(initialized.position()), 100)
        .await
        .unwrap();
    assert!(
        changes
            .iter()
            .all(|c| c.position == *initialized.position() && c.sequence == 2)
    );
    let lease = store.acquire(&run, "owner".into(), 0, 100).await.unwrap();
    let artifact = PreparedArtifact::new(
        &admitted,
        ArtifactInput {
            producer: ArtifactProducer::RunInput {
                run_id: run.clone(),
                extension_number: 0,
                port: "seed".parse().unwrap(),
            },
            value_type: &ValueType::builtin(BuiltinType::String),
            value: &Value::Text("retained".into()),
            input_artifact_ids: vec![],
            security_labels: SecurityLabels::default(),
            accepted_at: 1,
        },
        &limits,
    )
    .unwrap();
    let staged = store.inspection_position().await.unwrap();
    store.stage_artifact(&artifact).await.unwrap();
    assert_eq!(
        store.inspection_position().await.unwrap(),
        staged,
        "staging is not publication"
    );
    let mut publication = transition(
        "publish",
        vec![RecordWrite {
            key: RecordKey {
                family: RecordFamily::Artifact,
                id: artifact.metadata().artifact_id().to_string(),
            },
            expected_revision: None,
            final_record: true,
            payload: artifact.metadata().to_value(&limits).unwrap(),
        }],
        1,
    );
    publication.next_status = Some(RunStatus::Succeeded);
    store.commit(&lease, publication).await.unwrap();
    let retained = store.open_inspection_snapshot(&run, None).await.unwrap();
    let id = artifact.metadata().artifact_id();
    assert_eq!(
        store
            .inspection_artifact(&retained, id)
            .await
            .unwrap()
            .expired_at_ms,
        None
    );
    assert_eq!(
        store
            .expire_artifact(artifact.metadata(), "host", 20, 10)
            .await
            .unwrap(),
        crate::retention::RetentionDisposition::Expired { at_ms: 20 }
    );
    let expired = store.open_inspection_snapshot(&run, None).await.unwrap();
    assert_eq!(
        store
            .inspection_artifact(&expired, id)
            .await
            .unwrap()
            .expired_at_ms,
        Some(20)
    );
    assert_eq!(
        store
            .inspection_artifact(&retained, id)
            .await
            .unwrap()
            .expired_at_ms,
        None
    );
    assert_eq!(
        store
            .artifact_bytes(id, artifact.metadata().storage_reference())
            .await,
        Err(StoreError::Expired)
    );
    store.collect_garbage(None, 100).await.unwrap();
    // Removing the byte table makes accidental eager value reads fail loudly.
    store
        .connection
        .call(|db| db.execute_batch("DROP TABLE value_blobs"))
        .await
        .unwrap();
    assert_eq!(
        store
            .inspection_artifact(&retained, id)
            .await
            .unwrap()
            .metadata,
        *artifact.metadata()
    );
    assert_eq!(
        store
            .inspection_artifact(&expired, id)
            .await
            .unwrap()
            .expired_at_ms,
        Some(20)
    );
    assert_eq!(
        store
            .inspection_journal(&retained, 0, 100)
            .await
            .unwrap()
            .last()
            .unwrap()
            .sequence,
        retained.header().sequence
    );
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_initialization_is_one_boundary_and_expiry_does_not_hold_content() {
    actix::System::new().block_on(async {
        let store = Store::in_memory(StoreConfig::default()).await.unwrap();
        initialization_and_expiry(&store).await;
    });
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_projection_failure_and_uncommitted_writes_publish_no_boundary() {
    actix::System::new().block_on(async {
        let path=std::env::temp_dir().join(format!("htlk-inspection-{}-{}.sqlite",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let store=Store::open(&path,StoreConfig::default()).await.unwrap();
        let reader=Store::open(&path,StoreConfig::default()).await.unwrap();
        let run=seed("atomic");
        store.create_run(run.clone()).await.unwrap();
        let lease=store.acquire(&run.run_id,"owner".into(),0,100).await.unwrap();
        let before=reader.open_inspection_snapshot(&run.run_id,None).await.unwrap();
        let prepared=transitions::PreparedTransition::new(&lease,transition("held",vec![write("a",None,1)],1),&Limits::default()).unwrap();
        let (ready_tx,ready_rx)=tokio::sync::oneshot::channel();
        let (release_tx,release_rx)=std::sync::mpsc::channel();
        let writer=store.clone();
        let pending=actix::spawn(async move {
            writer.connection.call(move |db| -> Result<(),StoreError> {
                let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                prepared.apply(&tx,&lease,&Limits::default())?;
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                // Deliberately roll back after all projection writes.
                Ok(())
            }).await.unwrap();
        });
        ready_rx.await.unwrap();
        assert_eq!(reader.open_inspection_snapshot(&run.run_id,None).await.unwrap(),before);
        assert!(reader.inspection_changes(0,Some(before.position()),100).await.unwrap().iter().all(|c| c.key.is_none()));
        release_tx.send(()).unwrap();
        pending.await.unwrap();
        assert_eq!(reader.inspection_position().await.unwrap(),*before.position());
        store.connection.call(|db| db.execute_batch("CREATE TRIGGER reject_projection BEFORE INSERT ON inspection_records BEGIN SELECT RAISE(ABORT,'injected'); END")).await.unwrap();
        let lease=store.acquire(&run.run_id,"replacement".into(),100,200).await.unwrap();
        let failed_before=reader.inspection_position().await.unwrap();
        assert_eq!(store.commit(&lease,transition("failed",vec![write("a",None,1)],101)).await,Err(StoreError::Database));
        assert_eq!(reader.inspection_position().await.unwrap(),failed_before);
        assert_eq!(reader.record(&run.run_id,&write("a",None,0).key).await,Err(StoreError::Missing));
        store.close().await.unwrap();
        reader.close().await.unwrap();
        let reopened=Store::open(&path,StoreConfig::default()).await.unwrap();
        assert_eq!(reopened.inspection_position().await.unwrap(),failed_before);
        assert_eq!(reopened.open_inspection_snapshot(&run.run_id,Some(before.position())).await.unwrap(),before);
        reopened.close().await.unwrap();
        std::fs::remove_file(&path).unwrap();
    });
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_byte_bounded_pages_make_progress_without_losing_rows() {
    actix::System::new().block_on(async {
        let store = Store::in_memory(StoreConfig::default()).await.unwrap();
        for n in 0..8 {
            store
                .create_run(seed(&format!("{n}{}", "x".repeat(120))))
                .await
                .unwrap();
        }
        let mut bounded = store.clone();
        bounded.config.limits.max_document_bytes = 512;
        let mut after = 0;
        let mut runs = Vec::new();
        loop {
            let page = bounded
                .inspection_runs(None, InspectionRunFilter::default(), after, 100)
                .await
                .unwrap();
            assert!(page.runs.len() <= 2);
            runs.extend(page.runs);
            match page.next {
                Some(next) => {
                    assert!(next > after);
                    after = next
                }
                None => break,
            }
        }
        assert_eq!(runs.len(), 8);
        let mut after = 0;
        let mut changes = Vec::new();
        loop {
            let page = bounded.inspection_changes(after, None, 100).await.unwrap();
            if page.is_empty() {
                break;
            }
            after = page.last().unwrap().cursor;
            changes.extend(page);
        }
        assert_eq!(changes.len(), 8);
        let run = &runs[0].run_id;
        let lease = store.acquire(run, "owner".into(), 0, 100).await.unwrap();
        let writes = (0..5)
            .map(|n| RecordWrite {
                payload: Value::Bytes(vec![n as u8; 180]),
                ..write(&format!("record-{n}"), None, 0)
            })
            .collect();
        store
            .commit(&lease, transition("records", writes, 1))
            .await
            .unwrap();
        let snapshot = store.open_inspection_snapshot(run, None).await.unwrap();
        let mut after = 0;
        let mut records = Vec::new();
        loop {
            let page = bounded
                .inspection_record_page(&snapshot, RecordFamily::Observation, after, 100)
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() < 5);
            after = page.last().unwrap().cursor;
            records.extend(page);
        }
        assert_eq!(records.len(), 5);
    });
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_historical_storage_contract() {
    let url = std::env::var("HTLK_TEST_POSTGRES_URL")
        .expect("postgres tests require HTLK_TEST_POSTGRES_URL");
    let mut settings = PostgresConfig::new(url.clone());
    settings.schema = format!(
        "htlk_inspection_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let schemas=actix::System::new().block_on(async {
        let store=PostgresStore::initialize(settings.clone(),StoreConfig::default()).await.unwrap();
        frozen_contract(&store).await;
        let run=seed("rollback");
        store.create_run(run.clone()).await.unwrap();
        let lease=store.acquire(&run.run_id,"owner".into(),0,100).await.unwrap();
        let original=store.open_inspection_snapshot(&run.run_id,None).await.unwrap();
        store.connection.call(|db| db.execute_batch("CREATE FUNCTION reject_inspection() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected projection failure'; END $$; CREATE TRIGGER reject_projection BEFORE INSERT ON inspection_records FOR EACH ROW EXECUTE FUNCTION reject_inspection()")).await.unwrap();
        assert_eq!(store.commit(&lease,transition("failed",vec![write("a",None,1)],1)).await,Err(StoreError::Database));
        assert_eq!(store.open_inspection_snapshot(&run.run_id,None).await.unwrap(),original);
        assert_eq!(store.inspection_position().await.unwrap(),*original.position());
        assert_eq!(store.record(&run.run_id,&write("a",None,0).key).await,Err(StoreError::Missing));
        store.connection.call(|db| db.execute_batch("DROP TRIGGER reject_projection ON inspection_records; DROP FUNCTION reject_inspection()")).await.unwrap();
        let prior=store.inspection_position().await.unwrap();
        let batch_lease=lease.clone();
        store.connection.call(move |db| -> Result<(),StoreError> {
            // Rolled-back and released domain savepoints all belong to this one
            // physical PostgreSQL commit, never separate historical positions.
            for (command,id,revision,value,commit) in [("discard","discard",None,0,false),("one","a",None,1,true),("two","a",Some(1),2,true)] {
                let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                transitions::PreparedTransition::new(&batch_lease,transition(command,vec![write(id,revision,value)],1),&Limits::default())?.apply(&tx,&batch_lease,&Limits::default())?;
                if commit { tx.commit()?; }
            }
            Ok(())
        }).await.unwrap();
        let batched=store.open_inspection_snapshot(&run.run_id,None).await.unwrap();
        assert_eq!(batched.position().number(),prior.number()+1);
        assert_eq!(batched.header().sequence,original.header().sequence+2);
        assert_eq!(store.inspection_record(&batched,&write("a",None,0).key).await.unwrap().payload,Value::Integer(2));
        assert_eq!(store.inspection_record(&batched,&write("discard",None,0).key).await,Err(StoreError::Missing));
        let batch_changes=store.inspection_changes(0,Some(batched.position()),100).await.unwrap();
        assert!(batch_changes.iter().filter(|c| c.position==*batched.position()).all(|c| c.sequence==batched.header().sequence));
        let before=store.inspection_position().await.unwrap();
        store.clone().close().await.unwrap();
        let reopened=PostgresStore::open(settings.clone(),StoreConfig::default()).await.unwrap();
        assert_eq!(reopened.inspection_position().await.unwrap(),before);
        reopened.close().await.unwrap();
        #[allow(unused_mut)]
        let mut schemas=vec![settings.schema.clone()];
        #[cfg(feature="sqlite")]
        {
            settings.schema.push_str("_expiry");
            let store=PostgresStore::initialize(settings.clone(),StoreConfig::default()).await.unwrap();
            initialization_and_expiry(&store).await;
            store.close().await.unwrap();
            schemas.push(settings.schema);
        }
        schemas
    });
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    for schema in schemas {
        client
            .batch_execute(&format!("UPDATE \"{schema}\".htlk_metadata SET version=1"))
            .unwrap();
        let mut obsolete = PostgresConfig::new(url.clone());
        obsolete.schema = schema.clone();
        actix::System::new().block_on(async {
            assert!(matches!(
                PostgresStore::open(obsolete.clone(), StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
            assert!(matches!(
                PostgresStore::initialize(obsolete, StoreConfig::default()).await,
                Err(StoreError::Schema)
            ));
        });
        assert_eq!(
            client
                .query_one(
                    &format!("SELECT version FROM \"{schema}\".htlk_metadata"),
                    &[]
                )
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        client
            .batch_execute(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
            .unwrap();
    }
}
