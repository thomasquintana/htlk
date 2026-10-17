//! Checked additive scope mounting, with atomic optional planning-wait closure.
use super::{
    host::{self, Command, Operation as HostOperation},
    model::*,
};
use crate::{
    Authorization, HostContext, Runtime, RuntimeError,
    artifact::{ArtifactInput, ArtifactProducer, PreparedArtifact},
    failure::FailureCode,
    identity::{InvocationId, RunId, ScopeInstanceId, WaitId, host_request_digest},
    records::{InputManifest, InputValue, ManifestEntry, SecurityLabels},
    store::{
        RecordFamily, RecordWrite, RootReplacement, RunLease, StoredRecord, Transition,
        TransitionContext,
    },
};
use htlk_analyzer::ScopeUse;
use htlk_executable::{
    CanonicalDocument, EdgeDestination, EdgeSource, ExecutionLimits, Expression, ExpressionKind,
    Identifier, Node, Operation, Scope, cbor::Value, digest::Digest,
    record_accounting::RecordAccounting,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// An input-wait acknowledgement installed atomically with an extension.
#[derive(Clone, Debug)]
pub struct CloseWait {
    /// Exact input wait in the preserved operand.
    pub wait_id: WaitId,
    /// Typed reply proposal, subject to ordinary wait validation.
    pub value: Value,
    /// Principal/method/wait-scoped reply identity.
    pub idempotency_key: String,
}
/// Complete additive installation proposal. Graphs must already be registered.
#[derive(Clone, Debug)]
pub struct ExtensionRequest {
    /// Compare-and-swap active graph identity.
    pub expected_fingerprint: Digest,
    /// Exact registered binary composition.
    pub composed_fingerprint: Digest,
    /// Public inputs; preserved inputs may also inherit their existing values.
    pub inputs: BTreeMap<Identifier, InputValue>,
    /// Optional atomic planning-window acknowledgement.
    pub close_wait: Option<CloseWait>,
}
/// Immutable successful installation receipt, including historical retries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionReceipt {
    /// Installed active graph at the time of the receipt.
    pub active_fingerprint: Digest,
    /// One-based installation number.
    pub extension_number: u64,
}
impl Runtime {
    /// Installs an ordinary binary join around existing work without replaying or
    /// renaming it. Invalid installation or reply changes neither graph nor wait.
    ///
    /// # Errors
    /// Returns stale/terminal/nonadditive proposals, changed preserved inputs,
    /// policy/budget/type/reply failures, or unavailable fenced ownership.
    pub async fn extend_run(
        &self,
        context: &HostContext,
        run: &RunId,
        request: &ExtensionRequest,
        idempotency_key: &str,
    ) -> Result<ExtensionReceipt, RuntimeError> {
        self.policy.authorize(
            context,
            Authorization::ExtendRun {
                run_id: run,
                expected_fingerprint: request.expected_fingerprint,
                composed_fingerprint: request.composed_fingerprint,
            },
        )?;
        if idempotency_key.is_empty() {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let mut accounting = RecordAccounting::new(&self.config.limits)?;
        accounting.text(idempotency_key, 0).map_err(input_limit)?;
        accounting
            .collection(request.inputs.len(), 0)
            .map_err(input_limit)?;
        for (name, value) in &request.inputs {
            accounting.text(name.as_str(), 1).map_err(input_limit)?;
            match value {
                InputValue::Inline(value) => accounting.value(value, 1).map_err(input_limit)?,
                InputValue::Artifact(id) => {
                    accounting.text(&id.to_string(), 1).map_err(input_limit)?
                }
            }
        }
        if let Some(close) = &request.close_wait {
            accounting.value(&close.value, 1).map_err(input_limit)?;
            accounting
                .text(&close.idempotency_key, 1)
                .map_err(input_limit)?;
        }
        let inputs = Value::Map(htlk_executable::cbor::Map::try_from_entries(
            request
                .inputs
                .iter()
                .map(|(k, v)| Ok((k.to_string(), v.to_value(&self.config.limits)?)))
                .collect::<Result<Vec<_>, crate::records::RuntimeRecordError>>()?,
        )?);
        let close = request
            .close_wait
            .as_ref()
            .map(|w| {
                object([
                    ("wait_id", Value::Text(w.wait_id.to_string())),
                    ("value", w.value.clone()),
                    ("idempotency_key", Value::Text(w.idempotency_key.clone())),
                ])
            })
            .transpose()?
            .unwrap_or(Value::Null);
        let arguments = Value::Array(vec![
            Value::Text(request.expected_fingerprint.to_string()),
            Value::Text(request.composed_fingerprint.to_string()),
            inputs,
            close,
        ]);
        let digest = host_request_digest(
            context.principal(),
            "extend_run",
            run.as_str(),
            &arguments,
            &self.config.limits,
        )
        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
        let command = Command {
            run: run.clone(),
            context: context.clone(),
            method: "extend_run".into(),
            target: run.to_string(),
            key: idempotency_key.into(),
            digest,
            operation: HostOperation::Extend {
                request: request.clone(),
            },
        };
        if host::recorded(self, &command).await?.is_none() {
            let header = self.store.run_header(run).await?;
            if header.status.is_terminal() {
                return Err(invalid());
            }
            if header.fingerprint != request.expected_fingerprint {
                return Err(RuntimeError::failure(FailureCode::ExtensionConflict));
            }
        }
        let response = host::route(self, command).await?;
        Ok(ExtensionReceipt {
            active_fingerprint: digest_value(&response, "active_fingerprint")?,
            extension_number: number(&response, "extension_number")? as u64,
        })
    }
}
fn digest_value(value: &Value, name: &str) -> Result<Digest, RuntimeError> {
    digest(value, name)
}
fn invalid() -> RuntimeError {
    RuntimeError::failure(FailureCode::Extension)
}
fn input_limit(_: htlk_executable::record_accounting::EncodingLimitError) -> RuntimeError {
    RuntimeError::failure(FailureCode::Input)
}
fn truth(e: &Expression) -> bool {
    matches!(
        e.kind(),
        ExpressionKind::Literal(htlk_executable::ScalarLiteral::Boolean(true))
    )
}
fn shape<'a>(
    old: &CanonicalDocument,
    new: &'a CanonicalDocument,
) -> Result<(&'a Scope, Vec<&'a Node>), RuntimeError> {
    let old = old.fields();
    let new = new.fields();
    if old.profile != new.profile {
        return Err(invalid());
    }
    macro_rules! preserved {
        ($table:ident) => {
            if old.$table.iter().any(|(k, v)| new.$table.get(k) != Some(v)) {
                return Err(invalid());
            }
        };
    }
    preserved!(scopes);
    preserved!(templates);
    preserved!(bindings);
    preserved!(libraries);
    preserved!(documents);
    preserved!(schema_uris);
    let root = &new.scopes[&new.root_scope];
    let f = root.fields();
    if f.nodes.len() != 2
        || !f.carried.is_empty()
        || f.limits != ExecutionLimits::new()
        || !truth(&f.preconditions)
        || !truth(&f.postconditions)
    {
        return Err(invalid());
    }
    for n in &f.nodes {
        let n = n.fields();
        let Operation::Scope(target) = n.operation else {
            return Err(invalid());
        };
        if !truth(&n.guard)
            || !truth(&n.preconditions)
            || !truth(&n.postconditions)
            || n.limits != ExecutionLimits::new()
            || n.inputs != new.scopes[&target].fields().inputs
            || n.outputs != new.scopes[&target].fields().outputs
        {
            return Err(invalid());
        }
    }
    let mut destinations = BTreeSet::new();
    for edge in &f.edges {
        if !truth(edge.guard()) {
            return Err(invalid());
        }
        let destination = format!("{:?}", edge.destination());
        if !destinations.insert(destination) {
            return Err(invalid());
        }
        match (edge.source(), edge.destination()) {
            (EdgeSource::Input(_), EdgeDestination::Input { .. })
            | (EdgeSource::Output { .. }, EdgeDestination::Output(_)) => {}
            (EdgeSource::Output { node: a, .. }, EdgeDestination::Input { node: b, .. })
                if a != b => {}
            _ => return Err(invalid()),
        }
    }
    let preserved:Vec<_>=f.nodes.iter().filter(|n|matches!(n.fields().operation,Operation::Scope(d)if d==old.root_scope)&&!f.edges.iter().any(|e|matches!((e.source(),e.destination()),(EdgeSource::Output{..},EdgeDestination::Input{node,..}) if node==n.id()))).collect();
    if preserved.is_empty() {
        return Err(invalid());
    }
    Ok((root, preserved))
}

pub(super) async fn install(
    runtime: &Arc<Runtime>,
    lease: &RunLease,
    command: &Command,
    request: &ExtensionRequest,
) -> Result<Value, RuntimeError> {
    let header = runtime.store.run_header(lease.run_id()).await?;
    if header.status.is_terminal() {
        return Err(invalid());
    }
    if header.fingerprint != request.expected_fingerprint {
        return Err(RuntimeError::failure(FailureCode::ExtensionConflict));
    }
    runtime.policy.authorize(
        &command.context,
        Authorization::ExtendRun {
            run_id: lease.run_id(),
            expected_fingerprint: request.expected_fingerprint,
            composed_fingerprint: request.composed_fingerprint,
        },
    )?;
    let old = runtime.admitted(header.fingerprint).await?;
    let new = runtime.admitted(request.composed_fingerprint).await?;
    let (root, candidates) = shape(old.document(), new.document())?;
    for scope in new.document().fields().scopes.values() {
        crate::run::reject_unsupported(&scope.fields().limits)?;
        for node in &scope.fields().nodes {
            crate::run::reject_unsupported(&node.fields().limits)?;
        }
    }
    let old_scope = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Scope, header.root_scope))
        .await?;
    if old_scope.final_record
        || digest(&old_scope.payload, "definition_digest")? != old.document().fields().root_scope
    {
        return Err(invalid());
    }
    let generation = map(&old_scope.payload)?
        .get("extension_number")
        .map(|v| {
            if let Value::Integer(n) = v {
                Ok(*n)
            } else {
                Err(err())
            }
        })
        .transpose()?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(err)?;
    let id =
        ScopeInstanceId::extended_root(lease.run_id(), generation as u64, &runtime.config.limits)
            .map_err(|_| err())?;
    let Value::Bytes(checkpoint) = field(&old_scope.payload, "input_checkpoint")? else {
        return Err(err());
    };
    let original = InputManifest::decode(
        checkpoint,
        &old.document().fields().scopes[&old.document().fields().root_scope]
            .fields()
            .inputs,
        &runtime.config.limits,
    )?;
    let original_entries: BTreeMap<_, _> = original
        .entries()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    let mut selected = None;
    let mut failure = invalid();
    for preserved in candidates {
        let mut inherited = BTreeMap::new();
        let mut compatible = true;
        for edge in &root.fields().edges {
            if let EdgeDestination::Input { node, port } = edge.destination()
                && node == preserved.id()
            {
                let EdgeSource::Input(public) = edge.source() else {
                    return Err(invalid());
                };
                let entry = original_entries.get(port).ok_or_else(err)?.clone();
                if inherited
                    .insert(public.clone(), entry.clone())
                    .is_some_and(|old| !same_entry(&old, &entry))
                {
                    compatible = false;
                    break;
                }
            }
        }
        if !compatible {
            continue;
        }
        let (manifest, artifacts) = match inputs(
            runtime,
            &command.context,
            lease.run_id(),
            generation as u64,
            &new,
            root,
            &request.inputs,
            &inherited,
            &original,
        )
        .await
        {
            Ok(result) => result,
            Err(error @ RuntimeError::Storage(_)) => return Err(error),
            Err(error) => {
                failure = error;
                continue;
            }
        };
        let entries: BTreeMap<_, _> = manifest
            .entries()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (port, original_entry) in &original_entries {
            let incoming = root
                .fields()
                .edges
                .iter()
                .find_map(|e| match (e.source(), e.destination()) {
                    (EdgeSource::Input(public), EdgeDestination::Input { node, port: p })
                        if node == preserved.id() && p == port =>
                    {
                        entries.get(public)
                    }
                    _ => None,
                })
                .unwrap_or(&ManifestEntry::Absent);
            if !same_entry(original_entry, incoming) {
                compatible = false;
                failure = invalid();
                break;
            }
        }
        if compatible {
            selected = Some((preserved, manifest, artifacts));
            break;
        }
    }
    let (preserved, manifest, artifacts) = selected.ok_or(failure)?;
    let mount = InvocationId::new(id, preserved.id(), &runtime.config.limits).map_err(|_| err())?;
    let usage = runtime
        .store
        .record(lease.run_id(), &key(RecordFamily::Usage, "run"))
        .await?;
    let limits =
        ExecutionLimits::from_value(field(&usage.payload, "limits")?, &runtime.config.limits)
            .map_err(|_| err())?;
    let execution_context = HostContext::new(
        string(&old_scope.payload, "principal")?,
        &runtime.config.limits,
    )?;
    let principal_limits = runtime.policy.execution_limits(&execution_context)?;
    crate::run::reject_unsupported(&principal_limits)?;
    crate::run::reject_unsupported(&runtime.config.execution_limits)?;
    if crate::run::min_limits(&[&limits, &runtime.config.execution_limits, &principal_limits])?
        != limits
    {
        return Err(invalid());
    }
    let deadline = number(&usage.payload, "deadline_ms")?;
    if now >= deadline {
        return Err(RuntimeError::failure(FailureCode::Timeout));
    }
    runtime.policy.authorize(
        &execution_context,
        Authorization::ReadBoundary {
            run_id: lease.run_id(),
            scope_instance_id: id,
            labels: manifest.influence_labels(),
        },
    )?;
    let response = object([
        (
            "active_fingerprint",
            Value::Text(request.composed_fingerprint.to_string()),
        ),
        ("extension_number", Value::Integer(generation)),
    ])?;
    let root_payload = object([
        ("record_version", Value::Text("0.1".into())),
        (
            "definition_digest",
            Value::Text(new.document().fields().root_scope.to_string()),
        ),
        (
            "origin_executable",
            Value::Text(new.fingerprint().to_string()),
        ),
        ("scope_use", encode_use(&ScopeUse::Root)),
        (
            "principal",
            Value::Text(execution_context.principal().into()),
        ),
        ("inputs", manifest.to_value(&runtime.config.limits)?),
        (
            "input_checkpoint",
            Value::Bytes(manifest.encode(&runtime.config.limits)?),
        ),
        (
            "influence_labels",
            manifest
                .influence_labels()
                .to_value(&runtime.config.limits)?,
        ),
        ("status", Value::Text("active".into())),
        ("failure", Value::Null),
        ("precondition", Value::Bool(true)),
        ("admitted_at_ms", Value::Integer(now)),
        ("deadline_ms", Value::Integer(deadline)),
        ("effective_limits", field(&usage.payload, "limits")?.clone()),
        ("outputs", Value::Null),
        ("children_instantiated", Value::Bool(true)),
        (
            "run_context",
            field(&old_scope.payload, "run_context")?.clone(),
        ),
        ("extension_number", Value::Integer(generation)),
    ])?;
    let mut writes = vec![
        insert(RecordFamily::Scope, id, root_payload, false),
        update(
            &old_scope,
            replace(
                &old_scope.payload,
                [("parent_invocation", Value::Text(mount.to_string()))],
            )?,
            false,
        ),
        host::receipt(runtime, command, response.clone())?,
    ];
    for node in &root.fields().nodes {
        let invocation =
            InvocationId::new(id, node.id(), &runtime.config.limits).map_err(|_| err())?;
        let mut payload = node_payload(runtime, id, &new, node, manifest.influence_labels())?;
        if node.id() == preserved.id() {
            payload = replace(
                &payload,
                [
                    ("status", Value::Text("active".into())),
                    ("reason", Value::Text("mounted_scope".into())),
                    ("inputs", field(&old_scope.payload, "inputs")?.clone()),
                    ("input_checkpoint", Value::Bytes(checkpoint.clone())),
                    ("input_hash", Value::Text(original.input_hash().to_string())),
                    ("child_scope", Value::Text(header.root_scope.to_string())),
                    (
                        "admitted_at_ms",
                        field(&old_scope.payload, "admitted_at_ms")?.clone(),
                    ),
                    (
                        "deadline_ms",
                        field(&old_scope.payload, "deadline_ms")?.clone(),
                    ),
                    (
                        "effective_limits",
                        field(&old_scope.payload, "effective_limits")?.clone(),
                    ),
                    (
                        "influence_labels",
                        field(&old_scope.payload, "influence_labels")?.clone(),
                    ),
                ],
            )?;
        } else {
            let Operation::Scope(definition) = node.fields().operation else {
                return Err(invalid());
            };
            let sibling =
                ScopeInstanceId::child(invocation, &runtime.config.limits).map_err(|_| err())?;
            writes.push(insert(
                RecordFamily::Scope,
                sibling,
                object([
                    ("record_version", Value::Text("0.1".into())),
                    ("definition_digest", Value::Text(definition.to_string())),
                    (
                        "origin_executable",
                        Value::Text(new.fingerprint().to_string()),
                    ),
                    (
                        "scope_use",
                        encode_use(&ScopeUse::Node {
                            scope: new.document().fields().root_scope,
                            node: node.id().clone(),
                        }),
                    ),
                    (
                        "principal",
                        Value::Text(execution_context.principal().into()),
                    ),
                    ("status", Value::Text("pending".into())),
                    ("inputs", Value::Null),
                    ("input_checkpoint", Value::Null),
                    ("outputs", Value::Null),
                    ("failure", Value::Null),
                    ("admitted_at_ms", Value::Null),
                    ("deadline_ms", Value::Null),
                    ("effective_limits", Value::Null),
                    (
                        "influence_labels",
                        manifest
                            .influence_labels()
                            .to_value(&runtime.config.limits)?,
                    ),
                    ("parent_invocation", Value::Text(invocation.to_string())),
                    (
                        "run_context",
                        field(&old_scope.payload, "run_context")?.clone(),
                    ),
                ])?,
                false,
            ));
        }
        writes.push(insert(RecordFamily::Invocation, invocation, payload, false));
    }
    writes.extend(ancestry_usage(runtime, lease, &old_scope, &usage, id, mount).await?);
    for artifact in artifacts {
        runtime.store.stage_artifact(&artifact).await?;
        let metadata = artifact.metadata_at(now)?;
        writes.push(insert(
            RecordFamily::Artifact,
            metadata.artifact_id(),
            metadata.to_value(&runtime.config.limits)?,
            true,
        ));
    }
    if let Some(close) = &request.close_wait {
        if close.idempotency_key.is_empty() {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let digest = host_request_digest(
            command.context.principal(),
            "submit_reply",
            &close.wait_id.to_string(),
            &close.value,
            &runtime.config.limits,
        )
        .map_err(|_| invalid())?;
        let reply = Command {
            run: lease.run_id().clone(),
            context: command.context.clone(),
            method: "submit_reply".into(),
            target: close.wait_id.to_string(),
            key: close.idempotency_key.clone(),
            digest,
            operation: HostOperation::Reply {
                wait: close.wait_id,
                value: close.value.clone(),
            },
        };
        let (_, reply_writes) =
            host::reply_writes(runtime, lease, &reply, close.wait_id, &close.value).await?;
        writes.extend(reply_writes);
    }
    writes.push(insert(
        RecordFamily::Extension,
        generation,
        object([
            (
                "preserved_scope",
                Value::Text(header.root_scope.to_string()),
            ),
            ("preserved_wrapper", Value::Text(mount.to_string())),
            ("new_root", Value::Text(id.to_string())),
            (
                "previous_fingerprint",
                Value::Text(header.fingerprint.to_string()),
            ),
            (
                "active_fingerprint",
                Value::Text(new.fingerprint().to_string()),
            ),
        ])?,
        true,
    ));
    let committed_at = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    if committed_at >= deadline {
        return Err(RuntimeError::failure(FailureCode::Timeout));
    }
    runtime
        .store
        .commit(
            lease,
            Transition {
                context: TransitionContext {
                    command_id: format!("extend:{}", command.digest),
                    actor: command.context.principal().into(),
                    causation: "extend_run".into(),
                    timestamp_ms: committed_at,
                },
                writes,
                next_status: None,
                root: Some(RootReplacement {
                    fingerprint: new.fingerprint(),
                    root_scope: id,
                }),
                expected_fingerprint: Some(request.expected_fingerprint),
            },
        )
        .await
        .map_err(|e| {
            if matches!(e, crate::store::StoreError::Conflict) {
                RuntimeError::failure(FailureCode::ExtensionConflict)
            } else {
                e.into()
            }
        })?;
    Ok(response)
}
fn same_entry(a: &ManifestEntry, b: &ManifestEntry) -> bool {
    match (a, b) {
        (ManifestEntry::Absent, ManifestEntry::Absent) => true,
        (
            ManifestEntry::Value {
                content_digest: a,
                security_labels: la,
                ..
            },
            ManifestEntry::Value {
                content_digest: b,
                security_labels: lb,
                ..
            },
        ) => a == b && la == lb,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn inputs(
    runtime: &Runtime,
    context: &HostContext,
    run: &RunId,
    generation: u64,
    executable: &crate::VerifiedExecutable<'_>,
    root: &Scope,
    supplied: &BTreeMap<Identifier, InputValue>,
    inherited: &BTreeMap<Identifier, ManifestEntry>,
    original: &InputManifest,
) -> Result<(InputManifest, Vec<PreparedArtifact>), RuntimeError> {
    if supplied
        .keys()
        .any(|k| root.fields().inputs.get(k.as_str()).is_none())
    {
        return Err(RuntimeError::failure(FailureCode::Input));
    }
    let mut entries = Vec::new();
    let mut artifacts = Vec::new();
    let mut protection = vec![original.influence_labels().clone()];
    let mut values_budget = RecordAccounting::new(&runtime.config.limits)?;
    values_budget
        .collection(root.fields().inputs.len(), 0)
        .map_err(input_limit)?;
    for (name, port) in root.fields().inputs.iter() {
        let value = match supplied.get(name) {
            None => {
                if let Some(entry) = inherited.get(name) {
                    let mut inherited_value = None;
                    if let ManifestEntry::Value { artifact_id, .. } = entry {
                        let accepted = runtime.read_artifact(context, *artifact_id).await?;
                        values_budget
                            .value(&accepted.value, 1)
                            .map_err(input_limit)?;
                        crate::validate_typed_value(
                            &accepted.value,
                            port.value_type(),
                            Some(executable.schemas()),
                            &runtime.config.limits,
                            &executable.policy().fields().evaluator_limits,
                        )
                        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
                        inherited_value = Some(accepted.value);
                    }
                    let labels = runtime.policy.input_labels(
                        context,
                        executable.fingerprint(),
                        name,
                        inherited_value.as_ref(),
                    )?;
                    if labels.union(original.influence_labels(), &runtime.config.limits)?
                        != *original.influence_labels()
                    {
                        return Err(invalid());
                    }
                    protection.push(labels);
                    entries.push((name.clone(), entry.clone()));
                    continue;
                } else if port.required() {
                    return Err(RuntimeError::failure(FailureCode::Input));
                } else {
                    None
                }
            }
            Some(InputValue::Inline(value)) => Some((value.clone(), None)),
            Some(InputValue::Artifact(id)) => {
                let accepted = runtime.read_artifact(context, *id).await?;
                Some((accepted.value, Some(accepted.metadata)))
            }
        };
        let labels = runtime.policy.input_labels(
            context,
            executable.fingerprint(),
            name,
            value.as_ref().map(|(v, _)| v),
        )?;
        if inherited.contains_key(name)
            && labels.union(original.influence_labels(), &runtime.config.limits)?
                != *original.influence_labels()
        {
            return Err(invalid());
        }
        protection.push(labels.clone());
        let entry = if let Some((value, metadata)) = value {
            values_budget.value(&value, 1).map_err(input_limit)?;
            crate::validate_typed_value(
                &value,
                port.value_type(),
                Some(executable.schemas()),
                &runtime.config.limits,
                &executable.policy().fields().evaluator_limits,
            )
            .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
            if let Some(metadata) = metadata {
                ManifestEntry::Value {
                    artifact_id: metadata.artifact_id(),
                    content_digest: metadata.content_digest(),
                    security_labels: metadata.security_labels().clone(),
                }
            } else {
                let artifact = PreparedArtifact::new(
                    executable,
                    ArtifactInput {
                        producer: ArtifactProducer::RunInput {
                            run_id: run.clone(),
                            extension_number: generation,
                            port: name.clone(),
                        },
                        value_type: port.value_type(),
                        value: &value,
                        input_artifact_ids: vec![],
                        security_labels: labels,
                        accepted_at: runtime.clock.now_ms()?,
                    },
                    &runtime.config.limits,
                )?;
                let metadata = artifact.metadata();
                let entry = ManifestEntry::Value {
                    artifact_id: metadata.artifact_id(),
                    content_digest: metadata.content_digest(),
                    security_labels: metadata.security_labels().clone(),
                };
                artifacts.push(artifact);
                entry
            }
        } else {
            ManifestEntry::Absent
        };
        entries.push((name.clone(), entry));
    }
    let labels = SecurityLabels::combine(protection.iter(), &runtime.config.limits)?;
    Ok((
        InputManifest::freeze(
            &root.fields().inputs,
            entries,
            labels,
            &runtime.config.limits,
        )?,
        artifacts,
    ))
}
fn node_payload(
    runtime: &Runtime,
    scope: ScopeInstanceId,
    executable: &crate::VerifiedExecutable<'_>,
    node: &Node,
    labels: &SecurityLabels,
) -> Result<Value, RuntimeError> {
    object([
        ("record_version", Value::Text("0.1".into())),
        ("scope_instance_id", Value::Text(scope.to_string())),
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
            Value::Text(executable.fingerprint().to_string()),
        ),
        ("status", Value::Text("pending".into())),
        ("reason", Value::Text("guard".into())),
        ("inputs", Value::Null),
        ("input_checkpoint", Value::Null),
        ("input_hash", Value::Null),
        ("outputs", Value::Null),
        ("failure", Value::Null),
        ("influence_labels", labels.to_value(&runtime.config.limits)?),
        ("admitted_at_ms", Value::Null),
        ("deadline_ms", Value::Null),
        ("effective_limits", Value::Null),
        ("child_scope", Value::Null),
    ])
}
async fn ancestry_usage(
    runtime: &Runtime,
    lease: &RunLease,
    old_scope: &StoredRecord,
    run_usage: &StoredRecord,
    root: ScopeInstanceId,
    mount: InvocationId,
) -> Result<Vec<RecordWrite>, RuntimeError> {
    let root_key = format!("scope:{root}");
    let mount_key = format!("node:{mount}");
    let old_usage = match runtime
        .store
        .record(
            lease.run_id(),
            &key(RecordFamily::Usage, format!("scope:{}", old_scope.key.id)),
        )
        .await
    {
        Ok(record) => record.payload,
        Err(crate::store::StoreError::Missing) => object([
            (
                "limits",
                field(&old_scope.payload, "effective_limits")?.clone(),
            ),
            ("calls_spent", Value::Integer(0)),
            ("concurrency_reserved", Value::Integer(0)),
        ])?,
        Err(error) => return Err(error.into()),
    };
    let mut writes = vec![
        insert(
            RecordFamily::Usage,
            &root_key,
            run_usage.payload.clone(),
            false,
        ),
        insert(RecordFamily::Usage, &mount_key, old_usage, false),
    ];
    // Outstanding effects are already included in both new aggregate counters.
    // Extend their release inventory atomically so later definitive replies release
    // the adopted capacity as well, without refunding any spent calls.
    for reservation in super::control::all(runtime, lease, RecordFamily::Reservation).await? {
        if reservation.final_record {
            continue;
        }
        let Value::Array(mut keys) = field(&reservation.payload, "usage_keys")?.clone() else {
            return Err(err());
        };
        keys.extend([
            Value::Text(root_key.clone()),
            Value::Text(mount_key.clone()),
        ]);
        writes.push(update(
            &reservation,
            replace(&reservation.payload, [("usage_keys", Value::Array(keys))])?,
            false,
        ));
    }
    Ok(writes)
}
