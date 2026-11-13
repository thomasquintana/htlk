//! Native PostgreSQL transaction evidence, observed through independent connections.
#![cfg(feature = "postgres")]
#[path = "support/commit_proxy.rs"]
mod commit_proxy;

use htlk_executable::{
    cbor::{Limits, Value},
    digest::Digest,
};
use htlk_runtime::{
    identity::{RunId, ScopeInstanceId},
    store::*,
};
use std::time::{SystemTime, UNIX_EPOCH};

fn settings() -> PostgresConfig {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut settings = PostgresConfig::new(
        std::env::var("HTLK_TEST_POSTGRES_URL")
            .expect("postgres tests require HTLK_TEST_POSTGRES_URL"),
    );
    let identity = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let digest = htlk_executable::digest::hash_bytes(identity.as_bytes()).to_string();
    settings.schema = format!("htlk_contract_{}", &digest[7..47]);
    settings
}
fn admin<R: Send + 'static>(
    settings: &PostgresConfig,
    work: impl FnOnce(&mut postgres::Client, &str) -> R + Send + 'static,
) -> R {
    let connection = std::env::var("HTLK_TEST_POSTGRES_URL").unwrap();
    let schema = settings.schema.clone();
    std::thread::spawn(move || {
        let mut client = postgres::Client::connect(&connection, postgres::NoTls).unwrap();
        client
            .batch_execute(&format!("SET search_path TO \"{schema}\", pg_catalog"))
            .unwrap();
        work(&mut client, &schema)
    })
    .join()
    .unwrap()
}
fn seed() -> RunSeed {
    let run_id = RunId::new("contract\0run".into(), &Limits::default()).unwrap();
    let root_scope = ScopeInstanceId::root(&run_id, &Limits::default()).unwrap();
    RunSeed {
        run_id,
        root_scope,
        fingerprint: Digest::from_bytes([7; 32]),
        actor: "host".into(),
        causation: "test".into(),
        timestamp_ms: 1,
    }
}
fn proposal(command: &str, revision: Option<i64>) -> Transition {
    Transition {
        context: TransitionContext {
            command_id: command.into(),
            actor: "host".into(),
            causation: "test".into(),
            timestamp_ms: 3,
        },
        writes: vec![RecordWrite {
            key: RecordKey {
                family: RecordFamily::Invocation,
                id: "node".into(),
            },
            expected_revision: revision,
            final_record: false,
            payload: Value::Null,
        }],
        next_status: None,
        root: None,
        expected_fingerprint: None,
    }
}

#[test]
fn separate_connections_have_one_acquisition_and_cas_winner_with_replayable_receipts() {
    actix::System::new().block_on(async {
        let settings = settings();
        let left = PostgresStore::initialize(settings.clone(), StoreConfig::default()).await.unwrap();
        let right = PostgresStore::open(settings.clone(), StoreConfig::default()).await.unwrap();
        let run = seed(); left.create_run(run.clone()).await.unwrap();
        let (a,b) = tokio::join!(left.acquire(&run.run_id,"left".into(),2,100),right.acquire(&run.run_id,"right".into(),2,100));
        let lease = match (a,b) { (Ok(lease),Err(StoreError::Fenced)) | (Err(StoreError::Fenced),Ok(lease)) => lease, other => panic!("unexpected acquisition results: {other:?}") };
        let (a,b) = tokio::join!(left.commit(&lease,proposal("left",None)),right.commit(&lease,proposal("right",None)));
        let command = match (a,b) { (Ok(_),Err(StoreError::Conflict)) => "left", (Err(StoreError::Conflict),Ok(_)) => "right", other => panic!("unexpected CAS results: {other:?}") };
        let receipt = left.commit(&lease,proposal(command,None)).await.unwrap();
        assert_eq!(receipt.sequence,3);
        assert_eq!(right.commit(&lease,proposal(command,Some(1))).await,Err(StoreError::IdempotencyConflict));
        let entries = right.journal(&run.run_id,0,10).await.unwrap();
        let mut replay = ReplayState::begin(&entries[0],&Limits::default()).unwrap();
        for entry in &entries[1..] { replay.apply(entry,&Limits::default()).unwrap(); }
        assert_eq!(right.run_header(&run.run_id).await.unwrap().sequence,3);
        assert_eq!(right.pending_outbox(10).await.unwrap().len(),3);
        for notice in right.pending_outbox(10).await.unwrap() { left.acknowledge(&notice).await.unwrap(); right.acknowledge(&notice).await.unwrap(); }
        assert!(right.pending_outbox(10).await.unwrap().is_empty());
        assert_eq!(admin(&settings,|db,_| db.query_one("SELECT (SELECT count(*) FROM records),(SELECT count(*) FROM commands),(SELECT count(*) FROM journal)",&[]).map(|r|(r.get::<_,i64>(0),r.get::<_,i64>(1),r.get::<_,i64>(2))).unwrap()),(1,1,3));
        left.close().await.unwrap(); right.close().await.unwrap();
    });
}

#[test]
fn outbox_and_deferred_commit_failures_publish_nothing_and_do_not_poison_connections() {
    actix::System::new().block_on(async {
        let settings = settings();
        let store = PostgresStore::initialize(settings.clone(),StoreConfig::default()).await.unwrap();
        let observer = PostgresStore::open(settings.clone(),StoreConfig::default()).await.unwrap();
        let run = seed(); store.create_run(run.clone()).await.unwrap();
        let lease = store.acquire(&run.run_id,"worker".into(),2,100).await.unwrap();
        let before = observer.run_header(&run.run_id).await.unwrap();
        admin(&settings,|db,_| db.batch_execute("CREATE FUNCTION fail_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$; CREATE TRIGGER fail_outbox BEFORE INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION fail_outbox();").unwrap());
        assert_eq!(store.commit(&lease,proposal("rollback",None)).await,Err(StoreError::Database));
        assert_eq!(observer.run_header(&run.run_id).await.unwrap(),before);
        assert!(observer.records(&run.run_id,RecordFamily::Invocation,None,10).await.unwrap().is_empty());
        admin(&settings,|db,_| db.batch_execute("DROP TRIGGER fail_outbox ON outbox; DROP FUNCTION fail_outbox(); CREATE TABLE injected_reference(run_key bytea REFERENCES runs(run_key) DEFERRABLE INITIALLY DEFERRED); CREATE FUNCTION fail_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO injected_reference VALUES(decode(repeat('00',32),'hex')); RETURN NEW; END $$; CREATE TRIGGER fail_commit AFTER INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION fail_commit();").unwrap());
        assert_eq!(store.commit(&lease,proposal("rollback",None)).await,Err(StoreError::Database));
        assert_eq!(observer.run_header(&run.run_id).await.unwrap(),before);
        assert_eq!(observer.journal(&run.run_id,0,10).await.unwrap().len(),2);
        admin(&settings,|db,_| db.batch_execute("DROP TRIGGER fail_commit ON outbox; DROP FUNCTION fail_commit(); DROP TABLE injected_reference;").unwrap());
        assert_eq!(store.commit(&lease,proposal("rollback",None)).await.unwrap().sequence,3);
        assert_eq!(admin(&settings,|db,_| db.query_one("SELECT (SELECT count(*) FROM records),(SELECT count(*) FROM commands),(SELECT count(*) FROM outbox)",&[]).map(|r|(r.get::<_,i64>(0),r.get::<_,i64>(1),r.get::<_,i64>(2))).unwrap()),(1,1,3));
        store.close().await.unwrap(); observer.close().await.unwrap();
    });
}

#[test]
fn observed_commit_with_lost_ack_recovers_one_receipt_and_never_restarts_a_work_intent() {
    actix::System::new().block_on(async {
        let settings = settings();
        let observer = PostgresStore::initialize(settings.clone(), StoreConfig::default()).await.unwrap();
        let run = seed(); observer.create_run(run.clone()).await.unwrap();
        let lease = observer.acquire(&run.run_id, "owner".into(), 2, 100).await.unwrap();
        let proxy = commit_proxy::CommitProxy::start();
        let mut through_proxy = PostgresConfig::new(proxy.connection());
        through_proxy.schema = settings.schema.clone();
        let uncertain = PostgresStore::open(through_proxy, StoreConfig::default()).await.unwrap();
        proxy.arm();
        assert_eq!(uncertain.commit(&lease, proposal("uncertain", None)).await, Err(StoreError::CommitUnknown));
        assert!(proxy.observed_commit());
        assert_eq!(uncertain.commit(&lease, proposal("uncertain", None)).await, Err(StoreError::Database));
        let receipt = observer.commit(&lease, proposal("uncertain", None)).await.unwrap();
        assert_eq!(receipt.sequence, 3);
        assert_eq!(observer.journal(&run.run_id, 0, 10).await.unwrap().len(), 3);
        uncertain.close().await.unwrap();

        use htlk_runtime::{HostContext, mcp::client::McpRequestId, records::SecurityLabels};
        use htlk_executable::digest::hash_bytes;
        let host = HostContext::new("alice".into(), &Limits::default()).unwrap();
        let owner = observer.acquire_mcp_owner(&host, "work", 1, 1000).await.unwrap();
        let epoch = observer.open_mcp_epoch(&owner, hash_bytes(b"server"), 2).await.unwrap();
        let interaction = observer.receive_mcp_interaction(&owner, epoch.id, br#"{"jsonrpc":"2.0","id":"work","method":"sampling/createMessage","params":{"messages":[],"maxTokens":1}}"#.to_vec(), SecurityLabels::default(), 900, 3, 8).await.unwrap();
        assert_eq!(observer.mcp_interaction_id(epoch.id, &McpRequestId::String("work".into())).unwrap(), interaction.id);
        let proxy = commit_proxy::CommitProxy::start();
        let mut through_proxy = PostgresConfig::new(proxy.connection()); through_proxy.schema = settings.schema.clone();
        let uncertain = PostgresStore::open(through_proxy, StoreConfig::default()).await.unwrap();
        proxy.arm();
        assert!(matches!(uncertain.begin_mcp_work(&owner, interaction.id, 4, 8, 2).await, Err(StoreError::CommitUnknown)));
        assert!(proxy.observed_commit());
        let existing = observer.begin_mcp_work(&owner, interaction.id, 4, 8, 2).await.unwrap();
        assert!(matches!(existing, McpWorkClaim::Existing(ref work) if work.state == McpWorkState::Intent));
        let usage = observer.mcp_owner_usage(&owner).await.unwrap();
        assert_eq!((usage.calls_spent, usage.outstanding), (1, 1));
        uncertain.close().await.unwrap(); observer.close().await.unwrap();
    });
}

#[test]
fn commit_notices_follow_protocol_boundaries_without_stale_contamination() {
    use commit_proxy::Fault;
    actix::System::new().block_on(async {
        let settings = settings();
        let observer = PostgresStore::initialize(settings.clone(), StoreConfig::default())
            .await
            .unwrap();
        let run = seed();
        observer.create_run(run.clone()).await.unwrap();
        let lease = observer
            .acquire(&run.run_id, "owner".into(), 2, 100)
            .await
            .unwrap();
        for (index, fault) in [
            Fault::WarningBeforeComplete,
            Fault::WarningBeforeReady,
            Fault::FragmentedWarning,
            Fault::UnclassifiedNotice,
            Fault::WarningAfterReady,
            Fault::WarningBeforeCommit,
            Fault::BenignMessages,
        ]
        .into_iter()
        .enumerate()
        {
            let proxy = commit_proxy::CommitProxy::with_fault(fault);
            let mut config = PostgresConfig::new(proxy.connection());
            config.schema = settings.schema.clone();
            let store = PostgresStore::open(config, StoreConfig::default())
                .await
                .unwrap();
            proxy.arm();
            let command = format!("notice-{index}");
            let mut change = proposal(&command, None);
            change.writes[0].key.id = command.clone();
            let result = store.commit(&lease, change.clone()).await;
            let uncertain = matches!(
                fault,
                Fault::WarningBeforeComplete
                    | Fault::WarningBeforeReady
                    | Fault::FragmentedWarning
                    | Fault::UnclassifiedNotice
            );
            if uncertain {
                assert_eq!(result, Err(StoreError::CommitUnknown));
                assert_eq!(
                    store.run_header(&run.run_id).await,
                    Err(StoreError::Database)
                );
            } else {
                assert_eq!(result.as_ref().unwrap().sequence, 3 + index as i64);
                // Two more physical commits exercise a warning after the previous
                // ReadyForQuery and prove it cannot taint the next observation.
                assert_eq!(
                    store.run_header(&run.run_id).await.unwrap().sequence,
                    3 + index as i64
                );
                assert_eq!(store.commit(&lease, change.clone()).await, result);
            }
            assert_eq!(
                observer.commit(&lease, change).await.unwrap().sequence,
                3 + index as i64
            );
            store.close().await.unwrap();
        }
        assert_eq!(observer.journal(&run.run_id, 0, 10).await.unwrap().len(), 9);
        observer.close().await.unwrap();
    });
}

#[test]
fn initialization_and_opening_use_the_same_guarded_commit() {
    actix::System::new().block_on(async {
        for (initialize, fault) in [
            (true, commit_proxy::Fault::WarningBeforeReady),
            (false, commit_proxy::Fault::WarningBeforeReady),
            (true, commit_proxy::Fault::LostAck),
            (false, commit_proxy::Fault::LostAck),
        ] {
            let settings = settings();
            if !initialize {
                PostgresStore::initialize(settings.clone(), StoreConfig::default())
                    .await
                    .unwrap()
                    .close()
                    .await
                    .unwrap();
            }
            let proxy = commit_proxy::CommitProxy::with_fault(fault);
            let mut config = PostgresConfig::new(proxy.connection());
            config.schema = settings.schema.clone();
            proxy.arm();
            let result = if initialize {
                PostgresStore::initialize(config, StoreConfig::default()).await
            } else {
                PostgresStore::open(config, StoreConfig::default()).await
            };
            assert!(matches!(result, Err(StoreError::CommitUnknown)));
            assert!(proxy.observed_commit());
            // Uncertain initialization can have created a complete recognized
            // schema; inspect/reopen, never relabel or delete it as a rollback.
            PostgresStore::open(settings, StoreConfig::default())
                .await
                .unwrap()
                .close()
                .await
                .unwrap();
        }
    });
}

#[test]
fn stalled_authentication_and_tls_negotiation_obey_the_whole_connection_deadline() {
    use std::{
        net::TcpListener,
        sync::mpsc,
        time::{Duration, Instant},
    };
    for sslmode in ["disable", "require"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release, stalled) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            let _ = stalled.recv_timeout(Duration::from_secs(5));
        });
        let mut config = PostgresConfig::new(format!(
            "host=127.0.0.1 port={} user=postgres sslmode={sslmode}",
            address.port()
        ));
        config.connect_timeout = Duration::from_millis(50);
        let at = Instant::now();
        actix::System::new().block_on(async {
            assert!(matches!(
                PostgresStore::open(config, StoreConfig::default()).await,
                Err(StoreError::Database)
            ));
        });
        assert!(
            at.elapsed() < Duration::from_secs(1),
            "stalled handshake exceeded connection budget"
        );
        release.send(()).unwrap();
        server.join().unwrap();
    }
}

#[test]
fn stored_field_limits_are_enforced_before_transmission_and_pages_remain_bounded() {
    actix::System::new().block_on(async {
        let settings = settings();
        let writer = PostgresStore::initialize(settings.clone(), StoreConfig::default())
            .await
            .unwrap();
        let run = seed();
        writer.create_run(run.clone()).await.unwrap();
        let lease = writer
            .acquire(&run.run_id, "writer".into(), 2, 100)
            .await
            .unwrap();
        let mut change = proposal("large", None);
        change.writes[0].payload = Value::Bytes(vec![0; 2048]);
        writer.commit(&lease, change).await.unwrap();
        let config = StoreConfig {
            limits: Limits {
                max_document_bytes: 512,
                ..Default::default()
            },
            ..Default::default()
        };
        let reader = PostgresStore::open(settings.clone(), config).await.unwrap();
        let key = RecordKey {
            family: RecordFamily::Invocation,
            id: "node".into(),
        };
        assert_eq!(
            reader.record(&run.run_id, &key).await,
            Err(StoreError::Invalid)
        );
        assert_eq!(
            reader
                .records(&run.run_id, RecordFamily::Invocation, None, 10)
                .await,
            Err(StoreError::Invalid)
        );
        assert_eq!(
            writer.record(&run.run_id, &key).await.unwrap().payload,
            Value::Bytes(vec![0; 2048])
        );
        reader.close().await.unwrap();
        writer.close().await.unwrap();
    });
}

#[test]
fn a_database_lock_timeout_rolls_back_and_the_same_connection_remains_usable() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    actix::System::new().block_on(async {
        let mut settings = settings(); settings.lock_timeout = Duration::from_millis(50);
        let store = PostgresStore::initialize(settings.clone(),StoreConfig::default()).await.unwrap();
        let run = seed(); store.create_run(run.clone()).await.unwrap();
        let schema = settings.schema.clone();
        let url = std::env::var("HTLK_TEST_POSTGRES_URL").unwrap();
        let (ready, locked) = tokio::sync::oneshot::channel();
        let (release, held) = mpsc::channel();
        let locker = std::thread::spawn(move || {
            let mut db = postgres::Client::connect(&url,postgres::NoTls).unwrap();
            db.batch_execute(&format!("SET search_path TO \"{schema}\",pg_catalog;BEGIN;SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || current_schema(),1213484107));")).unwrap();
            ready.send(()).unwrap();
            let _ = held.recv_timeout(Duration::from_secs(5));
            db.batch_execute("ROLLBACK").unwrap();
        });
        locked.await.unwrap();
        let at = Instant::now();
        assert_eq!(store.run_header(&run.run_id).await,Err(StoreError::Database));
        assert!(at.elapsed()<Duration::from_secs(1));
        release.send(()).unwrap(); locker.join().unwrap();
        assert_eq!(store.run_header(&run.run_id).await.unwrap().sequence,1);
        store.close().await.unwrap();
    });
}

#[test]
fn statement_and_total_operation_deadlines_cannot_publish_a_delayed_transition() {
    use std::time::{Duration, Instant};
    for operation_deadline in [false, true] {
        actix::System::new().block_on(async {
            let mut settings = settings();
            let observer = PostgresStore::initialize(settings.clone(),StoreConfig::default()).await.unwrap();
            let run = seed(); observer.create_run(run.clone()).await.unwrap();
            admin(&settings,|db,_| db.batch_execute("CREATE FUNCTION delay_authority() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1); RETURN NEW; END $$; CREATE TRIGGER delay_authority BEFORE UPDATE ON runs FOR EACH ROW EXECUTE FUNCTION delay_authority();").unwrap());
            if operation_deadline { settings.operation_timeout=Duration::from_millis(50); }
            else { settings.statement_timeout=Duration::from_millis(50); }
            let store = PostgresStore::open(settings.clone(),StoreConfig::default()).await.unwrap();
            let at = Instant::now();
            assert_eq!(store.acquire(&run.run_id,"delayed".into(),2,100).await,Err(StoreError::Database));
            assert!(at.elapsed()<Duration::from_millis(750));
            assert_eq!(observer.run_header(&run.run_id).await.unwrap().fence,0);
            assert_eq!(observer.journal(&run.run_id,0,10).await.unwrap().len(),1);
            store.close().await.unwrap(); observer.close().await.unwrap();
        });
    }
}

#[test]
fn a_conflicting_lookup_preimage_fails_integrity_without_publishing_a_run() {
    actix::System::new().block_on(async {
        let settings = settings();
        let store = PostgresStore::initialize(settings.clone(), StoreConfig::default())
            .await
            .unwrap();
        let run = seed();
        let identity = run.run_id.as_str().as_bytes().to_vec();
        admin(&settings, move |db, _| {
            db.execute(
                "INSERT INTO htlk_opaque_keys(digest,original) VALUES(sha256($1),$2)",
                &[&identity, &b"different preimage".as_slice()],
            )
            .unwrap();
        });
        assert_eq!(
            store.create_run(run.clone()).await,
            Err(StoreError::Corrupt)
        );
        assert_eq!(
            store.run_header(&run.run_id).await,
            Err(StoreError::Missing)
        );
        assert!(store.pending_outbox(10).await.unwrap().is_empty());
        store.close().await.unwrap();
    });
}

#[test]
fn database_process_restart_preserves_authority_receipts_records_and_journal() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    struct Container(String);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args(["rm", "--force", "--volumes", &self.0])
                .output();
        }
    }
    let version = std::env::var("HTLK_TEST_POSTGRES_VERSION").unwrap_or_else(|_| "18".into());
    assert!(matches!(version.as_str(), "16" | "18"));
    let output = Command::new("docker")
        .args([
            "run",
            "--detach",
            "--publish",
            "127.0.0.1::5432",
            "--env",
            "POSTGRES_PASSWORD=htlk-test",
            "--env",
            "POSTGRES_DB=htlk_restart",
            &format!("postgres:{version}"),
        ])
        .output()
        .expect("Docker is required for the isolated PostgreSQL restart campaign");
    assert!(
        output.status.success(),
        "could not start isolated PostgreSQL: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let container = Container(String::from_utf8(output.stdout).unwrap().trim().into());
    assert_eq!(container.0.len(), 64);
    assert!(container.0.bytes().all(|b| b.is_ascii_hexdigit()));
    let output = Command::new("docker")
        .args(["port", &container.0, "5432/tcp"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let address: std::net::SocketAddr = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut config = PostgresConfig::new(format!(
        "host=127.0.0.1 port={} user=postgres password=htlk-test dbname=htlk_restart sslmode=disable",
        address.port()
    ));
    config.schema = "htlk_restart".into();
    actix::System::new().block_on(async {
        let deadline = Instant::now() + Duration::from_secs(20);
        let store = loop {
            match PostgresStore::initialize(config.clone(), StoreConfig::default()).await {
                Ok(store) => break store,
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "restart database did not initialize: {error}"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        let run = seed();
        store.create_run(run.clone()).await.unwrap();
        let lease = store
            .acquire(&run.run_id, "before-crash".into(), 2, 100)
            .await
            .unwrap();
        let receipt = store
            .commit(&lease, proposal("persisted", None))
            .await
            .unwrap();
        assert!(
            Command::new("docker")
                .args(["kill", "--signal", "KILL", &container.0])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(
            store.run_header(&run.run_id).await,
            Err(StoreError::Database)
        );
        assert!(
            Command::new("docker")
                .args(["start", &container.0])
                .output()
                .unwrap()
                .status
                .success()
        );
        // Docker may allocate a different ephemeral host port on start. The
        // database/volume identity is unchanged; reopen its current endpoint.
        let output = Command::new("docker").args(["port", &container.0, "5432/tcp"]).output().unwrap();
        assert!(output.status.success());
        let address: std::net::SocketAddr = String::from_utf8(output.stdout).unwrap().trim().parse().unwrap();
        let mut recovered_config = PostgresConfig::new(format!("host=127.0.0.1 port={} user=postgres password=htlk-test dbname=htlk_restart sslmode=disable", address.port()));
        recovered_config.schema = config.schema.clone();
        let deadline = Instant::now() + Duration::from_secs(20);
        let reopened = loop {
            match PostgresStore::open(recovered_config.clone(), StoreConfig::default()).await {
                Ok(store) => break store,
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "restart database did not recover: {error}"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        assert_eq!(
            reopened
                .commit(&lease, proposal("persisted", None))
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(reopened.journal(&run.run_id, 0, 10).await.unwrap().len(), 3);
        assert_eq!(
            reopened
                .records(&run.run_id, RecordFamily::Invocation, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let higher = reopened
            .acquire(&run.run_id, "after-crash".into(), 100, 200)
            .await
            .unwrap();
        assert!(higher.fence() > lease.fence());
        assert_eq!(
            reopened.commit(&lease, proposal("persisted", None)).await,
            Err(StoreError::Fenced)
        );
        assert_eq!(
            reopened
                .commit(&higher, proposal("persisted", None))
                .await
                .unwrap(),
            receipt
        );
        reopened.close().await.unwrap();
        store.close().await.unwrap();
    });
}
