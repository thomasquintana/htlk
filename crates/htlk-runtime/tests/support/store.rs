//! Test-only backend selection; production constructors never inspect test variables.
#![allow(dead_code)]

use htlk_runtime::store::{Store, StoreConfig, StoreError};
use std::{ops::Deref, path::Path};

#[derive(Clone)]
pub struct TestStore(Store);
impl TestStore {
    pub async fn in_memory(config: StoreConfig) -> Result<Self, StoreError> {
        if postgres_selected() {
            #[cfg(feature = "postgres")]
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let identity = format!(
                    "{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                return Self::postgres(identity.as_bytes(), config).await;
            }
            #[cfg(not(feature = "postgres"))]
            panic!("HTLK_TEST_BACKEND=postgres requires the postgres Cargo feature");
        }
        Store::in_memory(config).await.map(Self)
    }
    pub async fn open(path: impl AsRef<Path>, config: StoreConfig) -> Result<Self, StoreError> {
        if postgres_selected() {
            #[cfg(feature = "postgres")]
            return Self::postgres(path.as_ref().as_os_str().as_encoded_bytes(), config).await;
            #[cfg(not(feature = "postgres"))]
            panic!("HTLK_TEST_BACKEND=postgres requires the postgres Cargo feature");
        }
        Store::open(path, config).await.map(Self)
    }
    #[cfg(feature = "postgres")]
    async fn postgres(identity: &[u8], config: StoreConfig) -> Result<Self, StoreError> {
        use htlk_runtime::store::{PostgresConfig, PostgresStore};
        let url = std::env::var("HTLK_TEST_POSTGRES_URL")
            .expect("PostgreSQL tests require HTLK_TEST_POSTGRES_URL");
        let mut settings = PostgresConfig::new(url);
        settings.schema = schema(identity);
        PostgresStore::initialize(settings, config)
            .await
            .map(|store| Self(store.into()))
    }
    pub async fn close(self) -> Result<(), StoreError> {
        self.0.close().await
    }
}

#[cfg(feature = "postgres")]
fn schema(identity: &[u8]) -> String {
    let digest = htlk_executable::digest::hash_bytes(identity).to_string();
    format!("htlk_test_{}", &digest[7..47])
}

#[cfg(feature = "postgres")]
pub fn postgres_admin<R: Send + 'static>(
    path: impl AsRef<Path>,
    work: impl FnOnce(&mut postgres::Client) -> R + Send + 'static,
) -> R {
    let schema = schema(path.as_ref().as_os_str().as_encoded_bytes());
    let url = std::env::var("HTLK_TEST_POSTGRES_URL").unwrap();
    std::thread::spawn(move || {
        let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        client
            .batch_execute(&format!("SET search_path TO \"{schema}\", pg_catalog"))
            .unwrap();
        work(&mut client)
    })
    .join()
    .expect("PostgreSQL independent inspector panicked")
}

pub fn remove_file(path: impl AsRef<Path>) -> std::io::Result<()> {
    if postgres_selected() {
        #[cfg(feature = "postgres")]
        {
            let schema = schema(path.as_ref().as_os_str().as_encoded_bytes());
            let url = std::env::var("HTLK_TEST_POSTGRES_URL").unwrap();
            return std::thread::spawn(move || {
                let mut client = postgres::Client::connect(&url, postgres::NoTls)
                    .map_err(std::io::Error::other)?;
                client
                    .batch_execute(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
                    .map_err(std::io::Error::other)
            })
            .join()
            .expect("PostgreSQL cleanup worker panicked");
        }
        #[cfg(not(feature = "postgres"))]
        panic!("PostgreSQL cleanup requires the postgres feature");
    }
    std::fs::remove_file(path)
}
impl Deref for TestStore {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.0
    }
}
impl From<TestStore> for Store {
    fn from(store: TestStore) -> Self {
        store.0
    }
}
pub fn postgres_selected() -> bool {
    match std::env::var("HTLK_TEST_BACKEND").as_deref() {
        Ok("postgres") => true,
        Ok("sqlite") => false,
        Err(_) => cfg!(all(feature = "postgres", not(feature = "sqlite"))),
        Ok(value) => panic!("unsupported HTLK_TEST_BACKEND: {value}"),
    }
}

// Real wall-clock campaigns include a network database and durable commits.
// Fake-clock deadline tests keep their exact timestamps and do not use this.
pub fn lease_ms(sqlite: i64) -> i64 {
    if postgres_selected() {
        sqlite.max(5000)
    } else {
        sqlite
    }
}
pub fn io_budget_ms(sqlite: u64) -> u64 {
    if postgres_selected() {
        sqlite * 4
    } else {
        sqlite
    }
}
