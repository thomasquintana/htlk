//! Pure retry eligibility. Delivery evidence and current host policy must first
//! establish replay safety; graph retry settings cannot grant it themselves.
use super::model::err;
use crate::{RuntimeError, failure::FailureCode};
use htlk_executable::RetryPolicy;

pub(super) fn replay_delay(
    retry: &RetryPolicy,
    attempt: u64,
    failure: FailureCode,
    replay_safe: bool,
) -> Result<Option<u64>, RuntimeError> {
    if attempt == 0 || attempt > retry.max_attempts() {
        return Err(err());
    }
    let retryable = matches!(
        failure,
        FailureCode::McpTransport
            | FailureCode::McpTimeout
            | FailureCode::McpProtocol
            | FailureCode::McpToolError
            | FailureCode::OutputSchema
            | FailureCode::DeclaredContract
            | FailureCode::InferredSchema
            | FailureCode::ApplicationSchema
            | FailureCode::Postconditions
    );
    if !retryable
        || !replay_safe
        || attempt == retry.max_attempts()
        || !retry.on().iter().any(|code| code == failure.as_str())
    {
        return Ok(None);
    }
    let index = usize::try_from(attempt - 1).map_err(|_| err())?;
    Ok(Some(*retry.backoff_ms().get(index).ok_or_else(err)?))
}
