//! Private asynchronous access to a thread-owned SQLite connection.
//!
//! Each call runs to completion in FIFO order, even if its awaiting caller leaves.
//! A close message follows already queued work and closes all cloned handles.
//! SQLite transactions, not this queue, arbitrate independent store connections.

use std::{path::Path, sync::mpsc, thread};
use tokio::sync::oneshot;

#[derive(Debug)]
pub(crate) enum Error<E> {
    Closed,
    Error(E),
}

type Operation = Box<dyn FnOnce(&mut rusqlite::Connection) + Send>;

enum Message {
    Call(Operation),
    Close(oneshot::Sender<rusqlite::Result<()>>),
}

#[derive(Clone)]
pub(crate) struct Connection {
    sender: mpsc::Sender<Message>,
}

impl Connection {
    pub(crate) async fn open(path: impl AsRef<Path>) -> Result<Self, Error<rusqlite::Error>> {
        let path = path.as_ref().to_owned();
        Self::start(move || rusqlite::Connection::open(path)).await
    }

    pub(crate) async fn open_in_memory() -> Result<Self, Error<rusqlite::Error>> {
        Self::start(rusqlite::Connection::open_in_memory).await
    }

    async fn start(
        open: impl FnOnce() -> rusqlite::Result<rusqlite::Connection> + Send + 'static,
    ) -> Result<Self, Error<rusqlite::Error>> {
        let (sender, receiver) = mpsc::channel();
        let (ready, opened) = oneshot::channel();
        thread::Builder::new()
            .name("htlk-sqlite".into())
            .spawn(move || {
                let mut db = match open() {
                    Ok(db) => db,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(message) = receiver.recv() {
                    match message {
                        Message::Call(operation) => operation(&mut db),
                        Message::Close(reply) => match db.close() {
                            Ok(()) => {
                                let _ = reply.send(Ok(()));
                                return;
                            }
                            Err((connection, error)) => {
                                db = connection;
                                let _ = reply.send(Err(error));
                            }
                        },
                    }
                }
            })
            .map_err(|_| Error::Closed)?;
        opened
            .await
            .map_err(|_| Error::Closed)?
            .map_err(Error::Error)?;
        Ok(Self { sender })
    }

    pub(crate) async fn call<R, E>(
        &self,
        operation: impl FnOnce(&mut rusqlite::Connection) -> Result<R, E> + Send + 'static,
    ) -> Result<R, Error<E>>
    where
        R: Send + 'static,
        E: Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Message::Call(Box::new(move |db| {
                let _ = reply.send(operation(db));
            })))
            .map_err(|_| Error::Closed)?;
        result
            .await
            .map_err(|_| Error::Closed)?
            .map_err(Error::Error)
    }

    pub(crate) async fn close(self) -> Result<(), Error<rusqlite::Error>> {
        let (reply, result) = oneshot::channel();
        if self.sender.send(Message::Close(reply)).is_err() {
            return Ok(());
        }
        // Another cloned handle may already have closed the connection.
        result.await.unwrap_or(Ok(())).map_err(Error::Error)
    }
}
