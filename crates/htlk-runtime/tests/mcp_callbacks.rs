//! Actual nested client services, addressed replies, and durable callback deduplication.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
    digest::Digest,
};
use htlk_runtime::{
    HostContext,
    clock::SystemClock,
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, McpDelivery, McpEndpoint, McpFuture, client::*},
    records::SecurityLabels,
    store::{McpOwnerUsage, SqliteStore, StoreConfig},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const PEER: &str = r#"
import {createInterface} from 'node:readline';
const send=value=>process.stdout.write(JSON.stringify(value)+'\n');
const sample={jsonrpc:'2.0',id:'sample',method:'sampling/createMessage',params:{messages:[{role:'user',content:{type:'text',text:'Question'}}],maxTokens:10,modelPreferences:{hints:[{name:'preferred'}]}}};
let parent, first, replies=0;
for await(const line of createInterface({input:process.stdin})) {
  const r=JSON.parse(line);
  if(r.method==='initialize') {
    if(!r.params.capabilities.sampling || !r.params.capabilities.elicitation?.form || !r.params.capabilities.elicitation?.url) process.exit(21);
    send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{}}}});
  } else if(r.method==='notifications/initialized') {}
  else if(r.method==='tools/call') {parent=r.id;send(sample);}
  else if(r.method==='ping') send({jsonrpc:'2.0',id:r.id,result:{}});
  else if(r.id==='sample') {
    if(r.error) process.exit(22);
    if(++replies===1) {first=JSON.stringify(r.result);send({jsonrpc:'2.0',id:'barrier',method:'ping'});}
    else {
      if(first!==JSON.stringify(r.result)) process.exit(23);
      send({jsonrpc:'2.0',id:'form',method:'elicitation/create',params:{message:'Choose amount',requestedSchema:{type:'object',properties:{amount:{type:'number',minimum:0,maximum:10}},required:['amount']}}});
    }
  } else if(r.id==='barrier') send(sample);
  else if(r.id==='form') {
    if(r.result?.action!=='accept' || r.result.content.amount!==1.5) process.exit(24);
    send({jsonrpc:'2.0',id:'url',method:'elicitation/create',params:{mode:'url',message:'Authorize externally',elicitationId:'external',url:'https://example.invalid/authorize'}});
  } else if(r.id==='url') {
    if(r.result?.action!=='accept' || r.result.content!==undefined) process.exit(25);
    send({jsonrpc:'2.0',method:'notifications/elicitation/complete',params:{elicitationId:'external'}});
    send({jsonrpc:'2.0',id:parent,result:{content:[],structuredContent:{callbackReplies:replies,completed:true}}});
  } else process.exit(26);
}
"#;
fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(map) = value else {
        panic!("object")
    };
    map.get(key).unwrap()
}
fn text(value: &Value) -> &str {
    let Value::Text(text) = value else {
        panic!("string")
    };
    text
}
struct Policy;
impl McpClientAuthorizer for Policy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if actor.principal() != action.owner.host().principal() {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
    fn request_labels(
        &self,
        _: &McpOwner,
        _: Digest,
        _: &JsonDocument,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::new(vec!["private".into()], &Limits::default()).unwrap())
    }
}
struct Events(tokio::sync::mpsc::Sender<McpClientEvent>);
impl McpEventHandler for Events {
    fn receive<'a>(&'a self, event: McpClientEvent) -> McpFuture<'a, ()> {
        Box::pin(async move {
            self.0
                .send(event)
                .await
                .map_err(|_| RuntimeFailure::new(FailureCode::McpTransport))
        })
    }
}
struct Model {
    calls: AtomicUsize,
    session: Mutex<Option<McpClientSession>>,
}
impl McpClientHandler for Model {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        request: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                text(field(request.value(), "method")),
                "sampling/createMessage"
            );
            assert!(context.originating_request_id.is_some());
            assert_eq!(context.attribution, McpAttribution::SerializedOperation);
            let session = self.session.lock().unwrap().as_ref().unwrap().clone();
            // Reenter this exact session while the originating tool waits for us.
            let ping = session
                .request("ping", doc("{}"), Arc::new(McpCancellation::new()))
                .await;
            assert_eq!(ping.delivery, McpDelivery::ResponseReceived);
            let mut outcome = McpServiceOutcome::completed(doc(
                r#"{"role":"assistant","model":"actual-model","content":{"type":"text","text":"Answer"},"stopReason":"endTurn"}"#,
            ));
            outcome.usage = Some(McpUsage {
                input_tokens: Some(3),
                output_tokens: Some(5),
                provider_operation_id: Some("model-operation".into()),
                ..Default::default()
            });
            outcome
        })
    }
}

#[test]
fn sampling_and_addressed_elicitation_finish_inside_one_tool_without_reader_leases() {
    for headless in [false, true] {
        actix::System::new().block_on(async {
        let limits = Limits::default();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
        let caller = HostContext::new("alice".into(), &limits).unwrap();
        let owner = persistence
            .acquire_owner(&caller, "callbacks", 120_000)
            .await
            .unwrap();
        let model = Arc::new(Model {
            calls: AtomicUsize::new(0),
            session: Mutex::new(None),
        });
        let human: Arc<dyn McpClientHandler> = Arc::new(McpAwaitReply::new(persistence.clone()));
        let (events, mut receive) = tokio::sync::mpsc::channel(16);
        let config = McpClientConfig::new(
            McpClientLimits {
                timeout_ms: 5000,
                ..Default::default()
            },
            BTreeMap::from([
                (
                    McpClientService::Sampling,
                    model.clone() as Arc<dyn McpClientHandler>,
                ),
                (McpClientService::ElicitationForm, human.clone()),
                (McpClientService::ElicitationUrl, human),
            ]),
            Some(Arc::new(Events(events))),
        )
        .unwrap()
        .with_persistence(persistence)
        .with_authorizer(Arc::new(Policy));
        let host = McpInteractionHost::new(config.clone()).unwrap();
        let client = NativeMcpClient::new(config).unwrap();
        let session = client
            .open(
                owner.clone(),
                ServerIdentity::new(
                    "peer".into(),
                    McpTransport::Stdio,
                    "peer".into(),
                    "1".into(),
                    &limits,
                )
                .unwrap(),
                McpEndpoint::Stdio {
                    program: "node".into(),
                    arguments: vec!["--input-type=module".into(), "-e".into(), PEER.into()],
                    environment: BTreeMap::new(),
                },
                limits.clone(),
                Arc::new(McpCancellation::new()),
            )
            .await
            .unwrap();
        *model.session.lock().unwrap() = Some(session.clone());
        let interaction_client = async {
            let mut answered = 0;
            let mut ids = Vec::new();
            while answered < 2 || ids.len() < 3 {
                let id: Digest = if headless {
                    let page = host.list(&caller, &owner, None, 8).await.unwrap();
                    let Some(id) = page.interactions.into_iter().find(|id| !ids.contains(id)) else {
                        tokio::time::sleep(Duration::from_millis(5)).await; continue;
                    };
                    id
                } else {
                    let event = receive.recv().await.unwrap();
                    if event.method != "htlk/interaction" { continue; }
                    text(field(event.params.value(), "interaction_id")).parse().unwrap()
                };
                let view = host.inspect(&caller, &owner, id, 8192).await.unwrap();
                if view.record.state == htlk_runtime::store::McpWorkState::Pending {
                    tokio::time::sleep(Duration::from_millis(5)).await; continue;
                }
                let request_digest = view.record.request_digest;
                let request = JsonDocument::new(&view.record.request, &limits).unwrap();
                let kind = if text(field(request.value(), "method")) == "sampling/createMessage" { "sampling" }
                    else if matches!(field(request.value(), "params"), Value::Map(params) if params.get("mode") == Some(&Value::Text("url".into()))) { "url" }
                    else { "form" };
                ids.push(id);
                assert!(!view.request_truncated);
                let viewer = HostContext::new("viewer".into(), &limits).unwrap();
                assert_eq!(
                    host.inspect(&viewer, &owner, id, 8192)
                        .await
                        .unwrap_err()
                        .code(),
                    FailureCode::Policy
                );
                if kind == "sampling" {
                    continue;
                }
                let result = if kind == "form" {
                    assert!(
                        host.respond(
                            &caller,
                            &owner,
                            id,
                            request_digest,
                            doc(r#"{"action":"accept","content":{"amount":99}}"#),
                            "bad"
                        )
                        .await
                        .is_err()
                    );
                    doc(r#"{"action":"accept","content":{"amount":1.5}}"#)
                } else {
                    doc(r#"{"action":"accept"}"#)
                };
                let receipt = host
                    .respond(&caller, &owner, id, request_digest, result.clone(), kind)
                    .await
                    .unwrap();
                let repeated = host
                    .respond(&caller, &owner, id, request_digest, result, kind)
                    .await
                    .unwrap();
                assert_eq!(receipt, repeated);
                answered += 1;
            }
            ids
        };
        let (reply, ids) = tokio::time::timeout(Duration::from_secs(8), async {
            tokio::join!(
                session.request(
                    "tools/call",
                    doc(r#"{"name":"nested"}"#),
                    Arc::new(McpCancellation::new())
                ),
                interaction_client
            )
        })
        .await
        .unwrap();
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived, "{reply:?}");
        assert!(reply.result.unwrap().is_ok());
        assert_eq!(
            model.calls.load(Ordering::SeqCst),
            1,
            "redelivery cannot rerun the model"
        );
        assert_eq!(ids.len(), 3);
        assert_eq!(
            store.mcp_owner_usage(&owner).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 3,
                outstanding: 0
            }
        );
        let mut sample = None;
        for id in ids {
            let view = host.inspect(&caller, &owner, id, 8192).await.unwrap();
            if view.record.usage.is_some() { sample = Some(view); }
        }
        let sample = sample.unwrap();
        assert_eq!(sample.record.send_attempts, 2);
        assert_eq!(
            field(sample.record.usage.as_ref().unwrap(), "output_tokens"),
            &Value::Integer(5)
        );
        client.shutdown().await.unwrap();
    });
    }
}

struct SlowModel {
    started: tokio::sync::mpsc::Sender<Digest>,
}
impl McpClientHandler for SlowModel {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.started.send(context.interaction_id).await.unwrap();
            // Deliberately blocks its executor: the transport reader must still run.
            std::thread::sleep(Duration::from_millis(750));
            McpServiceOutcome::completed(doc(
                r#"{"role":"assistant","model":"late","content":{"type":"text","text":"late result"}}"#,
            ))
        })
    }
}

#[test]
fn blocking_callback_cannot_block_io_and_deadline_keeps_late_work_as_evidence() {
    actix::System::new().block_on(async {
        let limits = Limits::default();
        let store = SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
        let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
        let owner = persistence.acquire_owner(&HostContext::new("alice".into(), &limits).unwrap(), "slow", 120_000).await.unwrap();
        let (started, mut ready) = tokio::sync::mpsc::channel(1);
        let config = McpClientConfig::new(McpClientLimits { timeout_ms: 300, ..Default::default() },
            BTreeMap::from([(McpClientService::Sampling, Arc::new(SlowModel { started }) as Arc<dyn McpClientHandler>)]), None).unwrap()
            .with_persistence(persistence).with_authorizer(Arc::new(Policy));
        let client = NativeMcpClient::new(config).unwrap();
        let script = r#"
            import {createInterface} from 'node:readline';
            const send=v=>process.stdout.write(JSON.stringify(v)+'\n');let parent;
            for await(const line of createInterface({input:process.stdin})) {
              const r=JSON.parse(line);
              if(r.method==='initialize') send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{}}}});
              else if(r.method==='notifications/initialized') {}
              else if(r.method==='tools/call') {parent=r.id;send({jsonrpc:'2.0',id:'slow',method:'sampling/createMessage',params:{messages:[],maxTokens:10}});}
              else if(r.method==='ping') send({jsonrpc:'2.0',id:r.id,result:{}});
              else if(r.id==='slow') {if(!r.error) process.exit(29);send({jsonrpc:'2.0',id:parent,result:{content:[]}});}
              else if(r.method!=='notifications/cancelled') process.exit(30);
            }
        "#;
        let session = client.open(owner.clone(), ServerIdentity::new("peer".into(), McpTransport::Stdio, "peer".into(), "1".into(), &limits).unwrap(),
            McpEndpoint::Stdio { program: "node".into(), arguments: vec!["--input-type=module".into(), "-e".into(), script.into()], environment: BTreeMap::new() }, limits.clone(), Arc::new(McpCancellation::new())).await.unwrap();
        let call = session.invoke(htlk_runtime::mcp::McpRequest { method: "tools/call".into(), params: doc(r#"{"name":"slow"}"#).value().clone(),
            response_limits: limits, max_response_bytes: 4096, timeout_ms: 2000, cancellation_grace_ms: 100, cancellation: Arc::new(McpCancellation::new()), idempotency_key: None });
        let probe = async {
            let id = ready.recv().await.unwrap();
            let ping = tokio::time::timeout(Duration::from_millis(150), session.request("ping", doc("{}"), Arc::new(McpCancellation::new()))).await.unwrap();
            assert_eq!(ping.delivery, McpDelivery::ResponseReceived);
            id
        };
        let (reply, id) = tokio::join!(call, probe);
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
        assert_eq!(store.mcp_owner_usage(&owner).await.unwrap(), McpOwnerUsage { calls_spent: 1, outstanding: 1 });
        let late = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let evidence = store.mcp_late_replies(&owner, id, None, 4).await.unwrap();
                if !evidence.is_empty() { break evidence; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert_eq!(late[0].work_delivery, McpWorkDelivery::Completed);
        assert!(store.mcp_interaction(&owner, id).await.unwrap().outstanding);
        client.shutdown().await.unwrap();
    });
}

struct CountModel(AtomicUsize);
struct PeerProcess(std::process::Child);
impl Drop for PeerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl McpClientHandler for CountModel {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(context.attribution, McpAttribution::ResponseStream);
            McpServiceOutcome::completed(doc(
                r#"{"role":"assistant","model":"actual","content":{"type":"text","text":"Answer"}}"#,
            ))
        })
    }
}

#[test]
fn lost_http_reply_acknowledgement_retries_saved_bytes_without_another_model_call() {
    use std::io::BufRead;
    let mut peer = PeerProcess(
        std::process::Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/support/bidirectional_mcp_peer.mjs"
            ))
            .env("GET_MODE", "405")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    std::io::BufReader::new(peer.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    actix::System::new().block_on(async {
        let limits = Limits::default();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
        let caller = HostContext::new("alice".into(), &limits).unwrap();
        let owner = persistence
            .acquire_owner(&caller, "http", 120_000)
            .await
            .unwrap();
        let model = Arc::new(CountModel(AtomicUsize::new(0)));
        let config = McpClientConfig::new(
            McpClientLimits::default(),
            BTreeMap::from([(
                McpClientService::Sampling,
                model.clone() as Arc<dyn McpClientHandler>,
            )]),
            None,
        )
        .unwrap()
        .with_persistence(persistence)
        .with_authorizer(Arc::new(Policy));
        let client = NativeMcpClient::new(config).unwrap();
        let session = client
            .open(
                owner.clone(),
                ServerIdentity::new(
                    "peer".into(),
                    McpTransport::StreamableHttp,
                    "peer".into(),
                    "1".into(),
                    &limits,
                )
                .unwrap(),
                McpEndpoint::Http {
                    url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                    headers: BTreeMap::from([(
                        "Authorization".into(),
                        "Bearer fixture-secret".into(),
                    )]),
                },
                limits,
                Arc::new(McpCancellation::new()),
            )
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            session.request(
                "tools/call",
                doc(r#"{"name":"sampling"}"#),
                Arc::new(McpCancellation::new()),
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.delivery, McpDelivery::ResponseReceived);
        let result = result.result.unwrap().unwrap();
        assert_eq!(
            field(
                field(result.value(), "structuredContent"),
                "callbackReplies"
            ),
            &Value::Integer(2)
        );
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.mcp_owner_usage(&owner).await.unwrap(),
            McpOwnerUsage {
                calls_spent: 1,
                outstanding: 0
            }
        );
        client.shutdown().await.unwrap();
    });
}
