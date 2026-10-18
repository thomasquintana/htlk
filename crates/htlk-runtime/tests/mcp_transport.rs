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
            output_schema: JsonDocument::new(br#"{"type":"object"}"#, &l)
                .unwrap()
                .digest(),
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
fn initialize() -> &'static str {
    r#"{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}"#
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
                    if result.starts_with("key:") {
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
                let rpc = if index == 4 && result.starts_with("error:") {
                    format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{}}}"#, &result[6..])
                } else {
                    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{content}}}"#)
                };
                if sse {
                    (
                        "200 OK",
                        "text/event-stream",
                        format!("event: message\r\ndata: {rpc}\r\n\r\n"),
                    )
                } else {
                    ("200 OK", "application/json", rpc)
                }
            };
            http_response(&mut socket, status, mime, &body);
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
                .invoke(&binding(McpTransport::StreamableHttp), request(4096))
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
        assert!(adapter.invoke(&binding, request).await.is_ok());
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
                .invoke(&binding(McpTransport::StreamableHttp), request(max))
                .await;
            if let Some(code) = expected {
                assert_eq!(result.unwrap_err().code(), code)
            } else {
                assert_eq!(
                    JsonDocument::new(&result.unwrap(), &Limits::default()).unwrap(),
                    JsonDocument::new(RESULT.as_bytes(), &Limits::default()).unwrap()
                )
            }
            peer.join().unwrap();
        }
    })
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
            let context =
                htlk_runtime::HostContext::new("test".into(), &Limits::default()).unwrap();
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
        let context = htlk_runtime::HostContext::new("test".into(), &Limits::default()).unwrap();
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
            let result = adapter.invoke(&binding(McpTransport::Stdio), request(max)).await;
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
        let adapter=NativeMcpAdapter::new(BTreeMap::from([("test".into(),McpEndpoint::Stdio{program:"/bin/sh".into(),arguments:vec!["-c".into(),script.into()],environment:BTreeMap::from([("INIT".into(),initialize().into()),("DESCRIPTOR".into(),DESCRIPTOR.into())])})]),2000).unwrap();let binding=binding(McpTransport::Stdio);let limits=Limits::default();let context=htlk_runtime::HostContext::new("test".into(),&limits).unwrap();let mut connection=adapter.connect(&binding,&context,&limits).await.unwrap();let mut request=request(4096);request.cancellation_grace_ms=500;let signal=request.cancellation.clone();
        let (outcome,_)=tokio::join!(connection.invoke(request),async{actix::clock::sleep(Duration::from_millis(20)).await;signal.cancel();});assert_eq!(outcome.delivery,McpDelivery::ResponseReceived);assert_eq!(outcome.result.unwrap_err().code(),FailureCode::McpProtocol);let _=connection.close().await;
    })
}
