use crate::sqlite_worker::{Connection, Error};
use std::{future::Future, pin::pin, sync::mpsc, task::Context, task::Waker, time::Duration};

#[test]
fn linked_sqlite_is_patched_and_reports_its_actual_source() {
    actix::System::new().block_on(async {
        let db = Connection::open_in_memory().await.unwrap();
        let (version, source): (String, String) = db
            .call(|db| {
                db.query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
            })
            .await
            .unwrap();
        let parts: Vec<u32> = version
            .split('.')
            .map(|part| part.parse().unwrap())
            .collect();
        assert!(parts.as_slice() >= [3, 54, 0].as_slice(), "{version}");
        assert_eq!(version, rusqlite::version());
        assert!(rusqlite::version_number() >= 3_054_000);
        assert!(!source.is_empty());
        println!("linked SQLite {version}; source ID {source}");
        db.close().await.unwrap();
    });
}

#[test]
fn unsigned_store_counters_reject_negative_reads_and_overflowing_writes() {
    actix::System::new().block_on(async {
        let db = Connection::open_in_memory().await.unwrap();
        let largest: u64 = db
            .call(|db| db.query_row("SELECT ?1", [i64::MAX as u64], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(largest, i64::MAX as u64);
        let negative = db
            .call(|db| db.query_row("SELECT -1", [], |row| row.get::<_, u64>(0)))
            .await;
        assert!(matches!(negative, Err(Error::Error(_))));
        let overflow = db
            .call(|db| db.query_row("SELECT ?1", [u64::MAX], |row| row.get::<_, i64>(0)))
            .await;
        assert!(matches!(overflow, Err(Error::Error(_))));
        db.close().await.unwrap();
    });
}

#[test]
fn dropped_callers_do_not_cancel_queued_transactions_or_reorder_clones() {
    actix::System::new().block_on(async {
        let db = Connection::open_in_memory().await.unwrap();
        db.call(|db| db.execute_batch("CREATE TABLE effects(value INTEGER);"))
            .await
            .unwrap();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, blocked) = mpsc::channel();
        let first = db.clone();
        let waiter = actix::spawn(async move {
            first
                .call(move |db| {
                    started.send(()).unwrap();
                    blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                    db.execute("INSERT INTO effects VALUES(1)", [])
                })
                .await
                .unwrap();
        });
        running.await.unwrap();
        {
            let abandoned = db.call(|db| {
                let tx = db.transaction()?;
                tx.execute("INSERT INTO effects VALUES(2)", [])?;
                tx.commit()
            });
            let mut abandoned = pin!(abandoned);
            assert!(
                abandoned
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        release.send(()).unwrap();
        waiter.await.unwrap();
        let values = db
            .call(|db| -> rusqlite::Result<Vec<i64>> {
                db.prepare("SELECT value FROM effects ORDER BY rowid")?
                    .query_map([], |row| row.get(0))?
                    .collect()
            })
            .await
            .unwrap();
        assert_eq!(values, [1, 2]);
        db.close().await.unwrap();
    });
}

#[test]
fn cancelled_close_drains_prior_work_and_closes_every_clone() {
    actix::System::new().block_on(async {
        let db = Connection::open_in_memory().await.unwrap();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, blocked) = mpsc::channel();
        let worker = db.clone();
        let waiter = actix::spawn(async move {
            worker
                .call(move |_| -> Result<_, ()> {
                    started.send(()).unwrap();
                    blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                    Ok(42)
                })
                .await
                .unwrap()
        });
        running.await.unwrap();
        {
            let close = db.clone().close();
            let mut close = pin!(close);
            assert!(
                close
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        let after_close = db.call(|_| Ok::<_, ()>(99));
        let mut after_close = pin!(after_close);
        assert!(
            after_close
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        release.send(()).unwrap();
        assert_eq!(waiter.await.unwrap(), 42);
        assert!(matches!(after_close.await, Err(Error::Closed)));
        assert!(matches!(
            db.call(|_| Ok::<_, ()>(0)).await,
            Err(Error::Closed)
        ));
        db.clone().close().await.unwrap();
        db.clone().close().await.unwrap();
    });
}

#[test]
fn transaction_errors_rollback_without_poisoning_the_worker() {
    actix::System::new().block_on(async {
        let db = Connection::open_in_memory().await.unwrap();
        db.call(|db| db.execute_batch("CREATE TABLE effects(value INTEGER);"))
            .await
            .unwrap();
        let failed = db
            .call(|db| -> Result<(), crate::store::StoreError> {
                let tx = db.transaction()?;
                tx.execute("INSERT INTO effects VALUES(1)", [])?;
                Err(crate::store::StoreError::Fenced)
            })
            .await;
        assert!(matches!(
            failed,
            Err(Error::Error(crate::store::StoreError::Fenced))
        ));
        let count: i64 = db
            .call(|db| db.query_row("SELECT count(*) FROM effects", [], |row| row.get(0)))
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert!(matches!(
            db.call(|db| db.execute_batch("invalid sql")).await,
            Err(Error::Error(_))
        ));
        db.close().await.unwrap();
    });
}

#[test]
fn failed_open_and_worker_exit_return_errors_without_hanging() {
    actix::System::new().block_on(async {
        assert!(matches!(
            Connection::open("invalid\0database").await,
            Err(Error::Error(_))
        ));
        let db = Connection::open_in_memory().await.unwrap();
        let failed = db
            .call(|_| -> Result<(), ()> { panic!("injected worker failure") })
            .await;
        assert!(matches!(failed, Err(Error::Closed)));
        assert!(matches!(
            db.call(|_| Ok::<_, ()>(0)).await,
            Err(Error::Closed)
        ));
        db.close().await.unwrap();
    });
}
