//! Deadline-bounded protocol I/O on the connection's dedicated worker runtime.
//! Requests and asynchronous messages share one ordered driver. An abandoned or
//! warning-tainted commit response cannot authorize connection reuse.

use postgres::types::{ToSql, Type};
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::{Duration, Instant},
};
use tokio_postgres::{AsyncMessage, Row, Statement, ToStatement, error::Severity};

type Protocol = tokio_postgres::Connection<
    tokio_postgres::Socket,
    postgres_native_tls::TlsStream<tokio_postgres::Socket>,
>;

pub(crate) enum Error {
    BeforeSend,
    Timeout,
    Disconnected,
    CommitWarning,
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
            Self::Timeout | Self::Disconnected | Self::CommitWarning => false,
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
    protocol: RefCell<Option<Protocol>>,
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
        Ok(Self {
            runtime,
            inner,
            protocol: RefCell::new(Some(connection)),
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
        self.drive_ordered(future, false)
    }
    fn drive_ordered<T>(
        &self,
        future: impl Future<Output = Result<T, postgres::Error>>,
        committing: bool,
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
        let outcome = {
            let mut protocol = self.protocol.borrow_mut();
            let Some(protocol) = protocol.as_mut() else {
                return Err(Error::BeforeSend);
            };
            let mut future = pin!(future);
            // A latch, not a notice queue: no diagnostic text or execution content
            // is retained or forwarded to the dependency's general notice logger.
            let mut warning = false;
            let ordered = poll_fn(|cx| {
                // Bound each poll even if the server sends an endless notice stream,
                // allowing the existing operation/I/O deadline to keep making progress.
                for _ in 0..64 {
                    if let Poll::Ready(result) = future.as_mut().poll(cx) {
                        return Poll::Ready(commit_result(result, warning));
                    }
                    match protocol.poll_message(cx) {
                        Poll::Ready(Some(Ok(message))) => {
                            // poll_message may route ReadyForQuery before returning
                            // a *following* asynchronous message. Finish the request
                            // first in that case: an idle notice is not its warning.
                            if let Poll::Ready(result) = future.as_mut().poll(cx) {
                                return Poll::Ready(commit_result(result, warning));
                            }
                            if committing && let AsyncMessage::Notice(notice) = message {
                                // PostgreSQL >=16 supplies nonlocalized severity.
                                // Missing classification cannot prove benignity.
                                warning |= matches!(
                                    notice.parsed_severity(),
                                    Some(Severity::Warning) | None
                                );
                            }
                            // Notifications and benign notices remain ordinary
                            // asynchronous messages, never commit-failure evidence.
                        }
                        Poll::Ready(Some(Err(error))) => {
                            return Poll::Ready(Err(Error::Protocol(error)));
                        }
                        Poll::Ready(None) => return Poll::Ready(Err(Error::Disconnected)),
                        Poll::Pending => {
                            // Normal responses may have been delivered without any
                            // asynchronous message; consume them before yielding.
                            return future
                                .as_mut()
                                .poll(cx)
                                .map(|result| commit_result(result, warning));
                        }
                    }
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            });
            self.runtime
                .block_on(async { tokio::time::timeout(remaining, ordered).await })
        };
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                if !matches!(&error, Error::Protocol(error) if error.as_db_error().is_some()) {
                    self.abort();
                }
                Err(error)
            }
            Err(_) => {
                self.abort();
                Err(Error::Timeout)
            }
        }
    }
    pub(crate) fn abort(&self) {
        self.closed.set(true);
        // Drop the socket, including buffered responses. Nothing can subsequently
        // drain an uncertain completion and turn this connection reusable.
        self.protocol.borrow_mut().take();
    }
    pub(crate) fn commit(&self) -> Result<(), Error> {
        // Some driver methods (notably prepare) finish before ReadyForQuery. An
        // empty simple query fences all preceding responses/notices before opening
        // the commit-only observation window. A refusal here precedes COMMIT.
        self.batch_execute("").map_err(|_| Error::BeforeSend)?;
        let result = self.drive_ordered(self.inner.batch_execute("COMMIT"), true);
        if result
            .as_ref()
            .is_err_and(|error| !error.known_commit_rollback())
        {
            self.abort();
        }
        result
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

fn commit_result<T>(result: Result<T, postgres::Error>, warning: bool) -> Result<T, Error> {
    if warning {
        Err(Error::CommitWarning)
    } else {
        result.map_err(Error::Protocol)
    }
}
