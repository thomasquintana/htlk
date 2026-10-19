//! Ordinary task boundaries and explicit repeat-until checkpoints.
use super::{
    model::*,
    step::{self, ScopeState},
};
use crate::{
    EvaluationValue, Runtime, RuntimeError,
    failure::{FailureCode, RuntimeFailure},
    identity::{InvocationId, ScopeInstanceId},
    records::SecurityLabels,
    store::{RecordFamily, RunLease, StoredRecord},
};
use htlk_analyzer::{ExpressionSite, ScopeUse};
use htlk_executable::{
    EdgeDestination, Node, Operation, Port, PortTable, ValueReference,
    cbor::{Map, Value},
};
use std::sync::Arc;

pub(super) async fn operate(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    parent: &ScopeState,
    node: &Node,
    record: &StoredRecord,
) -> Result<bool, RuntimeError> {
    let child = field(&record.payload, "child_scope")?;
    if matches!(child, Value::Null) {
        let invocation: InvocationId = record.key.id.parse().map_err(|_| err())?;
        let (definition, iteration, initializers) = match &node.fields().operation {
            Operation::Scope(definition) => (*definition, None, None),
            Operation::Loop {
                body, initializers, ..
            } => (
                *body,
                Some(
                    map(&record.payload)?
                        .get("iteration")
                        .and_then(|v| {
                            if let Value::Integer(n) = v {
                                Some(*n as u64)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0),
                ),
                Some(initializers),
            ),
            _ => return Err(err()),
        };
        let site = ScopeUse::Node {
            scope: digest(&parent.record.payload, "definition_digest")?,
            node: node.id().clone(),
        };
        let target = parent
            .executable
            .document()
            .fields()
            .scopes
            .get(&definition)
            .ok_or_else(err)?;
        let id = if let Some(index) = iteration {
            ScopeInstanceId::iteration(invocation, index, &runtime.config.limits)
        } else {
            ScopeInstanceId::child(invocation, &runtime.config.limits)
        }
        .map_err(|_| err())?;
        let input_frame = step::frame(
            runtime,
            lease,
            parent,
            Some((&record.payload, &node.fields().inputs)),
            None,
        )
        .await?;
        let pre =
            input_frame.evaluate(&parent.executable, &site, &ExpressionSite::Preconditions)?;
        if let Some(error) = step::condition_error(&pre, FailureCode::Preconditions) {
            let mut diagnostic = step::contract_diagnostic(
                runtime,
                parent,
                Some(node),
                "preconditions",
                &error,
                &pre.influence,
                record.revision,
            )?;
            diagnostic.payload = replace(
                &diagnostic.payload,
                [(
                    "canonical_path",
                    Value::Text(format!("/scopes/{definition}/preconditions")),
                )],
            )?;
            let mut writes = vec![diagnostic];
            match runtime
                .store
                .record(lease.run_id(), &key(RecordFamily::Scope, id))
                .await
            {
                Ok(pending) if string(&pending.payload, "status")? == "pending" => {
                    writes.push(update(
                        &pending,
                        replace(
                            &pending.payload,
                            [
                                ("status", Value::Text("failed".into())),
                                (
                                    "failure",
                                    error.to_value(&runtime.config.limits).map_err(|_| err())?,
                                ),
                            ],
                        )?,
                        true,
                    ))
                }
                Err(crate::store::StoreError::Missing) => {}
                Ok(_) => return Err(err()),
                Err(error) => return Err(error.into()),
            }
            step::finish_node(
                runtime,
                lease,
                record,
                "failed",
                Some(error),
                pre.influence,
                writes,
            )
            .await?;
            return Ok(true);
        }
        let inherited = htlk_executable::ExecutionLimits::from_value(
            field(&record.payload, "effective_limits")?,
            &runtime.config.limits,
        )
        .map_err(|_| err())?;
        let effective = crate::run::min_limits(&[&inherited, &target.fields().limits])?;
        let now = runtime
            .store
            .coordinator_time(runtime.clock.now_ms()?)
            .await?;
        let deadline = now
            .checked_add(i64::try_from(effective.timeout_ms().ok_or_else(err)?).map_err(|_| err())?)
            .ok_or_else(err)?
            .min(number(&record.payload, "deadline_ms")?);
        let mut carried = Vec::new();
        if let Some(initializers) = initializers {
            if iteration == Some(0) {
                for (name, input) in initializers {
                    carried.push((
                        name.to_string(),
                        map(field(&record.payload, "inputs")?)?
                            .get(input.as_str())
                            .ok_or_else(err)?
                            .clone(),
                    ));
                }
            } else {
                carried.extend(
                    map(field(&record.payload, "next")?)?
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.clone())),
                );
            }
        }
        let mut payload = object([
            ("record_version", Value::Text("0.1".into())),
            ("definition_digest", Value::Text(definition.to_string())),
            (
                "origin_executable",
                Value::Text(parent.executable.fingerprint().to_string()),
            ),
            ("scope_use", encode_use(&site)),
            ("principal", Value::Text(parent.context.principal().into())),
            ("inputs", field(&record.payload, "inputs")?.clone()),
            (
                "input_checkpoint",
                field(&record.payload, "input_checkpoint")?.clone(),
            ),
            (
                "influence_labels",
                pre.influence.to_value(&runtime.config.limits)?,
            ),
            ("status", Value::Text("active".into())),
            ("failure", Value::Null),
            ("admitted_at_ms", Value::Integer(now)),
            ("deadline_ms", Value::Integer(deadline)),
            (
                "effective_limits",
                effective
                    .to_value(&runtime.config.limits)
                    .map_err(|_| err())?,
            ),
            ("outputs", Value::Null),
            ("carried", Value::Map(Map::try_from_entries(carried)?)),
            ("parent_invocation", Value::Text(invocation.to_string())),
            (
                "run_context",
                field(&parent.record.payload, "run_context")?.clone(),
            ),
        ])?;
        if let Some(index) = iteration {
            payload = replace(
                &payload,
                [
                    ("loop_invocation", Value::Text(invocation.to_string())),
                    ("iteration", Value::Integer(index as i64)),
                ],
            )?;
        }
        let mut node_payload = replace(
            &record.payload,
            [
                ("child_scope", Value::Text(id.to_string())),
                (
                    "reason",
                    Value::Text(
                        if iteration.is_some() {
                            "iteration"
                        } else {
                            "scope"
                        }
                        .into(),
                    ),
                ),
            ],
        )?;
        if let Some(index) = iteration {
            node_payload = replace(&node_payload, [("iteration", Value::Integer(index as i64))])?;
        }
        let child_write = match runtime
            .store
            .record(lease.run_id(), &key(RecordFamily::Scope, id))
            .await
        {
            Ok(pending) if string(&pending.payload, "status")? == "pending" => {
                update(&pending, payload, false)
            }
            Err(crate::store::StoreError::Missing) => {
                insert(RecordFamily::Scope, id, payload, false)
            }
            Ok(_) => return Err(err()),
            Err(error) => return Err(error.into()),
        };
        step::commit(
            runtime,
            lease,
            format!("open-scope:{}:{}", record.key.id, record.revision),
            vec![child_write, update(record, node_payload, false)],
            None,
        )
        .await?;
        return Ok(true);
    }
    let id = text(child)?;
    let child = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Scope, id))
        .await?;
    if !child.final_record {
        return Ok(false);
    }
    let influence = labels(&record.payload, &runtime.config.limits)?.union(
        &labels(&child.payload, &runtime.config.limits)?,
        &runtime.config.limits,
    )?;
    if string(&child.payload, "status")? != "succeeded" {
        let error = failure(&child.payload, &runtime.config.limits)?;
        step::finish_failure(runtime, lease, record, error.code(), influence).await?;
        return Ok(true);
    }
    if let Operation::Loop { max_iterations, .. } = &node.fields().operation {
        let Value::Bool(until) = field(&child.payload, "until")? else {
            return Err(err());
        };
        if !until {
            let iteration = number(&child.payload, "iteration")? as u64;
            if iteration + 1 >= *max_iterations {
                step::finish_failure(runtime, lease, record, FailureCode::LoopLimit, influence)
                    .await?;
                return Ok(true);
            }
            let payload = replace(
                &record.payload,
                [
                    ("iteration", Value::Integer((iteration + 1) as i64)),
                    ("next", field(&child.payload, "next")?.clone()),
                    ("child_scope", Value::Null),
                    (
                        "influence_labels",
                        influence.to_value(&runtime.config.limits)?,
                    ),
                ],
            )?;
            step::commit(
                runtime,
                lease,
                format!("loop-advance:{}:{}", record.key.id, iteration),
                vec![update(record, payload, false)],
                None,
            )
            .await?;
            return Ok(true);
        }
    }
    let outputs = field(&child.payload, "outputs")?;
    let parents = map(outputs)?
        .iter()
        .filter_map(|(_, v)| entry(v).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    step::accept_node(
        runtime,
        lease,
        parent,
        node,
        record,
        step::OutputProposal {
            values: outputs,
            influence,
            parents,
        },
    )
    .await?;
    Ok(true)
}

pub(super) async fn close_iteration(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    scope: &ScopeState,
) -> Result<bool, RuntimeError> {
    let next_dest = scope
        .definition
        .fields()
        .carried
        .iter()
        .map(|(n, _)| (n.clone(), EdgeDestination::Next(n.clone())))
        .collect();
    let out_dest = scope
        .definition
        .fields()
        .outputs
        .iter()
        .map(|(n, _)| (n.clone(), EdgeDestination::Output(n.clone())))
        .collect();
    let (next, mut writes) = step::bindings(runtime, lease, scope, next_dest).await?;
    let (outputs, other) = step::bindings(runtime, lease, scope, out_dest).await?;
    writes.extend(other);
    if !writes.is_empty() {
        step::commit(
            runtime,
            lease,
            format!(
                "iteration-guards:{}",
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
    if next
        .values()
        .chain(outputs.values())
        .any(crate::binding::BindingDecision::is_pending)
    {
        return Ok(false);
    }
    writes.extend(step::binding_writes(scope.id, "$next", &next, runtime)?);
    writes.extend(step::binding_writes(
        scope.id, "$outputs", &outputs, runtime,
    )?);
    let base = labels(&scope.record.payload, &runtime.config.limits)?;
    let influence = SecurityLabels::combine(
        std::iter::once(&base).chain(
            next.values()
                .chain(outputs.values())
                .map(crate::binding::BindingDecision::influence),
        ),
        &runtime.config.limits,
    )?;
    let relaxed = |ports: &PortTable| {
        PortTable::new(
            ports
                .iter()
                .map(|(n, p)| (n.clone(), Port::new(p.value_type().clone(), false)))
                .collect(),
            &runtime.config.limits,
        )
        .map_err(|_| err())
    };
    let (next_manifest, next_error, next_errors) = step::validate_selected(
        runtime,
        &scope.executable,
        &scope.context,
        &relaxed(&scope.definition.fields().carried)?,
        &next,
        &influence,
        true,
    )
    .await?;
    let (output_manifest, output_error, output_errors) = step::validate_selected(
        runtime,
        &scope.executable,
        &scope.context,
        &relaxed(&scope.definition.fields().outputs)?,
        &outputs,
        &influence,
        true,
    )
    .await?;
    writes.extend(step::validation_diagnostics(
        runtime,
        scope,
        None,
        "carried",
        &next_errors,
        &influence,
    )?);
    writes.extend(step::validation_diagnostics(
        runtime,
        scope,
        None,
        "outputs",
        &output_errors,
        &influence,
    )?);
    if let Some(error) = next_error.or(output_error) {
        step::scope_failure_with(runtime, lease, scope, error, influence, writes).await?;
        return Ok(true);
    }
    for (ports, selected) in [
        (&scope.definition.fields().carried, &next),
        (&scope.definition.fields().outputs, &outputs),
    ] {
        for (name, port) in ports.iter() {
            if port.required()
                && matches!(
                    selected[name],
                    crate::binding::BindingDecision::Absent { .. }
                )
            {
                step::scope_failure_with(
                    runtime,
                    lease,
                    scope,
                    RuntimeFailure::new(FailureCode::OutputMissing),
                    influence,
                    writes,
                )
                .await?;
                return Ok(true);
            }
        }
    }
    let next = next_manifest
        .ok_or_else(err)?
        .to_value(&runtime.config.limits)?;
    let outputs = output_manifest
        .ok_or_else(err)?
        .to_value(&runtime.config.limits)?;
    let mut frame = step::frame(
        runtime,
        lease,
        scope,
        None,
        Some((&outputs, &scope.definition.fields().outputs)),
    )
    .await?;
    frame.influence.base = influence.clone();
    for (name, value) in map(&next)?.iter() {
        frame.bind(
            ValueReference::Next(name.parse().map_err(|_| err())?),
            entry(value)?.map_or(super::frame::Slot::Absent, super::frame::Slot::Artifact),
            influence.clone(),
        );
    }
    let until = frame.evaluate(&scope.executable, &scope.site, &ExpressionSite::Until)?;
    let influence = influence.union(&until.influence, &runtime.config.limits)?;
    let value = match until.result {
        Ok(result) => match result.value {
            EvaluationValue::Present(Value::Bool(v)) => v,
            _ => {
                step::scope_failure_with(
                    runtime,
                    lease,
                    scope,
                    RuntimeFailure::new(FailureCode::Invariant),
                    influence,
                    writes,
                )
                .await?;
                return Ok(true);
            }
        },
        Err(error) => {
            writes.push(step::contract_diagnostic(
                runtime,
                scope,
                None,
                "until",
                &RuntimeFailure::new(error.code()),
                &influence,
                scope.record.revision,
            )?);
            step::scope_failure_with(
                runtime,
                lease,
                scope,
                RuntimeFailure::new(error.code()),
                influence,
                writes,
            )
            .await?;
            return Ok(true);
        }
    };
    let payload = replace(
        &scope.record.payload,
        [
            ("status", Value::Text("succeeded".into())),
            ("outputs", outputs),
            ("next", next),
            ("until", Value::Bool(value)),
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
    if fresh >= number(&scope.record.payload, "deadline_ms")? {
        super::control::stop(
            runtime,
            lease,
            Some(&scope.id.to_string()),
            None,
            FailureCode::Timeout,
            false,
            vec![],
        )
        .await?;
        return Ok(true);
    }
    writes.push(update(&scope.record, payload, true));
    step::commit(
        runtime,
        lease,
        format!("iteration-close:{}", scope.id),
        writes,
        None,
    )
    .await?;
    Ok(true)
}
