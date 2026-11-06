//! Independent peers observe real native child work and protected completion reuse.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
    digest::Digest,
};
use htlk_runtime::{
    HostContext,
    clock::SystemClock,
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, McpDelivery, McpEndpoint, McpRequest, client::*},
    store::{SqliteStore, StoreConfig},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(fields) = value else { panic!() };
    fields.get(key).unwrap()
}
struct Policy;
impl McpClientAuthorizer for Policy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if actor.principal() == action.owner.host().principal() {
            Ok(())
        } else {
            Err(RuntimeFailure::new(FailureCode::Policy))
        }
    }
}
struct Model {
    calls: AtomicUsize,
    completed: tokio::sync::mpsc::Sender<Digest>,
    combined: bool,
    interrupt_after_completion: bool,
    ambiguous_stdio: bool,
}
impl McpClientHandler for Model {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        request: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if context.parent_interaction.is_some() {
                assert_eq!(context.depth, 2);
                let outcome = context.work.as_ref().unwrap().run(doc(r#"{"selected_model":"nested-audio"}"#), |_| async {
                    McpServiceOutcome::completed(doc(r#"{"role":"assistant","model":"nested-audio","content":{"type":"audio","data":"TQ==","mimeType":"audio/wav"}}"#))
                }).await.unwrap();
                self.completed.send(context.interaction_id).await.unwrap();
                return outcome;
            }
            let params = field(request.value(), "params");
            let work = context.work.as_ref().expect("native admitted work");
            if self.combined {
                assert_eq!(
                    field(params, "includeContext"),
                    &Value::Text("thisServer".into())
                );
                assert_eq!(
                    field(params, "systemPrompt"),
                    &Value::Text("host policy is authoritative".into())
                );
                assert_eq!(
                    field(field(params, "modelPreferences"), "hints"),
                    doc(r#"[{"name":"advisory"}]"#).value()
                );
            }
            // The combined host service explicitly selects and bounds context;
            // model hints remain advisory. Both actual values reach the native peer.
            let acquired_context = doc(r#"{"source":"host-authorized","text":"bounded context"}"#);
            let descriptor = doc(&format!(
                r#"{{"selected_model":"actual-native-model","context_digest":"{}"}}"#,
                acquired_context.digest()
            ));
            let native_params = doc(&format!(
                r#"{{"name":"host-model","arguments":{{"model":"actual-native-model","context":{},"sampling":{}}}}}"#,
                std::str::from_utf8(acquired_context.as_bytes()).unwrap(),
                std::str::from_utf8(
                    JsonDocument::from_value(params, &Limits::default())
                        .unwrap()
                        .as_bytes()
                )
                .unwrap()
            ));
            let outcome = work
                .run(descriptor, |receipt| async move {
                    let repeated = work
                        .run(doc(r#"{"selected_model":"duplicate"}"#), |_| async {
                            panic!("second claim dispatched work")
                        })
                        .await
                        .unwrap_err();
                    assert_eq!(repeated.code(), FailureCode::EffectUncertain);
                    assert_eq!(receipt.interaction_id(), context.interaction_id);
                    assert_eq!(receipt.deadline_ms(), context.deadline_ms);
                    let request = || McpRequest {
                        method: "tools/call".into(),
                        params: native_params.value().clone(),
                        response_limits: Limits::default(),
                        max_response_bytes: 8192,
                        timeout_ms: 5000,
                        cancellation_grace_ms: 100,
                        cancellation: Arc::new(McpCancellation::new()),
                        idempotency_key: Some(receipt.claim_id().to_string()),
                    };
                    let reply = receipt.invoke(request()).await;
                    if self.ambiguous_stdio {
                        assert_ne!(reply.delivery, McpDelivery::ResponseReceived);
                        return McpServiceOutcome::failure(
                            McpWorkDelivery::Uncertain,
                            McpRpcError::new(doc(
                                r#"{"code":-1,"message":"Ambiguous child callback."}"#,
                            ))
                            .unwrap(),
                        );
                    }
                    assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
                    let result = reply.result.unwrap().unwrap();
                    let refused = receipt.invoke(request()).await;
                    assert_eq!(
                        refused.delivery,
                        McpDelivery::NotSent,
                        "one claim cannot pay for two native operations"
                    );
                    let mut outcome = McpServiceOutcome::completed(
                        JsonDocument::from_value(
                            field(result.value(), "sample"),
                            &Limits::default(),
                        )
                        .unwrap(),
                    );
                    outcome.usage = Some(McpUsage {
                        input_tokens: None,
                        output_tokens: Some(0),
                        provider_operation_id: Some(receipt.claim_id().to_string()),
                        ..Default::default()
                    });
                    outcome
                })
                .await;
            if self.ambiguous_stdio {
                return outcome.unwrap_or_else(|_| {
                    McpServiceOutcome::failure(
                        McpWorkDelivery::Uncertain,
                        McpRpcError::new(doc(
                            r#"{"code":-1,"message":"Ambiguous child callback."}"#,
                        ))
                        .unwrap(),
                    )
                });
            }
            let outcome = outcome.unwrap();
            // A second call to the wrapper must reuse the saved completion without
            // even constructing the operation. This is distinct from RPC replay.
            let cached = work
                .run(
                    doc(r#"{"selected_model":"actual-native-model"}"#),
                    |_| async { panic!("known work was dispatched again") },
                )
                .await
                .unwrap();
            assert_eq!(
                cached.result.as_ref().unwrap().digest(),
                outcome.result.as_ref().unwrap().digest()
            );
            assert_eq!(cached.usage.as_ref().unwrap().input_tokens, None);
            assert_eq!(cached.usage.as_ref().unwrap().output_tokens, Some(0));
            self.completed.send(context.interaction_id).await.unwrap();
            if self.interrupt_after_completion {
                std::future::pending::<()>().await;
            }
            outcome
        })
    }
}
struct Peer(std::process::Child);
struct WrongService;
impl McpClientHandler for WrongService {
    fn handle<'a>(
        &'a self,
        _: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        panic!("combined request dispatched to a partial service")
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn combined_multimodal_sampling_uses_one_native_work_claim_and_recovers_after_reopen() {
    use std::io::BufRead;
    // JSON mode uses the idle GET for its outer callback, SSE its actual POST.
    for campaign in ["stdio", "json", "sse", "interrupt", "ambiguous"] {
        actix::System::new().block_on(async {
            let mode = if matches!(campaign, "interrupt" | "ambiguous") {
                "stdio"
            } else {
                campaign
            };
            let limits = Limits::default();
            let path = std::env::temp_dir().join(format!(
                "htlk-sampling-{}-{campaign}.sqlite",
                std::process::id()
            ));
            let store = SqliteStore::open(&path, StoreConfig::default())
                .await
                .unwrap();
            let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
            let actor = HostContext::new("alice".into(), &limits).unwrap();
            let owner = persistence
                .acquire_owner(&actor, "sampling", 120_000)
                .await
                .unwrap();
            let (completed, mut receive) = tokio::sync::mpsc::channel(4);
            let model = Arc::new(Model {
                calls: AtomicUsize::new(0),
                completed,
                combined: true,
                interrupt_after_completion: campaign == "interrupt",
                ambiguous_stdio: campaign == "ambiguous",
            });
            let config = McpClientConfig::new(
                McpClientLimits {
                    timeout_ms: 5000,
                    retained_bytes: 32 * 1024 * 1024,
                    ..Default::default()
                },
                BTreeMap::from([
                    (
                        McpClientService::Sampling,
                        model.clone() as Arc<dyn McpClientHandler>,
                    ),
                    (
                        McpClientService::SamplingTools,
                        Arc::new(WrongService) as Arc<dyn McpClientHandler>,
                    ),
                    (
                        McpClientService::SamplingContext,
                        Arc::new(WrongService) as Arc<dyn McpClientHandler>,
                    ),
                    (
                        McpClientService::SamplingToolsContext,
                        model.clone() as Arc<dyn McpClientHandler>,
                    ),
                ]),
                None,
            )
            .unwrap()
            .with_persistence(persistence.clone())
            .with_authorizer(Arc::new(Policy));
            let host = McpInteractionHost::new(config.clone()).unwrap();
            let mut peer = None;
            let endpoint = if mode == "stdio" {
                McpEndpoint::Stdio {
                    program: "node".into(),
                    arguments: vec![
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../../tests/support/mcp_sampling_peer.mjs"
                        )
                        .into(),
                        if campaign == "ambiguous" { "ambiguous".into() } else { mode.into() },
                    ],
                    environment: BTreeMap::new(),
                }
            } else {
                let mut process = std::process::Command::new("node")
                    .args([
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../../tests/support/mcp_sampling_peer.mjs"
                        ),
                        mode,
                    ])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut port = String::new();
                std::io::BufReader::new(process.stdout.take().unwrap())
                    .read_line(&mut port)
                    .unwrap();
                peer = Some(Peer(process));
                McpEndpoint::Http {
                    url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                    headers: BTreeMap::new(),
                }
            };
            let server = ServerIdentity::new(
                "peer".into(),
                if mode == "stdio" {
                    McpTransport::Stdio
                } else {
                    McpTransport::StreamableHttp
                },
                "peer".into(),
                "1".into(),
                &limits,
            )
            .unwrap();
            let client = NativeMcpClient::new(config.clone()).unwrap();
            let session = client
                .open(
                    owner.clone(),
                    server,
                    endpoint,
                    limits.clone(),
                    Arc::new(McpCancellation::new()),
                )
                .await
                .unwrap();
            let call_session = session.clone();
            let call = tokio::spawn(async move {
                call_session
                    .request(
                        "tools/call",
                        doc(r#"{"name":"outer"}"#),
                        Arc::new(McpCancellation::new()),
                    )
                    .await
            });
            if campaign == "ambiguous" {
                let reply = tokio::time::timeout(Duration::from_secs(10), call).await.unwrap().unwrap();
                assert_ne!(reply.delivery, McpDelivery::ResponseReceived);
                assert_eq!(model.calls.load(Ordering::SeqCst), 1, "ambiguous stdio nesting must not dispatch standalone work");
                assert_eq!(session.ended(), Some(McpSessionEnd::CallbackFailure));
                assert_eq!(store.mcp_owner_usage(&owner).await.unwrap().calls_spent, 1);
                assert_eq!(store.mcp_owner_usage(&owner).await.unwrap().outstanding, 1);
                client.shutdown().await.unwrap(); store.close().await.unwrap(); std::fs::remove_file(path).unwrap();
                return;
            }
            let count = if campaign == "interrupt" { 1 } else if mode == "stdio" { 2 } else { 3 };
            let mut ids = Vec::new();
            for _ in 0..count {
                ids.push(
                    tokio::time::timeout(Duration::from_secs(10), receive.recv())
                        .await
                        .expect(campaign)
                        .unwrap(),
                );
            }
            if campaign == "interrupt" {
                // Stop the callback worker after real native work is durably known,
                // before the handler returns or any sampling reply is transmitted.
                call.abort();
                client.shutdown().await.unwrap();
            } else {
                let reply = call.await.unwrap();
                assert_eq!(
                    reply.delivery,
                    McpDelivery::ResponseReceived,
                    "{mode}: {reply:?}"
                );
                let result = reply.result.unwrap().unwrap();
                assert_eq!(field(result.value(), "modelOperations"), &Value::Integer(2));
                assert_eq!(
                    field(result.value(), "callbackReplies"),
                    &Value::Integer(if mode == "stdio" { 1 } else { 2 })
                );
            }
            let mut id = ids[0];
            for candidate in &ids {
                let record = store.mcp_interaction(&owner, *candidate).await.unwrap();
                if field(JsonDocument::new(&record.request, &limits).unwrap().value(), "id") == &Value::Text("sample".into()) {
                    id = *candidate;
                }
            }
            for child in ids.iter().filter(|child| **child != id) {
                let child = store.mcp_interaction(&owner, *child).await.unwrap();
                if child.parent_interaction.is_some() {
                    assert_eq!(child.parent_interaction, Some(id));
                    assert_eq!(child.depth, 2);
                }
            }
            assert_eq!(model.calls.load(Ordering::SeqCst), count);
            let view = host.inspect(&actor, &owner, id, 8192).await.unwrap();
            let work_evidence = host.inspect_work(&actor, &owner, id).await.unwrap().unwrap();
            let native = work_evidence.native_outcome.unwrap();
            assert_eq!(field(&native, "delivery"), &Value::Text("response_received".into()));
            let Value::Bytes(raw) = field(&native, "raw") else { panic!("exact native result evidence") };
            assert_eq!(field(JsonDocument::new(raw, &limits).unwrap().value(), "sample"), doc(r#"{"role":"assistant","model":"actual-native-model","content":[{"type":"tool_use","id":"next","name":"server-owned-tool","input":{}}],"stopReason":"toolUse"}"#).value());
            assert_eq!(view.record.work_delivery, McpWorkDelivery::Completed);
            assert!(!view.record.outstanding);
            let facts = store.mcp_journal(&owner, 0, 64).await.unwrap();
            assert_eq!(
                facts
                    .iter()
                    .filter(|fact| fact.kind == "host_operation_claim")
                    .count(),
                count
            );
            assert_eq!(
                store.mcp_owner_usage(&owner).await.unwrap().calls_spent,
                count as u64
            );
            client.shutdown().await.unwrap();
            drop(session);
            drop(client);
            drop(host);
            drop(config);
            drop(persistence);
            store.close().await.unwrap();
            let reopened = SqliteStore::open(&path, StoreConfig::default())
                .await
                .unwrap();
            let recovery_config =
                McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), None)
                    .unwrap()
                    .with_persistence(McpClientPersistence::new(
                        reopened.clone(),
                        Arc::new(SystemClock),
                    ))
                    .with_authorizer(Arc::new(Policy));
            let recovery = McpInteractionHost::new(recovery_config).unwrap();
            let native_after_restart = recovery.inspect_work(&actor, &owner, id).await.unwrap().unwrap();
            assert_eq!(native_after_restart.claim_id, work_evidence.claim_id);
            assert_eq!(native_after_restart.native_outcome.as_ref(), Some(&native));
            let recovered = recovery.recover_work(&actor, &owner, id).await.unwrap();
            assert_eq!(
                field(recovered.result.unwrap().value(), "model"),
                &Value::Text("actual-native-model".into())
            );
            assert_eq!(recovered.usage.unwrap().output_tokens, Some(0));
            assert_eq!(model.calls.load(Ordering::SeqCst), count);
            reopened.close().await.unwrap();
            drop(peer);
            std::fs::remove_file(path).unwrap();
        });
    }
}
