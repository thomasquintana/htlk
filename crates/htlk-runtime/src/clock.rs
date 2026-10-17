//! Trusted observed coordinator time, kept outside the pure evaluator.
use crate::failure::{FailureCode, RuntimeFailure};
use std::time::{SystemTime, UNIX_EPOCH};

/// Trusted host clock. Runtime persistence normalizes observations against durable
/// watermarks; this interface is never exposed to native expression functions.
pub trait CoordinatorClock: Send + Sync {
    /// Observes nonnegative signed-i64 epoch milliseconds.
    ///
    /// # Errors
    /// Returns a redacted operational failure when the clock is unavailable.
    fn now_ms(&self) -> Result<i64, RuntimeFailure>;
}
/// Default host wall-clock source. Backwards observations are clamped by storage.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;
impl CoordinatorClock for SystemClock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        let milliseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeFailure::new(FailureCode::Invariant))?
            .as_millis();
        i64::try_from(milliseconds).map_err(|_| RuntimeFailure::new(FailureCode::Invariant))
    }
}
