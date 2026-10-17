//! Deterministic guard-first destination selection and input presence reduction.
//! Decisions are proposals: actual-value authorization/type checks, contracts,
//! manifest freezing and final persistence remain coordinator responsibilities.
use crate::{
    failure::{FailureCause, FailureCode, RuntimeFailure},
    identity::ArtifactId,
    records::{RuntimeRecordError, SecurityLabels, SourceState},
};
use htlk_executable::{
    Identifier, PortTable,
    cbor::{self, Limits},
    record_accounting::{EncodingLimitError, RecordAccounting},
};
use std::{collections::BTreeMap, fmt};

/// One lazily evaluated strict Boolean guard, with pending separate from data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardState {
    /// A named evaluated dependency has not settled.
    Pending,
    /// Settled true or false.
    Value(bool),
    /// Immutable expression/contract failure, never logical false.
    Error(RuntimeFailure),
}
/// A guard's evaluated influence and protected causal links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardEvaluation {
    /// Strict result, pending or error.
    pub state: GuardState,
    /// Labels of evaluated dependencies/decisions, including absence.
    pub influence: SecurityLabels,
    /// Protected references retained if this guard fails.
    pub causes: Vec<FailureCause>,
}
/// Selected source availability and boundary/outcome protection. No source is
/// resolved at all unless candidate guards settle with exactly one true writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabeledSource {
    /// Accepted value ID, absence, pending or unavailable causal failure.
    pub state: SourceState,
    /// Source boundary/artifact or absence/outcome protection.
    pub influence: SecurityLabels,
    /// Optional selected producer cause.
    pub cause: Option<FailureCause>,
}
/// One deterministic destination failure and its protected causal evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingFailure {
    /// Canonical candidate edge location.
    pub edge: Identifier,
    /// Stable binding/guard failure.
    pub error: RuntimeFailure,
    /// Original selected producer error, distinct from E_DEPENDENCY.
    pub source_error: Option<RuntimeFailure>,
    /// Protected invocation/location links.
    pub causes: Vec<FailureCause>,
}
/// Uniform selection proposal for node inputs, public outputs and loop next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingDecision {
    /// Guards or the sole selected source are pending; never freeze this result.
    Pending {
        /// Pending candidate guards, or the sole waiting source edge.
        waiting_edges: Vec<Identifier>,
        /// Known source selection, only after every guard settles.
        selected_edge: Option<Identifier>,
        /// Already settled guard failures retained until every guard settles.
        guard_failures: Vec<BindingFailure>,
        /// Evaluated influence accumulated so far.
        influence: SecurityLabels,
    },
    /// Selected accepted artifact. The coordinator must authorize/validate transfer
    /// before making it available to the destination operation/boundary.
    Value {
        /// Sole true candidate.
        edge: Identifier,
        /// Existing immutable value identity, including present-null values.
        artifact_id: ArtifactId,
        /// All guard and selected source protection.
        influence: SecurityLabels,
    },
    /// No active candidate or a sole selected absent source.
    Absent {
        /// None if no guard selected a source.
        selected_edge: Option<Identifier>,
        /// Protection of the decisions producing absence.
        influence: SecurityLabels,
    },
    /// Settled failures ordered by canonical edge ID. The first is primary;
    /// remaining causes are retained rather than chosen by completion timing.
    Failed {
        /// Nonempty canonical failure list produced by the reducer.
        failures: Vec<BindingFailure>,
        /// All evaluated guard/selected source influences.
        influence: SecurityLabels,
    },
}
impl BindingDecision {
    /// Whether selection is unfinished.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
    /// Protection retained even for pending, absence and failure decisions.
    pub fn influence(&self) -> &SecurityLabels {
        match self {
            Self::Pending { influence, .. }
            | Self::Value { influence, .. }
            | Self::Absent { influence, .. }
            | Self::Failed { influence, .. } => influence,
        }
    }
}

/// Evaluates all guards in canonical edge order, waits for ALL of them, applies
/// error/conflict/absence precedence, and resolves only a unique selected source.
/// Resolver callbacks must perform pure/local reads, never dispatch effects.
/// No static exclusivity warning/proof suppresses the runtime uniqueness check.
///
/// # Errors
/// Returns duplicate candidate IDs or configured metadata/causal resource limits.
pub fn reduce_binding(
    candidates: &[Identifier],
    mut guard: impl FnMut(&Identifier) -> GuardEvaluation,
    mut source: impl FnMut(&Identifier) -> LabeledSource,
    limits: &Limits,
) -> Result<BindingDecision, ReducerError> {
    let mut budget = RecordAccounting::new(limits)?;
    budget.collection(candidates.len(), 0)?;
    for edge in candidates {
        budget.text(edge.as_str(), 1)?;
    }
    let mut edges = candidates.iter().collect::<Vec<_>>();
    edges.sort();
    if edges.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ReducerError::InvalidPlan);
    }
    let mut evaluated = Vec::new();
    let mut pending = Vec::new();
    let mut selected = Vec::new();
    let mut failures = Vec::new();
    for edge in edges {
        let result = guard(edge);
        budget.value(&result.influence.to_value(limits)?, 1)?;
        check_causes(&result.causes, &mut budget)?;
        match &result.state {
            GuardState::Pending => pending.push(edge.clone()),
            GuardState::Value(true) => selected.push(edge.clone()),
            GuardState::Value(false) => {}
            GuardState::Error(error) => {
                budget.value(
                    &error
                        .to_value(limits)
                        .map_err(|_| ReducerError::InvalidPlan)?,
                    1,
                )?;
                failures.push(BindingFailure {
                    edge: edge.clone(),
                    error: error.clone(),
                    source_error: None,
                    causes: result.causes.clone(),
                });
            }
        }
        evaluated.push(result);
    }
    let influence = SecurityLabels::combine(evaluated.iter().map(|g| &g.influence), limits)?;
    if !pending.is_empty() {
        return Ok(BindingDecision::Pending {
            waiting_edges: pending,
            selected_edge: None,
            guard_failures: failures,
            influence,
        });
    }
    if !failures.is_empty() {
        return Ok(BindingDecision::Failed {
            failures,
            influence,
        });
    }
    if selected.len() > 1 {
        let failures = selected
            .into_iter()
            .map(|edge| BindingFailure {
                edge,
                error: RuntimeFailure::new(FailureCode::BindingConflict),
                source_error: None,
                causes: vec![],
            })
            .collect();
        return Ok(BindingDecision::Failed {
            failures,
            influence,
        });
    }
    let Some(edge) = selected.pop() else {
        return Ok(BindingDecision::Absent {
            selected_edge: None,
            influence,
        });
    };
    let selected = source(&edge);
    budget.value(&selected.influence.to_value(limits)?, 1)?;
    if let Some(cause) = &selected.cause {
        check_causes(std::slice::from_ref(cause), &mut budget)?;
    }
    let influence = influence.union(&selected.influence, limits)?;
    Ok(match selected.state {
        SourceState::Pending => BindingDecision::Pending {
            waiting_edges: vec![edge.clone()],
            selected_edge: Some(edge),
            guard_failures: vec![],
            influence,
        },
        SourceState::Value(artifact_id) => BindingDecision::Value {
            edge,
            artifact_id,
            influence,
        },
        SourceState::Absent => BindingDecision::Absent {
            selected_edge: Some(edge),
            influence,
        },
        SourceState::Unavailable(error) => {
            budget.value(
                &error
                    .to_value(limits)
                    .map_err(|_| ReducerError::InvalidPlan)?,
                1,
            )?;
            BindingDecision::Failed {
                failures: vec![BindingFailure {
                    edge,
                    error: RuntimeFailure::new(FailureCode::Dependency),
                    source_error: Some(error),
                    causes: selected.cause.into_iter().collect(),
                }],
                influence,
            }
        }
    })
}
/// Stable per-input failure, sorted by declared decoded UTF-8 port name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputBindingFailure {
    /// Destination input.
    pub port: Identifier,
    /// Nonempty canonical edge failures.
    pub failures: Vec<BindingFailure>,
}
/// Guard/input selection gate. InputsReady still requires actual-value checks,
/// preconditions and one-time manifest admission; it is not an execution permit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// Guard or at least one relevant input is unfinished.
    Pending,
    /// False guard, with no input inspection/evaluation.
    GuardSkipped(SecurityLabels),
    /// Failed guard, with no input inspection/evaluation.
    GuardFailed(GuardEvaluation),
    /// All bindings settled; the UTF-8-lowest failing input is primary.
    InputsFailed {
        /// Nonempty sorted destination failures, retaining all edge causes.
        failures: Vec<InputBindingFailure>,
        /// Guard and all relevant binding influences.
        influence: SecurityLabels,
    },
    /// Required absence only after binding errors were ruled out.
    RequiredAbsent {
        /// All absent required ports in UTF-8 order.
        ports: Vec<Identifier>,
        /// Includes protected absence/guard decisions.
        influence: SecurityLabels,
    },
    /// Ready for value validation and the precondition phase.
    InputsReady(SecurityLabels),
}
/// Applies guard-first and all-input error-before-absence admission precedence.
/// False/pending/error guards do not even inspect a supplied input inventory.
///
/// # Errors
/// Returns incomplete/unknown/duplicate diagnostic inventory or metadata limits.
pub fn reduce_admission(
    guard: GuardEvaluation,
    ports: &PortTable,
    bindings: &BTreeMap<Identifier, BindingDecision>,
    limits: &Limits,
) -> Result<AdmissionDecision, ReducerError> {
    let mut guard_budget = RecordAccounting::new(limits)?;
    guard_budget.value(&guard.influence.to_value(limits)?, 0)?;
    check_causes(&guard.causes, &mut guard_budget)?;
    if let GuardState::Error(error) = &guard.state {
        guard_budget.value(
            &error
                .to_value(limits)
                .map_err(|_| ReducerError::InvalidPlan)?,
            0,
        )?;
    }
    match guard.state {
        GuardState::Pending => return Ok(AdmissionDecision::Pending),
        GuardState::Value(false) => return Ok(AdmissionDecision::GuardSkipped(guard.influence)),
        GuardState::Error(_) => return Ok(AdmissionDecision::GuardFailed(guard)),
        GuardState::Value(true) => {}
    }
    if bindings.len() != ports.len()
        || bindings
            .keys()
            .any(|name| ports.get(name.as_str()).is_none())
    {
        return Err(ReducerError::InvalidPlan);
    }
    if bindings.values().any(BindingDecision::is_pending) {
        return Ok(AdmissionDecision::Pending);
    }
    let influence = SecurityLabels::combine(
        std::iter::once(&guard.influence).chain(bindings.values().map(BindingDecision::influence)),
        limits,
    )?;
    let mut failures = Vec::new();
    let mut absent = Vec::new();
    let mut budget = RecordAccounting::new(limits)?;
    budget.collection(ports.len(), 0)?;
    for (name, port) in ports.iter() {
        budget.text(name.as_str(), 1)?;
        match &bindings[name] {
            BindingDecision::Failed {
                failures: causes, ..
            } => {
                if causes.is_empty() || causes.windows(2).any(|p| p[0].edge >= p[1].edge) {
                    return Err(ReducerError::InvalidPlan);
                }
                for cause in causes {
                    budget.value(
                        &cause
                            .error
                            .to_value(limits)
                            .map_err(|_| ReducerError::InvalidPlan)?,
                        1,
                    )?;
                    if let Some(error) = &cause.source_error {
                        budget.value(
                            &error
                                .to_value(limits)
                                .map_err(|_| ReducerError::InvalidPlan)?,
                            1,
                        )?;
                    }
                    check_causes(&cause.causes, &mut budget)?;
                }
                failures.push(InputBindingFailure {
                    port: name.clone(),
                    failures: causes.clone(),
                });
            }
            BindingDecision::Absent { .. } if port.required() => absent.push(name.clone()),
            BindingDecision::Value { .. } | BindingDecision::Absent { .. } => {}
            BindingDecision::Pending { .. } => return Err(ReducerError::InvalidPlan),
        }
    }
    if !failures.is_empty() {
        Ok(AdmissionDecision::InputsFailed {
            failures,
            influence,
        })
    } else if !absent.is_empty() {
        Ok(AdmissionDecision::RequiredAbsent {
            ports: absent,
            influence,
        })
    } else {
        Ok(AdmissionDecision::InputsReady(influence))
    }
}
fn check_causes(
    causes: &[FailureCause],
    budget: &mut RecordAccounting<'_>,
) -> Result<(), ReducerError> {
    budget.collection(causes.len(), 1)?;
    for cause in causes {
        budget.text(&cause.invocation.to_string(), 2)?;
        budget.text(cause.code.as_str(), 2)?;
        if let Some(location) = &cause.location {
            budget.text(&location.to_string(), 2)?;
        }
    }
    Ok(())
}
/// Structured reducer invariant/resource failure, never a logical false result.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReducerError {
    /// Duplicate edge IDs or invalid settled input/diagnostic inventory.
    InvalidPlan,
    /// Codec configuration or allocation failure.
    Codec(cbor::Error),
    /// Complete deterministic metadata allowance exhausted.
    Encoding(EncodingLimitError),
    /// Trusted influence metadata conversion failed.
    Metadata(RuntimeRecordError),
}
impl fmt::Display for ReducerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPlan => "invalid verified reducer inventory",
            Self::Codec(_) => "reducer codec configuration failed",
            Self::Encoding(_) | Self::Metadata(_) => "reducer metadata allowance failed",
        })
    }
}
impl std::error::Error for ReducerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPlan => None,
            Self::Codec(e) => Some(e),
            Self::Encoding(e) => Some(e),
            Self::Metadata(e) => Some(e),
        }
    }
}
impl From<cbor::Error> for ReducerError {
    fn from(e: cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<EncodingLimitError> for ReducerError {
    fn from(e: EncodingLimitError) -> Self {
        Self::Encoding(e)
    }
}
impl From<RuntimeRecordError> for ReducerError {
    fn from(e: RuntimeRecordError) -> Self {
        Self::Metadata(e)
    }
}
