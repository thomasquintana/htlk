//! Owned disposable PostgreSQL processes; never uses HTLK_TEST_POSTGRES_URL.

use std::{
    net::SocketAddr,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct Cluster {
    container: String,
    pub connection: String,
    pub standby_connection: String,
}

fn docker(args: &[&str]) -> String {
    let result = Command::new("docker")
        .args(args)
        .output()
        .expect("Docker is required for isolated synchronous replication tests");
    assert!(
        result.status.success(),
        "disposable PostgreSQL command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

pub fn until<T>(label: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "checkpoint not observed: {label}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

impl Cluster {
    pub fn start() -> Self {
        let version = std::env::var("HTLK_TEST_POSTGRES_VERSION").unwrap_or_else(|_| "18".into());
        assert!(matches!(version.as_str(), "16" | "18"));
        let name = format!(
            "htlk-sync-ack-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        docker(&[
            "run",
            "--detach",
            "--name",
            &name,
            "--label",
            "htlk.test=sync-ack",
            "--publish",
            "127.0.0.1::5432",
            "--publish",
            "127.0.0.1::5433",
            "--env",
            "POSTGRES_PASSWORD=htlk-test",
            "--env",
            "POSTGRES_DB=htlk_sync_ack",
            "--env",
            "PGDATA=/var/lib/postgresql/htlk",
            &format!("postgres:{version}"),
        ]);
        // Install the cleanup guard before readiness/configuration assertions.
        let mut cluster = Self {
            container: name,
            connection: String::new(),
            standby_connection: String::new(),
        };
        cluster.connection = cluster.endpoint("5432/tcp");
        cluster.standby_connection = cluster.endpoint("5433/tcp");
        until("primary accepting connections", || {
            postgres::Client::connect(&cluster.connection, postgres::NoTls).ok()
        });
        let mut admin = cluster.admin();
        let version: String = admin.query_one("SHOW server_version", &[]).unwrap().get(0);
        println!("disposable synchronous replication fixture: PostgreSQL {version}");
        cluster
    }

    fn endpoint(&self, port: &str) -> String {
        let address: SocketAddr = docker(&["port", &self.container, port]).parse().unwrap();
        format!(
            "host=127.0.0.1 port={} user=postgres password=htlk-test dbname=htlk_sync_ack sslmode=disable",
            address.port()
        )
    }

    pub fn admin(&self) -> postgres::Client {
        let mut client = postgres::Client::connect(&self.connection, postgres::NoTls).unwrap();
        assert_eq!(
            client
                .query_one("SELECT current_database()", &[])
                .unwrap()
                .get::<_, String>(0),
            "htlk_sync_ack"
        );
        client
    }

    pub fn bootstrap_standby(&self) {
        // A second actual physical PostgreSQL server in the owned container.
        // Loopback replication uses initdb's local replication authentication.
        docker(&[
            "exec",
            "--user",
            "postgres",
            &self.container,
            "pg_basebackup",
            "--host=127.0.0.1",
            "--username=postgres",
            "--pgdata=/tmp/htlk_standby",
            "--write-recovery-conf",
            "--wal-method=stream",
        ]);
        self.start_standby();
        self.admin()
            .batch_execute("ALTER SYSTEM SET synchronous_standby_names='FIRST 1 (htlk_required)'")
            .unwrap();
        self.admin()
            .query_one("SELECT pg_reload_conf()", &[])
            .unwrap();
        until("required named synchronous standby", || {
            let mut admin = self.admin();
            let configured: String = admin
                .query_one("SHOW synchronous_standby_names", &[])
                .unwrap()
                .get(0);
            let ready: bool = admin.query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_replication WHERE application_name='htlk_required' AND state='streaming' AND sync_state='sync')", &[]
            ).unwrap().get(0);
            (configured == "FIRST 1 (htlk_required)" && ready).then_some(())
        });
    }

    pub fn start_standby(&self) {
        docker(&[
            "exec",
            "--detach",
            "--user",
            "postgres",
            &self.container,
            "postgres",
            "-D",
            "/tmp/htlk_standby",
            "-p",
            "5433",
            "-c",
            "listen_addresses=*",
            "-c",
            "unix_socket_directories=/tmp",
            "-c",
            "primary_conninfo=host=127.0.0.1 port=5432 user=postgres application_name=htlk_required",
        ]);
        until("physical standby accepting connections", || {
            postgres::Client::connect(&self.standby_connection, postgres::NoTls).ok()
        });
    }

    pub fn stop_standby(&self) {
        docker(&[
            "exec",
            "--user",
            "postgres",
            &self.container,
            "pg_ctl",
            "-D",
            "/tmp/htlk_standby",
            "--mode=immediate",
            "--wait",
            "stop",
        ]);
        until("required standby disconnected", || {
            let count: i64 = self
                .admin()
                .query_one("SELECT count(*) FROM pg_stat_replication", &[])
                .unwrap()
                .get(0);
            (count == 0).then_some(())
        });
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.container])
            .output();
    }
}

pub fn sync_wait(connection: &str, application: &str) -> i32 {
    let mut observer = postgres::Client::connect(connection, postgres::NoTls).unwrap();
    until("exact backend COMMIT / SyncRep", || {
        observer.query_opt(
            "SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND application_name=$1 AND query='COMMIT' AND wait_event='SyncRep'",
            &[&application],
        ).unwrap().map(|row| row.get(0))
    })
}

pub fn cancel_wait(connection: &str, pid: i32) {
    let mut observer = postgres::Client::connect(connection, postgres::NoTls).unwrap();
    assert!(observer.query_one(
        "SELECT COALESCE(bool_or(pg_cancel_backend(pid)),false) FROM pg_stat_activity WHERE pid=$1 AND datname=current_database() AND query='COMMIT' AND wait_event='SyncRep'",
        &[&pid],
    ).unwrap().get::<_, bool>(0));
    until("cancelled SyncRep wait released", || {
        let waiting: bool = observer.query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event='SyncRep')", &[&pid]
        ).unwrap().get(0);
        (!waiting).then_some(())
    });
}
