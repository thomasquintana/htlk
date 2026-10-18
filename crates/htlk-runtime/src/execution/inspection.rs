//! Authorized observations derived from authoritative checkpoints.
use super::model::*;
use crate::{
    Authorization, HostContext, Runtime, RuntimeError,
    failure::{FailureCause, FailureCode, RuntimeFailure},
    identity::{AttemptId, InvocationId, RunId, ScopeInstanceId, WaitId},
    records::{InputManifest, RunStatus, SecurityLabels},
    store::{JournalEvent, RecordFamily, RecordKey, RunHeader, StoredRecord},
};
use htlk_executable::{
    CanonicalDocument, ExecutionLimits, JsonPointer, Operation,
    cbor::{self, Value},
    digest::Digest,
};
/// Host-visible typed input/approval request; evidence bytes require separate access.
#[derive(Clone, Debug)]
pub struct WaitView {
    /// Stable owning invocation, unchanged through scope mounting.
    pub invocation_id: InvocationId,
    /// Original runtime scope instance.
    pub scope_instance_id: ScopeInstanceId,
    /// Input generation zero or exact-action approval generation.
    pub generation: u64,
    /// Trusted input-reply routing policy frozen when the request opened.
    pub authorization_policy: Option<Value>,
    /// Exact approval request identity, if this is an approval wait.
    pub request_digest: Option<Digest>,
    /// Protected exact-action request metadata, after authorization.
    pub approval_request: Option<Value>,
    /// Stable addressed request.
    pub wait_id: WaitId,
    /// Input or approval purpose.
    pub purpose: String,
    /// Host routing topic.
    pub topic: String,
    /// Request artifact, separately authorized on read.
    pub request_artifact_id: Option<crate::identity::ArtifactId>,
    /// Absolute deadline.
    pub expires_at: i64,
    /// Open/terminal disposition.
    pub state: String,
}
/// Durable run observation with derived blocked status.
#[derive(Clone, Debug)]
pub struct RunView {
    /// Persisted run-wide guard choices and protected causes.
    pub guards: Vec<DecisionView>,
    /// Node/public-output/loop-next binding decisions.
    pub bindings: Vec<DecisionView>,
    /// Fixed graph-visible root error, independent of viewer-specific source data.
    pub failure: Option<RuntimeFailure>,
    /// Conservative protection of this complete observation.
    pub labels: SecurityLabels,
    /// Runtime instance hierarchy, including preserved roots and loop iterations.
    pub scopes: Vec<ScopeView>,
    /// Active graph/root/ownership and terminal result.
    pub header: RunHeader,
    /// Running with only external prerequisites.
    pub blocked: bool,
    /// Active invocation reasons.
    pub reasons: Vec<String>,
    /// Frozen root input manifest.
    pub inputs: Value,
    /// Committed boundary only.
    pub outputs: Option<Value>,
    /// Open authorized requests.
    pub waits: Vec<WaitView>,
    /// Authoritative aggregate usage.
    pub usage: Value,
    /// Additive installation history.
    pub extensions: Vec<Value>,
}
/// Node occurrence observation. Returned IDs grant no value access.
#[derive(Clone, Debug)]
pub struct NodeView {
    /// Owning runtime instance and original local name.
    pub scope_instance_id: ScopeInstanceId,
    /// Original source-local occurrence name.
    pub local_id: String,
    /// Canonical node definition location in its origin executable.
    pub canonical_path: JsonPointer,
    /// Protected complete frozen input checkpoint, if admitted.
    pub input_manifest: Option<InputManifest>,
    /// Conservative value/control/evidence protection.
    pub labels: SecurityLabels,
    /// Effective admitted ceilings and original absolute deadline.
    pub limits: Option<ExecutionLimits>,
    /// Absolute authoritative admission deadline.
    pub deadline_ms: Option<i64>,
    /// Mounted/ordinary/current-iteration scope, if opened.
    pub child_scope: Option<ScopeInstanceId>,
    /// Current zero-based loop iteration, if any.
    pub iteration: Option<u64>,
    /// Committed next manifest for loop feedback, if any.
    pub next: Option<Value>,
    /// Current iteration's carried manifest, if any.
    pub carried: Option<Value>,
    /// Persisted candidate/node guard evidence, after protection checks.
    pub guards: Vec<DecisionView>,
    /// Frozen input binding choices, including source/guard failures.
    pub bindings: Vec<DecisionView>,
    /// Delivery-aware attempt metadata; raw unvalidated values are not exposed.
    pub attempts: Vec<AttemptView>,
    /// Immutable input/approval request history for this invocation.
    pub waits: Vec<WaitView>,
    /// Actual protected dependency/outcome links retained by the reducer.
    pub causes: Vec<FailureCause>,
    /// Runtime occurrence identity.
    pub invocation_id: InvocationId,
    /// Complete immutable node record identity.
    pub definition_digest: Digest,
    /// Origin executable preserved through extension.
    pub origin_executable: Digest,
    /// Terminal/active/pending status.
    pub status: String,
    /// Active reason if any.
    pub reason: Option<String>,
    /// Frozen inputs if admitted.
    pub inputs: Option<Value>,
    /// Recorded input hash if admitted.
    pub input_hash: Option<Digest>,
    /// Accepted outputs only.
    pub outputs: Option<Value>,
    /// Stored graph-visible failure.
    pub failure: Option<RuntimeFailure>,
}
/// Immutable public graph definitions and admitted use hierarchy.
pub struct GraphView {
    /// Native admission is currently available, independently of historical display.
    pub native_available: bool,
    /// Bounded authorized source metadata page.
    pub sources: crate::source::SourcePage,
    /// Semantic registration identity.
    pub fingerprint: Digest,
    /// Canonical portable graph document.
    pub document: CanonicalDocument,
    /// Root/task/loop analysis use locations.
    pub scope_uses: Vec<htlk_analyzer::ScopeUse>,
}
/// Protected runtime scope-instance metadata; returned artifact IDs grant no access.
#[derive(Clone, Debug)]
pub struct ScopeView {
    /// Stable scope instance identity.
    pub scope_instance_id: ScopeInstanceId,
    /// Original executable defining this scope's semantics.
    pub origin_executable: Digest,
    /// Original reusable definition digest.
    pub definition_digest: Digest,
    /// Original analysis use site, even after mounting.
    pub scope_use: htlk_analyzer::ScopeUse,
    /// Current containing invocation, if any.
    pub parent_invocation: Option<InvocationId>,
    /// Pending/active/terminal state.
    pub status: String,
    /// Absolute deadline, excluding unadmitted pending scopes.
    pub deadline_ms: Option<i64>,
    /// Loop body iteration index, if any.
    pub iteration: Option<u64>,
    /// Fixed stored failure, if any.
    pub failure: Option<RuntimeFailure>,
}
/// One protected guard/binding decision. Values contain metadata, not raw artifacts.
#[derive(Clone, Debug)]
pub struct DecisionView {
    /// Stable private decision ID.
    pub id: String,
    /// Canonical persisted decision/protection/causal evidence.
    pub decision: Value,
}
/// An attempt's observable durable facts, separate from invocation lifecycle.
#[derive(Clone, Debug)]
pub struct AttemptView {
    /// Stable one-based attempt identity.
    pub attempt_id: AttemptId,
    /// One-based count within its invocation.
    pub number: u64,
    /// Prepared/possibly_sent/response_recorded checkpoint.
    pub dispatch_state: String,
    /// Adapter delivery evidence, when a proposal is recorded.
    pub delivery: Option<crate::mcp::McpDelivery>,
    /// Fixed operational classification, if present.
    pub failure_code: Option<FailureCode>,
    /// Exact pinned method.
    pub method: String,
    /// Ownership fence recorded at preparation/dispatch.
    pub owner_fence: i64,
    /// Finite attempt bound in milliseconds.
    pub timeout_ms: u64,
    /// Trusted policy version at dispatch, if sent.
    pub policy_version: Option<String>,
    /// Trusted replay permission recorded with intent.
    pub replay_authorized: Option<bool>,
}
/// Bounded audit event summary, without raw arguments, responses or private values.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// Strict per-run journal order.
    pub sequence: i64,
    /// Closed private event schema.
    pub event: JournalEvent,
    /// Authenticated/trusted writer attribution.
    pub actor: String,
    /// Persisted coordinator timestamp.
    pub timestamp_ms: i64,
    /// Fixed causal operation identity.
    pub causation: String,
    /// Changed authoritative record keys only.
    pub changed_records: Vec<RecordKey>,
}
/// One authorized history page, bounded by the store's journal byte/row allowance.
#[derive(Clone, Debug)]
pub struct HistoryPage {
    /// Committed event summaries.
    pub entries: Vec<HistoryEntry>,
    /// Exclusive sequence cursor; None ends this pass.
    pub next: Option<i64>,
}
impl Runtime {
    /// Inspects durable run state under value/control protection. No partial root
    /// output or unauthenticated wait request is exposed.
    ///
    /// # Errors
    /// Returns denial, unknown run or corrupt checkpoint.
    pub async fn inspect_run(
        &self,
        context: &HostContext,
        run: &RunId,
    ) -> Result<RunView, RuntimeError> {
        for _ in 0..8 {
            let view = self.inspect_run_snapshot(context, run).await?;
            if self.store.run_header(run).await?.sequence == view.header.sequence {
                return Ok(view);
            }
        }
        Err(RuntimeError::failure(FailureCode::Fenced))
    }
    async fn inspect_run_snapshot(
        &self,
        context: &HostContext,
        run: &RunId,
    ) -> Result<RunView, RuntimeError> {
        let header = self.store.run_header(run).await?;
        let root = self
            .store
            .record(run, &key(RecordFamily::Scope, header.root_scope))
            .await?;
        let nodes = self
            .store
            .inspection_records(run, RecordFamily::Invocation, &["status", "reason"])
            .await?;
        let protection = self.store.record_protection(run).await?;
        self.policy.authorize(
            context,
            Authorization::InspectRun {
                run_id: run,
                labels: &protection,
            },
        )?;
        let reasons = nodes
            .iter()
            .filter_map(|node| map(&node.payload).ok()?.get("reason"))
            .filter_map(|v| {
                if let Value::Text(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let mut waits = Vec::new();
        for wait in wait_records(self, run).await? {
            if string(&wait.payload, "state")? != "open" {
                continue;
            }
            let protection = labels(&wait.payload, &self.config.limits)?;
            self.policy.authorize(
                context,
                Authorization::ReadBoundary {
                    run_id: run,
                    scope_instance_id: string(&wait.payload, "scope_instance_id")?
                        .parse()
                        .map_err(|_| err())?,
                    labels: &protection,
                },
            )?;
            waits.push(WaitView {
                invocation_id: string(&wait.payload, "invocation_id")?
                    .parse()
                    .map_err(|_| err())?,
                scope_instance_id: string(&wait.payload, "scope_instance_id")?
                    .parse()
                    .map_err(|_| err())?,
                generation: u64::try_from(number(&wait.payload, "generation")?)
                    .map_err(|_| err())?,
                authorization_policy: map(&wait.payload)?.get("authorization_policy").cloned(),
                request_digest: map(&wait.payload)?.get("request_digest").and_then(|v| {
                    if let Value::Text(s) = v {
                        s.parse().ok()
                    } else {
                        None
                    }
                }),
                approval_request: map(&wait.payload)?.get("approval_request").cloned(),
                wait_id: wait.key.id.parse().map_err(|_| err())?,
                purpose: string(&wait.payload, "purpose")?,
                topic: string(&wait.payload, "topic")?,
                request_artifact_id: map(&wait.payload)?
                    .get("request_artifact_id")
                    .and_then(|v| {
                        if let Value::Text(id) = v {
                            id.parse().ok()
                        } else {
                            None
                        }
                    }),
                expires_at: number(&wait.payload, "expires_at")?,
                state: "open".into(),
            });
        }
        let blocked = header.status == RunStatus::Running
            && !waits.is_empty()
            && nodes.iter().filter(|n| !n.final_record).all(|n| {
                string(&n.payload, "status").is_ok_and(|s| s == "pending")
                    || string(&n.payload, "reason").is_ok_and(|s| {
                        matches!(
                            s.as_str(),
                            "waiting_reply"
                                | "awaiting_approval"
                                | "scope"
                                | "iteration"
                                | "retry_delay"
                        )
                    })
            });
        let outputs = if header.status == RunStatus::Succeeded
            && string(&root.payload, "status")? == "succeeded"
        {
            Some(field(&root.payload, "outputs")?.clone())
        } else {
            None
        };
        let usage = self
            .store
            .record(run, &key(RecordFamily::Usage, "run"))
            .await?
            .payload;
        let extensions = all(self, run, RecordFamily::Extension)
            .await?
            .into_iter()
            .map(|r| r.payload)
            .collect();
        Ok(RunView {
            guards: self
                .store
                .inspection_records(run, RecordFamily::Guard, &["value", "failure", "causes"])
                .await?
                .into_iter()
                .map(|r| DecisionView {
                    id: r.key.id,
                    decision: r.payload,
                })
                .collect(),
            bindings: self
                .store
                .inspection_records(
                    run,
                    RecordFamily::Binding,
                    &["state", "artifact_id", "selected_edge", "failures"],
                )
                .await?
                .into_iter()
                .map(|r| DecisionView {
                    id: r.key.id,
                    decision: r.payload,
                })
                .collect(),
            failure: optional_failure(&root.payload, &self.config.limits)?,
            labels: protection,
            scopes: self
                .store
                .inspection_records(
                    run,
                    RecordFamily::Scope,
                    &[
                        "origin_executable",
                        "definition_digest",
                        "scope_use",
                        "parent_invocation",
                        "status",
                        "deadline_ms",
                        "iteration",
                        "failure",
                    ],
                )
                .await?
                .iter()
                .map(|r| scope_view(r, &self.config.limits))
                .collect::<Result<Vec<_>, _>>()?,
            header,
            blocked,
            reasons,
            inputs: field(&root.payload, "inputs")?.clone(),
            outputs,
            waits,
            usage,
            extensions,
        })
    }
    /// Inspects a node under its immutable outcome and influence boundary.
    ///
    /// # Errors
    /// Returns denial, unknown occurrence or corrupt checkpoint.
    pub async fn inspect_node(
        &self,
        context: &HostContext,
        run: &RunId,
        id: InvocationId,
    ) -> Result<NodeView, RuntimeError> {
        for _ in 0..8 {
            let sequence = self.store.run_header(run).await?.sequence;
            let view = self.inspect_node_snapshot(context, run, id).await?;
            if self.store.run_header(run).await?.sequence == sequence {
                return Ok(view);
            }
        }
        Err(RuntimeError::failure(FailureCode::Fenced))
    }
    async fn inspect_node_snapshot(
        &self,
        context: &HostContext,
        run: &RunId,
        id: InvocationId,
    ) -> Result<NodeView, RuntimeError> {
        let record = self
            .store
            .record(run, &key(RecordFamily::Invocation, id))
            .await?;
        let scope_instance_id: ScopeInstanceId = string(&record.payload, "scope_instance_id")?
            .parse()
            .map_err(|_| err())?;
        let scope = self
            .store
            .record(run, &key(RecordFamily::Scope, scope_instance_id))
            .await?;
        let local_id = string(&record.payload, "local_id")?;
        let origin = digest(&record.payload, "origin_executable")?;
        let (_, document) = self.source_document(origin).await?;
        let definition = digest(&scope.payload, "definition_digest")?;
        let parent = document.fields().scopes.get(&definition).ok_or_else(err)?;
        let index = parent
            .fields()
            .nodes
            .iter()
            .position(|n| n.id().as_str() == local_id)
            .ok_or_else(err)?;
        let canonical_path = JsonPointer::new(
            &format!("/scopes/{definition}/nodes/{index}"),
            &self.config.limits,
        )
        .map_err(|_| err())?;
        let manifest = match field(&record.payload, "input_checkpoint")? {
            Value::Null => None,
            Value::Bytes(bytes) => Some(InputManifest::decode(
                bytes,
                &parent.fields().nodes[index].fields().inputs,
                &self.config.limits,
            )?),
            _ => return Err(err()),
        };
        let guard_ids=std::iter::once(format!("{scope_instance_id}:node:{local_id}")).chain(parent.fields().edges.iter().filter(|e|matches!(e.destination(),htlk_executable::EdgeDestination::Input{node,..}if node.as_str()==local_id)).map(|e|format!("{scope_instance_id}:edge:{}",e.id()))).collect::<std::collections::BTreeSet<_>>();
        let guards = self
            .store
            .inspection_records(run, RecordFamily::Guard, &["value", "failure", "causes"])
            .await?
            .into_iter()
            .filter(|r| guard_ids.contains(&r.key.id))
            .collect::<Vec<_>>();
        let bindings = self
            .store
            .inspection_records(
                run,
                RecordFamily::Binding,
                &["state", "artifact_id", "selected_edge", "failures"],
            )
            .await?
            .into_iter()
            .filter(|r| {
                r.key
                    .id
                    .starts_with(&format!("{scope_instance_id}:{local_id}:"))
            })
            .collect::<Vec<_>>();
        let attempts = self
            .store
            .inspection_records(
                run,
                RecordFamily::Attempt,
                &[
                    "invocation_id",
                    "number",
                    "dispatch_state",
                    "method",
                    "owner_fence",
                    "timeout_ms",
                    "policy_version",
                    "replay_authorized",
                ],
            )
            .await?
            .into_iter()
            .filter(|r| {
                map(&r.payload).ok().and_then(|m| m.get("invocation_id"))
                    == Some(&Value::Text(id.to_string()))
            })
            .collect::<Vec<_>>();
        let attempt_ids = attempts
            .iter()
            .map(|r| r.key.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let responses = self
            .store
            .inspection_records(run, RecordFamily::Response, &["delivery", "failure_code"])
            .await?
            .into_iter()
            .filter(|r| attempt_ids.contains(&r.key.id))
            .collect::<Vec<_>>();
        let waits = wait_records(self, run)
            .await?
            .into_iter()
            .filter(|r| {
                map(&r.payload).ok().and_then(|m| m.get("invocation_id"))
                    == Some(&Value::Text(id.to_string()))
            })
            .collect::<Vec<_>>();
        let child_scope = optional_text(&record.payload, "child_scope")?
            .map(|v| v.parse().map_err(|_| err()))
            .transpose()?;
        let child = if let Some(child) = child_scope {
            Some(
                self.store
                    .record(run, &key(RecordFamily::Scope, child))
                    .await?,
            )
        } else {
            None
        };
        let mut protection = labels(&record.payload, &self.config.limits)?;
        for evidence in guards
            .iter()
            .chain(bindings.iter())
            .chain(attempts.iter())
            .chain(responses.iter())
            .chain(waits.iter())
            .chain(child.iter())
        {
            if map(&evidence.payload)?.get("influence_labels").is_some() {
                protection = protection.union(
                    &labels(&evidence.payload, &self.config.limits)?,
                    &self.config.limits,
                )?;
            }
        }
        self.policy.authorize(
            context,
            Authorization::InspectRun {
                run_id: run,
                labels: &protection,
            },
        )?;
        let optional = |name| -> Result<Option<Value>, RuntimeError> {
            let value = field(&record.payload, name)?;
            Ok(if matches!(value, Value::Null) {
                None
            } else {
                Some(value.clone())
            })
        };
        let input_hash = optional("input_hash")?
            .map(|v| text(&v)?.parse().map_err(|_| err()))
            .transpose()?;
        let failure = optional("failure")?
            .map(|v| RuntimeFailure::from_value(&v, &self.config.limits).map_err(|_| err()))
            .transpose()?;
        let mut causes = Vec::new();
        for guard in &guards {
            for cause in decode_causes(map(&guard.payload)?.get("causes"), &self.config.limits)? {
                if !causes.contains(&cause) {
                    causes.push(cause)
                }
            }
        }
        for binding in &bindings {
            if let Some(Value::Array(failures)) = map(&binding.payload)?.get("failures") {
                for failure in failures {
                    for cause in decode_causes(map(failure)?.get("causes"), &self.config.limits)? {
                        if !causes.contains(&cause) {
                            causes.push(cause)
                        }
                    }
                }
            }
        }
        let mut attempts = attempts
            .into_iter()
            .map(|a| {
                let response = responses.iter().find(|r| r.key.id == a.key.id);
                Ok(AttemptView {
                    attempt_id: a.key.id.parse().map_err(|_| err())?,
                    number: u64::try_from(number(&a.payload, "number")?).map_err(|_| err())?,
                    dispatch_state: string(&a.payload, "dispatch_state")?,
                    method: string(&a.payload, "method")?,
                    owner_fence: number(&a.payload, "owner_fence")?,
                    timeout_ms: u64::try_from(number(&a.payload, "timeout_ms")?)
                        .map_err(|_| err())?,
                    policy_version: optional_text(&a.payload, "policy_version")?,
                    replay_authorized: match map(&a.payload)?.get("replay_authorized") {
                        None | Some(Value::Null) => None,
                        Some(Value::Bool(v)) => Some(*v),
                        _ => return Err(err()),
                    },
                    delivery: response
                        .map(|r| {
                            crate::mcp::McpDelivery::parse(&string(&r.payload, "delivery")?)
                                .ok_or_else(err)
                        })
                        .transpose()?,
                    failure_code: response
                        .map(|r| optional_text(&r.payload, "failure_code"))
                        .transpose()?
                        .flatten()
                        .map(|s| s.parse().map_err(|_| err()))
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        attempts.sort_by_key(|a| a.number);
        Ok(NodeView {
            scope_instance_id,
            local_id,
            canonical_path,
            input_manifest: manifest,
            labels: protection,
            limits: optional_limits(&record.payload, &self.config.limits)?,
            deadline_ms: optional_number(&record.payload, "deadline_ms")?,
            child_scope,
            iteration: optional_number(&record.payload, "iteration")?
                .map(|n| u64::try_from(n).map_err(|_| err()))
                .transpose()?,
            next: map(&record.payload)?
                .get("next")
                .filter(|v| !matches!(v, Value::Null))
                .cloned(),
            carried: child
                .as_ref()
                .and_then(|c| map(&c.payload).ok()?.get("carried").cloned()),
            guards: guards
                .into_iter()
                .map(|r| DecisionView {
                    id: r.key.id,
                    decision: r.payload,
                })
                .collect(),
            bindings: bindings
                .into_iter()
                .map(|r| DecisionView {
                    id: r.key.id,
                    decision: r.payload,
                })
                .collect(),
            attempts,
            waits: waits.iter().map(wait_view).collect::<Result<Vec<_>, _>>()?,
            causes,
            invocation_id: id,
            definition_digest: digest(&record.payload, "definition_digest")?,
            origin_executable: digest(&record.payload, "origin_executable")?,
            status: string(&record.payload, "status")?,
            reason: optional("reason")?
                .map(|v| text(&v).map(str::to_owned))
                .transpose()?,
            inputs: optional("inputs")?,
            input_hash,
            outputs: optional("outputs")?,
            failure,
        })
    }
    /// Inspects canonical graph definitions, profiles and admitted hierarchy.
    ///
    /// # Errors
    /// Returns authorization, integrity or native admission failures.
    pub async fn inspect_graph(
        &self,
        context: &HostContext,
        fingerprint: Digest,
    ) -> Result<GraphView, RuntimeError> {
        self.policy
            .authorize(context, Authorization::InspectGraph { fingerprint })?;
        let (_, document) = self.source_document(fingerprint).await?;
        let mut scope_uses = vec![htlk_analyzer::ScopeUse::Root];
        for (digest, scope) in &document.fields().scopes {
            for node in &scope.fields().nodes {
                if matches!(
                    node.fields().operation,
                    Operation::Scope(_) | Operation::Loop { .. }
                ) {
                    scope_uses.push(htlk_analyzer::ScopeUse::Node {
                        scope: *digest,
                        node: node.id().clone(),
                    });
                }
            }
        }
        Ok(GraphView {
            native_available: self.admitted(fingerprint).await.is_ok(),
            sources: self
                .inspect_sources(
                    context,
                    fingerprint,
                    None,
                    self.store.config().max_page_size,
                )
                .await?,
            fingerprint,
            document,
            scope_uses,
        })
    }
    /// Inspects committed audit summaries without exposing raw parameters/results.
    /// Stable sequence checks and complete current protection prevent torn history.
    ///
    /// # Errors
    /// Returns denial, unknown run, invalid cursor/page or corrupt event metadata.
    pub async fn inspect_history(
        &self,
        context: &HostContext,
        run: &RunId,
        after: i64,
        page_size: usize,
    ) -> Result<HistoryPage, RuntimeError> {
        for _ in 0..8 {
            let header = self.store.run_header(run).await?;
            let labels = self.store.record_protection(run).await?;
            self.policy.authorize(
                context,
                Authorization::InspectRun {
                    run_id: run,
                    labels: &labels,
                },
            )?;
            let events = self.store.journal(run, after, page_size).await?;
            let mut entries = Vec::new();
            for entry in events {
                let body = match entry.event {
                    JournalEvent::RecordsCommitted => match &entry.payload {
                        Value::Array(a) => a.get(2),
                        _ => None,
                    },
                    JournalEvent::RunInitialized => match &entry.payload {
                        Value::Array(a) => a.first(),
                        _ => None,
                    },
                    JournalEvent::TerminalReceipt | JournalEvent::ArtifactExpired => {
                        Some(&entry.payload)
                    }
                    _ => None,
                };
                let mut changed_records = Vec::new();
                if let Some(Value::Array(body)) = body
                    && let Some(Value::Array(writes)) = body.first()
                {
                    for write in writes {
                        let Value::Array(write) = write else {
                            return Err(err());
                        };
                        if write.len() != 5 {
                            return Err(err());
                        }
                        changed_records.push(RecordKey {
                            family: RecordFamily::parse(text(&write[0])?)?,
                            id: text(&write[1])?.into(),
                        });
                    }
                }
                entries.push(HistoryEntry {
                    sequence: entry.sequence,
                    event: entry.event,
                    actor: entry.actor,
                    timestamp_ms: entry.timestamp_ms,
                    causation: entry.causation,
                    changed_records,
                });
            }
            if self.store.run_header(run).await?.sequence == header.sequence {
                let next = entries.last().map(|e| e.sequence);
                return Ok(HistoryPage { entries, next });
            }
        }
        Err(RuntimeError::failure(FailureCode::Fenced))
    }
}
async fn wait_records(runtime: &Runtime, run: &RunId) -> Result<Vec<StoredRecord>, RuntimeError> {
    Ok(runtime
        .store
        .inspection_records(
            run,
            RecordFamily::Wait,
            &[
                "scope_instance_id",
                "invocation_id",
                "generation",
                "request_digest",
                "approval_request",
                "authorization_policy",
                "purpose",
                "topic",
                "request_artifact_id",
                "expires_at",
                "state",
            ],
        )
        .await?)
}
fn optional_text(value: &Value, name: &str) -> Result<Option<String>, RuntimeError> {
    match map(value)?.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(text(value)?.into())),
    }
}
fn optional_number(value: &Value, name: &str) -> Result<Option<i64>, RuntimeError> {
    match map(value)?.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Integer(n)) => Ok(Some(*n)),
        _ => Err(err()),
    }
}
fn optional_limits(
    value: &Value,
    limits: &cbor::Limits,
) -> Result<Option<ExecutionLimits>, RuntimeError> {
    match map(value)?.get("effective_limits") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(
            ExecutionLimits::from_value(value, limits).map_err(|_| err())?,
        )),
    }
}
fn optional_failure(
    value: &Value,
    limits: &cbor::Limits,
) -> Result<Option<RuntimeFailure>, RuntimeError> {
    match map(value)?.get("failure") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(
            RuntimeFailure::from_value(value, limits).map_err(|_| err())?,
        )),
    }
}
fn scope_view(record: &StoredRecord, limits: &cbor::Limits) -> Result<ScopeView, RuntimeError> {
    Ok(ScopeView {
        scope_instance_id: record.key.id.parse().map_err(|_| err())?,
        origin_executable: digest(&record.payload, "origin_executable")?,
        definition_digest: digest(&record.payload, "definition_digest")?,
        scope_use: use_site(&record.payload)?,
        parent_invocation: optional_text(&record.payload, "parent_invocation")?
            .map(|s| s.parse().map_err(|_| err()))
            .transpose()?,
        status: string(&record.payload, "status")?,
        deadline_ms: optional_number(&record.payload, "deadline_ms")?,
        iteration: optional_number(&record.payload, "iteration")?
            .map(|n| u64::try_from(n).map_err(|_| err()))
            .transpose()?,
        failure: optional_failure(&record.payload, limits)?,
    })
}
fn wait_view(wait: &StoredRecord) -> Result<WaitView, RuntimeError> {
    Ok(WaitView {
        invocation_id: string(&wait.payload, "invocation_id")?
            .parse()
            .map_err(|_| err())?,
        scope_instance_id: string(&wait.payload, "scope_instance_id")?
            .parse()
            .map_err(|_| err())?,
        generation: u64::try_from(number(&wait.payload, "generation")?).map_err(|_| err())?,
        authorization_policy: map(&wait.payload)?.get("authorization_policy").cloned(),
        request_digest: optional_text(&wait.payload, "request_digest")?
            .map(|s| s.parse().map_err(|_| err()))
            .transpose()?,
        approval_request: map(&wait.payload)?.get("approval_request").cloned(),
        wait_id: wait.key.id.parse().map_err(|_| err())?,
        purpose: string(&wait.payload, "purpose")?,
        topic: string(&wait.payload, "topic")?,
        request_artifact_id: optional_text(&wait.payload, "request_artifact_id")?
            .map(|s| s.parse().map_err(|_| err()))
            .transpose()?,
        expires_at: number(&wait.payload, "expires_at")?,
        state: string(&wait.payload, "state")?,
    })
}
async fn all(
    runtime: &Runtime,
    run: &RunId,
    family: RecordFamily,
) -> Result<Vec<StoredRecord>, RuntimeError> {
    let mut rows = Vec::new();
    let mut budget =
        htlk_executable::record_accounting::RecordAccounting::new(&runtime.config.limits)?;
    let mut cursor = None;
    loop {
        let page = runtime
            .store
            .records(
                run,
                family,
                cursor.as_deref(),
                runtime.store.config().max_page_size,
            )
            .await?;
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|r| r.key.id.clone());
        for row in page {
            budget
                .value(&row.payload, 1)
                .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
            rows.push(row);
        }
    }
    Ok(rows)
}
