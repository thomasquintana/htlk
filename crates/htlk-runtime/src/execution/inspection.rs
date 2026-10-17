//! Authorized observations derived from authoritative checkpoints.
use super::model::*;
use crate::{
    Authorization, HostContext, Runtime, RuntimeError,
    failure::RuntimeFailure,
    identity::{InvocationId, RunId, WaitId},
    records::{RunStatus, SecurityLabels},
    store::{RecordFamily, RunHeader, StoredRecord},
};
use htlk_executable::{CanonicalDocument, cbor::Value, digest::Digest};
/// Host-visible typed input/approval request; evidence bytes require separate access.
#[derive(Clone, Debug)]
pub struct WaitView {
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
    /// Semantic registration identity.
    pub fingerprint: Digest,
    /// Canonical portable graph document.
    pub document: CanonicalDocument,
    /// Root/task/loop analysis use locations.
    pub scope_uses: Vec<htlk_analyzer::ScopeUse>,
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
        Err(RuntimeError::failure(crate::failure::FailureCode::Fenced))
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
        let nodes = all(self, run, RecordFamily::Invocation).await?;
        let protections = nodes
            .iter()
            .map(|n| labels(&n.payload, &self.config.limits))
            .collect::<Result<Vec<_>, _>>()?;
        let protection = SecurityLabels::combine(
            std::iter::once(&labels(&root.payload, &self.config.limits)?).chain(protections.iter()),
            &self.config.limits,
        )?;
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
        for wait in all(self, run, RecordFamily::Wait).await? {
            if string(&wait.payload, "state")? != "open" {
                continue;
            }
            let protection = labels(&wait.payload, &self.config.limits)?;
            self.policy.authorize(
                context,
                Authorization::ReadBoundary {
                    run_id: run,
                    scope_instance_id: header.root_scope,
                    labels: &protection,
                },
            )?;
            waits.push(WaitView {
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
        let record = self
            .store
            .record(run, &key(RecordFamily::Invocation, id))
            .await?;
        let protection = labels(&record.payload, &self.config.limits)?;
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
        Ok(NodeView {
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
        let executable = self.executable(context, fingerprint).await?;
        Ok(GraphView {
            fingerprint,
            document: executable.document().clone(),
            scope_uses: executable
                .graphs()
                .plans()
                .map(|(site, _, _)| site.clone())
                .collect(),
        })
    }
}
async fn all(
    runtime: &Runtime,
    run: &RunId,
    family: RecordFamily,
) -> Result<Vec<StoredRecord>, RuntimeError> {
    let mut rows = Vec::new();
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
        rows.extend(page);
    }
    Ok(rows)
}
