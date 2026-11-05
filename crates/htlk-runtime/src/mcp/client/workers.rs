//! A bounded host-work pool separate from the transport reader reactor. A blocked
//! host future cannot monopolize transport reception or the owner's stop signal.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc, oneshot};

type Job = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone, Default)]
pub(super) struct Executors {
    pub callbacks: Option<Arc<Workers>>,
    pub events: Option<Arc<Workers>>,
}
impl Executors {
    pub fn launch(
        config: &McpClientConfig,
        stop: Arc<McpCancellation>,
    ) -> Result<Self, RuntimeFailure> {
        Ok(Self {
            callbacks: if config.handlers.is_empty() {
                None
            } else {
                Some(Workers::launch(
                    config.limits.callbacks.min(config.limits.callback_depth),
                    stop.clone(),
                )?)
            },
            events: if config.events.is_none() {
                None
            } else {
                Some(Workers::launch(config.limits.pending_requests, stop)?)
            },
        })
    }
}

pub(super) struct Workers {
    senders: Vec<mpsc::Sender<Job>>,
    slots: Arc<Semaphore>,
    next: AtomicUsize,
}
impl Workers {
    pub fn launch(
        capacity: usize,
        stop: Arc<McpCancellation>,
    ) -> Result<Arc<Self>, RuntimeFailure> {
        let mut senders = Vec::new();
        for index in 0..capacity.min(4) {
            let (sender, mut receiver) = mpsc::channel::<Job>(capacity);
            let stop = stop.clone();
            std::thread::Builder::new().name(format!("htlk-mcp-host-{index}")).spawn(move || {
                actix::System::new().block_on(async move {
                    let mut jobs = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            biased;
                            _ = stop.cancelled() => break,
                            _ = jobs.join_next(), if !jobs.is_empty() => {},
                            job = receiver.recv() => match job { Some(job) => { jobs.spawn(job); }, None => break },
                        }
                    }
                    jobs.abort_all();
                });
            }).map_err(|_| fail(FailureCode::McpTransport))?;
            senders.push(sender);
        }
        Ok(Arc::new(Self {
            senders,
            slots: Arc::new(Semaphore::new(capacity)),
            next: AtomicUsize::new(0),
        }))
    }
    pub fn submit<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Result<oneshot::Receiver<T>, RuntimeFailure> {
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(FailureCode::Budget))?;
        let (reply, receive) = oneshot::channel();
        let job = Box::pin(async move {
            let _slot = slot;
            let _ = reply.send(future.await);
        });
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.senders.len();
        self.senders[index]
            .try_send(job)
            .map_err(|_| fail(FailureCode::McpTransport))?;
        Ok(receive)
    }
}
