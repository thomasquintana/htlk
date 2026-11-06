//! Independent stdio/HTTP peers exercise typed notification admission, actual
//! token association, roots services and callback progress with zero content reads.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use htlk_runtime::{
    HostContext,
    failure::FailureCode,
    mcp::{McpCancellation, McpDelivery, McpEndpoint, McpFuture, McpRequest, client::*},
    store::SqliteStore,
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
import {createServer} from 'node:http';
const [transport,mode]=process.argv.slice(1);
const counts={content:0,calls:0,rootChanges:0,rootReplies:0,rootErrors:0,progress:[],roots:[]};
let eventStream;let timer;let held=[];let taskToken;let taskTime;
const capabilities=mode==='unsupported'?{}:mode==='bits'?{tools:{},prompts:{},resources:{}}:{tools:{listChanged:true},prompts:{listChanged:true},resources:{subscribe:true,listChanged:true},logging:{}};
if(mode==='task-progress')capabilities.tasks={requests:{tools:{call:{}}},list:{},cancel:{}};
const task=status=>({taskId:'remote-task',status,createdAt:taskTime,lastUpdatedAt:taskTime,ttl:5000,pollInterval:1});
const emit=r=>{if(transport==='stdio')process.stdout.write(JSON.stringify(r)+'\n');else eventStream?.write('data: '+JSON.stringify(r)+'\n\n');};
const notification=(method,params)=>emit({jsonrpc:'2.0',method,...(params===undefined?{}:{params})});
const progress=(token,progress,total)=>notification('notifications/progress',{progressToken:token,progress,...(total===undefined?{}:{total})});
function idle(malformed=false){
 if(mode==='quiet'||mode==='roots'||mode==='roots-only'||mode==='bad-root')return;
 if(mode.startsWith('invalid-')){
  if(!malformed)return;
  const cases={resource:['resources/updated',{uri:7}],tools:['tools/list_changed',[]],prompts:['prompts/list_changed',null],resources:['resources/list_changed',{_meta:7}],log:['message',{level:'bogus',data:0}],progress:['progress',{progressToken:'x',progress:-1,total:2}],total:['progress',{progressToken:'x',progress:3,total:2}]};
  const [method,params]=cases[mode.slice(8)];notification('notifications/'+method,params);return;
 }
 notification('notifications/resources/updated',{uri:'test://unsolicited'});
 notification('notifications/tools/list_changed');
 notification('notifications/prompts/list_changed',{});
 notification('notifications/resources/list_changed',{});
 notification('notifications/message',{level:'notice',logger:'fixture',data:{text:'protected log'}});
}
function request(r,reply){
 if(r.method==='initialize'){
  if(mode.startsWith('roots')||mode==='bad-root'){
   if(!r.params.capabilities.roots)process.exit(61);
   if(r.params.capabilities.roots.listChanged!==(mode!=='roots-only'))process.exit(62);
  }
  reply({protocolVersion:'2025-11-25',serverInfo:{name:'notifications',version:'1'},capabilities});
 } else if(r.method==='notifications/initialized'){if(transport==='stdio')idle();}
 else if(r.method==='notifications/roots/list_changed'){counts.rootChanges++;}
 else if(r.method==='notifications/progress'){counts.progress.push(r.params);}
  else if(r.method==='ping'){
   if(r.params.taskProgress)progress(taskToken,4,4);
   if(r.params.emitInvalid)idle(true);
   if(r.params.askRoots)emit({jsonrpc:'2.0',id:mode==='roots-integer'?9:'roots-request',method:'roots/list',params:mode==='roots-only'?{}:{_meta:{progressToken:mode==='roots-integer'?7:'host-token'}}});
  reply(counts);
  } else if(r.id==='roots-request'||r.id===9){
  if(r.result){counts.rootReplies++;counts.roots=r.result.roots;notification('notifications/message',{level:'info',data:'roots-received'});}
  else if(r.error){counts.rootErrors++;notification('notifications/message',{level:'info',data:'roots-rejected'});}
  } else if(r.method==='tasks/get'){
   if(transport==='sse')reply.progress(taskToken,2,3);else progress(taskToken,2,3);
   setTimeout(()=>{reply(task('completed'));if(transport==='stdio')progress(taskToken,3,3);else setTimeout(()=>progress(taskToken,3,3),30);},30);
  } else if(r.method==='resources/subscribe'||r.method==='resources/unsubscribe'){
  notification('notifications/resources/updated',{uri:r.params.uri});reply({});notification('notifications/resources/updated',{uri:r.params.uri});
 } else if(r.method==='tools/call'){
  counts.calls++;
  const token=r.params._meta?.progressToken;
   if(mode==='task-progress'){
    taskToken=token;taskTime=new Date().toISOString();reply({task:task('working')});progress(token,1,3);
   }else if(mode==='wrong-stream'){
    held.push([r,reply]);
    if(held.length===2)for(const [r,reply]of held){reply.progress(held.find(([other])=>other.id!==r.id)[0].params._meta.progressToken);reply.progress(r.params._meta.progressToken);reply({content:[]});}
   }else if(mode==='concurrent'){
   held.push([r,reply]);
   if(held.length===2)for(const [r,reply]of held.reverse()){progress(r.params._meta.progressToken,1,2);reply({content:[],name:r.params.name});}
  }else if(mode==='deadline'){
   let n=0;timer=setInterval(()=>progress(token,++n,1000),10);
  }else{
   progress(typeof token==='number'?String(token):7,0,3);
   progress(token,0,3);progress(token,1,3);progress(token,.5,3);progress(token,2,2);
   progress(token,4,5);progress(token,5);progress(token,6);
   setTimeout(()=>{reply({content:[]});if(transport==='stdio')progress(token,7,7);else setTimeout(()=>progress(token,7,7),30);},50);
  }
 } else if(['resources/read','prompts/get'].includes(r.method)){counts.content++;reply({});}
 else if(r.method==='notifications/cancelled'){}
 else process.exit(63);
}
if(transport==='stdio'){
 for await(const line of createInterface({input:process.stdin})){
  const r=JSON.parse(line);request(r,result=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\n'));
 }
 clearInterval(timer);
}else{
 const server=createServer(async(req,res)=>{
  if(req.method==='DELETE'){res.writeHead(204).end();return;}
  if(req.method==='GET'){eventStream=res;res.writeHead(200,{'content-type':'text/event-stream','mcp-session-id':'fixture'});res.flushHeaders();idle();return;}
  const chunks=[];for await(const c of req)chunks.push(c);const r=JSON.parse(Buffer.concat(chunks).toString('utf8'));
  if(r.id===undefined||r.method===undefined){request(r,()=>{});res.writeHead(202).end();return;}
   const reply=result=>{
    if(transport==='sse'&&r.method!=='initialize'){if(!res.headersSent)res.writeHead(200,{'content-type':'text/event-stream','mcp-session-id':'fixture'});res.end('data: '+JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\n\n');}
   else{res.writeHead(200,{'content-type':'application/json','mcp-session-id':'fixture'});res.end(JSON.stringify({jsonrpc:'2.0',id:r.id,result}));}
   };
   reply.progress=(token,progress=1,total=2)=>{if(!res.headersSent)res.writeHead(200,{'content-type':'text/event-stream','mcp-session-id':'fixture'});res.write('data: '+JSON.stringify({jsonrpc:'2.0',method:'notifications/progress',params:{progressToken:token,progress,total}})+'\n\n');};
   request(r,reply);
 });server.listen(0,'127.0.0.1',()=>process.stdout.write(String(server.address().port)+'\n'));
}
"#;

fn cancel() -> Arc<McpCancellation> {
    Arc::new(McpCancellation::new())
}
fn document(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
struct Events(tokio::sync::mpsc::UnboundedSender<McpClientEvent>);
impl McpEventHandler for Events {
    fn receive<'a>(&'a self, event: McpClientEvent) -> McpFuture<'a, ()> {
        Box::pin(async move {
            self.0.send(event).unwrap();
            Ok(())
        })
    }
}
struct Permit {
    deny_progress: bool,
}
impl McpClientAuthorizer for Permit {
    fn authorize(
        &self,
        _: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), htlk_runtime::failure::RuntimeFailure> {
        if self.deny_progress && action.action==McpClientAction::Deliver
            && action.response.is_some_and(|document|matches!(document.value(),Value::Map(fields)if fields.get("method")==Some(&Value::Text("notifications/progress".into())))) {
            Err(htlk_runtime::failure::RuntimeFailure::new(FailureCode::Policy))
        } else { Ok(()) }
    }
}
struct Roots {
    calls: AtomicUsize,
    bad: bool,
    stream: bool,
    integer: bool,
    denied: bool,
    reporter: Mutex<Option<McpProgressReporter>>,
}
impl McpClientHandler for Roots {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(reporter) = context.progress.as_ref() {
                assert_eq!(
                    reporter.token(),
                    &if self.integer {
                        McpProgressToken::integer(7)
                    } else {
                        McpProgressToken::string("host-token".into())
                    }
                );
                assert_eq!(
                    context.request_id,
                    if self.integer {
                        McpRequestId::Integer(9)
                    } else {
                        McpRequestId::String("roots-request".into())
                    }
                );
                *self.reporter.lock().unwrap() = Some(reporter.clone());
                let wrong = McpProgress::new(
                    if self.integer {
                        McpProgressToken::string("7".into())
                    } else {
                        McpProgressToken::integer(7)
                    },
                    0.,
                    None,
                    None,
                    &Limits::default(),
                )
                .unwrap();
                assert_eq!(
                    reporter.report(wrong).await.unwrap_err().code(),
                    FailureCode::Input
                );
                if self.denied {
                    let update = McpProgress::new(
                        reporter.token().clone(),
                        0.,
                        Some(1.),
                        None,
                        &Limits::default(),
                    )
                    .unwrap();
                    let refused = reporter.report(update).await.unwrap();
                    assert_eq!(refused.delivery, McpDelivery::NotSent);
                    assert_eq!(refused.result.unwrap_err().code(), FailureCode::Policy);
                } else if self.stream {
                    for n in 0..100 {
                        let update = McpProgress::new(
                            reporter.token().clone(),
                            n as f64,
                            Some(100.),
                            None,
                            &Limits::default(),
                        )
                        .unwrap();
                        if !matches!(
                            reporter.report(update).await,
                            Ok(McpNotificationReply { result: Ok(()), .. })
                        ) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } else {
                    for (progress, total, valid) in [
                        (0., 2., true),
                        (1., 1., false),
                        (1., 3., true),
                        (3., 3., true),
                    ] {
                        let update = McpProgress::new(
                            reporter.token().clone(),
                            progress,
                            Some(total),
                            None,
                            &Limits::default(),
                        )
                        .unwrap();
                        let sent = reporter.report(update).await.unwrap();
                        assert_eq!(sent.result.is_ok(), valid);
                        if !valid {
                            assert_eq!(sent.delivery, McpDelivery::NotSent);
                        }
                    }
                }
            }
            McpServiceOutcome::completed(document(if self.bad {
                r#"{"roots":[{"uri":"https://example.test/not-a-root"}]}"#
            } else {
                r#"{"roots":[{"uri":"file:///advisory-root","name":"workspace"}]}"#
            }))
        })
    }
}
struct Peer {
    child: Option<std::process::Child>,
    endpoint: McpEndpoint,
    transport: McpTransport,
}
impl Peer {
    fn start(transport: &str, mode: &str) -> Self {
        let arguments = vec![
            "--input-type=module".into(),
            "-e".into(),
            PEER.into(),
            transport.into(),
            mode.into(),
        ];
        if transport == "stdio" {
            return Self {
                child: None,
                endpoint: McpEndpoint::Stdio {
                    program: "node".into(),
                    arguments,
                    environment: BTreeMap::new(),
                },
                transport: McpTransport::Stdio,
            };
        }
        use std::io::BufRead;
        let mut child = std::process::Command::new("node")
            .args(arguments)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        Self {
            child: Some(child),
            endpoint: McpEndpoint::Http {
                url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                headers: BTreeMap::new(),
            },
            transport: McpTransport::StreamableHttp,
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
async fn open(
    peer: &Peer,
    config: McpClientConfig,
    owner: McpOwner,
) -> (NativeMcpClient, McpClientSession) {
    let client = NativeMcpClient::new(config).unwrap();
    let session = client
        .open(
            owner,
            ServerIdentity::new(
                "notifications".into(),
                peer.transport,
                "notifications".into(),
                "1".into(),
                &Limits::default(),
            )
            .unwrap(),
            peer.endpoint.clone(),
            Limits::default(),
            cancel(),
        )
        .await
        .unwrap();
    (client, session)
}
fn owner() -> McpOwner {
    McpOwner::new(
        HostContext::new("alice".into(), &Limits::default()).unwrap(),
        McpOwnerScope::Host("events".into()),
        1,
        &Limits::default(),
    )
    .unwrap()
}
fn config(events: Option<Arc<dyn McpEventHandler>>) -> McpClientConfig {
    McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), events).unwrap()
}
async fn typed(
    receive: &mut tokio::sync::mpsc::UnboundedReceiver<McpClientEvent>,
) -> McpClientEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = receive.recv().await.unwrap();
            if event.notification.is_some() {
                return event;
            }
        }
    })
    .await
    .unwrap()
}
async fn stats(session: &McpClientSession, ask: bool) -> JsonDocument {
    session
        .request(
            "ping",
            document(if ask { r#"{"askRoots":true}"# } else { "{}" }),
            cancel(),
        )
        .await
        .result
        .unwrap()
        .unwrap()
}
fn field<'a>(document: &'a JsonDocument, key: &str) -> &'a Value {
    let Value::Map(fields) = document.value() else {
        panic!()
    };
    fields.get(key).unwrap()
}

#[test]
fn idle_notifications_are_typed_gated_and_never_acquire_content() {
    for transport in ["stdio", "json", "sse"] {
        actix::System::new().block_on(async{
  let peer=Peer::start(transport,"normal");let (events,mut receive)=tokio::sync::mpsc::unbounded_channel();
  let (client,session)=open(&peer,config(Some(Arc::new(Events(events)))),owner()).await;
  let event=typed(&mut receive).await;assert!(matches!(event.notification,Some(McpNotification::ResourceUpdated{subscription:McpResourceUpdateState::Unknown,..})));
  for category in [McpCatalogChanged::Tools,McpCatalogChanged::Prompts,McpCatalogChanged::ResourcesAndTemplates]{assert!(matches!(typed(&mut receive).await.notification,Some(McpNotification::CatalogChanged(actual))if actual==category));}
   let Some(McpNotification::Logging{level:McpLoggingLevel::Notice,logger:Some(logger),data})=typed(&mut receive).await.notification else{panic!()};
   assert_eq!(logger,"fixture");assert_eq!(field(&data,"text"),&Value::Text("protected log".into()));
  let counts=stats(&session,false).await;assert_eq!(field(&counts,"content"),&Value::Integer(0));assert_eq!(field(&counts,"calls"),&Value::Integer(0));
  client.shutdown().await.unwrap();
 });
    }
    for mode in ["unsupported", "bits"] {
        actix::System::new().block_on(async {
            let peer = Peer::start("stdio", mode);
            let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
            let (client, session) =
                open(&peer, config(Some(Arc::new(Events(events)))), owner()).await;
            for _ in 0..5 {
                assert!(matches!(
                    typed(&mut receive).await.notification,
                    Some(McpNotification::Ignored(
                        McpNotificationIgnored::UnsupportedCapability
                    ))
                ));
            }
            assert_eq!(
                field(&stats(&session, false).await, "content"),
                &Value::Integer(0)
            );
            client.shutdown().await.unwrap();
        });
    }
}

#[test]
fn resource_updates_preserve_subscribe_and_unsubscribe_races_without_reads() {
    for transport in ["stdio", "json", "sse"] {
        actix::System::new().block_on(async {
            let peer = Peer::start(transport, "normal");
            let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
            let (client, session) =
                open(&peer, config(Some(Arc::new(Events(events)))), owner()).await;
            for _ in 0..5 {
                typed(&mut receive).await;
            }
            let limits = McpServiceLimits::new(4096, 2000).unwrap();
            for subscribing in [true, false] {
                if subscribing {
                    session
                        .subscribe_resource("test://resource", limits, cancel())
                        .await
                        .unwrap();
                } else {
                    session
                        .unsubscribe_resource("test://resource", limits, cancel())
                        .await
                        .unwrap();
                }
                let states = if subscribing {
                    [
                        McpResourceUpdateState::Subscribing,
                        McpResourceUpdateState::Subscribed,
                    ]
                } else {
                    [
                        McpResourceUpdateState::Unsubscribing,
                        McpResourceUpdateState::Unsubscribed,
                    ]
                };
                for index in 0..2 {
                    let event = typed(&mut receive).await;
                    let Some(McpNotification::ResourceUpdated { uri, subscription }) =
                        event.notification
                    else {
                        panic!()
                    };
                    assert_eq!(uri, "test://resource");
                    // Stdio fixes ordering with ACK. Independent HTTP GET and
                    // POST streams expose whichever state is known at receipt.
                    if transport == "stdio" {
                        assert_eq!(subscription, states[index]);
                    } else {
                        assert!(states.contains(&subscription));
                    }
                }
            }
            assert_eq!(
                field(&stats(&session, false).await, "content"),
                &Value::Integer(0)
            );
            client.shutdown().await.unwrap();
        });
    }
}

#[test]
fn incoming_progress_uses_only_actual_tokens_and_monotonic_totals() {
    for transport in ["stdio", "json", "sse"] {
        actix::System::new().block_on(async {
            let peer = Peer::start(transport, "normal");
            let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
            let (client, session) =
                open(&peer, config(Some(Arc::new(Events(events)))), owner()).await;
            for _ in 0..5 {
                typed(&mut receive).await;
            }
            let reply = session
                .request(
                    "tools/call",
                    document(r#"{"name":"progress","_meta":{"progressToken":7}}"#),
                    cancel(),
                )
                .await;
            assert!(reply.result.is_ok());
            let request_id = reply.request_id.unwrap();
            for (position, expected) in [
                None,
                Some((0., 3.)),
                Some((1., 3.)),
                None,
                None,
                Some((4., 5.)),
                Some((5., 5.)),
                None,
                None,
            ]
            .into_iter()
            .enumerate()
            {
                let event = typed(&mut receive).await;
                match (event.notification, expected) {
                    (
                        Some(McpNotification::Progress {
                            request_id: actual,
                            origin: None,
                            update,
                        }),
                        Some((progress, total)),
                    ) => {
                        assert_eq!(actual, request_id);
                        assert_eq!(update.token(), &McpProgressToken::integer(7));
                        assert_eq!(update.progress(), progress);
                        assert_eq!(update.total(), Some(total));
                    }
                    (Some(McpNotification::Ignored(reason)), None) => assert_eq!(
                        reason,
                        if position == 0 || position == 8 {
                            McpNotificationIgnored::UnknownProgressToken
                        } else {
                            McpNotificationIgnored::NonMonotonicProgress
                        }
                    ),
                    _ => panic!("unexpected progress disposition at {position}"),
                }
            }
            let reused = session
                .request(
                    "tools/call",
                    document(r#"{"name":"duplicate","_meta":{"progressToken":7}}"#),
                    cancel(),
                )
                .await;
            assert_eq!(reused.delivery, McpDelivery::NotSent);
            assert_eq!(reused.result.unwrap_err().code(), FailureCode::Input);
            assert_eq!(
                field(&stats(&session, false).await, "calls"),
                &Value::Integer(1)
            );
            client.shutdown().await.unwrap();
        });
    }
}

#[test]
fn concurrent_integer_and_string_tokens_never_alias_or_guess_an_origin() {
    actix::System::new().block_on(async {
        let peer = Peer::start("stdio", "concurrent");
        let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let (client, session) = open(&peer, config(Some(Arc::new(Events(events)))), owner()).await;
        for _ in 0..5 {
            typed(&mut receive).await;
        }
        let (integer, string) = tokio::join!(
            session.request(
                "tools/call",
                document(r#"{"name":"integer","_meta":{"progressToken":7}}"#),
                cancel()
            ),
            session.request(
                "tools/call",
                document(r#"{"name":"string","_meta":{"progressToken":"7"}}"#),
                cancel()
            )
        );
        for _ in 0..2 {
            let event = typed(&mut receive).await;
            let Some(McpNotification::Progress {
                request_id,
                origin,
                update,
            }) = event.notification
            else {
                panic!()
            };
            assert!(origin.is_none());
            assert_eq!(
                Some(request_id),
                if update.token() == &McpProgressToken::integer(7) {
                    integer.request_id.clone()
                } else {
                    assert_eq!(update.token(), &McpProgressToken::string("7".into()));
                    string.request_id.clone()
                }
            );
        }
        assert_eq!(
            field(&stats(&session, false).await, "calls"),
            &Value::Integer(2)
        );
        client.shutdown().await.unwrap();
    });
}

#[test]
fn progress_never_extends_the_actual_request_deadline() {
    actix::System::new().block_on(async {
        let peer = Peer::start("stdio", "deadline");
        let (client, session) = open(&peer, config(None), owner()).await;
        let start = std::time::Instant::now();
        let reply = session
            .invoke(McpRequest {
                response_limits: Limits::default(),
                cancellation: cancel(),
                cancellation_grace_ms: 10,
                method: "tools/call".into(),
                params: document(r#"{"name":"held","_meta":{"progressToken":"deadline"}}"#)
                    .value()
                    .clone(),
                timeout_ms: 150,
                max_response_bytes: 4096,
                idempotency_key: None,
            })
            .await;
        assert_eq!(reply.delivery, McpDelivery::PossiblySent);
        assert_eq!(reply.result.unwrap_err().code(), FailureCode::McpTimeout);
        assert!(start.elapsed() < Duration::from_secs(2));
        client.shutdown().await.unwrap();
    });
}

#[test]
fn malformed_negotiated_notifications_fail_pinned_admission() {
    for category in [
        "resource",
        "tools",
        "prompts",
        "resources",
        "log",
        "progress",
        "total",
    ] {
        actix::System::new().block_on(async {
            let peer = Peer::start("stdio", &format!("invalid-{category}"));
            let (client, session) = open(&peer, config(None), owner()).await;
            let _ = session
                .request("ping", document(r#"{"emitInvalid":true}"#), cancel())
                .await;
            tokio::time::timeout(Duration::from_secs(2), async {
                while session.ended().is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(session.ended(), Some(McpSessionEnd::Protocol));
            client.shutdown().await.unwrap();
        });
    }
}

async fn root_config(
    mode: &str,
    events: Option<Arc<dyn McpEventHandler>>,
    limits: McpClientLimits,
) -> (McpClientConfig, McpOwner, Arc<Roots>) {
    let store = SqliteStore::in_memory(Default::default()).await.unwrap();
    let persistence = McpClientPersistence::new(store, Arc::new(htlk_runtime::clock::SystemClock));
    let owner = persistence
        .acquire_owner(
            &HostContext::new("alice".into(), &Limits::default()).unwrap(),
            "roots",
            30_000,
        )
        .await
        .unwrap();
    let roots = Arc::new(Roots {
        calls: AtomicUsize::new(0),
        bad: mode == "bad-root",
        stream: mode == "roots-deadline",
        integer: mode == "roots-integer",
        denied: mode == "roots-denied",
        reporter: Mutex::new(None),
    });
    let mut handlers: BTreeMap<McpClientService, Arc<dyn McpClientHandler>> = BTreeMap::from([(
        McpClientService::Roots,
        roots.clone() as Arc<dyn McpClientHandler>,
    )]);
    if mode != "roots-only" {
        handlers.insert(McpClientService::RootsChanged, roots.clone());
    }
    (
        McpClientConfig::new(limits, handlers, events)
            .unwrap()
            .with_persistence(persistence)
            .with_authorizer(Arc::new(Permit {
                deny_progress: mode == "roots-denied",
            })),
        owner,
        roots,
    )
}

#[test]
fn roots_change_inquiry_and_outgoing_callback_progress_work_on_every_transport() {
    for transport in ["stdio", "json", "sse"] {
        for mode in ["roots", "roots-integer"] {
            actix::System::new().block_on(async {
                let peer = Peer::start(transport, mode);
                let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
                let (config, owner, roots) = root_config(
                    mode,
                    Some(Arc::new(Events(events))),
                    McpClientLimits::default(),
                )
                .await;
                let (client, session) = open(&peer, config, owner).await;
                if transport != "stdio" {
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while session.server_events_available() != Some(true) {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
                }
                let changed = session.roots_changed(cancel()).await.unwrap();
                assert_eq!(changed.delivery, McpDelivery::PossiblySent);
                changed.result.unwrap();
                assert_eq!(roots.calls.load(Ordering::SeqCst), 0);
                let before = stats(&session, false).await;
                assert_eq!(field(&before, "rootChanges"), &Value::Integer(1));
                assert_eq!(field(&before, "rootReplies"), &Value::Integer(0));
                stats(&session, true).await;
                // The peer confirms receipt only after observing the actual roots reply.
                loop {
                    if matches!(
                        typed(&mut receive).await.notification,
                        Some(McpNotification::Logging {
                            level: McpLoggingLevel::Info,
                            ..
                        })
                    ) {
                        break;
                    }
                }
                assert_eq!(roots.calls.load(Ordering::SeqCst), 1);
                let after = stats(&session, false).await;
                assert_eq!(field(&after, "rootReplies"), &Value::Integer(1));
                assert_eq!(field(&after, "content"), &Value::Integer(0));
                let Value::Array(progress) = field(&after, "progress") else {
                    panic!()
                };
                assert_eq!(progress.len(), 3);
                for message in progress {
                    let Value::Map(fields) = message else {
                        panic!()
                    };
                    assert_eq!(
                        fields.get("progressToken"),
                        Some(&if mode == "roots-integer" {
                            Value::Integer(7)
                        } else {
                            Value::Text("host-token".into())
                        })
                    );
                }
                let late = roots.reporter.lock().unwrap().clone().unwrap();
                let update =
                    McpProgress::new(late.token().clone(), 4., Some(4.), None, &Limits::default())
                        .unwrap();
                let rejected = late.report(update).await.unwrap();
                assert_eq!(rejected.delivery, McpDelivery::NotSent);
                assert!(rejected.result.is_err());
                client.shutdown().await.unwrap();
            });
        }
    }
}

#[test]
fn roots_changes_are_gated_bounded_and_bad_roots_never_publish() {
    actix::System::new().block_on(async {
        let peer = Peer::start("stdio", "roots-only");
        let (config, owner, roots) =
            root_config("roots-only", None, McpClientLimits::default()).await;
        let (client, session) = open(&peer, config, owner).await;
        assert_eq!(
            session.roots_changed(cancel()).await.unwrap_err().code(),
            FailureCode::Profile
        );
        assert_eq!(roots.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            field(&stats(&session, false).await, "rootChanges"),
            &Value::Integer(0)
        );
        stats(&session, true).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while field(&stats(&session, false).await, "rootReplies") != &Value::Integer(1) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(roots.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            field(&stats(&session, false).await, "progress"),
            &Value::Array(vec![])
        );
        client.shutdown().await.unwrap();
        let peer = Peer::start("stdio", "roots-denied");
        let (config, owner, _) =
            root_config("roots-denied", None, McpClientLimits::default()).await;
        let (client, session) = open(&peer, config, owner).await;
        stats(&session, true).await;
        let after = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let counts = stats(&session, false).await;
                if field(&counts, "rootReplies") == &Value::Integer(1) {
                    break counts;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(field(&after, "progress"), &Value::Array(vec![]));
        assert_eq!(field(&after, "rootErrors"), &Value::Integer(0));
        client.shutdown().await.unwrap();
        let peer = Peer::start("stdio", "quiet");
        let (config, owner, _) = root_config(
            "quiet",
            None,
            McpClientLimits {
                messages_per_session: 2,
                ..Default::default()
            },
        )
        .await;
        let (client, session) = open(&peer, config, owner).await;
        for _ in 0..2 {
            session
                .roots_changed(cancel())
                .await
                .unwrap()
                .result
                .unwrap();
        }
        let refused = session.roots_changed(cancel()).await.unwrap();
        assert_eq!(refused.delivery, McpDelivery::NotSent);
        assert_eq!(refused.result.unwrap_err().code(), FailureCode::Budget);
        assert_eq!(
            field(&stats(&session, false).await, "rootChanges"),
            &Value::Integer(2)
        );
        client.shutdown().await.unwrap();
        let peer = Peer::start("stdio", "bad-root");
        let (config, owner, roots) =
            root_config("bad-root", None, McpClientLimits::default()).await;
        let (client, session) = open(&peer, config, owner).await;
        stats(&session, true).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while field(&stats(&session, false).await, "rootErrors") != &Value::Integer(1) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(roots.calls.load(Ordering::SeqCst), 1);
        assert_eq!(session.ended(), None);
        assert_eq!(
            field(&stats(&session, false).await, "rootReplies"),
            &Value::Integer(0)
        );
        client.shutdown().await.unwrap();
    });
}

#[test]
fn progress_on_the_wrong_http_response_stream_cannot_claim_another_request() {
    actix::System::new().block_on(async {
        let peer = Peer::start("sse", "wrong-stream");
        let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let (client, session) = open(&peer, config(Some(Arc::new(Events(events)))), owner()).await;
        for _ in 0..5 {
            typed(&mut receive).await;
        }
        let (one, two) = tokio::join!(
            session.request(
                "tools/call",
                document(r#"{"name":"one","_meta":{"progressToken":"one"}}"#),
                cancel()
            ),
            session.request(
                "tools/call",
                document(r#"{"name":"two","_meta":{"progressToken":"two"}}"#),
                cancel()
            )
        );
        assert!(one.result.is_ok());
        assert!(two.result.is_ok());
        let mut matched = 0;
        let mut wrong = 0;
        for _ in 0..4 {
            match typed(&mut receive).await.notification.unwrap() {
                McpNotification::Progress {
                    request_id,
                    origin: None,
                    update,
                } => {
                    assert_eq!(
                        Some(request_id),
                        if update.token() == &McpProgressToken::string("one".into()) {
                            one.request_id.clone()
                        } else {
                            two.request_id.clone()
                        }
                    );
                    matched += 1;
                }
                McpNotification::Ignored(McpNotificationIgnored::WrongProgressRequest) => {
                    wrong += 1
                }
                McpNotification::Ignored(McpNotificationIgnored::UnknownProgressToken) => {}
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(matched, 2);
        assert!(wrong >= 1);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn outgoing_callback_progress_does_not_extend_its_deadline() {
    actix::System::new().block_on(async {
        let peer = Peer::start("stdio", "roots-deadline");
        let (config, owner, roots) = root_config(
            "roots-deadline",
            None,
            McpClientLimits {
                timeout_ms: 180,
                cancellation_grace_ms: 20,
                ..Default::default()
            },
        )
        .await;
        let (client, session) = open(&peer, config, owner).await;
        let started = std::time::Instant::now();
        stats(&session, true).await;
        let after = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let counts = stats(&session, false).await;
                if field(&counts, "rootErrors") == &Value::Integer(1) {
                    break counts;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(roots.calls.load(Ordering::SeqCst), 1);
        assert_eq!(field(&after, "rootReplies"), &Value::Integer(0));
        let Value::Array(progress) = field(&after, "progress") else {
            panic!()
        };
        assert!(progress.len() >= 2 && progress.len() < 50);
        let reporter = roots.reporter.lock().unwrap().clone().unwrap();
        let update = McpProgress::new(
            reporter.token().clone(),
            100.,
            Some(100.),
            None,
            &Limits::default(),
        )
        .unwrap();
        let result = reporter.report(update).await;
        assert!(matches!(
            result,
            Err(_)
                | Ok(McpNotificationReply {
                    delivery: McpDelivery::NotSent,
                    result: Err(_)
                })
        ));
        client.shutdown().await.unwrap();
    });
}

#[test]
fn task_progress_retains_the_actual_creation_request_until_terminal_observation() {
    for durable in [false, true] {
        for transport in ["stdio", "json", "sse"] {
            actix::System::new().block_on(async{
        let peer=Peer::start(transport,"task-progress");
        let (events,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let events=Some(Arc::new(Events(events)) as Arc<dyn McpEventHandler>);
        let (config,owner)=if durable{let (config,owner,_)=root_config("task-progress",events,McpClientLimits::default()).await;(config,owner)}else{(config(events),owner())};
        let (client,session)=open(&peer,config,owner).await;
        for _ in 0..5{typed(&mut receive).await;}
        let limits=McpTaskLimits::new(4096,2000,8,8,64*1024,1).unwrap();
        let prepared=session.prepare_tool_task_call(&document(r#"{"name":"task","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}"#),document(r#"{"name":"task","_meta":{"progressToken":"whole-task"}}"#),McpToolTaskMode::Task{ttl_ms:Some(5000)}).unwrap();
        let accepted=prepared.dispatch(limits,cancel()).await.unwrap();
        let request=accepted.reply.request_id.clone().unwrap();
        let McpToolTaskAcceptance::Accepted(mut remote)=accepted.value.unwrap().unwrap() else{panic!()};
        let event=typed(&mut receive).await;
        let observed=format!("{:?}",event.notification);
        assert!(matches!(event.notification,Some(McpNotification::Progress{request_id,origin:None,update})if request_id==request&&update.progress()==1.),"{durable}:{transport}: {observed}");
        let terminal=remote.get(limits,cancel()).await.unwrap();assert_eq!(terminal.value.unwrap().unwrap().status(),McpTaskStatus::Completed);
        let event=typed(&mut receive).await;
        assert!(matches!(event.notification,Some(McpNotification::Progress{request_id,origin:None,update})if request_id==request&&update.progress()==2.));
        assert!(matches!(typed(&mut receive).await.notification,Some(McpNotification::Ignored(McpNotificationIgnored::UnknownProgressToken))));
        assert_eq!(field(&stats(&session,false).await,"calls"),&Value::Integer(1));
        assert_eq!(field(&stats(&session,false).await,"content"),&Value::Integer(0));
        client.shutdown().await.unwrap();
    });
        }
    }
}
