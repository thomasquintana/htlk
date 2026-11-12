//! Private typed relational boundary shared by the storage transaction bodies.
//!
//! Drivers own SQL execution and row lifetimes; domain code owns acceptance rules.
// Pure-evaluation builds retain API types but cannot construct driver state.
#![cfg_attr(not(any(feature = "sqlite", feature = "postgres")), allow(dead_code))]

use std::{fmt, ops::Deref, path::Path, time::Duration};

#[derive(Debug)]
pub(crate) enum Error {
    Database,
    Missing,
    Invalid,
    #[cfg(feature = "postgres")]
    Schema,
    #[cfg(feature = "postgres")]
    Corrupt,
    #[cfg(feature = "postgres")]
    CommitUnknown,
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("runtime relational operation failed")
    }
}
impl std::error::Error for Error {}
#[cfg(feature = "sqlite")]
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
            Self::Missing
        } else {
            Self::Database
        }
    }
}

#[derive(Clone)]
pub(crate) enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
pub(crate) mod types {
    use super::{Error, Result};
    #[derive(Clone, Copy)]
    pub(crate) enum ValueRef<'a> {
        Null,
        Other,
        Text(&'a str),
        Blob(&'a [u8]),
    }
    impl<'a> ValueRef<'a> {
        pub(crate) fn as_str(self) -> Result<&'a str> {
            match self {
                Self::Text(value) => Ok(value),
                _ => Err(Error::Invalid),
            }
        }
        pub(crate) fn as_blob(self) -> Result<&'a [u8]> {
            match self {
                Self::Blob(value) => Ok(value),
                _ => Err(Error::Invalid),
            }
        }
    }
}
impl Value {
    fn as_ref(&self) -> types::ValueRef<'_> {
        match self {
            Self::Null => types::ValueRef::Null,
            Self::Integer(_) | Self::Real(_) => types::ValueRef::Other,
            Self::Text(value) => types::ValueRef::Text(value),
            Self::Blob(value) => types::ValueRef::Blob(value),
        }
    }
}

pub(crate) trait ToSql {
    fn value(&self) -> Result<Value>;
}
impl<T: ToSql + ?Sized> ToSql for &T {
    fn value(&self) -> Result<Value> {
        (*self).value()
    }
}
impl<T: ToSql> ToSql for Option<T> {
    fn value(&self) -> Result<Value> {
        self.as_ref().map_or(Ok(Value::Null), ToSql::value)
    }
}
impl ToSql for str {
    fn value(&self) -> Result<Value> {
        Ok(Value::Text(self.into()))
    }
}
impl ToSql for String {
    fn value(&self) -> Result<Value> {
        self.as_str().value()
    }
}
impl ToSql for [u8] {
    fn value(&self) -> Result<Value> {
        Ok(Value::Blob(self.to_vec()))
    }
}
impl ToSql for Vec<u8> {
    fn value(&self) -> Result<Value> {
        self.as_slice().value()
    }
}
impl ToSql for bool {
    fn value(&self) -> Result<Value> {
        Ok(Value::Integer(i64::from(*self)))
    }
}
impl ToSql for f64 {
    fn value(&self) -> Result<Value> {
        Ok(Value::Real(*self))
    }
}
macro_rules! integers {
    ($($ty:ty),* $(,)?) => {$(
        impl ToSql for $ty {
            fn value(&self) -> Result<Value> {
                i64::try_from(*self).map(Value::Integer).map_err(|_| Error::Invalid)
            }
        }
        impl FromSql for $ty {
            fn read(value: &Value) -> Result<Self> {
                match value {
                    Value::Integer(n) => Self::try_from(*n).map_err(|_| Error::Invalid),
                    _ => Err(Error::Invalid),
                }
            }
        }
    )*};
}
integers!(i64, i32, u64, u32, usize, u8);

pub(crate) trait Params {
    fn values(self) -> Result<Vec<Value>>;
}
impl Params for [(); 0] {
    fn values(self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
}
macro_rules! arrays {
    ($($n:literal),*) => {$(impl<T: ToSql> Params for [T; $n] {
        fn values(self) -> Result<Vec<Value>> { self.iter().map(ToSql::value).collect() }
    })*};
}
arrays!(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
impl Params for &[&dyn ToSql] {
    fn values(self) -> Result<Vec<Value>> {
        self.iter().map(ToSql::value).collect()
    }
}
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        &[$(&$value as &dyn $crate::store_db::ToSql),*] as &[&dyn $crate::store_db::ToSql]
    };
}
pub(crate) use params;

pub(crate) trait FromSql: Sized {
    fn read(value: &Value) -> Result<Self>;
}
impl FromSql for String {
    fn read(value: &Value) -> Result<Self> {
        match value {
            Value::Text(value) => Ok(value.clone()),
            _ => Err(Error::Invalid),
        }
    }
}
impl FromSql for Vec<u8> {
    fn read(value: &Value) -> Result<Self> {
        match value {
            Value::Blob(value) => Ok(value.clone()),
            _ => Err(Error::Invalid),
        }
    }
}
impl FromSql for bool {
    fn read(value: &Value) -> Result<Self> {
        match value {
            Value::Integer(0) => Ok(false),
            Value::Integer(1) => Ok(true),
            _ => Err(Error::Invalid),
        }
    }
}
impl<T: FromSql> FromSql for Option<T> {
    fn read(value: &Value) -> Result<Self> {
        if matches!(value, Value::Null) {
            Ok(None)
        } else {
            T::read(value).map(Some)
        }
    }
}
pub(crate) trait OptionalExtension<T> {
    fn optional(self) -> Result<Option<T>>;
}
impl<T> OptionalExtension<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(Error::Missing) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub(crate) struct Row<'a> {
    values: Vec<Value>,
    marker: std::marker::PhantomData<&'a ()>,
}
impl Row<'_> {
    pub(crate) fn get<I: TryInto<usize>, T: FromSql>(&self, index: I) -> Result<T> {
        T::read(
            self.values
                .get(index.try_into().map_err(|_| Error::Invalid)?)
                .ok_or(Error::Invalid)?,
        )
    }
    pub(crate) fn get_ref(&self, index: usize) -> Result<types::ValueRef<'_>> {
        Ok(self.values.get(index).ok_or(Error::Invalid)?.as_ref())
    }
}

#[cfg(feature = "sqlite")]
pub(crate) struct SqliteConnection {
    inner: rusqlite::Connection,
    max_field_bytes: usize,
}
#[cfg(feature = "sqlite")]
impl SqliteConnection {
    fn new(inner: rusqlite::Connection) -> Self {
        Self {
            inner,
            max_field_bytes: usize::MAX,
        }
    }
    fn close(self) -> std::result::Result<(), (Box<Self>, Error)> {
        let Self {
            inner,
            max_field_bytes,
        } = self;
        inner.close().map_err(|(inner, error)| {
            (
                Box::new(Self {
                    inner,
                    max_field_bytes,
                }),
                error.into(),
            )
        })
    }
}
#[cfg(feature = "sqlite")]
impl Deref for SqliteConnection {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub(crate) enum Connection {
    #[cfg(feature = "sqlite")]
    Sqlite(Box<SqliteConnection>),
    #[cfg(feature = "postgres")]
    Postgres(Box<crate::store_postgres::Database>),
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    #[allow(dead_code)] // Pure-evaluation builds cannot construct a storage handle.
    Disabled,
}
#[derive(Clone, Copy)]
pub(crate) enum TransactionBehavior {
    Deferred,
    Immediate,
}

impl Connection {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        #[cfg(feature = "sqlite")]
        {
            Ok(Self::Sqlite(Box::new(SqliteConnection::new(
                rusqlite::Connection::open(path)?,
            ))))
        }
        #[cfg(not(feature = "sqlite"))]
        {
            let _ = path;
            Err(Error::Invalid)
        }
    }
    pub(crate) fn open_in_memory() -> Result<Self> {
        #[cfg(feature = "sqlite")]
        {
            Ok(Self::Sqlite(Box::new(SqliteConnection::new(
                rusqlite::Connection::open_in_memory()?,
            ))))
        }
        #[cfg(not(feature = "sqlite"))]
        {
            Err(Error::Invalid)
        }
    }
    pub(crate) fn close(self) -> std::result::Result<(), (Self, Error)> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => (*db)
                .close()
                .map_err(|(db, error)| (Self::Sqlite(db), error)),
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => Ok(()),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => Ok(()),
        }
    }
    pub(crate) fn busy_timeout(&self, timeout: Duration) -> Result<()> {
        let _ = timeout;
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => Ok(db.busy_timeout(timeout)?),
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => Ok(()),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = timeout;
                Err(Error::Invalid)
            }
        }
    }
    pub(crate) fn set_max_field_bytes(&mut self, maximum: usize) {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => db.max_field_bytes = maximum,
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => {
                let _ = maximum;
            }
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = maximum;
            }
        }
    }
    pub(crate) fn pragma_update(
        &self,
        schema: Option<&str>,
        name: &str,
        value: impl ToSql,
    ) -> Result<()> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => {
                if schema.is_some() {
                    return Err(Error::Invalid);
                }
                let value = sqlite_value(value.value()?);
                Ok(db.pragma_update(None, name, value)?)
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => {
                let _ = (schema, name, value);
                Err(Error::Invalid)
            }
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = (schema, name, value);
                Err(Error::Invalid)
            }
        }
    }
    pub(crate) fn pragma_query_value<T>(
        &self,
        schema: Option<&str>,
        name: &str,
        f: impl FnOnce(&Row<'_>) -> Result<T>,
    ) -> Result<T> {
        if schema.is_some() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(Error::Invalid);
        }
        self.query_row(&format!("PRAGMA {name}"), [], f)
    }
    pub(crate) fn execute_batch(&self, sql: &str) -> Result<()> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => Ok(db.execute_batch(sql)?),
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => db.batch(sql),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = sql;
                Err(Error::Invalid)
            }
        }
    }
    pub(crate) fn execute(&self, sql: &str, params: impl Params) -> Result<usize> {
        let values = params.values()?;
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => Ok(db.execute(
                sql,
                rusqlite::params_from_iter(values.into_iter().map(sqlite_value)),
            )?),
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => db.execute(sql, values),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = (sql, values);
                Err(Error::Invalid)
            }
        }
    }
    pub(crate) fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => Ok(Statement::Sqlite(db.prepare(sql)?, db.max_field_bytes)),
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => Ok(Statement::Postgres {
                db,
                sql: sql.into(),
            }),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = sql;
                Err(Error::Invalid)
            }
        }
    }
    pub(crate) fn query_row<T>(
        &self,
        sql: &str,
        params: impl Params,
        f: impl FnOnce(&Row<'_>) -> Result<T>,
    ) -> Result<T> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => {
                let values = db.query_one(sql, params.values()?)?;
                f(&Row {
                    values,
                    marker: std::marker::PhantomData,
                })
            }
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => {
                let mut statement = self.prepare(sql)?;
                let mut rows = statement.query(params)?;
                f(rows.next()?.ok_or(Error::Missing)?)
            }
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = (sql, params, f);
                Err(Error::Invalid)
            }
        }
    }
    pub(crate) fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.transaction_with_behavior(TransactionBehavior::Deferred)
    }
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub(crate) fn transaction_with_behavior(
        &mut self,
        behavior: TransactionBehavior,
    ) -> Result<Transaction<'_>> {
        let (begin, commit, rollback) = match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => (
                "SAVEPOINT htlk_domain",
                "RELEASE SAVEPOINT htlk_domain",
                "ROLLBACK TO SAVEPOINT htlk_domain",
            ),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => (
                match behavior {
                    TransactionBehavior::Deferred => "BEGIN DEFERRED",
                    TransactionBehavior::Immediate => "BEGIN IMMEDIATE",
                },
                "COMMIT",
                "ROLLBACK",
            ),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => return Err(Error::Invalid),
        };
        let _ = behavior;
        self.execute_batch(begin)?;
        let inspection_position_before = match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => db.inspection_position.get(),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => None,
        };
        Ok(Transaction {
            db: self,
            commit,
            rollback,
            finished: false,
            #[cfg(feature = "sqlite")]
            inspection_position: std::cell::Cell::new(None),
            inspection_position_before,
        })
    }
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    pub(crate) fn transaction_with_behavior(
        &mut self,
        behavior: TransactionBehavior,
    ) -> Result<Transaction<'_>> {
        let _ = behavior;
        Err(Error::Invalid)
    }
    pub(crate) fn last_insert_rowid(&self) -> i64 {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(db) => db.last_insert_rowid(),
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => db.last_insert_id(),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => 0,
        }
    }
    pub(crate) fn begin_operation(&self) -> Result<()> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => db.begin(),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => Ok(()),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => Err(Error::Invalid),
        }
    }
    pub(crate) fn finish_operation(&self, success: bool) -> Result<()> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(db) => db.finish(success),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => {
                let _ = success;
                Ok(())
            }
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled => {
                let _ = success;
                Err(Error::Invalid)
            }
        }
    }
}

pub(crate) struct Transaction<'a> {
    db: &'a Connection,
    commit: &'static str,
    rollback: &'static str,
    finished: bool,
    #[cfg(any(feature = "sqlite", not(feature = "postgres")))]
    inspection_position: std::cell::Cell<Option<i64>>,
    inspection_position_before: Option<i64>,
}
impl Transaction<'_> {
    // PostgreSQL domain transactions are savepoints inside one worker-operation
    // transaction: all released savepoints must publish the same physical pin.
    pub(crate) fn inspection_position(&self) -> Option<i64> {
        match self.db {
            #[cfg(feature = "postgres")]
            Connection::Postgres(db) => db.inspection_position.get(),
            #[cfg(any(feature = "sqlite", not(feature = "postgres")))]
            _ => self.inspection_position.get(),
        }
    }
    pub(crate) fn set_inspection_position(&self, position: Option<i64>) {
        match self.db {
            #[cfg(feature = "postgres")]
            Connection::Postgres(db) => db.inspection_position.set(position),
            #[cfg(any(feature = "sqlite", not(feature = "postgres")))]
            _ => self.inspection_position.set(position),
        }
    }
    pub(crate) fn commit(mut self) -> Result<()> {
        self.db.execute_batch(self.commit)?;
        self.finished = true;
        Ok(())
    }
}
impl Deref for Transaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        self.db
    }
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.db.execute_batch(self.rollback);
            self.set_inspection_position(self.inspection_position_before);
        }
    }
}

pub(crate) enum Statement<'a> {
    #[cfg(feature = "sqlite")]
    Sqlite(rusqlite::Statement<'a>, usize),
    #[cfg(feature = "postgres")]
    Postgres {
        db: &'a crate::store_postgres::Database,
        sql: String,
    },
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    #[allow(dead_code)] // No statement is issued without an enabled backend.
    Disabled(std::marker::PhantomData<&'a ()>),
}
impl Statement<'_> {
    // End the statement's borrow/resource lifetime before committing its transaction.
    pub(crate) fn finish(self) {}
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub(crate) fn query(&mut self, params: impl Params) -> Result<Rows<'_>> {
        let values = params.values()?;
        let inner = match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(statement, maximum) => RowSource::Sqlite(
                statement.query(rusqlite::params_from_iter(
                    values.into_iter().map(sqlite_value),
                ))?,
                *maximum,
            ),
            #[cfg(feature = "postgres")]
            Self::Postgres { db, sql } => RowSource::Postgres(db.rows(sql, values)?),
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            Self::Disabled(_) => {
                let _ = values;
                return Err(Error::Invalid);
            }
        };
        Ok(Rows {
            inner,
            current: None,
        })
    }
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    pub(crate) fn query(&mut self, params: impl Params) -> Result<Rows<'_>> {
        let _ = params;
        Err(Error::Invalid)
    }
    pub(crate) fn query_map<T, F: FnMut(&Row<'_>) -> Result<T>>(
        &mut self,
        params: impl Params,
        f: F,
    ) -> Result<MappedRows<'_, F>> {
        Ok(MappedRows {
            rows: self.query(params)?,
            f,
        })
    }
}
enum RowSource<'a> {
    #[cfg(feature = "sqlite")]
    Sqlite(rusqlite::Rows<'a>, usize),
    #[cfg(feature = "postgres")]
    Postgres(crate::store_postgres::Cursor<'a>),
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    #[allow(dead_code)] // No row cursor is opened in a pure-evaluation build.
    Disabled(std::marker::PhantomData<&'a ()>),
}
pub(crate) struct Rows<'a> {
    inner: RowSource<'a>,
    current: Option<Row<'static>>,
}
impl Rows<'_> {
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub(crate) fn next(&mut self) -> Result<Option<&Row<'_>>> {
        let values = match &mut self.inner {
            #[cfg(feature = "sqlite")]
            RowSource::Sqlite(rows, maximum) => match rows.next()? {
                None => None,
                Some(row) => Some(
                    (0..row.as_ref().column_count())
                        .map(|index| {
                            use rusqlite::types::ValueRef;
                            let value = row.get_ref(index)?;
                            if matches!(value, ValueRef::Text(bytes) | ValueRef::Blob(bytes) if bytes.len() > *maximum) {
                                return Err(Error::Invalid);
                            }
                            Ok(match value {
                                ValueRef::Null => Value::Null,
                                ValueRef::Integer(value) => Value::Integer(value),
                                ValueRef::Real(value) => Value::Real(value),
                                ValueRef::Text(value) => Value::Text(
                                    std::str::from_utf8(value)
                                        .map_err(|_| Error::Invalid)?
                                        .into(),
                                ),
                                ValueRef::Blob(value) => Value::Blob(value.to_vec()),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
            },
            #[cfg(feature = "postgres")]
            RowSource::Postgres(cursor) => cursor.next()?,
            #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
            RowSource::Disabled(_) => return Err(Error::Invalid),
        };
        self.current = values.map(|values| Row {
            values,
            marker: std::marker::PhantomData,
        });
        Ok(self.current.as_ref())
    }
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    pub(crate) fn next(&mut self) -> Result<Option<&Row<'_>>> {
        Err(Error::Invalid)
    }
}
pub(crate) struct MappedRows<'a, F> {
    rows: Rows<'a>,
    f: F,
}
impl<T, F: FnMut(&Row<'_>) -> Result<T>> Iterator for MappedRows<'_, F> {
    type Item = Result<T>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.rows.next() {
            Ok(Some(row)) => Some((self.f)(row)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

#[cfg(feature = "sqlite")]
fn sqlite_value(value: Value) -> rusqlite::types::Value {
    match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Integer(n) => rusqlite::types::Value::Integer(n),
        Value::Real(n) => rusqlite::types::Value::Real(n),
        Value::Text(s) => rusqlite::types::Value::Text(s),
        Value::Blob(b) => rusqlite::types::Value::Blob(b),
    }
}
