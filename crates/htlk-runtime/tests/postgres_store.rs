//! Native PostgreSQL transaction evidence, observed through independent connections.
#![cfg(feature = "postgres")]

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
    let mut settings = PostgresConfig::new(
        std::env::var("HTLK_TEST_POSTGRES_URL")
            .expect("postgres tests require HTLK_TEST_POSTGRES_URL"),
    );
    settings.schema = format!(
        "htlk_contract_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
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
