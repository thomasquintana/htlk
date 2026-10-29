//! Actual bounded stdio and Streamable HTTP sessions, including hostile raw ingress.
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use htlk_runtime::{
    failure::FailureCode,
    mcp::{McpAdapter, McpDelivery, McpEndpoint, McpRequest, NativeMcpAdapter},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const DESCRIPTOR: &str =
    r#"{"name":"lookup","inputSchema":{"type":"object"},"outputSchema":{"type":"object"}}"#;
const RESULT: &str = r#"{"content":[],"structuredContent":{"answer":42}}"#;
const UTF8_RESULT: &str =
    r#"{"content":[{"type":"text","text":"café"}],"structuredContent":{"answer":42}}"#;
fn run_context() -> htlk_runtime::mcp::McpRunContext {
    let limits = Limits::default();
    htlk_runtime::mcp::McpRunContext {
        run_id: htlk_runtime::identity::RunId::new("wire-test".into(), &limits).unwrap(),
        host: htlk_runtime::HostContext::new("test".into(), &limits).unwrap(),
        fence: 1,
        cancellation: Arc::new(htlk_runtime::mcp::McpCancellation::new()),
    }
}
fn binding(transport: McpTransport) -> McpBinding {
    let l = Limits::default();
    McpBinding::new(
        ServerIdentity::new("test".into(), transport, "fixture".into(), "1".into(), &l).unwrap(),
        JsonDocument::new(DESCRIPTOR.as_bytes(), &l)
            .unwrap()
            .digest(),
        McpBindingKind::Tool {
            name: "lookup".into(),
            input_schema: JsonDocument::new(br#"{"type":"object"}"#, &l)
                .unwrap()
                .digest(),
            output_schema: Some(
                JsonDocument::new(br#"{"type":"object"}"#, &l)
                    .unwrap()
                    .digest(),
            ),
            result_policy: Box::default(),
        },
        &l,
    )
    .unwrap()
}
fn request(max: usize) -> McpRequest {
    McpRequest {
        response_limits: Limits::default(),
        cancellation: Arc::new(htlk_runtime::mcp::McpCancellation::new()),
        cancellation_grace_ms: 100,
        method: "tools/call".into(),
        params: JsonDocument::new(
            br#"{"name":"lookup","arguments":{"query":"question"}}"#,
            &Limits::default(),
        )
        .unwrap()
        .value()
        .clone(),
        timeout_ms: 2000,
        max_response_bytes: max,
        idempotency_key: None,
    }
}

#[test]
fn native_discovery_and_request_preflight_fail_without_dispatch() {
    actix::System::new().block_on(async {
        assert!(NativeMcpAdapter::new(BTreeMap::new(), 0).is_err());
        let adapter = NativeMcpAdapter::new(BTreeMap::new(), 1000).unwrap();
        let binding = binding(McpTransport::Stdio);
        let context = run_context();
        let limits = Limits::default();
        assert_eq!(adapter.descriptor(&binding, &context, &limits).await.unwrap_err().code(), FailureCode::McpIdentity);
        for endpoint in [McpEndpoint::Http{url:"invalid://unused".into(),headers:BTreeMap::new()},McpEndpoint::Stdio{program:"/htlk-test-nonexistent-program".into(),arguments:vec![],environment:BTreeMap::new()},McpEndpoint::Stdio{program:"/bin/sh".into(),arguments:vec!["-c".into(),"IFS= read -r line".into()],environment:BTreeMap::new()}] {
            let expected=if matches!(endpoint,McpEndpoint::Http{..}){FailureCode::McpIdentity}else{FailureCode::McpTransport};
            let isolated=NativeMcpAdapter::new(BTreeMap::from([("test".into(),endpoint)]),1000).unwrap();
            assert_eq!(isolated.descriptor(&binding,&context,&limits).await.unwrap_err().code(),expected);
            isolated.shutdown().await.unwrap();
            isolated.shutdown().await.unwrap();
            assert!(matches!(isolated.connect(&binding,&context,&limits).await,Err(error) if error.code()==FailureCode::McpTransport));
        }
        // Argument/deadline preflight must precede even endpoint resolution.
        let cancelled = request(4096);
        cancelled.cancellation.cancel();
        assert_eq!(adapter.invoke(&binding, &context, cancelled).await.unwrap_err().code(), FailureCode::Cancelled);
        for grace in [false, true] {
            let mut invalid = request(4096);
            if grace { invalid.cancellation_grace_ms = 0; } else { invalid.timeout_ms = 0; }
            assert_eq!(adapter.invoke(&binding, &context, invalid).await.unwrap_err().code(), FailureCode::Profile);
        }
        context.cancellation.cancel();
        assert!(matches!(adapter.connect(&binding, &context, &limits).await, Err(error) if error.code() == FailureCode::Cancelled));
        adapter.shutdown().await.unwrap();
    });
}

#[test]
fn stdio_reverse_requests_and_live_preflight_preserve_authority_and_not_sent_evidence() {
    actix::System::new().block_on(async {
        let script = r#"
          import {createInterface} from 'node:readline';
          const send=v=>process.stdout.write(JSON.stringify(v)+'\n');
          let initialize;
          for await (const line of createInterface({input:process.stdin})) {
            const r=JSON.parse(line);
            if(r.method==='initialize') {initialize=r; const packet=JSON.parse(process.env.PACKET);for(let i=0;i<(packet.params?.flood ? 1025:1);i++)send(packet); if(process.env.EXPECTED==='notify') send({jsonrpc:'2.0',id:r.id,result:JSON.parse(process.env.INIT)}); continue;}
            if(r.id===-7 && !r.method) {
              const code=process.env.EXPECTED;
              if(code==='ok' ? !r.result : r.error?.code!==Number(code)) process.exit(21);
              send({jsonrpc:'2.0',id:initialize.id,result:JSON.parse(process.env.INIT)}); continue;
            }
            if(r.method==='notifications/initialized') continue;
            if(r.method==='tools/list') {send({jsonrpc:'2.0',id:r.id,result:{tools:[JSON.parse(process.env.DESCRIPTOR)]}});continue;}
            if(r.method==='tools/call') {send({jsonrpc:'2.0',id:r.id,result:{content:[{type:'text',text:'private-payload'}]}});continue;}
            process.exit(22);
          }
        "#;
        for (packet, expected) in [
            (r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#,Some("notify")),
            (r#"{"jsonrpc":"2.0","id":-7,"method":"ping","params":{}}"#, Some("ok")),
            (r#"{"jsonrpc":"2.0","id":-7,"method":"ping","params":false}"#, Some("-32602")),
            (r#"{"jsonrpc":"2.0","id":-7,"method":"sampling/createMessage","params":{}}"#, Some("-32601")),
            (r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, None),
            (r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"flood":true}}"#,None),
            (r#"{"jsonrpc":"2.0","id":-7,"method":"ping","result":{}}"#, None),
        ] {
            let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Stdio{program:"node".into(),arguments:vec!["--input-type=module".into(),"-e".into(),script.into()],environment:BTreeMap::from([("PACKET".into(),packet.into()),("INIT".into(),initialize().into()),("DESCRIPTOR".into(),DESCRIPTOR.into()),("EXPECTED".into(),expected.unwrap_or("bad").into())])})]),2000).unwrap();
            let binding=binding(McpTransport::Stdio);
            let context=run_context();
            let limits=Limits::default();
            let connected=adapter.connect(&binding,&context,&limits).await;
            if expected.is_none() {
                assert!(matches!(connected,Err(error) if error.code()==FailureCode::McpProtocol));
            } else {
                let mut connection=connected.unwrap();
                for mode in ["cancelled","timeout","depth","bytes"] {
                    let mut isolated=run_context();
                    isolated.run_id=htlk_runtime::identity::RunId::new(format!("preflight-{mode}"),&limits).unwrap();
                    let mut preflight=adapter.connect(&binding,&isolated,&limits).await.unwrap();
                    let mut invalid=request(4096);
                    let code=match mode {
                        "cancelled"=>{invalid.cancellation.cancel();FailureCode::Cancelled},
                        "timeout"=>{invalid.timeout_ms=0;FailureCode::Profile},
                        "depth"=>{invalid.response_limits.max_depth=129;FailureCode::Profile},
                        _=>{invalid.max_response_bytes=1;FailureCode::Input},
                    };
                    let outcome=preflight.invoke(invalid).await;
                    assert_eq!(outcome.delivery,McpDelivery::NotSent,"{mode}");
                    assert_eq!(outcome.result.unwrap_err().code(),code,"{mode}");
                    let rejected=preflight.invoke(request(4096)).await;
                    assert_eq!(rejected.delivery,McpDelivery::NotSent);
                    assert_eq!(rejected.result.unwrap_err().code(),FailureCode::McpTransport);
                    preflight.close().await.unwrap();
                }
                let outcome=connection.invoke(request(4096)).await;
                assert_eq!(outcome.delivery,McpDelivery::ResponseReceived);
                assert!(outcome.result.is_ok());
                assert!(!format!("{outcome:?}").contains("private-payload"));
                connection.close().await.unwrap();
                let rejected=connection.invoke(request(4096)).await;
                assert_eq!(rejected.delivery,McpDelivery::NotSent);
                assert_eq!(rejected.result.unwrap_err().code(),FailureCode::McpTransport);
            }
            adapter.shutdown().await.unwrap();
        }
    });
}
fn initialize() -> &'static str {
    r#"{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}"#
}

struct HttpStep {
    method: &'static str,
    session: Option<&'static str>,
    status: u16,
    result: String,
    new_session: Option<&'static str>,
}
fn lifecycle_peer(steps: Vec<HttpStep>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let peer = std::thread::spawn(move || {
        for step in steps {
            let until = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
                    {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("lifecycle accept: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let (end, length) = loop {
                let mut block = [0; 1024];
                let count = socket.read(&mut block).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&block[..count]);
                assert!(bytes.len() < 65536);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break (end + 4, length);
                    }
                }
            };
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let session = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("mcp-session-id")
                    .then(|| value.trim())
            });
            assert_eq!(session, step.session);
            let document =
                JsonDocument::new(&bytes[end..end + length], &Limits::default()).unwrap();
            let Value::Map(fields) = document.value() else {
                panic!()
            };
            assert_eq!(fields.get("method"), Some(&Value::Text(step.method.into())));
            let body = if step.status == 200 {
                let Some(Value::Integer(id)) = fields.get("id") else {
                    panic!()
                };
                format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{}}}"#, step.result)
            } else {
                String::new()
            };
            let session = step
                .new_session
                .map_or(String::new(), |id| format!("Mcp-Session-Id: {id}\r\n"));
            write!(socket,"HTTP/1.1 {} response\r\nContent-Type: application/json\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",step.status,body.len()).unwrap();
        }
    });
    (url, peer)
}

#[test]
fn expired_http_sessions_fail_without_renewal_or_implicit_operation_replay() {
    actix::System::new().block_on(async {
        for during_call in [false, true] {
            let step = |method, session, status, result: &str, new_session| HttpStep {
                method,
                session,
                status,
                result: result.into(),
                new_session,
            };
            let listed = format!(r#"{{"tools":[{DESCRIPTOR}]}}"#);
            let mut steps = vec![
                step("initialize", None, 200, initialize(), Some("old")),
                step("notifications/initialized", Some("old"), 202, "", None),
                step(
                    "tools/list",
                    Some("old"),
                    if during_call { 200 } else { 404 },
                    &listed,
                    None,
                ),
            ];
            if during_call {
                steps.push(step("tools/call", Some("old"), 404, "", None));
            }
            let (url, peer) = lifecycle_peer(steps);
            let adapter = NativeMcpAdapter::new(
                BTreeMap::from([(
                    "test".into(),
                    McpEndpoint::Http {
                        url,
                        headers: BTreeMap::new(),
                    },
                )]),
                2000,
            )
            .unwrap();
            let binding = binding(McpTransport::StreamableHttp);
            let limits = Limits::default();
            let context = run_context();
            if during_call {
                let mut connection = adapter.connect(&binding, &context, &limits).await.unwrap();
                assert_eq!(connection.descriptor().digest(), binding.descriptor());
                let outcome = connection.invoke(request(4096)).await;
                assert_eq!(outcome.delivery, McpDelivery::PossiblySent);
                assert_eq!(
                    outcome.result.unwrap_err().code(),
                    FailureCode::McpTransport
                );
                drop(connection);
            }
            assert!(matches!(adapter.connect(&binding, &context, &limits).await, Err(e) if e.code() == FailureCode::McpTransport));
            peer.join().unwrap();
        }
    });
}

#[test]
fn native_task_required_tools_are_rejected_during_discovery() {
    actix::System::new().block_on(async {
        for support in ["required","optional","forbidden"] {
            let descriptor=DESCRIPTOR.strip_suffix('}').unwrap().to_string()+&format!(r#", "execution":{{"taskSupport":"{support}"}}}}"#);
            let init=r#"{"protocolVersion":"2025-11-25","capabilities":{"tools":{},"tasks":{"requests":{"tools":{"call":{}}}}},"serverInfo":{"name":"fixture","version":"1"}}"#;
            let steps=vec![HttpStep{method:"initialize",session:None,status:200,result:init.into(),new_session:Some("test")},HttpStep{method:"notifications/initialized",session:Some("test"),status:202,result:String::new(),new_session:None},HttpStep{method:"tools/list",session:Some("test"),status:200,result:format!(r#"{{"tools":[{descriptor}]}}"#),new_session:None}];
            let (url,peer)=lifecycle_peer(steps);
            let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Http{url,headers:BTreeMap::new()})]),2000).unwrap();
            let l=Limits::default();let base=binding(McpTransport::StreamableHttp);let binding=McpBinding::new(base.server().clone(),JsonDocument::new(descriptor.as_bytes(),&l).unwrap().digest(),base.kind().clone(),&l).unwrap();
            let context=run_context();
            match adapter.connect(&binding,&context,&l).await {Err(e)=>{assert_eq!(support,"required");assert_eq!(e.code(),FailureCode::McpDescriptor);},Ok(connection)=>{assert_ne!(support,"required");drop(connection);}}
            peer.join().unwrap();
        }
    });
}

#[test]
fn discovery_rejects_malformed_identity_capabilities_and_inventory_before_tool_dispatch() {
    actix::System::new().block_on(async {
        let init_cases=[
            (r#"{}"#,FailureCode::McpProtocol,false),
            (r#"{"protocolVersion":"old","serverInfo":{"name":"fixture","version":"1"},"capabilities":{"tools":{}}}"#,FailureCode::McpIdentity,false),
            (r#"{"protocolVersion":1,"serverInfo":{"name":"fixture","version":"1"},"capabilities":{}}"#,FailureCode::McpProtocol,false),
            (r#"{"protocolVersion":"2025-11-25","serverInfo":[],"capabilities":{}}"#,FailureCode::McpProtocol,false),
            (r#"{"protocolVersion":"2025-11-25","serverInfo":{"name":"fixture","version":1},"capabilities":{}}"#,FailureCode::McpProtocol,false),
            (r#"{"protocolVersion":"2025-11-25","serverInfo":{"name":"fixture","version":"1"},"capabilities":null}"#,FailureCode::McpProtocol,false),
            (r#"{"protocolVersion":"2025-11-25","serverInfo":{"name":"fixture","version":"1"},"capabilities":{}}"#,FailureCode::McpDescriptor,true),
        ];
        for (init,code,notified) in init_cases {
            let mut steps=vec![HttpStep{method:"initialize",session:None,status:200,result:init.into(),new_session:None}];
            if notified {steps.push(HttpStep{method:"notifications/initialized",session:None,status:202,result:String::new(),new_session:None});}
            let (url,peer)=lifecycle_peer(steps);
            let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Http{url,headers:BTreeMap::new()})]),2000).unwrap();
            let error=adapter.descriptor(&binding(McpTransport::StreamableHttp),&run_context(),&Limits::default()).await.unwrap_err();
            assert_eq!(error.code(),code,"{init}");
            adapter.shutdown().await.unwrap();peer.join().unwrap();
        }
        for inventory in ["[]", r#"{"tools":{}}"#, r#"{"tools":[null]}"#, r#"{"tools":[],"nextCursor":0}"#] {
            let steps=vec![
                HttpStep{method:"initialize",session:None,status:200,result:initialize().into(),new_session:None},
                HttpStep{method:"notifications/initialized",session:None,status:202,result:String::new(),new_session:None},
                HttpStep{method:"tools/list",session:None,status:200,result:inventory.into(),new_session:None},
            ];
            let (url,peer)=lifecycle_peer(steps);
            let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Http{url,headers:BTreeMap::new()})]),2000).unwrap();
            assert_eq!(adapter.descriptor(&binding(McpTransport::StreamableHttp),&run_context(),&Limits::default()).await.unwrap_err().code(),FailureCode::McpProtocol,"{inventory}");
            adapter.shutdown().await.unwrap();peer.join().unwrap();
        }
        let changed=DESCRIPTOR.strip_suffix('}').unwrap().to_owned()+r#", "description":"live drift"}"#;
        let steps=vec![
            HttpStep{method:"initialize",session:None,status:200,result:initialize().into(),new_session:None},
            HttpStep{method:"notifications/initialized",session:None,status:202,result:String::new(),new_session:None},
            HttpStep{method:"tools/list",session:None,status:200,result:format!(r#"{{"tools":[{changed}]}}"#),new_session:None},
        ];
        let (url,peer)=lifecycle_peer(steps);
        let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Http{url,headers:BTreeMap::new()})]),2000).unwrap();
        assert_eq!(adapter.invoke(&binding(McpTransport::StreamableHttp),&run_context(),request(4096)).await.unwrap_err().code(),FailureCode::McpDescriptor);
        adapter.shutdown().await.unwrap();peer.join().unwrap();
    });
}

// A deliberately independent HTTP peer uses raw bytes, so duplicate keys and
// unsafe numbers reach the adapter without an intermediary JSON parser.
fn http_peer(result: String, sse: bool) -> (String, std::thread::JoinHandle<()>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let sent = Arc::new(AtomicBool::new(false));
    let signal = sent.clone();
    let thread = std::thread::spawn(move || {
        let side = result == "ping" || result == "sampling";
        let hold = result == "hold" || side;
        let mut held = None;
        for index in 0..if hold { 6 } else { 5 } {
            let until = Instant::now() + Duration::from_secs(5);
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(socket) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
                    {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("peer accept: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut header_end = None;
            let mut length = 0;
            loop {
                let mut block = [0; 1024];
                let n = socket.read(&mut block).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&block[..n]);
                assert!(bytes.len() < 65536);
                if header_end.is_none()
                    && let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    header_end = Some(end + 4);
                    length = std::str::from_utf8(&bytes[..end])
                        .unwrap()
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                }
                if header_end.is_some_and(|end| bytes.len() >= end + length) {
                    break;
                }
            }
            let end = header_end.unwrap();
            let message = JsonDocument::new(&bytes[end..end + length], &Limits::default()).unwrap();
            let Value::Map(fields) = message.value() else {
                panic!()
            };
            let method = match fields.get("method") {
                Some(Value::Text(s)) => s.as_str(),
                None if side && index == 5 => "client_reply",
                _ => panic!(),
            };
            if index > 0 {
                assert!(
                    std::str::from_utf8(&bytes[..end])
                        .unwrap()
                        .to_ascii_lowercase()
                        .contains("mcp-session-id: test-session")
                );
            }
            let content = match (index, method) {
                (0, "initialize") => initialize().to_string(),
                (1, "notifications/initialized") => String::new(),
                (2, "tools/list") => r#"{"tools":[],"nextCursor":"page2"}"#.into(),
                (3, "tools/list") => {
                    assert!(
                        matches!(fields.get("params"), Some(Value::Map(p)) if p.get("cursor") == Some(&Value::Text("page2".into())))
                    );
                    format!(r#"{{"tools":[{DESCRIPTOR}]}}"#)
                }
                (4, "tools/call") => {
                    assert_eq!(fields.get("params"), Some(&request(4096).params));
                    if let Some(key) = result.strip_prefix("key:") {
                        assert!(
                            std::str::from_utf8(&bytes[..end])
                                .unwrap()
                                .to_ascii_lowercase()
                                .contains(&format!("idempotency-key: {key}"))
                        );
                    }
                    signal.store(true, Ordering::SeqCst);
                    if hold {
                        if side {
                            let method = if result == "ping" {
                                "ping"
                            } else {
                                "sampling/createMessage"
                            };
                            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: test-session\r\nConnection: close\r\n\r\nevent: message\r\ndata: {{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"{method}\",\"params\":{{}}}}\r\n\r\n").unwrap();
                        }
                        held = Some(socket);
                        continue;
                    }
                    if result == "utf8-split" {
                        UTF8_RESULT.into()
                    } else if result.starts_with("key:") {
                        RESULT.into()
                    } else {
                        result.clone()
                    }
                }
                (5, "notifications/cancelled") => {
                    assert!(
                        matches!(fields.get("params"),Some(Value::Map(p))if p.get("requestId")==Some(&Value::Integer(4)))
                    );
                    String::new()
                }
                (5, "client_reply") => {
                    assert_eq!(fields.get("id"), Some(&Value::Integer(4)));
                    if result == "ping" {
                        assert!(matches!(fields.get("result"),Some(Value::Map(m))if m.is_empty()));
                    } else {
                        assert!(
                            matches!(fields.get("error"),Some(Value::Map(m))if m.get("code")==Some(&Value::Integer(-32601)))
                        );
                    }
                    String::new()
                }
                other => panic!("unexpected {other:?}"),
            };
            let (status, mime, body) = if index == 1 || index == 5 {
                ("202 Accepted", "application/json", String::new())
            } else {
                let Value::Integer(id) = fields.get("id").unwrap() else {
                    panic!()
                };
                let rpc = if index == 4 && result.starts_with("wire:") {
                    result[5..].to_owned()
                } else if index == 4 && result.starts_with("error:") {
                    format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{}}}"#, &result[6..])
                } else {
                    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{content}}}"#)
                };
                if index == 4 && result.starts_with("sse:") {
                    ("200 OK", "text/event-stream", result[4..].to_owned())
                } else if index == 4 && result == "bad-mime" {
                    ("200 OK", "text/plain", rpc)
                } else if sse {
                    (
                        "200 OK",
                        "text/event-stream",
                        format!("event: message\r\ndata: {rpc}\r\n\r\n"),
                    )
                } else {
                    ("200 OK", "application/json", rpc)
                }
            };
            if index == 4 && result == "utf8-split" {
                let split = body.find('é').unwrap() + 1;
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: test-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                socket.write_all(&body.as_bytes()[..split]).unwrap();
                socket.flush().unwrap();
                std::thread::sleep(Duration::from_millis(20));
                socket.write_all(&body.as_bytes()[split..]).unwrap();
            } else if index == 4 && result == "missing-mime" {
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            } else if index == 4 && result == "changed-session" {
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: forged\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            } else if index == 4 && result == "invalid-utf8" {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff").unwrap();
            } else {
                http_response(&mut socket, status, mime, &body);
            }
            if index == 5 {
                if side {
                    write!(held.take().unwrap(),"event: message\rdata: {{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{RESULT}}}\r\r").unwrap();
                } else {
                    http_response(
                        &mut held.take().unwrap(),
                        "200 OK",
                        "application/json",
                        r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32800,"message":"cancelled"}}"#,
                    );
                }
            }
        }
    });
    (url, thread, sent)
}
#[test]
fn sse_reverse_ping_is_answered_and_unadvertised_sampling_cannot_execute() {
    actix::System::new().block_on(async {
        for response in ["ping", "sampling"] {
            let (url, peer, _) = http_peer(response.into(), true);
            let adapter = NativeMcpAdapter::new(
                BTreeMap::from([(
                    "test".into(),
                    McpEndpoint::Http {
                        url,
                        headers: BTreeMap::new(),
                    },
                )]),
                2000,
            )
            .unwrap();
            let result = adapter
                .invoke(
                    &binding(McpTransport::StreamableHttp),
                    &run_context(),
                    request(4096),
                )
                .await
                .unwrap();
            assert_eq!(
                JsonDocument::new(&result, &Limits::default()).unwrap(),
                JsonDocument::new(RESULT.as_bytes(), &Limits::default()).unwrap()
            );
            peer.join().unwrap();
        }
    })
}
#[test]
fn native_protocol_dedup_requires_explicit_compound_binding_support_and_sends_the_exact_key() {
    actix::System::new().block_on(async {
        let limits = Limits::default();
        let binding = binding(McpTransport::StreamableHttp);
        let run = htlk_runtime::identity::RunId::new("wire-key".into(), &limits).unwrap();
        let scope = htlk_runtime::identity::ScopeInstanceId::root(&run, &limits).unwrap();
        let invocation =
            htlk_runtime::identity::InvocationId::new(scope, &"lookup".parse().unwrap(), &limits)
                .unwrap();
        let key = invocation.effect_key(&limits).unwrap().to_string();
        let (url, peer, _) = http_peer(format!("key:{key}"), false);
        let adapter = NativeMcpAdapter::new(
            BTreeMap::from([(
                "test".into(),
                McpEndpoint::Http {
                    url,
                    headers: BTreeMap::new(),
                },
            )]),
            2000,
        )
        .unwrap();
        let mut request = request(4096);
        assert_eq!(
            adapter
                .deduplication(&binding, invocation, &request.params, &limits)
                .unwrap(),
            None
        );
        let adapter =
            adapter.with_protocol_idempotency(std::collections::BTreeSet::from([binding
                .digest(&limits)
                .unwrap()]));
        assert_eq!(
            adapter
                .deduplication(&binding, invocation, &request.params, &limits)
                .unwrap(),
            Some(htlk_runtime::mcp::McpDeduplication::Protocol)
        );
        request.idempotency_key = Some(key);
        assert_eq!(
            adapter
                .deduplication(
                    &binding,
                    invocation,
                    &request.params,
                    &Limits {
                        max_document_bytes: 1,
                        ..limits.clone()
                    }
                )
                .unwrap_err()
                .code(),
            FailureCode::McpIdentity
        );
        assert!(
            adapter
                .invoke(&binding, &run_context(), request)
                .await
                .is_ok()
        );
        peer.join().unwrap();
    })
}
fn http_response(socket: &mut TcpStream, status: &str, mime: &str, body: &str) {
    write!(socket,"HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nMcp-Session-Id: test-session\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
}
#[test]
fn http_json_and_sse_preserve_frozen_requests_and_validate_raw_ingress() {
    actix::System::new().block_on(async {
        for (response, sse, max, expected) in [
            (RESULT.to_string(), false, 4096, None),
            (RESULT.to_string(), true, 4096, None),
            ("utf8-split".to_owned(), true, 4096, None),
            (
                r#"{"content":[],"content":[],"structuredContent":{}}"#.into(),
                false,
                4096,
                Some(FailureCode::McpProtocol),
            ),
            (
                r#"{"content":[],"structuredContent":{"n":9007199254740993}}"#.into(),
                true,
                4096,
                Some(FailureCode::McpProtocol),
            ),
            (
                format!(
                    r#"{{"content":[],"structuredContent":{{"s":"{}"}}}}"#,
                    "x".repeat(8192)
                ),
                false,
                4096,
                Some(FailureCode::McpProtocol),
            ),
        ] {
            let split = response == "utf8-split";
            let (url, peer, _) = http_peer(response, sse);
            let adapter = NativeMcpAdapter::new(
                BTreeMap::from([(
                    "test".into(),
                    McpEndpoint::Http {
                        url,
                        headers: BTreeMap::new(),
                    },
                )]),
                2000,
            )
            .unwrap();
            let result = adapter
                .invoke(
                    &binding(McpTransport::StreamableHttp),
                    &run_context(),
                    request(max),
                )
                .await;
            if let Some(code) = expected {
                assert_eq!(result.unwrap_err().code(), code)
            } else {
                assert_eq!(
                    JsonDocument::new(&result.unwrap(), &Limits::default()).unwrap(),
                    JsonDocument::new(
                        if split {
                            UTF8_RESULT.as_bytes()
                        } else {
                            RESULT.as_bytes()
                        },
                        &Limits::default()
                    )
                    .unwrap()
                )
            }
            peer.join().unwrap();
        }
    })
}

#[test]
fn hostile_http_framing_and_rpc_envelopes_never_claim_received_completion() {
    actix::System::new().block_on(async {
        for response in [
            "bad-mime".to_owned(), "missing-mime".into(), "changed-session".into(), "invalid-utf8".into(),
            r#"wire:{"jsonrpc":"1.0","id":4,"result":{}}"#.into(),
            r#"wire:{"jsonrpc":"2.0","id":5,"result":{}}"#.into(),
            r#"wire:{"jsonrpc":"2.0","id":4,"result":{},"error":{"code":-1,"message":"ambiguous"}}"#.into(),
            r#"wire:{"jsonrpc":"2.0","id":4,"error":{"code":"bad","message":"malformed"}}"#.into(),
            "sse:data: null\n\n".into(),
            "sse:data: {\n\n".into(),
            "sse:: keepalive\n\n".into(),
            format!("sse:{}", ": keepalive\n\n".repeat(1025)),
        ] {
            let (url,peer,sent)=http_peer(response.clone(),false);
            let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Http{url,headers:BTreeMap::new()})]),2000).unwrap();
            let binding=binding(McpTransport::StreamableHttp);
            let context=run_context();
            let limits=Limits::default();
            let mut connection=adapter.connect(&binding,&context,&limits).await.unwrap();
            let outcome=connection.invoke(request(65536)).await;
            assert!(sent.load(Ordering::SeqCst));
            assert_eq!(outcome.delivery,McpDelivery::PossiblySent,"{response}");
            assert_eq!(outcome.result.unwrap_err().code(),FailureCode::McpProtocol,"{response}");
            connection.close().await.unwrap();
            adapter.shutdown().await.unwrap();
            peer.join().unwrap();
        }
    });
}
#[test]
fn live_http_connection_reports_correlated_errors_and_cancels_the_actual_request() {
    actix::System::new().block_on(async {
        for response in [
            r#"error:{"code":-32601,"message":"missing"}"#.to_string(),
            "hold".into(),
        ] {
            let hold = response == "hold";
            let (url, peer, sent) = http_peer(response, false);
            let adapter = NativeMcpAdapter::new(
                BTreeMap::from([(
                    "test".into(),
                    McpEndpoint::Http {
                        url,
                        headers: BTreeMap::new(),
                    },
                )]),
                2000,
            )
            .unwrap();
            let binding = binding(McpTransport::StreamableHttp);
            let context = run_context();
            let limits = Limits::default();
            let mut connection = adapter.connect(&binding, &context, &limits).await.unwrap();
            let mut request = request(4096);
            request.cancellation_grace_ms = 500;
            let signal = request.cancellation.clone();
            let cancelling = async {
                if hold {
                    for _ in 0..1000 {
                        if sent.load(Ordering::SeqCst) {
                            break;
                        }
                        actix::clock::sleep(Duration::from_millis(1)).await;
                    }
                    signal.cancel();
                }
            };
            let (outcome, _) = tokio::join!(connection.invoke(request), cancelling);
            assert_eq!(outcome.delivery, McpDelivery::ResponseReceived);
            assert_eq!(outcome.result.unwrap_err().code(), FailureCode::McpProtocol);
            let _ = connection.close().await;
            peer.join().unwrap();
        }
    })
}
#[test]
fn full_response_depth_limit_is_enforced_on_the_live_connection() {
    actix::System::new().block_on(async {
        let (url, peer, _) = http_peer(
            r#"{"content":[],"structuredContent":{"a":{"b":{"c":{"d":{"e":true}}}}}}"#.into(),
            false,
        );
        let adapter = NativeMcpAdapter::new(
            BTreeMap::from([(
                "test".into(),
                McpEndpoint::Http {
                    url,
                    headers: BTreeMap::new(),
                },
            )]),
            2000,
        )
        .unwrap();
        let binding = binding(McpTransport::StreamableHttp);
        let context = run_context();
        let limits = Limits::default();
        let mut connection = adapter.connect(&binding, &context, &limits).await.unwrap();
        let mut request = request(4096);
        request.response_limits.max_depth = 6;
        let outcome = connection.invoke(request).await;
        assert_eq!(outcome.delivery, McpDelivery::PossiblySent);
        assert_eq!(outcome.result.unwrap_err().code(), FailureCode::McpProtocol);
        let _ = connection.close().await;
        peer.join().unwrap();
    })
}
#[cfg(unix)]
#[test]
fn stdio_negotiates_pages_checks_identity_and_bounds_raw_lines() {
    actix::System::new().block_on(async {
        for (reply, max, code) in [(RESULT.to_string(), 4096, None), ("x".repeat(8192), 4096, Some(FailureCode::McpProtocol))] {
            let script = r#"
                id=0
                page=0
                while IFS= read -r line; do
                    case "$line" in
                        *'"method":"notifications/initialized"'*) continue;;
                        *'"method":"initialize"'*) result="$INIT";;
                        *'"method":"tools/list"'*)
                            if [ "$page" = 0 ]; then result='{"tools":[],"nextCursor":"page2"}'; page=1
                            else result="{\"tools\":[$DESCRIPTOR]}"; fi;;
                        *'"method":"tools/call"'*) result="$REPLY";;
                        *) exit 2;;
                    esac
                    id=$((id+1))
                    printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
                done
            "#;
            let adapter = NativeMcpAdapter::new(BTreeMap::from([("test".into(), McpEndpoint::Stdio {program: "/bin/sh".into(), arguments: vec!["-c".into(), script.into()], environment: BTreeMap::from([("DESCRIPTOR".into(), DESCRIPTOR.into()), ("INIT".into(), initialize().into()), ("REPLY".into(), reply)])})]), 2000).unwrap();
            let result = adapter.invoke(&binding(McpTransport::Stdio), &run_context(), request(max)).await;
            if let Some(code) = code {assert_eq!(result.unwrap_err().code(), code)} else {assert!(result.is_ok(), "{result:?}")}
        }
    })
}
#[cfg(unix)]
#[test]
fn stdio_cancellation_keeps_the_reader_live_for_the_correlated_completion() {
    actix::System::new().block_on(async {
        let script=r#"
            id=0
            page=0
            pending=0
            while IFS= read -r line; do
                case "$line" in
                    *'"method":"notifications/initialized"'*) continue;;
                    *'"method":"notifications/cancelled"'*)
                        case "$line" in *'"requestId":4'*) :;; *) exit 3;; esac
                        printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32800,"message":"cancelled"}}\n' "$pending"
                        continue;;
                    *'"method":"initialize"'*) result="$INIT";;
                    *'"method":"tools/list"'*)
                        if [ "$page" = 0 ]; then result='{"tools":[],"nextCursor":"page2"}'; page=1
                        else result="{\"tools\":[$DESCRIPTOR]}"; fi;;
                    *'"method":"tools/call"'*) id=$((id+1)); pending="$id"; continue;;
                    *) exit 2;;
                esac
                id=$((id+1))
                printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
            done
        "#;
        let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Stdio{program:"/bin/sh".into(),arguments:vec!["-c".into(),script.into()],environment:BTreeMap::from([("INIT".into(),initialize().into()),("DESCRIPTOR".into(),DESCRIPTOR.into())])})]),2000).unwrap();let binding=binding(McpTransport::Stdio);let limits=Limits::default();let context=run_context();let mut connection=adapter.connect(&binding,&context,&limits).await.unwrap();let mut request=request(4096);request.cancellation_grace_ms=500;let signal=request.cancellation.clone();
        let (outcome,_)=tokio::join!(connection.invoke(request),async{actix::clock::sleep(Duration::from_millis(20)).await;signal.cancel();});assert_eq!(outcome.delivery,McpDelivery::ResponseReceived);assert_eq!(outcome.result.unwrap_err().code(),FailureCode::McpProtocol);let _=connection.close().await;
    })
}
