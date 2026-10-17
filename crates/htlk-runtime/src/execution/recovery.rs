//! Re-admit exact origins, verify checkpoints/producer links and resume saved states.
use super::{
    control,
    model::*,
    step::{self, Progress},
};
use crate::{
    Runtime, RuntimeError,
    artifact::{ArtifactMetadata, ArtifactProducer},
    failure::FailureCode,
    identity::{ArtifactId, AttemptId, InvocationId, ScopeInstanceId, WaitId},
    records::{InputManifest, ManifestEntry},
    store::{RecordFamily, RunLease, StoreError, StoredRecord},
};
use htlk_analyzer::ScopeUse;
use htlk_executable::{ExecutionLimits, JsonDocument, Operation, PortTable, cbor::Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) async fn drive(
    runtime: Arc<Runtime>,
    lease: RunLease,
    recovering: bool,
) -> Result<Progress, RuntimeError> {
    let result = if recovering {
        validate(&runtime, &lease).await.map(|()| Progress::Changed)
    } else {
        step::tick(runtime.clone(), lease.clone()).await
    };
    match result {
        Err(
            error @ RuntimeError::Storage(
                StoreError::Fenced
                | StoreError::Conflict
                | StoreError::Terminal
                | StoreError::Database
                | StoreError::Closed,
            ),
        ) => Err(error),
        Err(error) => {
            control::stop(&runtime, &lease, None, None, error.code(), false, vec![]).await?;
            Ok(Progress::Terminal)
        }
        result => result,
    }
}
fn corrupt() -> RuntimeError {
    RuntimeError::failure(FailureCode::Digest)
}
async fn manifest(
    runtime: &Runtime,
    executable: &crate::VerifiedExecutable<'_>,
    record: &StoredRecord,
    ports: &PortTable,
    ids: &mut BTreeSet<ArtifactId>,
) -> Result<(), RuntimeError> {
    let Value::Bytes(bytes) = field(&record.payload, "input_checkpoint")? else {
        return Err(corrupt());
    };
    let checkpoint =
        InputManifest::decode(bytes, ports, &runtime.config.limits).map_err(|_| corrupt())?;
    if checkpoint.to_value(&runtime.config.limits)? != *field(&record.payload, "inputs")? {
        return Err(corrupt());
    }
    if let Some(Value::Text(hash)) = map(&record.payload)?.get("input_hash")
        && *hash != checkpoint.input_hash().to_string()
    {
        return Err(corrupt());
    }
    for (name, entry) in checkpoint.entries() {
        if let ManifestEntry::Value {
            artifact_id,
            content_digest,
            security_labels,
        } = entry
        {
            let metadata = runtime.store.artifact_metadata(*artifact_id).await?;
            if metadata.content_digest() != *content_digest
                || metadata.security_labels() != security_labels
            {
                return Err(corrupt());
            }
            let bytes = runtime
                .store
                .artifact_bytes(*artifact_id, metadata.storage_reference())
                .await?;
            let value = metadata.verify_value(&bytes, &runtime.config.limits)?;
            crate::validate_typed_value(
                &value,
                ports.get(name.as_str()).ok_or_else(corrupt)?.value_type(),
                Some(executable.schemas()),
                &runtime.config.limits,
                &executable.policy().fields().evaluator_limits,
            )
            .map_err(|_| corrupt())?;
            ids.insert(*artifact_id);
        }
    }
    Ok(())
}
fn collect(value: &Value, ids: &mut BTreeSet<ArtifactId>) -> Result<(), RuntimeError> {
    let Value::Map(fields) = value else {
        return Ok(());
    };
    for name in ["inputs", "outputs", "carried", "next"] {
        if let Some(Value::Map(manifest)) = fields.get(name) {
            for (_, entry) in manifest.iter() {
                if let Value::Map(entry) = entry
                    && let Some(Value::Text(id)) = entry.get("artifact_id")
                {
                    ids.insert(id.parse().map_err(|_| corrupt())?);
                }
            }
        }
    }
    Ok(())
}
async fn validate(runtime: &Arc<Runtime>, lease: &RunLease) -> Result<(), RuntimeError> {
    let header = runtime.store.run_header(lease.run_id()).await?;
    if header.status.is_terminal() {
        return Ok(());
    }
    runtime.admitted(header.fingerprint).await?;
    let scopes = control::all(runtime, lease, RecordFamily::Scope).await?;
    let root = scopes
        .iter()
        .find(|s| s.key.id == header.root_scope.to_string())
        .ok_or_else(corrupt)?;
    if root.final_record || string(&root.payload, "status")? != "active" {
        return Err(corrupt());
    }
    let mut definitions = BTreeMap::new();
    let mut artifacts = BTreeSet::new();
    for scope in &scopes {
        let id: ScopeInstanceId = scope.key.id.parse().map_err(|_| corrupt())?;
        let executable = runtime
            .admitted(digest(&scope.payload, "origin_executable")?)
            .await?;
        let definition = digest(&scope.payload, "definition_digest")?;
        let site = use_site(&scope.payload)?;
        let expected = match &site {
            ScopeUse::Root => executable.document().fields().root_scope,
            ScopeUse::Node { scope, node } => {
                let scope = executable
                    .document()
                    .fields()
                    .scopes
                    .get(scope)
                    .ok_or_else(corrupt)?;
                match scope
                    .fields()
                    .nodes
                    .iter()
                    .find(|n| n.id() == node)
                    .ok_or_else(corrupt)?
                    .fields()
                    .operation
                {
                    Operation::Scope(d) | Operation::Loop { body: d, .. } => d,
                    _ => return Err(corrupt()),
                }
            }
        };
        if expected != definition || executable.graphs().plan(&site).is_none() {
            return Err(corrupt());
        }
        let target = executable
            .document()
            .fields()
            .scopes
            .get(&definition)
            .ok_or_else(corrupt)?;
        let status = string(&scope.payload, "status")?;
        if !matches!(
            status.as_str(),
            "pending" | "active" | "succeeded" | "failed" | "cancelled"
        ) || scope.final_record
            != matches!(status.as_str(), "succeeded" | "failed" | "cancelled")
        {
            return Err(corrupt());
        }
        if matches!(field(&scope.payload, "input_checkpoint")?, Value::Bytes(_)) {
            manifest(
                runtime,
                &executable,
                scope,
                &target.fields().inputs,
                &mut artifacts,
            )
            .await?;
        } else if !matches!(status.as_str(), "pending" | "failed" | "cancelled")
            || !matches!(field(&scope.payload, "admitted_at_ms")?, Value::Null)
        {
            return Err(corrupt());
        }
        labels(&scope.payload, &runtime.config.limits)?;
        collect(&scope.payload, &mut artifacts)?;
        definitions.insert(id, (executable, definition));
    }
    let mut invocations = BTreeMap::new();
    for node in control::all(runtime, lease, RecordFamily::Invocation).await? {
        let scope: ScopeInstanceId = string(&node.payload, "scope_instance_id")?
            .parse()
            .map_err(|_| corrupt())?;
        let (executable, definition) = definitions.get(&scope).ok_or_else(corrupt)?;
        let local = string(&node.payload, "local_id")?;
        let definition = executable.document().fields().scopes[definition]
            .fields()
            .nodes
            .iter()
            .find(|n| n.id().as_str() == local)
            .ok_or_else(corrupt)?;
        let id = InvocationId::new(scope, definition.id(), &runtime.config.limits)
            .map_err(|_| corrupt())?;
        if id.to_string() != node.key.id
            || digest(&node.payload, "origin_executable")? != executable.fingerprint()
            || digest(&node.payload, "definition_digest")?
                != definition
                    .digest(&runtime.config.limits)
                    .map_err(|_| corrupt())?
        {
            return Err(corrupt());
        }
        let status = string(&node.payload, "status")?;
        if !matches!(
            status.as_str(),
            "pending" | "active" | "succeeded" | "failed" | "cancelled" | "skipped"
        ) || node.final_record
            != matches!(
                status.as_str(),
                "succeeded" | "failed" | "cancelled" | "skipped"
            )
        {
            return Err(corrupt());
        }
        if !matches!(field(&node.payload, "input_checkpoint")?, Value::Null) {
            manifest(
                runtime,
                executable,
                &node,
                &definition.fields().inputs,
                &mut artifacts,
            )
            .await?;
        } else if matches!(status.as_str(), "active" | "succeeded") {
            return Err(corrupt());
        }
        labels(&node.payload, &runtime.config.limits)?;
        collect(&node.payload, &mut artifacts)?;
        if let Some(Value::Text(child)) = map(&node.payload)?.get("child_scope")
            && !definitions.contains_key(&child.parse().map_err(|_| corrupt())?)
        {
            return Err(corrupt());
        }
        invocations.insert(id, node);
    }
    for artifact in control::all(runtime, lease, RecordFamily::Artifact).await? {
        if !artifact.final_record {
            return Err(corrupt());
        }
        let metadata = ArtifactMetadata::from_value(&artifact.payload, &runtime.config.limits)
            .map_err(|_| corrupt())?;
        let (executable, ports, port) = match metadata.producer() {
            ArtifactProducer::RunInput {
                run_id,
                extension_number,
                port,
            } => {
                if run_id != lease.run_id() {
                    return Err(corrupt());
                }
                let scope = if *extension_number == 0 {
                    ScopeInstanceId::root(run_id, &runtime.config.limits)
                } else {
                    ScopeInstanceId::extended_root(
                        run_id,
                        *extension_number,
                        &runtime.config.limits,
                    )
                }
                .map_err(|_| corrupt())?;
                let (executable, definition) = definitions.get(&scope).ok_or_else(corrupt)?;
                (
                    executable,
                    &executable.document().fields().scopes[definition]
                        .fields()
                        .inputs,
                    port,
                )
            }
            ArtifactProducer::NodeOutput {
                invocation_id,
                port,
            } => {
                let node = invocations.get(invocation_id).ok_or_else(corrupt)?;
                if metadata.security_labels() != &labels(&node.payload, &runtime.config.limits)? {
                    return Err(corrupt());
                }
                let scope = string(&node.payload, "scope_instance_id")?
                    .parse()
                    .map_err(|_| corrupt())?;
                let (executable, definition) = definitions.get(&scope).ok_or_else(corrupt)?;
                let local = string(&node.payload, "local_id")?;
                let node = executable.document().fields().scopes[definition]
                    .fields()
                    .nodes
                    .iter()
                    .find(|n| n.id().as_str() == local)
                    .ok_or_else(corrupt)?;
                (executable, &node.fields().outputs, port)
            }
        };
        if metadata.artifact_id().to_string() != artifact.key.id
            || metadata.validation_profile() != executable.fingerprint()
            || metadata.value_type() != ports.get(port.as_str()).ok_or_else(corrupt)?.value_type()
        {
            return Err(corrupt());
        }
    }
    for attempt in control::all(runtime, lease, RecordFamily::Attempt).await? {
        let invocation: InvocationId = string(&attempt.payload, "invocation_id")?
            .parse()
            .map_err(|_| corrupt())?;
        let node = invocations.get(&invocation).ok_or_else(corrupt)?;
        let scope: ScopeInstanceId = string(&node.payload, "scope_instance_id")?
            .parse()
            .map_err(|_| corrupt())?;
        let (executable, scope) = definitions.get(&scope).ok_or_else(corrupt)?;
        let local = string(&node.payload, "local_id")?;
        let definition = executable.document().fields().scopes[scope]
            .fields()
            .nodes
            .iter()
            .find(|n| n.id().as_str() == local)
            .ok_or_else(corrupt)?;
        let Operation::Mcp { binding, .. } = definition.fields().operation else {
            return Err(corrupt());
        };
        let binding = executable
            .document()
            .fields()
            .bindings
            .get(&binding)
            .ok_or_else(corrupt)?;
        let args = if let Some(value) = map(field(&node.payload, "inputs")?)?.get("arguments") {
            let id = entry(value)?.ok_or_else(corrupt)?;
            let metadata = runtime.store.artifact_metadata(id).await?;
            let bytes = runtime
                .store
                .artifact_bytes(id, metadata.storage_reference())
                .await?;
            metadata.verify_value(&bytes, &runtime.config.limits)?
        } else {
            object([])?
        };
        let (method, params, uri) =
            super::mcp::encode_request(binding, args, &runtime.config.limits)?;
        if string(&attempt.payload, "method")? != method
            || *field(&attempt.payload, "params")? != params
            || *field(&attempt.payload, "requested_uri")? != uri.map_or(Value::Null, Value::Text)
        {
            return Err(corrupt());
        }
        let effective = ExecutionLimits::from_value(
            field(&node.payload, "effective_limits")?,
            &runtime.config.limits,
        )
        .map_err(|_| corrupt())?;
        let timeout = number(&attempt.payload, "timeout_ms")?;
        let owner = number(&attempt.payload, "owner_fence")?;
        if timeout <= 0
            || timeout as u64 > effective.attempt_timeout_ms().ok_or_else(corrupt)?
            || owner <= 0
            || owner > header.fence
        {
            return Err(corrupt());
        }
        let number = number(&attempt.payload, "number")?;
        let id = AttemptId::new(
            invocation,
            u64::try_from(number).map_err(|_| corrupt())?,
            &runtime.config.limits,
        )
        .map_err(|_| corrupt())?;
        if id.to_string() != attempt.key.id {
            return Err(corrupt());
        }
        let Value::Bytes(encoded) = field(&attempt.payload, "encoded_request")? else {
            return Err(corrupt());
        };
        if JsonDocument::from_value(field(&attempt.payload, "params")?, &runtime.config.limits)
            .map_err(|_| corrupt())?
            .as_bytes()
            != encoded
        {
            return Err(corrupt());
        }
        match string(&attempt.payload, "dispatch_state")?.as_str() {
            "prepared" | "possibly_sent" if !attempt.final_record => {}
            "response_recorded" if attempt.final_record => {
                let response = runtime
                    .store
                    .record(
                        lease.run_id(),
                        &key(RecordFamily::Response, &attempt.key.id),
                    )
                    .await?;
                if !response.final_record {
                    return Err(corrupt());
                }
                labels(&response.payload, &runtime.config.limits)?;
            }
            _ => return Err(corrupt()),
        }
    }
    for wait in control::all(runtime, lease, RecordFamily::Wait).await? {
        let invocation: InvocationId = string(&wait.payload, "invocation_id")?
            .parse()
            .map_err(|_| corrupt())?;
        let node = invocations.get(&invocation).ok_or_else(corrupt)?;
        let generation = number(&wait.payload, "generation")?;
        let expected = match string(&wait.payload, "purpose")?.as_str() {
            "input" if generation == 0 => WaitId::input(invocation, &runtime.config.limits),
            "approval" => WaitId::approval(
                invocation,
                u64::try_from(generation).map_err(|_| corrupt())?,
                &runtime.config.limits,
            ),
            _ => return Err(corrupt()),
        }
        .map_err(|_| corrupt())?;
        if expected.to_string() != wait.key.id
            || number(&wait.payload, "expires_at")? > number(&node.payload, "deadline_ms")?
        {
            return Err(corrupt());
        }
        let state = string(&wait.payload, "state")?;
        if !matches!(
            state.as_str(),
            "open" | "accepted" | "expired" | "cancelled"
        ) || wait.final_record != (state != "open")
        {
            return Err(corrupt());
        }
        if string(&wait.payload, "purpose")? == "input" && state == "accepted" {
            let Value::Bytes(raw) = field(&wait.payload, "reply_value")? else {
                return Err(corrupt());
            };
            let value = htlk_executable::cbor::decode(raw, &runtime.config.limits)
                .map_err(|_| corrupt())?;
            let ty = htlk_executable::ValueType::from_value(
                field(&wait.payload, "response_type")?,
                htlk_executable::TypeContext::Value,
                &runtime.config.limits,
            )
            .map_err(|_| corrupt())?;
            let digest = crate::identity::ValueDigest::new(&ty, &value, &runtime.config.limits)
                .map_err(|_| corrupt())?;
            if string(&wait.payload, "accepted_reply_digest")? != digest.to_string() {
                return Err(corrupt());
            }
        }
        if string(&wait.payload, "purpose")? == "approval" {
            let request = field(&wait.payload, "approval_request")?;
            let digest =
                crate::identity::hash("approval_request", &[request], &runtime.config.limits)
                    .map_err(|_| corrupt())?;
            if string(&wait.payload, "request_digest")? != digest.to_string()
                || field(request, "input_hash")? != field(&node.payload, "input_hash")?
                || string(request, "invocation_id")? != invocation.to_string()
            {
                return Err(corrupt());
            }
        }
        labels(&wait.payload, &runtime.config.limits)?;
    }
    let mut outstanding = BTreeMap::<String, i64>::new();
    for reservation in control::all(runtime, lease, RecordFamily::Reservation).await? {
        if reservation.final_record {
            continue;
        }
        let Value::Array(keys) = field(&reservation.payload, "usage_keys")? else {
            return Err(corrupt());
        };
        let mut seen = BTreeSet::new();
        for id in keys {
            let id = text(id)?.to_string();
            if !seen.insert(id.clone()) {
                return Err(corrupt());
            }
            *outstanding.entry(id).or_default() += 1;
        }
    }
    for usage in control::all(runtime, lease, RecordFamily::Usage).await? {
        ExecutionLimits::from_value(field(&usage.payload, "limits")?, &runtime.config.limits)
            .map_err(|_| corrupt())?;
        let reserved = number(&usage.payload, "concurrency_reserved")?;
        if reserved != outstanding.remove(&usage.key.id).unwrap_or(0)
            || number(&usage.payload, "calls_spent")? < reserved
        {
            return Err(corrupt());
        }
    }
    if !outstanding.is_empty() {
        return Err(corrupt());
    }
    let now = runtime
        .store
        .coordinator_time(runtime.clock.now_ms()?)
        .await?;
    runtime
        .store
        .recover_artifacts(lease, artifacts.into_iter().collect(), now)
        .await?;
    Ok(())
}
