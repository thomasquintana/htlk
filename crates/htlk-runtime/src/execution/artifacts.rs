//! Explicit worker-only lazy I/O. Frozen expression evaluation has no other
//! storage or transport capability. Unvisited branches never acquire values.
use crate::{
    HostContext, RuntimeError,
    artifact::AuthorizedValue,
    artifact_access::ArtifactReader,
    failure::{FailureCode, RuntimeFailure},
    identity::ArtifactId,
};
use htlk_executable::cbor::Value;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

pub(super) struct WorkerArtifactResolver {
    reader: ArtifactReader,
    context: HostContext,
    cache: RefCell<BTreeMap<ArtifactId, Arc<Value>>>,
}
impl WorkerArtifactResolver {
    pub fn new(reader: ArtifactReader, context: HostContext) -> Self {
        Self {
            reader,
            context,
            cache: RefCell::new(BTreeMap::new()),
        }
    }
    pub fn read_on_worker(&self, id: ArtifactId) -> Result<Arc<Value>, RuntimeFailure> {
        if let Some(value) = self.cache.borrow().get(&id).cloned() {
            return Ok(value);
        }
        let accepted = read_on_worker(&self.reader, &self.context, id).map_err(|error| {
            RuntimeFailure::new(if error.code() == FailureCode::Input {
                FailureCode::Digest
            } else {
                error.code()
            })
        })?;
        let value = Arc::new(accepted.value);
        self.cache.borrow_mut().insert(id, value.clone());
        Ok(value)
    }
}

// This bridge can poll only artifact-read futures backed by SQLite's independent
// worker. Its API cannot be used to block on network or reactor/timer work.
fn read_on_worker(
    reader: &ArtifactReader,
    context: &HostContext,
    id: ArtifactId,
) -> Result<AuthorizedValue, RuntimeError> {
    struct Thread(std::thread::Thread);
    impl Wake for Thread {
        fn wake(self: Arc<Self>) {
            self.0.unpark()
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark()
        }
    }
    let waker = Waker::from(Arc::new(Thread(std::thread::current())));
    let mut task = Context::from_waker(&waker);
    let mut future = std::pin::pin!(reader.read(context, id));
    loop {
        match future.as_mut().poll(&mut task) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}
