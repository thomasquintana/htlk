//! One fenced, replayable execution decision. Calculation happens on workers.
use super::{frame::Frame, model::*};
use crate::{
    EvaluationValue, HostContext, Runtime, RuntimeError,
    artifact::{ArtifactInput, ArtifactProducer, PreparedArtifact},
    binding::{
        self, AdmissionDecision, BindingDecision, GuardEvaluation, GuardState, LabeledSource,
    },
    failure::{FailureCode, RuntimeFailure},
    identity::{ArtifactId, InvocationId, ScopeInstanceId},
    records::{InputManifest, ManifestEntry, RunStatus, SecurityLabels, SourceState},
    store::{
        RecordFamily, RecordWrite, RunLease, StoreError, StoredRecord, Transition,
        TransitionContext,
    },
};
use htlk_analyzer::{ExpressionSite, ScopeUse};
use htlk_executable::{
    EdgeDestination, EdgeSource, ExecutionLimits, Identifier, Node, Operation, PortTable, Scope,
    ValueReference,
    cbor::{self, Map, Value},
};
use std::{collections::BTreeMap, sync::Arc};

pub(super) struct ScopeState {
    pub record: StoredRecord,
    pub executable: Arc<crate::VerifiedExecutable<'static>>,
    pub definition: Scope,
    pub site: ScopeUse,
    pub id: ScopeInstanceId,
    pub context: HostContext,
    pub nodes: BTreeMap<Identifier, StoredRecord>,
}
pub(super) enum Progress {
    Changed,
    Blocked,
    Terminal,
}
pub(super) async fn tick(runtime: Arc<Runtime>, lease: RunLease) -> Result<Progress, RuntimeError> {
    let header = runtime.store.run_header(lease.run_id()).await?;
    if header.status.is_terminal() {
        return Ok(Progress::Terminal);
    }
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    let mut cursor = None;
    let mut scopes = Vec::new();
    loop {
        let page = runtime
            .store
            .records(
                lease.run_id(),
                RecordFamily::Scope,
                cursor.as_deref(),
                runtime.store.config().max_page_size,
            )
            .await?;
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|r| r.key.id.clone());
        scopes.extend(page);
    }
    for record in scopes {
        if record.final_record
            || map(&record.payload)?.get("status") == Some(&Value::Text("pending".into()))
        {
            continue;
        }
        let id: ScopeInstanceId = record.key.id.parse().map_err(|_| err())?;
        let executable = runtime
            .admitted(digest(&record.payload, "origin_executable")?)
            .await?;
        let definition = executable
            .document()
            .fields()
            .scopes
            .get(&digest(&record.payload, "definition_digest")?)
            .ok_or_else(err)?
            .clone();
        let site = use_site(&record.payload)?;
        let context = HostContext::new(
            string(&record.payload, "principal")?,
            &runtime.config.limits,
        )?;
        let mut scope = ScopeState {
            record,
            executable,
            definition,
            site,
            id,
            context,
            nodes: BTreeMap::new(),
        };
        for node in &scope.definition.fields().nodes {
            let invocation =
                InvocationId::new(id, node.id(), &runtime.config.limits).map_err(|_| err())?;
            match runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Invocation, invocation))
                .await
            {
                Ok(record) => {
                    scope.nodes.insert(node.id().clone(), record);
                }
                Err(StoreError::Missing) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if due(&scope.record.payload, now)? {
            stop_scope(&runtime, &lease, &scope, FailureCode::Timeout).await?;
            return Ok(Progress::Changed);
        }
        if !map(&scope.record.payload)?
            .get("children_instantiated")
            .is_some_and(|v| *v == Value::Bool(true))
        {
            let mut writes = Vec::new();
            for node in &scope.definition.fields().nodes {
                if scope.nodes.contains_key(node.id()) {
                    return Err(err());
                }
                let invocation = InvocationId::new(scope.id, node.id(), &runtime.config.limits)
                    .map_err(|_| err())?;
                let payload = object([
                    ("record_version", Value::Text("0.1".into())),
                    ("scope_instance_id", Value::Text(scope.id.to_string())),
                    ("local_id", Value::Text(node.id().to_string())),
                    (
                        "definition_digest",
                        Value::Text(
                            node.digest(&runtime.config.limits)
                                .map_err(|_| err())?
                                .to_string(),
                        ),
                    ),
                    (
                        "origin_executable",
                        Value::Text(scope.executable.fingerprint().to_string()),
                    ),
                    ("status", Value::Text("pending".into())),
                    ("reason", Value::Text("guard".into())),
                    ("inputs", Value::Null),
                    ("input_checkpoint", Value::Null),
                    ("input_hash", Value::Null),
                    ("outputs", Value::Null),
                    ("failure", Value::Null),
                    (
                        "influence_labels",
                        labels(&scope.record.payload, &runtime.config.limits)?
                            .to_value(&runtime.config.limits)?,
                    ),
                    ("admitted_at_ms", Value::Null),
                    ("deadline_ms", Value::Null),
                    ("effective_limits", Value::Null),
                    ("child_scope", Value::Null),
                ])?;
                writes.push(insert(RecordFamily::Invocation, invocation, payload, false));
            }
            writes.push(update(
                &scope.record,
                replace(
                    &scope.record.payload,
                    [("children_instantiated", Value::Bool(true))],
                )?,
                false,
            ));
            commit(
                &runtime,
                &lease,
                format!("instantiate:{}", scope.id),
                writes,
                None,
            )
            .await?;
            return Ok(Progress::Changed);
        }
        let definitions = scope.definition.fields().nodes.clone();
        for definition in &definitions {
            let record = scope.nodes.get(definition.id()).ok_or_else(err)?;
            if record.final_record {
                continue;
            }
            if due(&record.payload, now)? {
                finish_failure(
                    &runtime,
                    &lease,
                    record,
                    FailureCode::Timeout,
                    labels(&record.payload, &runtime.config.limits)?,
                )
                .await?;
                return Ok(Progress::Changed);
            }
            let changed = if string(&record.payload, "status")? == "pending" {
                admit(&runtime, &lease, &scope, definition, record).await?
            } else {
                operate(&runtime, &lease, &scope, definition, record).await?
            };
            if changed {
                return Ok(Progress::Changed);
            }
        }
        if scope.nodes.values().all(|node| node.final_record)
            && close_scope(&runtime, &lease, &scope).await?
        {
            return Ok(Progress::Changed);
        }
    }
    if runtime.store.run_header(lease.run_id()).await?.sequence != header.sequence {
        return Ok(Progress::Changed);
    }
    for wait in super::control::all(&runtime, &lease, RecordFamily::Wait).await? {
        if !wait.final_record && string(&wait.payload, "state")? == "open" {
            return Ok(Progress::Blocked);
        }
    }
    for invocation in super::control::all(&runtime, &lease, RecordFamily::Invocation).await? {
        if !invocation.final_record
            && matches!(map(&invocation.payload)?.get("retry_at"),Some(Value::Integer(due))if *due>now)
        {
            return Ok(Progress::Blocked);
        }
    }
    Err(RuntimeError::failure(FailureCode::Invariant))
}
pub(super) async fn commit(
    runtime: &Runtime,
    lease: &RunLease,
    command: String,
    writes: Vec<RecordWrite>,
    status: Option<RunStatus>,
) -> Result<(), RuntimeError> {
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    runtime
        .store
        .commit(
            lease,
            Transition {
                context: TransitionContext {
                    command_id: command,
                    actor: lease.owner().into(),
                    causation: "execution".into(),
                    timestamp_ms: now,
                },
                writes,
                next_status: status,
                root: None,
                expected_fingerprint: None,
            },
        )
        .await?;
    Ok(())
}
fn due(value: &Value, now: i64) -> Result<bool, RuntimeError> {
    Ok(matches!(map(value)?.get("deadline_ms"),Some(Value::Integer(deadline))if now>=*deadline))
}
pub(super) async fn frame(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    own: Option<(&Value, &PortTable)>,
    proposed: Option<(&Value, &PortTable)>,
) -> Result<Frame, RuntimeError> {
    let base = if let Some((value, _)) = own {
        labels(value, &runtime.config.limits)?
    } else {
        labels(&scope.record.payload, &runtime.config.limits)?
    };
    let mut frame = Frame::new(
        runtime.clone(),
        scope.context.clone(),
        lease.run_id().clone(),
        scope.id,
        base,
    );
    if let Some((value, ports)) = own {
        frame.bind_manifest(field(value, "inputs")?, ports, false)?;
    } else {
        frame.bind_manifest(
            field(&scope.record.payload, "inputs")?,
            &scope.definition.fields().inputs,
            false,
        )?;
    }
    frame.siblings(&scope.nodes, &scope.definition.fields().nodes)?;
    if let Some((values, ports)) = proposed {
        for (name, _) in ports.iter() {
            let value = map(values)?.get(name.as_str()).ok_or_else(err)?;
            let slot = if text(field(value, "state")?)? == "absent" {
                super::frame::Slot::Absent
            } else if let Some(value) = map(value)?.get("value") {
                super::frame::Slot::Value(Arc::new(value.clone()))
            } else {
                super::frame::Slot::Artifact(entry(value)?.ok_or_else(err)?)
            };
            frame.bind(
                ValueReference::ScopeOutput(name.clone()),
                slot,
                SecurityLabels::default(),
            );
        }
    }
    if let Some(carried) = map(&scope.record.payload)?.get("carried") {
        for (name, value) in map(carried)?.iter() {
            frame.bind(
                ValueReference::Carried(name.parse().map_err(|_| err())?),
                entry(value)?.map_or(super::frame::Slot::Absent, super::frame::Slot::Artifact),
                labels(&scope.record.payload, &runtime.config.limits)?,
            );
        }
    }
    Ok(frame)
}
fn guard_result(result: crate::influence::InfluencedEvaluation) -> GuardEvaluation {
    GuardEvaluation {
        state: match result.result {
            Ok(value) => match value.value {
                EvaluationValue::Pending => GuardState::Pending,
                EvaluationValue::Present(Value::Bool(v)) => GuardState::Value(v),
                _ => GuardState::Error(RuntimeFailure::new(FailureCode::Invariant)),
            },
            Err(e) => GuardState::Error(RuntimeFailure::new(e.code())),
        },
        influence: result.influence,
        causes: result.causes,
    }
}
async fn guard(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    site: ExpressionSite,
    id: String,
) -> Result<(GuardEvaluation, Option<RecordWrite>), RuntimeError> {
    if let Ok(record) = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Guard, &id))
        .await
    {
        let state = match field(&record.payload, "value")? {
            Value::Bool(v) => GuardState::Value(*v),
            Value::Null => GuardState::Error(failure(&record.payload, &runtime.config.limits)?),
            _ => return Err(err()),
        };
        return Ok((
            GuardEvaluation {
                state,
                influence: labels(&record.payload, &runtime.config.limits)?,
                causes: vec![],
            },
            None,
        ));
    }
    let frame = frame(runtime, lease, scope, None, None).await?;
    let result = guard_result(frame.evaluate(&scope.executable, &scope.site, &site)?);
    if matches!(result.state, GuardState::Pending) {
        return Ok((result, None));
    }
    let (value, error) = match &result.state {
        GuardState::Value(v) => (Value::Bool(*v), Value::Null),
        GuardState::Error(error) => (
            Value::Null,
            error.to_value(&runtime.config.limits).map_err(|_| err())?,
        ),
        GuardState::Pending => return Err(err()),
    };
    let payload = object([
        ("value", value),
        ("failure", error),
        (
            "influence_labels",
            result.influence.to_value(&runtime.config.limits)?,
        ),
    ])?;
    Ok((result, Some(insert(RecordFamily::Guard, id, payload, true))))
}
fn source(
    scope: &ScopeState,
    source: &EdgeSource,
    limits: &cbor::Limits,
) -> Result<LabeledSource, RuntimeError> {
    let (state, influence) = match source {
        EdgeSource::Input(port) => {
            let value = map(field(&scope.record.payload, "inputs")?)?
                .get(port.as_str())
                .ok_or_else(err)?;
            (
                entry(value)?.map_or(SourceState::Absent, SourceState::Value),
                labels(&scope.record.payload, limits)?,
            )
        }
        EdgeSource::Carried(port) => {
            let value = map(field(&scope.record.payload, "carried")?)?
                .get(port.as_str())
                .ok_or_else(err)?;
            (
                entry(value)?.map_or(SourceState::Absent, SourceState::Value),
                labels(&scope.record.payload, limits)?,
            )
        }
        EdgeSource::Output { node, port } => {
            let node = scope.nodes.get(node).ok_or_else(err)?;
            let state = match string(&node.payload, "status")?.as_str() {
                "succeeded" => entry(
                    map(field(&node.payload, "outputs")?)?
                        .get(port.as_str())
                        .ok_or_else(err)?,
                )?
                .map_or(SourceState::Absent, SourceState::Value),
                "skipped" => SourceState::Absent,
                "failed" => SourceState::Unavailable(failure(&node.payload, limits)?),
                "cancelled" => {
                    SourceState::Unavailable(RuntimeFailure::new(FailureCode::Cancelled))
                }
                _ => SourceState::Pending,
            };
            (state, labels(&node.payload, limits)?)
        }
    };
    Ok(LabeledSource {
        state,
        influence,
        cause: None,
    })
}
pub(super) async fn bindings(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    destinations: Vec<(Identifier, EdgeDestination)>,
) -> Result<(BTreeMap<Identifier, BindingDecision>, Vec<RecordWrite>), RuntimeError> {
    let mut result = BTreeMap::new();
    let mut writes = Vec::new();
    for (name, destination) in destinations {
        let edges = scope
            .definition
            .fields()
            .edges
            .iter()
            .filter(|edge| edge.destination() == &destination)
            .collect::<Vec<_>>();
        let mut guards = BTreeMap::new();
        for edge in &edges {
            let (evaluated, write) = guard(
                runtime,
                lease,
                scope,
                ExpressionSite::EdgeGuard(edge.id().clone()),
                format!("{}:edge:{}", scope.id, edge.id()),
            )
            .await?;
            if let Some(write) = write {
                writes.push(write)
            }
            guards.insert(edge.id().clone(), evaluated);
        }
        let mut sources = BTreeMap::new();
        for edge in &edges {
            sources.insert(
                edge.id().clone(),
                source(scope, edge.source(), &runtime.config.limits)?,
            );
        }
        let selected = binding::reduce_binding(
            &edges
                .iter()
                .map(|edge| edge.id().clone())
                .collect::<Vec<_>>(),
            |id| guards[id].clone(),
            |id| sources[id].clone(),
            &runtime.config.limits,
        )
        .map_err(|_| err())?;
        result.insert(name, selected);
    }
    Ok((result, writes))
}
async fn admit(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    node: &Node,
    record: &StoredRecord,
) -> Result<bool, RuntimeError> {
    let (guard, write) = guard(
        runtime,
        lease,
        scope,
        ExpressionSite::NodeGuard(node.id().clone()),
        format!("{}:node:{}", scope.id, node.id()),
    )
    .await?;
    if let Some(write) = write {
        commit(
            runtime,
            lease,
            format!("guard:{}", write.key.id),
            vec![write],
            None,
        )
        .await?;
        return Ok(true);
    }
    match &guard.state {
        GuardState::Pending => return Ok(false),
        GuardState::Value(false) => {
            finish_node(
                runtime,
                lease,
                record,
                "skipped",
                None,
                guard.influence,
                vec![],
            )
            .await?;
            return Ok(true);
        }
        GuardState::Error(failure) => {
            finish_node(
                runtime,
                lease,
                record,
                "failed",
                Some(failure.clone()),
                guard.influence,
                vec![],
            )
            .await?;
            return Ok(true);
        }
        _ => {}
    }
    let destinations = node
        .fields()
        .inputs
        .iter()
        .map(|(port, _)| {
            (
                port.clone(),
                EdgeDestination::Input {
                    node: node.id().clone(),
                    port: port.clone(),
                },
            )
        })
        .collect();
    let (selected, writes) = bindings(runtime, lease, scope, destinations).await?;
    if !writes.is_empty() {
        commit(
            runtime,
            lease,
            format!(
                "input-guards:{}",
                writes
                    .iter()
                    .map(|w| w.key.id.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            writes,
            None,
        )
        .await?;
        return Ok(true);
    }
    let gate = binding::reduce_admission(
        guard,
        &node.fields().inputs,
        &selected,
        &runtime.config.limits,
    )
    .map_err(|_| err())?;
    let influence = match gate {
        AdmissionDecision::Pending => return Ok(false),
        AdmissionDecision::InputsFailed {
            failures,
            influence,
        } => {
            finish_node(
                runtime,
                lease,
                record,
                "failed",
                Some(failures[0].failures[0].error.clone()),
                influence,
                binding_writes(scope.id, node.id(), &selected, runtime)?,
            )
            .await?;
            return Ok(true);
        }
        AdmissionDecision::RequiredAbsent { influence, .. } => {
            finish_node(
                runtime,
                lease,
                record,
                "skipped",
                None,
                influence,
                binding_writes(scope.id, node.id(), &selected, runtime)?,
            )
            .await?;
            return Ok(true);
        }
        AdmissionDecision::InputsReady(influence) => influence,
        _ => return Err(err()),
    };
    let (manifest, first_error) = validate_selected(
        runtime,
        &scope.executable,
        &scope.context,
        &node.fields().inputs,
        &selected,
        &influence,
        false,
    )
    .await?;
    if let Some(error) = first_error {
        finish_node(
            runtime,
            lease,
            record,
            "failed",
            Some(error),
            influence,
            binding_writes(scope.id, node.id(), &selected, runtime)?,
        )
        .await?;
        return Ok(true);
    }
    let manifest = manifest.ok_or_else(err)?;
    let temporary = replace(
        &record.payload,
        [
            ("inputs", manifest.to_value(&runtime.config.limits)?),
            (
                "influence_labels",
                manifest
                    .influence_labels()
                    .to_value(&runtime.config.limits)?,
            ),
        ],
    )?;
    let frame = frame(
        runtime,
        lease,
        scope,
        Some((&temporary, &node.fields().inputs)),
        None,
    )
    .await?;
    let evaluated = frame.evaluate(
        &scope.executable,
        &scope.site,
        &ExpressionSite::NodePreconditions(node.id().clone()),
    )?;
    let error = condition_error(&evaluated, FailureCode::Preconditions);
    if let Some(error) = error {
        finish_node(
            runtime,
            lease,
            record,
            "failed",
            Some(error),
            evaluated.influence,
            binding_writes(scope.id, node.id(), &selected, runtime)?,
        )
        .await?;
        return Ok(true);
    }
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    let inherited = ExecutionLimits::from_value(
        field(&scope.record.payload, "effective_limits")?,
        &runtime.config.limits,
    )
    .map_err(|_| err())?;
    let effective = crate::run::min_limits(&[&inherited, &node.fields().limits])?;
    let deadline = now
        .checked_add(i64::try_from(effective.timeout_ms().ok_or_else(err)?).map_err(|_| err())?)
        .ok_or_else(err)?
        .min(number(&scope.record.payload, "deadline_ms")?);
    let payload = replace(
        &temporary,
        [
            ("status", Value::Text("active".into())),
            ("reason", Value::Text("ready".into())),
            (
                "input_checkpoint",
                Value::Bytes(manifest.encode(&runtime.config.limits)?),
            ),
            ("input_hash", Value::Text(manifest.input_hash().to_string())),
            ("admitted_at_ms", Value::Integer(now)),
            ("deadline_ms", Value::Integer(deadline)),
            (
                "effective_limits",
                effective
                    .to_value(&runtime.config.limits)
                    .map_err(|_| err())?,
            ),
        ],
    )?;
    let mut writes = binding_writes(scope.id, node.id(), &selected, runtime)?;
    writes.push(update(record, payload, false));
    commit(
        runtime,
        lease,
        format!("admit:{}", record.key.id),
        writes,
        None,
    )
    .await?;
    Ok(true)
}
pub(super) fn condition_error(
    result: &crate::influence::InfluencedEvaluation,
    false_code: FailureCode,
) -> Option<RuntimeFailure> {
    match &result.result {
        Err(e) => Some(RuntimeFailure::new(e.code())),
        Ok(value) => match value.value {
            EvaluationValue::Present(Value::Bool(true)) => None,
            EvaluationValue::Present(Value::Bool(false)) => Some(RuntimeFailure::new(false_code)),
            EvaluationValue::Pending => Some(RuntimeFailure::new(FailureCode::Invariant)),
            _ => Some(RuntimeFailure::new(FailureCode::Expression)),
        },
    }
}
pub(super) async fn validate_selected(
    runtime: &Runtime,
    executable: &crate::VerifiedExecutable<'_>,
    context: &HostContext,
    ports: &PortTable,
    selected: &BTreeMap<Identifier, BindingDecision>,
    influence: &SecurityLabels,
    public: bool,
) -> Result<(Option<InputManifest>, Option<RuntimeFailure>), RuntimeError> {
    let mut entries = Vec::new();
    let mut first = None;
    let mut extra = Vec::new();
    for (name, port) in ports.iter() {
        match &selected[name] {
            BindingDecision::Value {
                artifact_id,
                influence: edge_labels,
                ..
            } => match runtime.read_artifact(context, *artifact_id).await {
                Ok(value) => {
                    extra.push(value.metadata.security_labels().clone());
                    if let Err(error) = crate::validate_typed_value(
                        &value.value,
                        port.value_type(),
                        Some(executable.schemas()),
                        &runtime.config.limits,
                        &executable.policy().fields().evaluator_limits,
                    ) && first.is_none()
                    {
                        first = Some(RuntimeFailure::new(FailureCode::for_evaluation(
                            &error,
                            if public {
                                crate::failure::FailureBoundary::Output
                            } else {
                                crate::failure::FailureBoundary::Input
                            },
                        )))
                    }
                    entries.push((
                        name.clone(),
                        ManifestEntry::Value {
                            artifact_id: *artifact_id,
                            content_digest: value.metadata.content_digest(),
                            security_labels: value.metadata.security_labels().clone(),
                        },
                    ));
                    extra.push(edge_labels.clone());
                }
                Err(error) => {
                    if first.is_none() {
                        first = Some(RuntimeFailure::new(error.code()))
                    }
                }
            },
            BindingDecision::Absent { .. } => entries.push((name.clone(), ManifestEntry::Absent)),
            BindingDecision::Failed { failures, .. } => {
                if first.is_none() {
                    first = Some(failures[0].error.clone())
                }
            }
            BindingDecision::Pending { .. } => return Err(err()),
        }
    }
    if first.is_some() {
        return Ok((None, first));
    }
    if public
        && entries.iter().any(|(name, entry)| {
            ports.get(name.as_str()).is_some_and(|p| p.required())
                && matches!(entry, ManifestEntry::Absent)
        })
    {
        return Ok((None, Some(RuntimeFailure::new(FailureCode::OutputMissing))));
    }
    let labels = SecurityLabels::combine(
        std::iter::once(influence).chain(extra.iter()),
        &runtime.config.limits,
    )?;
    Ok((
        Some(InputManifest::freeze(
            ports,
            entries,
            labels,
            &runtime.config.limits,
        )?),
        None,
    ))
}
fn binding_writes(
    scope: ScopeInstanceId,
    node: &Identifier,
    selected: &BTreeMap<Identifier, BindingDecision>,
    runtime: &Runtime,
) -> Result<Vec<RecordWrite>, RuntimeError> {
    selected
        .iter()
        .map(|(port, binding)| {
            let (state, artifact) = match binding {
                BindingDecision::Value { artifact_id, .. } => {
                    ("value", Value::Text(artifact_id.to_string()))
                }
                BindingDecision::Absent { .. } => ("absent", Value::Null),
                BindingDecision::Failed { .. } => ("failed", Value::Null),
                BindingDecision::Pending { .. } => return Err(err()),
            };
            Ok(insert(
                RecordFamily::Binding,
                format!("{scope}:{node}:{port}"),
                object([
                    ("state", Value::Text(state.into())),
                    ("artifact_id", artifact),
                    (
                        "influence_labels",
                        binding.influence().to_value(&runtime.config.limits)?,
                    ),
                ])?,
                true,
            ))
        })
        .collect()
}
async fn operate(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    node: &Node,
    record: &StoredRecord,
) -> Result<bool, RuntimeError> {
    match &node.fields().operation {
        Operation::Eval(_) => {
            let frame = frame(
                runtime,
                lease,
                scope,
                Some((&record.payload, &node.fields().inputs)),
                None,
            )
            .await?;
            let evaluated = frame.evaluate(
                &scope.executable,
                &scope.site,
                &ExpressionSite::Eval(node.id().clone()),
            )?;
            let result = match evaluated.result {
                Ok(result) => result.value,
                Err(error) => {
                    finish_failure(runtime, lease, record, error.code(), evaluated.influence)
                        .await?;
                    return Ok(true);
                }
            };
            if matches!(result, EvaluationValue::Pending) {
                return Ok(false);
            }
            let value = match result {
                EvaluationValue::Present(value) => {
                    object([("state", Value::Text("value".into())), ("value", value)])?
                }
                EvaluationValue::Absent => absent()?,
                EvaluationValue::Pending => return Err(err()),
            };
            let proposed = Value::Map(Map::try_from_entries([("value".into(), value)])?);
            accept_node(
                runtime,
                lease,
                scope,
                node,
                record,
                OutputProposal {
                    values: &proposed,
                    influence: evaluated.influence,
                    parents: vec![],
                },
            )
            .await?;
            Ok(true)
        }
        Operation::Scope(_) | Operation::Loop { .. } => {
            super::composite::operate(runtime, lease, scope, node, record).await
        }
        Operation::Wait { .. } => super::waits::operate(runtime, lease, scope, node, record).await,
        Operation::Mcp { .. } => {
            match super::mcp::operate(runtime, lease, scope, node, record).await {
                Ok(changed) => Ok(changed),
                Err(error @ RuntimeError::Storage(_)) => Err(error),
                Err(error) => {
                    finish_failure(
                        runtime,
                        lease,
                        record,
                        error.code(),
                        labels(&record.payload, &runtime.config.limits)?,
                    )
                    .await?;
                    Ok(true)
                }
            }
        }
    }
}
pub(super) struct OutputProposal<'a> {
    pub values: &'a Value,
    pub influence: SecurityLabels,
    pub parents: Vec<ArtifactId>,
}
pub(super) async fn accept_node(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
    node: &Node,
    record: &StoredRecord,
    proposal: OutputProposal<'_>,
) -> Result<(), RuntimeError> {
    let OutputProposal {
        values: proposed,
        influence,
        parents,
    } = proposal;
    let mut parents = parents;
    for (_, input) in map(field(&record.payload, "inputs")?)?.iter() {
        if let Some(id) = entry(input)? {
            parents.push(id)
        }
    }
    let mut artifacts = Vec::new();
    let mut outputs = Vec::new();
    let invocation: InvocationId = record.key.id.parse().map_err(|_| err())?;
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    for (port, declared) in node.fields().outputs.iter() {
        let value = map(proposed)?.get(port.as_str()).ok_or_else(err)?;
        if text(field(value, "state")?)? == "absent" {
            if declared.required() {
                return finish_failure(
                    runtime,
                    lease,
                    record,
                    FailureCode::OutputMissing,
                    influence,
                )
                .await;
            }
            outputs.push((port.to_string(), absent()?));
            continue;
        }
        let value = if let Some(value) = map(value)?.get("value") {
            value.clone()
        } else {
            runtime
                .read_artifact(&scope.context, entry(value)?.ok_or_else(err)?)
                .await?
                .value
        };
        let artifact = match PreparedArtifact::new(
            &scope.executable,
            ArtifactInput {
                producer: ArtifactProducer::NodeOutput {
                    invocation_id: invocation,
                    port: port.clone(),
                },
                value_type: declared.value_type(),
                value: &value,
                input_artifact_ids: parents.clone(),
                security_labels: influence.clone(),
                accepted_at: now,
            },
            &runtime.config.limits,
        ) {
            Ok(value) => value,
            Err(error) => {
                return finish_failure(
                    runtime,
                    lease,
                    record,
                    error.code(crate::failure::FailureBoundary::Output),
                    influence,
                )
                .await;
            }
        };
        outputs.push((
            port.to_string(),
            present(artifact.metadata().artifact_id())?,
        ));
        artifacts.push(artifact);
    }
    let mut frame = frame(
        runtime,
        lease,
        scope,
        Some((&record.payload, &node.fields().inputs)),
        Some((proposed, &node.fields().outputs)),
    )
    .await?;
    frame.influence.base = influence.clone();
    let post = frame.evaluate(
        &scope.executable,
        &scope.site,
        &ExpressionSite::NodePostconditions(node.id().clone()),
    )?;
    let influence = influence.union(&post.influence, &runtime.config.limits)?;
    if let Some(error) = condition_error(&post, FailureCode::Postconditions) {
        return finish_node(
            runtime,
            lease,
            record,
            "failed",
            Some(error),
            influence,
            vec![],
        )
        .await;
    }
    // Rebuild after the contract to include all additional control influence.
    let mut writes = Vec::new();
    let mut final_outputs = Vec::new();
    for artifact in artifacts {
        let metadata = artifact.metadata();
        let value = metadata.verify_value(artifact.bytes(), &runtime.config.limits)?;
        let artifact = PreparedArtifact::new(
            &scope.executable,
            ArtifactInput {
                producer: metadata.producer().clone(),
                value_type: metadata.value_type(),
                value: &value,
                input_artifact_ids: metadata.input_artifact_ids().to_vec(),
                security_labels: influence.clone(),
                accepted_at: runtime
                    .store
                    .coordinator_time(runtime.clock.now_ms()?)
                    .await?,
            },
            &runtime.config.limits,
        )?;
        runtime.store.stage_artifact(&artifact).await?;
        let port = match artifact.metadata().producer() {
            ArtifactProducer::NodeOutput { port, .. } => port.to_string(),
            _ => return Err(err()),
        };
        final_outputs.push((port, present(artifact.metadata().artifact_id())?));
        writes.push(insert(
            RecordFamily::Artifact,
            artifact.metadata().artifact_id(),
            artifact.metadata().to_value(&runtime.config.limits)?,
            true,
        ));
    }
    for (port, value) in outputs {
        if entry(&value)?.is_none() {
            final_outputs.push((port, value));
        }
    }
    let payload = replace(
        &record.payload,
        [
            ("status", Value::Text("succeeded".into())),
            ("reason", Value::Null),
            ("outputs", Value::Map(Map::try_from_entries(final_outputs)?)),
            (
                "influence_labels",
                influence.to_value(&runtime.config.limits)?,
            ),
        ],
    )?;
    let fresh = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    if due(&record.payload, fresh)? || due(&scope.record.payload, fresh)? {
        return finish_failure(runtime, lease, record, FailureCode::Timeout, influence).await;
    }
    writes.push(update(record, payload, true));
    commit(
        runtime,
        lease,
        format!("succeed:{}:{}", record.key.id, record.revision),
        writes,
        None,
    )
    .await
}
pub(super) async fn finish_failure(
    runtime: &Runtime,
    lease: &RunLease,
    record: &StoredRecord,
    code: FailureCode,
    influence: SecurityLabels,
) -> Result<(), RuntimeError> {
    finish_node(
        runtime,
        lease,
        record,
        "failed",
        Some(RuntimeFailure::new(code)),
        influence,
        vec![],
    )
    .await
}
pub(super) async fn finish_node(
    runtime: &Runtime,
    lease: &RunLease,
    record: &StoredRecord,
    status: &str,
    error: Option<RuntimeFailure>,
    influence: SecurityLabels,
    mut writes: Vec<RecordWrite>,
) -> Result<(), RuntimeError> {
    let payload = replace(
        &record.payload,
        [
            ("status", Value::Text(status.into())),
            ("reason", Value::Null),
            (
                "failure",
                error.map_or(Ok(Value::Null), |e| {
                    e.to_value(&runtime.config.limits).map_err(|_| err())
                })?,
            ),
            (
                "influence_labels",
                influence.to_value(&runtime.config.limits)?,
            ),
        ],
    )?;
    writes.push(update(record, payload, true));
    commit(
        runtime,
        lease,
        format!("{status}:{}:{}", record.key.id, record.revision),
        writes,
        None,
    )
    .await
}
async fn close_scope(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
) -> Result<bool, RuntimeError> {
    if map(&scope.record.payload)?.get("loop_invocation").is_some() {
        return super::composite::close_iteration(runtime, lease, scope).await;
    }
    let destinations = scope
        .definition
        .fields()
        .outputs
        .iter()
        .map(|(port, _)| (port.clone(), EdgeDestination::Output(port.clone())))
        .collect();
    let (selected, writes) = bindings(runtime, lease, scope, destinations).await?;
    if !writes.is_empty() {
        commit(
            runtime,
            lease,
            format!(
                "scope-guards:{}",
                writes
                    .iter()
                    .map(|w| w.key.id.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            writes,
            None,
        )
        .await?;
        return Ok(true);
    }
    if selected.values().any(BindingDecision::is_pending) {
        return Ok(false);
    }
    let influence = SecurityLabels::combine(
        std::iter::once(&labels(&scope.record.payload, &runtime.config.limits)?)
            .chain(selected.values().map(BindingDecision::influence))
            .chain(
                scope
                    .nodes
                    .values()
                    .map(|node| labels(&node.payload, &runtime.config.limits))
                    .collect::<Result<Vec<_>, _>>()?
                    .iter(),
            ),
        &runtime.config.limits,
    )?;
    let (manifest, error) = validate_selected(
        runtime,
        &scope.executable,
        &scope.context,
        &scope.definition.fields().outputs,
        &selected,
        &influence,
        true,
    )
    .await?;
    if let Some(error) = error {
        scope_failure(runtime, lease, scope, error, influence).await?;
        return Ok(true);
    }
    let manifest = manifest.ok_or_else(err)?;
    let frame = frame(
        runtime,
        lease,
        scope,
        None,
        Some((
            &manifest.to_value(&runtime.config.limits)?,
            &scope.definition.fields().outputs,
        )),
    )
    .await?;
    let post = frame.evaluate(
        &scope.executable,
        &scope.site,
        &ExpressionSite::Postconditions,
    )?;
    let influence = manifest
        .influence_labels()
        .union(&post.influence, &runtime.config.limits)?;
    if let Some(error) = condition_error(&post, FailureCode::Postconditions) {
        scope_failure(runtime, lease, scope, error, influence).await?;
        return Ok(true);
    }
    let payload = replace(
        &scope.record.payload,
        [
            ("status", Value::Text("succeeded".into())),
            ("outputs", manifest.to_value(&runtime.config.limits)?),
            (
                "influence_labels",
                influence.to_value(&runtime.config.limits)?,
            ),
        ],
    )?;
    let root = runtime.store.run_header(lease.run_id()).await?.root_scope == scope.id;
    commit(
        runtime,
        lease,
        format!("scope-succeed:{}:{}", scope.id, scope.record.revision),
        vec![update(&scope.record, payload, true)],
        if root {
            Some(RunStatus::Succeeded)
        } else {
            None
        },
    )
    .await?;
    Ok(true)
}
pub(super) async fn scope_failure(
    runtime: &Runtime,
    lease: &RunLease,
    scope: &ScopeState,
    error: RuntimeFailure,
    influence: SecurityLabels,
) -> Result<(), RuntimeError> {
    let payload = replace(
        &scope.record.payload,
        [
            ("status", Value::Text("failed".into())),
            (
                "failure",
                error.to_value(&runtime.config.limits).map_err(|_| err())?,
            ),
            (
                "influence_labels",
                influence.to_value(&runtime.config.limits)?,
            ),
        ],
    )?;
    let root = runtime.store.run_header(lease.run_id()).await?.root_scope == scope.id;
    commit(
        runtime,
        lease,
        format!("scope-fail:{}:{}", scope.id, scope.record.revision),
        vec![update(&scope.record, payload, true)],
        if root { Some(RunStatus::Failed) } else { None },
    )
    .await
}
async fn stop_scope(
    runtime: &Runtime,
    lease: &RunLease,
    scope: &ScopeState,
    code: FailureCode,
) -> Result<(), RuntimeError> {
    super::control::stop(
        runtime,
        lease,
        Some(&scope.id.to_string()),
        None,
        code,
        false,
        vec![],
    )
    .await
}
