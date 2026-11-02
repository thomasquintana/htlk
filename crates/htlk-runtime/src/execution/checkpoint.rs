//! Typed immutable invocation checkpoints, with wire/state validation at entry.
use super::model::*;
use crate::{
    EvaluationOutcome, RuntimeError,
    failure::{FailureCause, FailureCode, RuntimeFailure},
    records::{SecurityLabels, SourceState},
    store::StoredRecord,
};
use htlk_executable::{
    Identifier, JsonPointer,
    cbor::{Limits, Map, Value},
    digest::Digest,
};

enum InvocationState<'a> {
    Pending,
    Active,
    Succeeded(&'a Map),
    Failed(RuntimeFailure),
    Skipped,
    Cancelled,
}

pub(super) struct InvocationCheckpoint<'a> {
    record: &'a StoredRecord,
    state: InvocationState<'a>,
    influence: SecurityLabels,
}

impl<'a> InvocationCheckpoint<'a> {
    pub fn parse(record: &'a StoredRecord, limits: &Limits) -> Result<Self, RuntimeError> {
        let state = match text(field(&record.payload, "status")?)? {
            "pending" => InvocationState::Pending,
            "active" => InvocationState::Active,
            "succeeded" => InvocationState::Succeeded(map(field(&record.payload, "outputs")?)?),
            "failed" => InvocationState::Failed(failure(&record.payload, limits)?),
            "skipped" => InvocationState::Skipped,
            "cancelled" => InvocationState::Cancelled,
            _ => return Err(err()),
        };
        let terminal = !matches!(state, InvocationState::Pending | InvocationState::Active);
        if record.final_record != terminal {
            return Err(err());
        }
        if matches!(
            state,
            InvocationState::Active | InvocationState::Succeeded(_)
        ) && !matches!(field(&record.payload, "input_checkpoint")?, Value::Bytes(_))
        {
            return Err(err());
        }
        if !matches!(state, InvocationState::Failed(_))
            && !matches!(field(&record.payload, "failure")?, Value::Null)
        {
            return Err(err());
        }
        Ok(Self {
            record,
            state,
            influence: labels(&record.payload, limits)?,
        })
    }

    pub fn is_pending(&self) -> bool {
        matches!(self.state, InvocationState::Pending)
    }

    pub fn is_admitted(&self) -> bool {
        matches!(
            self.state,
            InvocationState::Active | InvocationState::Succeeded(_)
        )
    }

    pub fn influence(&self) -> &SecurityLabels {
        &self.influence
    }

    pub fn output(&self, port: &Identifier) -> Result<SourceState, RuntimeError> {
        Ok(match &self.state {
            InvocationState::Pending | InvocationState::Active => SourceState::Pending,
            InvocationState::Succeeded(outputs) => {
                entry(outputs.get(port.as_str()).ok_or_else(err)?)?
                    .map_or(SourceState::Absent, SourceState::Value)
            }
            InvocationState::Skipped => SourceState::Absent,
            InvocationState::Failed(failure) => SourceState::Unavailable(failure.clone()),
            InvocationState::Cancelled => {
                SourceState::Unavailable(RuntimeFailure::new(FailureCode::Cancelled))
            }
        })
    }

    pub fn outcome(&self) -> EvaluationOutcome {
        match &self.state {
            InvocationState::Pending | InvocationState::Active => EvaluationOutcome::Pending,
            InvocationState::Succeeded(_) => EvaluationOutcome::Succeeded,
            InvocationState::Failed(failure) => EvaluationOutcome::Failed {
                code: failure.code().to_string(),
                message: failure.message().into(),
            },
            InvocationState::Skipped => EvaluationOutcome::Skipped,
            InvocationState::Cancelled => EvaluationOutcome::Cancelled,
        }
    }

    pub fn cause(
        &self,
        scope: Digest,
        index: usize,
        limits: &Limits,
    ) -> Result<Option<FailureCause>, RuntimeError> {
        let code = match &self.state {
            InvocationState::Failed(failure) => failure.code(),
            InvocationState::Cancelled => FailureCode::Cancelled,
            _ => return Ok(None),
        };
        Ok(Some(FailureCause {
            invocation: self.record.key.id.parse().map_err(|_| err())?,
            code,
            location: Some(
                JsonPointer::new(&format!("/scopes/{scope}/nodes/{index}"), limits)
                    .map_err(|_| err())?,
            ),
        }))
    }
}

/// Terminal proposals encode required failure data. Successful publication uses
/// artifact acceptance rather than this failure/skip path.
pub(super) enum NodeCompletion {
    Skipped,
    Failed(RuntimeFailure),
}
impl NodeCompletion {
    pub fn status(&self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Failed(_) => "failed",
        }
    }
    pub fn failure_value(&self, limits: &Limits) -> Result<Value, RuntimeError> {
        match self {
            Self::Skipped => Ok(Value::Null),
            Self::Failed(error) => error.to_value(limits).map_err(|_| err()),
        }
    }
}
