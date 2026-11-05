//! Independent stdio peers observe idle delivery, correlation and bounded teardown.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use htlk_runtime::{
    HostContext,
    mcp::{McpCancellation, McpDelivery, McpEndpoint, McpFuture, client::*},
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const PEER: &str = r#"
import {createInterface} from 'node:readline';
const send = value => process.stdout.write(JSON.stringify(value)+'\n');
let held=[];
for await (const line of createInterface({input:process.stdin})) {
  const r=JSON.parse(line);
  if(r.method==='initialize') {
    if(JSON.stringify(r.params.capabilities)!=='{}') process.exit(21);
    send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{}}}});
  } else if(r.method==='notifications/initialized') {
    send({jsonrpc:'2.0',method:'notifications/resources/updated',params:{uri:'test://idle'}});
    send({jsonrpc:'2.0',method:'ping',id:'reverse'});
  } else if(r.id==='reverse' && r.result) {
    send({jsonrpc:'2.0',method:'notifications/message',params:{level:'info',data:'reverse-ok'}});
  } else if(r.method==='tools/list') {
    held.push(r);
    if(held.length===2) for(const call of held.reverse())
      send({jsonrpc:'2.0',id:call.id,result:{tools:[],marker:call.params.marker}});
  } else if(r.method==='tools/call') {
    if(r.params.name==='flood') {
      for(let i=0;i<30;i++) send({jsonrpc:'2.0',method:'notifications/message',params:{level:'info',data:i}});
    } else if(r.params.name==='url') {
      send({jsonrpc:'2.0',id:r.id,error:{code:-32042,message:'private-error',data:{elicitations:[{mode:'url',elicitationId:'one',url:'https://example.test/continue',message:'Continue'}]}}});
    }
  } else if(r.method==='notifications/cancelled') {
    send({jsonrpc:'2.0',id:r.params.requestId,error:{code:-1,message:'Cancelled'}});
  } else process.exit(22);
}
"#;
fn document(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn owner(principal: &str, generation: i64) -> McpOwner {
    let limits = Limits::default();
    McpOwner::new(
        HostContext::new(principal.into(), &limits).unwrap(),
        McpOwnerScope::Host("workspace".into()),
        generation,
        &limits,
    )
    .unwrap()
}
fn server() -> ServerIdentity {
    ServerIdentity::new(
        "peer".into(),
        McpTransport::Stdio,
        "peer".into(),
        "1".into(),
        &Limits::default(),
    )
    .unwrap()
}
fn endpoint() -> McpEndpoint {
    McpEndpoint::Stdio {
        program: "node".into(),
        arguments: vec!["--input-type=module".into(), "-e".into(), PEER.into()],
        environment: BTreeMap::new(),
    }
}
struct Events(tokio::sync::mpsc::Sender<McpClientEvent>);
impl McpEventHandler for Events {
    fn receive<'a>(&'a self, event: McpClientEvent) -> McpFuture<'a, ()> {
        Box::pin(async move {
            self.0.send(event).await.unwrap();
            Ok(())
        })
    }
}
fn client(events: Option<Arc<dyn McpEventHandler>>, limits: McpClientLimits) -> NativeMcpClient {
    NativeMcpClient::new(McpClientConfig::new(limits, BTreeMap::new(), events).unwrap()).unwrap()
}
async fn open(client: &NativeMcpClient, principal: &str, generation: i64) -> McpClientSession {
    client
        .open(
            owner(principal, generation),
            server(),
            endpoint(),
            Limits::default(),
            Arc::new(McpCancellation::new()),
        )
        .await
        .unwrap()
}

struct HttpPeer {
    child: std::process::Child,
    port: u16,
}
impl HttpPeer {
    fn start(mode: &str, get: &str) -> Self {
        use std::io::BufRead;
        let mut child = std::process::Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/support/bidirectional_mcp_peer.mjs"
            ))
            .env("RESPONSE_MODE", mode)
            .env("GET_MODE", get)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        Self {
            child,
            port: port.trim().parse().unwrap(),
        }
    }
    async fn open(&self, client: &NativeMcpClient) -> McpClientSession {
        client
            .open(
                owner("alice", 1),
                ServerIdentity::new(
                    "peer".into(),
                    McpTransport::StreamableHttp,
                    "peer".into(),
                    "1".into(),
                    &Limits::default(),
                )
                .unwrap(),
                McpEndpoint::Http {
                    url: format!("http://127.0.0.1:{}/mcp", self.port),
                    headers: BTreeMap::from([(
                        "Authorization".into(),
                        "Bearer fixture-secret".into(),
                    )]),
                },
                Limits::default(),
                Arc::new(McpCancellation::new()),
            )
            .await
            .unwrap()
    }
}
impl Drop for HttpPeer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn http_idle_stream_reverse_requests_and_post_resumption_never_repeat_effectful_posts() {
    for mode in ["json", "sse"] {
        let peer = HttpPeer::start(mode, "events");
        actix::System::new().block_on(async {
            let (events, mut receive) = tokio::sync::mpsc::channel(8);
            let client = client(Some(Arc::new(Events(events))), McpClientLimits::default());
            let session = peer.open(&client).await;
            let event = tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.method, "notifications/resources/updated");
            assert_eq!(session.server_events_available(), Some(true));
            let listed = session
                .request(
                    "tools/list",
                    document("{}"),
                    Arc::new(McpCancellation::new()),
                )
                .await;
            assert!(
                std::str::from_utf8(listed.result.unwrap().unwrap().as_bytes())
                    .unwrap()
                    .contains("café")
            );
            for name in ["resume", "ping"] {
                let result = session
                    .request(
                        "tools/call",
                        document(&format!(r#"{{"name":"{name}"}}"#)),
                        Arc::new(McpCancellation::new()),
                    )
                    .await;
                assert_eq!(
                    result.delivery,
                    McpDelivery::ResponseReceived,
                    "{name}: {result:?}"
                );
                let result = result.result.unwrap().unwrap();
                if name == "resume" {
                    let Value::Map(fields) = result.value() else {
                        panic!()
                    };
                    assert_eq!(fields.get("posts"), Some(&Value::Integer(1)));
                }
            }
            let cancellation = Arc::new(McpCancellation::new());
            let cancel = cancellation.clone();
            let (_, result) = tokio::join!(
                async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    cancel.cancel();
                },
                session.request("tools/call", document(r#"{"name":"wait"}"#), cancellation)
            );
            assert_eq!(result.delivery, McpDelivery::ResponseReceived);
            assert_eq!(result.result.unwrap().unwrap_err().code(), -1);
            let expired = session
                .request(
                    "tools/call",
                    document(r#"{"name":"expire"}"#),
                    Arc::new(McpCancellation::new()),
                )
                .await;
            assert!(expired.result.is_err());
            assert_eq!(session.ended(), Some(McpSessionEnd::SessionExpired));
            let fresh = peer.open(&client).await;
            assert_ne!(fresh.epoch(), session.epoch());
            session.close().await.unwrap();
            let reply = fresh
                .request(
                    "tools/list",
                    document("{}"),
                    Arc::new(McpCancellation::new()),
                )
                .await;
            assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
            client.shutdown().await.unwrap();
        });
    }
}

#[test]
fn http_get_405_does_not_disable_normal_post_requests() {
    let peer = HttpPeer::start("json", "405");
    actix::System::new().block_on(async {
        let client = client(None, McpClientLimits::default());
        let session = peer.open(&client).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while session.server_events_available().is_none() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(session.server_events_available(), Some(false));
        let reply = session
            .request(
                "tools/list",
                document("{}"),
                Arc::new(McpCancellation::new()),
            )
            .await;
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn idle_events_and_reverse_ping_arrive_before_any_foreground_call_and_responses_correlate() {
    actix::System::new().block_on(async {
        let (events, mut receive) = tokio::sync::mpsc::channel(8);
        let client = client(Some(Arc::new(Events(events))), McpClientLimits::default());
        let session = open(&client, "alice", 1).await;
        for expected in ["notifications/resources/updated", "notifications/message"] {
            let event = tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.method, expected);
            assert_eq!(event.epoch, session.epoch());
            assert_eq!(event.owner.key(), session.owner().key());
        }
        // The peer only replies after both requests arrive and deliberately reverses replies.
        let (one, two) = tokio::join!(
            session.request(
                "tools/list",
                document(r#"{"marker":1}"#),
                Arc::new(McpCancellation::new())
            ),
            session.request(
                "tools/list",
                document(r#"{"marker":2}"#),
                Arc::new(McpCancellation::new())
            )
        );
        for (reply, marker) in [(one, 1), (two, 2)] {
            assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
            let result = reply.result.unwrap().unwrap();
            let Value::Map(fields) = result.value() else {
                panic!()
            };
            assert_eq!(fields.get("marker"), Some(&Value::Integer(marker)));
        }
        let reply = session
            .request(
                "tools/call",
                document(r#"{"name":"url"}"#),
                Arc::new(McpCancellation::new()),
            )
            .await;
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
        let error = reply.result.unwrap().unwrap_err();
        assert_eq!(error.code(), -32042);
        assert!(
            std::str::from_utf8(error.document().as_bytes())
                .unwrap()
                .contains("elicitations")
        );
        assert!(!format!("{error:?}").contains("private-error"));
        session.close().await.unwrap();
        assert_eq!(session.ended(), Some(McpSessionEnd::Cancelled));
        client.shutdown().await.unwrap();
        client.shutdown().await.unwrap();
    });
}

#[test]
fn sessions_survive_caller_reactors_and_generation_takeover_never_reuses_handles() {
    let client = client(None, McpClientLimits::default());
    let first = actix::System::new().block_on(open(&client, "alice", 1));
    actix::System::new().block_on(async {
        let same = open(&client, "alice", 1).await;
        assert_eq!(same.epoch(), first.epoch());
        let other = open(&client, "bob", 1).await;
        assert_ne!(other.epoch(), first.epoch());
        let newer = open(&client, "alice", 2).await;
        assert_ne!(newer.epoch(), first.epoch());
        let reply = first
            .request(
                "tools/list",
                document("{}"),
                Arc::new(McpCancellation::new()),
            )
            .await;
        assert_eq!(reply.delivery, McpDelivery::NotSent);
        assert!(
            client
                .open(
                    owner("alice", 1),
                    server(),
                    endpoint(),
                    Limits::default(),
                    Arc::new(McpCancellation::new())
                )
                .await
                .is_err()
        );
        // Old cleanup is keyed to the old generation, not to the whole owner.
        first.close().await.unwrap();
        let reply = newer
            .request(
                "tools/call",
                document(r#"{"name":"url"}"#),
                Arc::new(McpCancellation::new()),
            )
            .await;
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn cancellation_uses_actual_request_id_and_queued_event_overflow_is_observable() {
    actix::System::new().block_on(async {
        let normal = client(None, McpClientLimits::default());
        let session = open(&normal, "alice", 1).await;
        let cancellation = Arc::new(McpCancellation::new());
        let cancel = cancellation.clone();
        let (_, reply) = tokio::join!(
            async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                cancel.cancel();
            },
            session.request("tools/call", document(r#"{"name":"wait"}"#), cancellation)
        );
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
        assert_eq!(reply.result.unwrap().unwrap_err().code(), -1);
        normal.shutdown().await.unwrap();

        // Stall the host consumer after one event. Protocol control/close must remain usable.
        let (events, _receive) = tokio::sync::mpsc::channel(1);
        let limits = McpClientLimits {
            events: 2,
            timeout_ms: 2000,
            ..Default::default()
        };
        let bounded = client(Some(Arc::new(Events(events))), limits);
        let session = open(&bounded, "alice", 1).await;
        let reply = session
            .request(
                "tools/call",
                document(r#"{"name":"flood"}"#),
                Arc::new(McpCancellation::new()),
            )
            .await;
        assert!(reply.result.is_err());
        assert_eq!(session.ended(), Some(McpSessionEnd::EventOverflow));
        bounded.shutdown().await.unwrap();
    });
}
