//! Deadline-bounded protocol I/O on the connection's dedicated worker runtime.
//! Timeouts abort the protocol task: an abandoned response cannot authorize reuse.

use postgres::types::{ToSql, Type};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    time::{Duration, Instant},
};
use tokio_postgres::{Row, Statement, ToStatement};

pub(crate) enum Error {
    BeforeSend,
    Timeout,
    Protocol(postgres::Error),
}
impl Error {
    pub(crate) fn known_commit_rollback(&self) -> bool {
        match self {
            Self::BeforeSend => true,
            Self::Protocol(error) => error.code().is_some_and(|code| {
                let code = code.code();
                code.starts_with("23") || matches!(code, "40001" | "40P01")
            }),
            Self::Timeout => false,
        }
    }
}
impl From<postgres::Error> for Error {
    fn from(error: postgres::Error) -> Self {
        Self::Protocol(error)
    }
}

pub(crate) struct Client {
    runtime: tokio::runtime::Runtime,
    inner: tokio_postgres::Client,
    protocol: RefCell<tokio::task::JoinHandle<()>>,
    timeout: Duration,
    deadline: Cell<Option<Instant>>,
    closed: Cell<bool>,
}
impl Client {
    pub(crate) fn connect(
        options: tokio_postgres::Config,
        tls: postgres_native_tls::MakeTlsConnector,
        connect_timeout: Duration,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::BeforeSend)?;
        let (inner, connection) = runtime.block_on(async {
            tokio::time::timeout(connect_timeout, options.connect(tls))
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(Error::Protocol)
        })?;
        let protocol = runtime.spawn(async move {
            let _ = connection.await;
        });
        Ok(Self {
            runtime,
            inner,
            protocol: RefCell::new(protocol),
            timeout,
            deadline: Cell::new(None),
            closed: Cell::new(false),
        })
    }
    pub(crate) fn set_deadline(&self, deadline: Option<Instant>) {
        self.deadline.set(deadline);
    }
    fn drive<T>(
        &self,
        future: impl Future<Output = Result<T, postgres::Error>>,
    ) -> Result<T, Error> {
        if self.closed.get() {
            return Err(Error::BeforeSend);
        }
        let remaining = self.deadline.get().map_or(self.timeout, |until| {
            until
                .saturating_duration_since(Instant::now())
                .min(self.timeout)
        });
        if remaining.is_zero() {
            self.abort();
            return Err(Error::BeforeSend);
        }
        let outcome = self
            .runtime
            .block_on(async { tokio::time::timeout(remaining, future).await });
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                if error.as_db_error().is_none() {
                    self.abort();
                }
                Err(Error::Protocol(error))
            }
            Err(_) => {
                self.abort();
                Err(Error::Timeout)
            }
        }
    }
    fn abort(&self) {
        if self.closed.replace(true) {
            return;
        }
        let mut protocol = self.protocol.borrow_mut();
        protocol.abort();
        self.runtime.block_on(async {
            let _ = (&mut *protocol).await;
        });
    }
    pub(crate) fn batch_execute(&self, sql: &str) -> Result<(), Error> {
        self.drive(self.inner.batch_execute(sql))
    }
    pub(crate) fn prepare(&self, sql: &str) -> Result<Statement, Error> {
        self.drive(self.inner.prepare(sql))
    }
    pub(crate) fn prepare_typed(&self, sql: &str, types: &[Type]) -> Result<Statement, Error> {
        self.drive(self.inner.prepare_typed(sql, types))
    }
    pub(crate) fn query_one<T: ToStatement + ?Sized>(
        &self,
        sql: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Row, Error> {
        self.drive(self.inner.query_one(sql, params))
    }
    pub(crate) fn query_opt<T: ToStatement + ?Sized>(
        &self,
        sql: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, Error> {
        self.drive(self.inner.query_opt(sql, params))
    }
    pub(crate) fn query<T: ToStatement + ?Sized>(
        &self,
        sql: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        self.drive(self.inner.query(sql, params))
    }
    pub(crate) fn execute<T: ToStatement + ?Sized>(
        &self,
        sql: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, Error> {
        self.drive(self.inner.execute(sql, params))
    }
}
