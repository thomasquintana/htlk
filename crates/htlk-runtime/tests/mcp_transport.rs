//! Actual bounded stdio and Streamable HTTP sessions, including hostile raw ingress.
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use htlk_runtime::{
    failure::FailureCode,
    mcp::{McpAdapter, McpEndpoint, McpRequest, NativeMcpAdapter},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
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
fn http_peer(result: String, sse: bool) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for index in 0..5 {
            let (mut socket, _) = listener.accept().unwrap();
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
            let method = match fields.get("method").unwrap() {
                Value::Text(s) => s.as_str(),
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
                    result.clone()
                }
                other => panic!("unexpected {other:?}"),
            };
            let (status, mime, body) = if index == 1 {
                ("202 Accepted", "application/json", String::new())
            } else {
                let Value::Integer(id) = fields.get("id").unwrap() else {
                    panic!()
                };
                let rpc = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{content}}}"#);
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
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nMcp-Session-Id: test-session\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (url, thread)
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
            let (url, peer) = http_peer(response, sse);
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
