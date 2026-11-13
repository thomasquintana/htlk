//! Native acknowledgment qualification, not runtime/failover qualification.
#![cfg(feature = "postgres")]
#[path = "support/postgres_cluster.rs"]
mod postgres_cluster;

use htlk_executable::{
    cbor::{Limits, Value},
    digest::hash_bytes,
};
use htlk_runtime::{
    identity::{RunId, ScopeInstanceId},
    store::*,
};
use postgres_cluster::{Cluster, cancel_wait, sync_wait, until};
use std::{thread, time::Duration};

fn blocking<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    thread::scope(|scope| scope.spawn(work).join().unwrap())
}

fn settings(cluster: &Cluster, application: &str) -> PostgresConfig {
    let mut config = PostgresConfig::new(format!(
        "{} application_name={application} options='-c client_min_messages=error'",
        cluster.connection
    ));
    config.schema = "htlk_sync_ack".into();
    config
}

fn proposal(command: &str, key: &str) -> Transition {
    Transition {
        context: TransitionContext {
            command_id: command.into(),
            actor: "fixture-worker".into(),
            causation: "native-sync-ack".into(),
            timestamp_ms: 3,
        },
        writes: vec![RecordWrite {
            key: RecordKey {
                family: RecordFamily::Invocation,
                id: key.into(),
            },
            expected_revision: None,
            final_record: false,
            payload: Value::Text(key.into()),
        }],
        next_status: None,
        root: None,
        expected_fingerprint: None,
    }
}

#[test]
fn synchronous_replication_success_timeout_and_observed_server_cancellation() {
    let cluster = Cluster::start();
    actix::System::new().block_on(async {
        let store = PostgresStore::initialize(settings(&cluster, "htlk_sync_bootstrap"), StoreConfig::default()).await.unwrap();
        let run_id = RunId::new("sync-ack".into(), &Limits::default()).unwrap();
        store.create_run(RunSeed {
            run_id: run_id.clone(), root_scope: ScopeInstanceId::root(&run_id, &Limits::default()).unwrap(),
            fingerprint: hash_bytes(b"native store acknowledgment fixture"),
            actor: "fixture".into(), causation: "unprotected-bootstrap".into(), timestamp_ms: 1,
        }).await.unwrap();
        let lease = store.acquire(&run_id, "fixture-worker".into(), 2, 1000).await.unwrap();
        blocking(|| cluster.bootstrap_standby());
        let healthy = store.commit(&lease, proposal("healthy", "healthy")).await.unwrap();
        assert_eq!(healthy.sequence, 3);

        let key = RecordKey { family: RecordFamily::Invocation, id: "healthy".into() };
        assert_eq!(store.record(&run_id, &key).await.unwrap().revision, 1);
        // Independently verify replay through a primary WAL checkpoint obtained
        // after the receipt, without reading private native tables on the standby.
        let target: String = blocking(|| cluster.admin().query_one("SELECT pg_current_wal_flush_lsn()::text", &[]).unwrap().get(0));
        let standby_connection = cluster.standby_connection.clone();
        thread::spawn(move || {
            let mut standby = postgres::Client::connect(&standby_connection, postgres::NoTls).unwrap();
            until("standby replay reaches acknowledged primary WAL", || {
                standby.query_one("SELECT pg_last_wal_replay_lsn() >= $1::text::pg_lsn", &[&target]).unwrap().get::<_, bool>(0).then_some(())
            });
        }).join().unwrap();
        // PostgresStore requires a writable primary; standby verification uses WAL
        // replay checkpoints rather than opening an unsupported standby Store.
        blocking(|| cluster.stop_standby());

        let mut timed_settings = settings(&cluster, "htlk_sync_timeout");
        timed_settings.statement_timeout = Duration::from_secs(1);
        timed_settings.operation_timeout = Duration::from_secs(3);
        let timed = PostgresStore::open(timed_settings, StoreConfig::default()).await.unwrap();
        let connection = cluster.connection.clone();
        let monitor = thread::spawn(move || sync_wait(&connection, "htlk_sync_timeout"));
        let result = timed.commit(&lease, proposal("timeout-original", "timeout")).await;
        let pid = monitor.join().unwrap();
        println!("observed COMMIT/SyncRep timeout: {result:?}");
        assert_eq!(result, Err(StoreError::CommitUnknown));
        assert_eq!(timed.run_header(&run_id).await, Err(StoreError::Database));
        // A client timeout does not establish rollback or end the server wait.
        blocking(|| cancel_wait(&cluster.connection, pid));
        timed.close().await.unwrap();

        let cancelled = PostgresStore::open(settings(&cluster, "htlk_sync_cancel"), StoreConfig::default()).await.unwrap();
        let connection = cluster.connection.clone();
        let monitor = thread::spawn(move || {
            let pid = sync_wait(&connection, "htlk_sync_cancel");
            cancel_wait(&connection, pid);
        });
        let result = cancelled.commit(&lease, proposal("cancel-original\0identity", "cancelled")).await;
        monitor.join().unwrap();
        println!("observed COMMIT/SyncRep server cancellation: {result:?}");
        assert_eq!(result, Err(StoreError::CommitUnknown));
        assert_eq!(cancelled.run_header(&run_id).await, Err(StoreError::Database));
        assert_eq!(cancelled.commit(&lease, proposal("cancel-original\0identity", "cancelled")).await, Err(StoreError::Database));
        cancelled.close().await.unwrap();

        // Receipt reconciliation preserves the original identity and evidence.
        // These local read-only receipt returns DO NOT prove replicated durability:
        // the required standby is still absent throughout this section.
        let recovered = PostgresStore::open(settings(&cluster, "htlk_sync_reconcile"), StoreConfig::default()).await.unwrap();
        assert_eq!(recovered.commit(&lease, proposal("timeout-original", "timeout")).await.unwrap().sequence, 4);
        let receipt = recovered.commit(&lease, proposal("cancel-original\0identity", "cancelled")).await.unwrap();
        assert_eq!(receipt.sequence, 5);
        assert_eq!(recovered.commit(&lease, proposal("cancel-original\0identity", "different")).await, Err(StoreError::IdempotencyConflict));
        let key = RecordKey { family: RecordFamily::Invocation, id: "cancelled".into() };
        assert_eq!(recovered.record(&run_id, &key).await.unwrap().revision, 1);
        let journal = recovered.journal(&run_id, 0, 10).await.unwrap();
        assert_eq!(journal.len(), 5);
        assert_eq!(journal[4].command_id.as_deref(), Some("cancel-original\0identity"));
        assert_eq!(recovered.commit(&lease, proposal("cancel-original\0identity", "cancelled")).await.unwrap(), receipt);
        assert_eq!(blocking(|| cluster.admin().query_one("SELECT count(*) FROM pg_stat_replication", &[]).unwrap().get::<_, i64>(0)), 0);

        blocking(|| { cluster.start_standby();
        until("synchronous standby restored", || {
            cluster.admin().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_replication WHERE application_name='htlk_required' AND sync_state='sync')", &[]).unwrap().get::<_, bool>(0).then_some(())
        }); });
        assert_eq!(recovered.commit(&lease, proposal("after-restoration", "restored")).await.unwrap().sequence, 6);
        recovered.close().await.unwrap();
        store.close().await.unwrap();
    });
}
