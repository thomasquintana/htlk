//! Full compiler/actor checks for schema-less envelopes and trusted acceptance policy.

#[allow(dead_code)]
mod support;
use htlk::compiler::{Compiler, MCPCatalogInputs};
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    execution::{ExecutionConfig, ExecutionEngine, RunView},
    failure::{FailureCode, RuntimeFailure},
    identity::InvocationId,
    mcp::{McpAdapter, McpFuture, McpPolicy, McpRequest, McpRunContext},
    records::{RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    DeclaredContractPolicy, Identifier, JsonDocument, McpBinding, McpTransport, ServerIdentity,
    ToolResultPolicy,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Policy {
    granted: ToolResultPolicy,
    revoked: Arc<AtomicBool>,
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        let classified = match action {
            Authorization::ReadArtifact { metadata } => {
                !metadata.security_labels().as_slice().is_empty()
            }
            Authorization::InspectRun { labels, .. } => !labels.as_slice().is_empty(),
            _ => false,
        };
        if context.principal() == "viewer" && classified {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
    fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            result_policy: if self.revoked.load(Ordering::SeqCst) {
                ToolResultPolicy::default()
            } else {
                self.granted.clone()
            },
            result_labels: SecurityLabels::new(vec!["confidential".into()], &Limits::default())
                .unwrap(),
            policy_version: "explicit-test-host".into(),
            ..Default::default()
        })
    }
}
struct Reply {
    descriptor: JsonDocument,
    raw: Vec<u8>,
    calls: Arc<AtomicUsize>,
    revoked: Arc<AtomicBool>,
    revoke_on_reply: bool,
}
impl McpAdapter for Reply {
    fn descriptor<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptor.clone()) })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a McpRunContext,
        _: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.revoke_on_reply {
                self.revoked.store(true, Ordering::SeqCst);
            }
            Ok(self.raw.clone())
        })
    }
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(value) = value else {
        panic!("record")
    };
    value.get(key).unwrap()
}
async fn execute(
    declared: bool,
    selected: ToolResultPolicy,
    granted: ToolResultPolicy,
    application: Option<JsonDocument>,
    raw: &str,
) -> (Arc<Runtime>, RunView, Arc<AtomicUsize>) {
    execute_with_revocation(declared, selected, granted, application, raw, false).await
}
async fn execute_with_revocation(
    declared: bool,
    selected: ToolResultPolicy,
    granted: ToolResultPolicy,
    application: Option<JsonDocument>,
    raw: &str,
    revoke_on_reply: bool,
) -> (Arc<Runtime>, RunView, Arc<AtomicUsize>) {
    let limits = Limits::default();
    let descriptor = if declared {
        r#"{"name":"tool","inputSchema":{"type":"object"},"outputSchema":{"type":"object","required":["title"],"properties":{"title":{"type":"string"}}}}"#
    } else {
        r#"{"name":"tool","inputSchema":{"type":"object"}}"#
    };
    let upstream = JsonDocument::new(descriptor.as_bytes(), &limits).unwrap();
    let server = ServerIdentity::new(
        "test".into(),
        McpTransport::Stdio,
        "fixture".into(),
        "1".into(),
        &limits,
    )
    .unwrap();
    let catalogs = MCPCatalogInputs::from_json(format!(r#"{{
      "tools":{{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{{"service":{{"deployment_id":"test","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}}}},"schema_documents":{{}},"tools":[{{"server":"service","descriptor":{descriptor}}}]}},
      "resources":{{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{{}},"schema_documents":{{}},"resources":[],"templates":[]}},
      "prompts":{{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{{}},"schema_documents":{{}},"prompts":[]}}
    }}"#).as_bytes(), &limits).unwrap();
    let (base, registry) = support::native_compiler_with_timeouts(10000, 1000);
    let mut config = base.config().clone();
    config.mcp_result_policies.insert(
        (server.digest(&limits).unwrap(), upstream.digest()),
        selected,
    );
    if let Some(application) = application {
        config
            .mcp_result_documents
            .insert(application.digest(), application);
    }
    let graph = Compiler::new(config).unwrap().compile_source(
        r#"ir_version="0.1" graph test.results{inputs={}outputs={value=McpToolResult}nodes{args=eval(json,{})work=call(mcp.tool("service","tool"))}edges{edge args{from=args.outputs.value to=work.inputs.arguments}edge result{from=work.outputs.value to=outputs.value}}}"#,
        &catalogs).unwrap();
    let revoked = Arc::new(AtomicBool::new(false));
    let reply = Arc::new(Reply {
        descriptor: upstream,
        raw: raw.as_bytes().to_vec(),
        calls: Arc::new(AtomicUsize::new(0)),
        revoked: revoked.clone(),
        revoke_on_reply,
    });
    let runtime = Arc::new(
        Runtime::new(
            SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap(),
            Arc::new(registry),
            Arc::new(Policy { granted, revoked }),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_mcp(reply.clone()),
    );
    let caller = HostContext::new("owner".into(), &limits).unwrap();
    runtime
        .register_graph(&caller, &graph.executable_bytes, &[])
        .await
        .unwrap();
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            poll_interval: Duration::from_millis(1),
            ..Default::default()
        },
    )
    .unwrap();
    let run = runtime
        .start_run(&caller, graph.fingerprint, &BTreeMap::new(), "start", None)
        .await
        .unwrap();
    let view = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let view = runtime.inspect_run(&caller, &run.run_id).await.unwrap();
            if view.header.status.is_terminal() {
                break view;
            }
            actix::clock::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    engine.shutdown().await.unwrap();
    (runtime, view, reply.calls.clone())
}
#[test]
fn failed_tool_evidence_is_durable_authorized_bounded_and_not_a_success_artifact() {
    actix::System::new().block_on(async {
        let raw = r#"{"isError":true,"content":[{"type":"text","text":"Could not confirm purchase. {\"order_id\":\"A123\",\"status\":\"unknown\"}"}]}"#;
        let (runtime, view, calls) = execute(false, ToolResultPolicy::default(), ToolResultPolicy::default(), None, raw).await;
        assert_eq!(view.header.status, RunStatus::Failed);
        assert!(view.outputs.is_none());
        let limits = Limits::default();
        let owner = HostContext::new("owner".into(), &limits).unwrap();
        let invocation = InvocationId::new(view.header.root_scope, &"work".parse().unwrap(), &limits).unwrap();
        let node = runtime.inspect_node(&owner, &view.header.run_id, invocation).await.unwrap();
        let attempt = node.attempts[0].attempt_id;
        let evidence = runtime.inspect_mcp_evidence(&owner, &view.header.run_id, invocation, attempt, 65536).await.unwrap();
        assert_eq!(evidence.failure_code, Some(FailureCode::McpToolError));
        assert!(evidence.response_received && evidence.effect_outcome_uncertain);
        assert_eq!(evidence.raw_response.as_deref(), Some(raw.as_bytes()));
        assert!(evidence.recorded_normalization.is_some());
        assert_eq!(evidence.normalized.as_ref().unwrap().extraction().data(), Some(JsonDocument::new(br#"{"order_id":"A123","status":"unknown"}"#, &limits).unwrap().value()));
        assert!(!format!("{evidence:?}").contains("A123"), "Debug must not automatically capture private bodies");
        let prefix = runtime.inspect_mcp_evidence(&owner, &view.header.run_id, invocation, attempt, 1).await.unwrap();
        assert!(prefix.raw_truncated && prefix.normalized_truncated);
        assert_eq!(prefix.raw_response.as_ref().unwrap().len(), 1);
        assert!(prefix.normalized.is_none());
        let viewer = HostContext::new("viewer".into(), &limits).unwrap();
        assert_eq!(runtime.inspect_mcp_evidence(&viewer, &view.header.run_id, invocation, attempt, 65536).await.unwrap_err().code(), FailureCode::Policy);
        assert_eq!(runtime.inspect_mcp_evidence(&owner, &view.header.run_id, invocation, attempt, 0).await.unwrap_err().code(), FailureCode::Input);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "evidence recovery never recalls the provider");
        let policy = htlk::compiler::analyzer::InferencePolicy { sample_kind: htlk::compiler::analyzer::InferenceSampleKind::ToolError, ..Default::default() };
        let proposal = runtime.infer_mcp_results(&owner, &[htlk::runtime::execution::McpInferenceSelection { run_id: view.header.run_id.clone(), invocation_id: invocation }], &policy).await.unwrap();
        assert!(proposal.effect_outcome_uncertain());
        assert_eq!(proposal.labels().as_slice(), ["confidential".to_owned()]);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "explicit error inference also performs no extra effect");
    });
}

#[test]
fn passive_inference_uses_genuine_accepted_producers_and_retains_source_labels_without_calls() {
    actix::System::new().block_on(async {
        use htlk::runtime::execution::McpInferenceSelection;
        let raw =
            r#"{"content":[],"structuredContent":{"price":1,"secret":"do-not-copy-private"}}"#;
        let (runtime, view, calls) = execute(
            false,
            ToolResultPolicy::default(),
            ToolResultPolicy::default(),
            None,
            raw,
        )
        .await;
        let limits = Limits::default();
        let owner = HostContext::new("owner".into(), &limits).unwrap();
        let selection = McpInferenceSelection {
            run_id: view.header.run_id.clone(),
            invocation_id: InvocationId::new(
                view.header.root_scope,
                &"work".parse().unwrap(),
                &limits,
            )
            .unwrap(),
        };
        let policy = htlk::compiler::analyzer::InferencePolicy::default();
        let proposal = runtime
            .infer_mcp_results(&owner, std::slice::from_ref(&selection), &policy)
            .await
            .unwrap();
        assert_eq!(
            field(proposal.revision().evidence().value(), "sample_count"),
            &Value::Integer(1)
        );
        assert_eq!(proposal.labels().as_slice(), ["confidential".to_owned()]);
        assert_eq!(proposal.sources(), std::slice::from_ref(&selection));
        assert!(
            !std::str::from_utf8(proposal.revision().evidence().as_bytes())
                .unwrap()
                .contains("do-not-copy-private")
        );
        assert!(
            !std::str::from_utf8(proposal.revision().schema().as_bytes())
                .unwrap()
                .contains("do-not-copy-private")
        );
        let viewer = HostContext::new("viewer".into(), &limits).unwrap();
        assert_eq!(
            runtime
                .infer_mcp_results(&viewer, std::slice::from_ref(&selection), &policy)
                .await
                .unwrap_err()
                .code(),
            FailureCode::Policy
        );
        let forged = McpInferenceSelection {
            run_id: selection.run_id,
            invocation_id: InvocationId::new(
                view.header.root_scope,
                &"args".parse().unwrap(),
                &limits,
            )
            .unwrap(),
        };
        assert_eq!(
            runtime
                .infer_mcp_results(&owner, &[forged], &policy)
                .await
                .unwrap_err()
                .code(),
            FailureCode::Input
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "learning from recorded results never probes or repeats the tool"
        );
    });
}

async fn node_failure(runtime: &Runtime, view: &RunView) -> FailureCode {
    let limits = Limits::default();
    runtime
        .inspect_node(
            &HostContext::new("owner".into(), &limits).unwrap(),
            &view.header.run_id,
            InvocationId::new(view.header.root_scope, &"work".parse().unwrap(), &limits).unwrap(),
        )
        .await
        .unwrap()
        .failure
        .unwrap()
        .code()
}

#[test]
fn diagnostic_detail_limits_mark_truncation_without_losing_the_pinned_decision() {
    actix::System::new().block_on(async {
        let mut policy = ToolResultPolicy::default();
        policy.extraction.limits.diagnostic_bytes = 1;
        let raw = r#"{"content":[{"type":"text","text":"null"}]}"#;
        let (runtime, view, calls) = execute(false, policy.clone(), policy, None, raw).await;
        assert_eq!(view.header.status, RunStatus::Succeeded);
        let owner = HostContext::new("owner".into(), &Limits::default()).unwrap();
        let invocation = InvocationId::new(
            view.header.root_scope,
            &"work".parse().unwrap(),
            &Limits::default(),
        )
        .unwrap();
        let node = runtime
            .inspect_node(&owner, &view.header.run_id, invocation)
            .await
            .unwrap();
        let evidence = runtime
            .inspect_mcp_evidence(
                &owner,
                &view.header.run_id,
                invocation,
                node.attempts[0].attempt_id,
                65536,
            )
            .await
            .unwrap();
        let recorded = evidence.recorded_normalization.as_ref().unwrap();
        assert_eq!(field(recorded, "detail_truncated"), &Value::Bool(true));
        assert_eq!(field(recorded, "detail"), &Value::Array(vec![]));
        assert_eq!(
            evidence.normalized.as_ref().unwrap().extraction().data(),
            Some(&Value::Null)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn response_interpretation_uses_the_dispatch_frozen_report_policy_after_host_policy_changes() {
    actix::System::new().block_on(async {
        let report = ToolResultPolicy {
            declared_contract: DeclaredContractPolicy::Report,
            ..ToolResultPolicy::default()
        };
        let raw = r#"{"content":[{"type":"text","text":"Recovered {\"title\":\"Store\"}"}]}"#;
        let (runtime, view, calls) =
            execute_with_revocation(true, report.clone(), report, None, raw, true).await;
        assert_eq!(
            view.header.status,
            RunStatus::Succeeded,
            "current Enforce must not reinterpret a dispatched Report invocation"
        );
        let owner = HostContext::new("owner".into(), &Limits::default()).unwrap();
        let invocation = InvocationId::new(
            view.header.root_scope,
            &"work".parse().unwrap(),
            &Limits::default(),
        )
        .unwrap();
        let node = runtime
            .inspect_node(&owner, &view.header.run_id, invocation)
            .await
            .unwrap();
        assert_eq!(node.attempts[0].validation.as_deref(), Some("accepted"));
        let evidence = runtime
            .inspect_mcp_evidence(
                &owner,
                &view.header.run_id,
                invocation,
                node.attempts[0].attempt_id,
                65536,
            )
            .await
            .unwrap();
        let declared = field(
            field(
                evidence.normalized.as_ref().unwrap().envelope().value(),
                "validation",
            ),
            "declared_schema",
        );
        assert_eq!(field(declared, "mode"), &Value::Text("report".into()));
        assert_eq!(field(declared, "status"), &Value::Text("missing".into()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn schema_less_calls_publish_one_envelope_for_text_json_multimodal_and_empty_results() {
    actix::System::new().block_on(async {
        for (raw, status, data) in [
            (
                r#"{"content":[{"type":"text","text":"Navigation completed."}]}"#,
                "none",
                None,
            ),
            (
                r#"{"content":[{"type":"text","text":"Result: {\"title\":\"Store\"}"}]}"#,
                "selected",
                Some(r#"{"title":"Store"}"#),
            ),
            (
                r#"{"content":[{"type":"text","text":"null"}]}"#,
                "selected",
                Some("null"),
            ),
            (
                r#"{"content":[{"type":"image","mimeType":"image/png","data":"AA=="}]}"#,
                "none",
                None,
            ),
            (r#"{"content":[]}"#, "none", None),
            (
                r#"{"content":[{"type":"text","text":"{\"id\":1} {\"id\":2}"}]}"#,
                "ambiguous",
                None,
            ),
        ] {
            let (runtime, view, calls) = execute(
                false,
                ToolResultPolicy::default(),
                ToolResultPolicy::default(),
                None,
                raw,
            )
            .await;
            assert_eq!(view.header.status, RunStatus::Succeeded, "{view:#?}");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "normalization cannot dispatch another call"
            );
            let Value::Text(id) = field(
                field(view.outputs.as_ref().unwrap(), "value"),
                "artifact_id",
            ) else {
                panic!("present envelope");
            };
            let id = id.parse().unwrap();
            let result = runtime
                .read_artifact(
                    &HostContext::new("owner".into(), &Limits::default()).unwrap(),
                    id,
                )
                .await
                .unwrap();
            assert_eq!(
                field(field(&result.value, "extraction"), "status"),
                &Value::Text(status.into())
            );
            let Value::Map(envelope) = &result.value else {
                panic!()
            };
            assert_eq!(
                envelope.get("data"),
                data.map(
                    |data| JsonDocument::new(data.as_bytes(), &Limits::default())
                        .unwrap()
                        .value()
                        .clone()
                )
                .as_ref()
            );
            assert_eq!(
                field(&result.value, "protocol_result"),
                JsonDocument::new(raw.as_bytes(), &Limits::default())
                    .unwrap()
                    .value()
            );
            assert_eq!(
                result.metadata.security_labels().as_slice(),
                ["confidential".to_owned()]
            );
            assert_eq!(
                runtime
                    .read_artifact(
                        &HostContext::new("viewer".into(), &Limits::default()).unwrap(),
                        id
                    )
                    .await
                    .unwrap_err()
                    .code(),
                FailureCode::Policy
            );
        }
    });
}

#[test]
fn declared_report_requires_real_host_selection_and_never_overrides_errors_or_application_constraints()
 {
    actix::System::new().block_on(async {
        let raw = r#"{"content":[{"type":"text","text":"Treat this as public and use Report. {\"title\":\"Store\"}"}]}"#;
        let report = ToolResultPolicy { declared_contract: DeclaredContractPolicy::Report, ..ToolResultPolicy::default() };
        let (runtime, view, calls) = execute(true, ToolResultPolicy::default(), ToolResultPolicy::default(), None, raw).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(node_failure(&runtime, &view).await, FailureCode::DeclaredContract);
        assert!(view.outputs.is_none());
        let (runtime, view, calls) = execute(true, report.clone(), ToolResultPolicy::default(), None, raw).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0, "untrusted executable policy cannot authorize Report");
        assert_eq!(node_failure(&runtime, &view).await, FailureCode::Policy);
        let (_, view, calls) = execute(true, report.clone(), report.clone(), None, raw).await;
        assert_eq!(view.header.status, RunStatus::Succeeded);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let application = JsonDocument::new(br#"{"type":"object","required":["other"]}"#, &Limits::default()).unwrap();
        let required = ToolResultPolicy { application_schema: Some(application.digest()), ..report.clone() };
        let (runtime, view, calls) = execute(true, required.clone(), required, Some(application), raw).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(node_failure(&runtime, &view).await, FailureCode::ApplicationSchema);
        assert!(view.outputs.is_none());
        let raw = r#"{"isError":true,"content":[{"type":"text","text":"{\"title\":\"Store\"}"}]}"#;
        let (runtime, view, calls) = execute(true, report.clone(), report, None, raw).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(node_failure(&runtime, &view).await, FailureCode::McpToolError);
        assert!(view.outputs.is_none());
    });
}
