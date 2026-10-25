//! Capability spike: graph-level state continuity and independent session-local peers.
#[path = "support/e2e.rs"]
#[allow(dead_code)]
mod e2e;
#[allow(dead_code)]
mod support;

use e2e::{Harness, Peer, caller, output, terminal};
use htlk::{
    compiler::MCPCatalogInputs,
    runtime::{
        HostContext,
        failure::FailureCode,
        identity::RunId,
        mcp::{
            McpAdapter, McpCancellation, McpDelivery, McpRequest, McpRunContext, NativeMcpAdapter,
        },
        records::{InputValue, RunStatus},
    },
};
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

const BROWSER: &str = include_str!("../examples/mcp_capability_spike/browser.htlk");
const SANDBOX: &str = include_str!("../examples/mcp_capability_spike/sandbox.htlk");
const TOOLS: &[u8] = include_bytes!("../examples/mcp_capability_spike/tools.json");

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(map) = value else {
        panic!("object expected")
    };
    map.get(key).unwrap_or_else(|| panic!("missing {key}"))
}
fn text(value: &Value) -> &str {
    let Value::Text(text) = value else {
        panic!("text expected")
    };
    text
}
fn json(raw: &str) -> Value {
    JsonDocument::new(raw.as_bytes(), &Limits::default())
        .unwrap()
        .value()
        .clone()
}
fn tools() -> Vec<Value> {
    let Value::Array(tools) = JsonDocument::new(TOOLS, &Limits::default())
        .unwrap()
        .value()
        .clone()
    else {
        panic!()
    };
    tools
}
fn catalogs(transport: &str) -> MCPCatalogInputs {
    let common = format!(
        r#""catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{{"fixture":{{"deployment_id":"test","transport":"{transport}","implementation_name":"fixture","implementation_version":"1"}}}},"schema_documents":{{}}"#
    );
    let entries = tools()
        .iter()
        .map(|tool| {
            let descriptor = JsonDocument::from_value(tool, &Limits::default()).unwrap();
            format!(
                r#"{{"server":"fixture","descriptor":{}}}"#,
                std::str::from_utf8(descriptor.as_bytes()).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    MCPCatalogInputs::from_json(format!(r#"{{"tools":{{{common},"tools":[{entries}]}},"resources":{{{common},"resources":[],"templates":[]}},"prompts":{{{common},"prompts":[]}}}}"#).as_bytes(), &Limits::default()).unwrap()
}
fn binding(peer: &Peer, name: &str) -> McpBinding {
    let limits = Limits::default();
    let tool = tools()
        .into_iter()
        .find(|tool| text(field(tool, "name")) == name)
        .unwrap();
    let digest = |value: &Value| JsonDocument::from_value(value, &limits).unwrap().digest();
    McpBinding::new(
        ServerIdentity::new(
            "test".into(),
            if peer.transport == "stdio" {
                McpTransport::Stdio
            } else {
                McpTransport::StreamableHttp
            },
            "fixture".into(),
            "1".into(),
            &limits,
        )
        .unwrap(),
        digest(&tool),
        McpBindingKind::Tool {
            name: name.into(),
            input_schema: digest(field(&tool, "inputSchema")),
            output_schema: digest(field(&tool, "outputSchema")),
        },
        &limits,
    )
    .unwrap()
}
fn context(run: &str) -> McpRunContext {
    McpRunContext {
        run_id: RunId::new(run.into(), &Limits::default()).unwrap(),
        host: HostContext::new("alice".into(), &Limits::default()).unwrap(),
        fence: 1,
        cancellation: Arc::new(McpCancellation::new()),
    }
}
fn request(name: &str, arguments: &str, timeout_ms: u64) -> McpRequest {
    McpRequest {
        response_limits: Limits::default(),
        cancellation: Arc::new(McpCancellation::new()),
        cancellation_grace_ms: 50,
        method: "tools/call".into(),
        params: json(&format!(r#"{{"name":"{name}","arguments":{arguments}}}"#)),
        timeout_ms,
        max_response_bytes: 65536,
        idempotency_key: None,
    }
}
async fn call(
    adapter: &NativeMcpAdapter,
    peer: &Peer,
    context: &McpRunContext,
    name: &str,
    arguments: &str,
) -> Value {
    let binding = binding(peer, name);
    let limits = Limits::default();
    let mut lease = adapter.connect(&binding, context, &limits).await.unwrap();
    assert_eq!(lease.descriptor().digest(), binding.descriptor());
    let outcome = lease.invoke(request(name, arguments, 2000)).await;
    assert_eq!(outcome.delivery, McpDelivery::ResponseReceived);
    lease.close().await.unwrap();
    JsonDocument::new(&outcome.result.unwrap(), &limits)
        .unwrap()
        .value()
        .clone()
}
fn methods(peer: &Peer, method: &str) -> Vec<Value> {
    peer.requests()
        .into_iter()
        .filter(|entry| text(field(entry, "method")) == method)
        .collect()
}
async fn wait_cleanup(peer: &Peer, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let done = if peer.transport == "stdio" {
            #[cfg(unix)]
            {
                methods(peer, "initialize").len() == count
                    && methods(peer, "initialize").iter().all(|entry| {
                        let Value::Integer(pid) = field(entry, "pid") else {
                            panic!()
                        };
                        !std::process::Command::new("kill")
                            .args(["-0", &pid.to_string()])
                            .stderr(std::process::Stdio::null())
                            .status()
                            .unwrap()
                            .success()
                    })
            }
            #[cfg(not(unix))]
            {
                true
            }
        } else {
            methods(peer, "session/closed").len() == count
        };
        if done {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "session cleanup stalled: {:?}",
            peer.requests()
        );
        actix::clock::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn compiled_browser_and_sandbox_sequences_preserve_state_and_isolate_concurrent_runs() {
    actix::System::new().block_on(async {
        for transport in ["stdio", "http", "sse"] {
            for (source, names) in [
                (BROWSER, vec!["browser_open", "browser_inspect"]),
                (
                    SANDBOX,
                    vec![
                        "sandbox_create",
                        "sandbox_execute",
                        "sandbox_inspect",
                        "sandbox_destroy",
                    ],
                ),
            ] {
                let peer = Peer::new(transport, "stateful");
                let harness = Harness::new(Some(&peer), 2).await;
                let graph = harness
                    .compiler
                    .compile_source(source, &catalogs(peer.transport))
                    .unwrap();
                harness
                    .runtime
                    .register_graph(&caller(), &graph.executable_bytes, &[])
                    .await
                    .unwrap();
                let mut runs = Vec::new();
                for marker in ["first-run-marker", "second-run-marker"] {
                    let run = harness
                        .runtime
                        .start_run(
                            &caller(),
                            graph.fingerprint,
                            &BTreeMap::from([(
                                "marker".parse().unwrap(),
                                InputValue::Inline(Value::Text(marker.into())),
                            )]),
                            marker,
                            None,
                        )
                        .await
                        .unwrap();
                    runs.push((run.run_id, marker));
                }
                for (run, marker) in runs {
                    let view = terminal(&harness.runtime, &run).await;
                    assert_eq!(
                        view.header.status,
                        RunStatus::Succeeded,
                        "{transport}: {view:#?}"
                    );
                    assert_eq!(
                        output(&harness.runtime, &view, "marker").await,
                        Value::Text(marker.into())
                    );
                    if source == SANDBOX {
                        assert_eq!(
                            output(&harness.runtime, &view, "destroyed").await,
                            Value::Bool(true)
                        );
                    }
                }
                wait_cleanup(&peer, 2).await;
                let initializations = methods(&peer, "initialize");
                assert_eq!(initializations.len(), 2);
                let sessions: BTreeSet<_> = initializations
                    .iter()
                    .map(|entry| text(field(entry, "session")))
                    .collect();
                assert_eq!(sessions.len(), 2);
                for session in sessions {
                    let calls: Vec<_> = peer
                        .calls()
                        .into_iter()
                        .filter(|entry| text(field(entry, "session")) == session)
                        .collect();
                    assert_eq!(
                        calls
                            .iter()
                            .map(|entry| text(field(field(entry, "params"), "name")))
                            .collect::<Vec<_>>(),
                        names
                    );
                    if peer.transport != "stdio" {
                        let log = peer.requests();
                        let closed = log
                            .iter()
                            .position(|entry| {
                                text(field(entry, "session")) == session
                                    && text(field(entry, "method")) == "session/closed"
                            })
                            .unwrap();
                        assert!(
                            !log[closed + 1..]
                                .iter()
                                .any(|entry| text(field(entry, "session")) == session)
                        );
                        if source == SANDBOX {
                            assert_eq!(
                                field(field(&log[closed], "params"), "sandboxes"),
                                &Value::Integer(0)
                            );
                        }
                    }
                }
                harness.engine.shutdown().await.unwrap();
            }
        }
    });
}

#[test]
fn handles_reject_fresh_sessions_unknown_objects_and_use_after_destroy() {
    actix::System::new().block_on(async {
        for transport in ["stdio", "http", "sse"] {
            let peer = Peer::new(transport, "stateful");
            let adapter = peer.adapter();
            let first = context("first");
            let fresh = context("fresh-negative-control");
            let opened = call(
                &adapter,
                &peer,
                &first,
                "browser_open",
                r#"{"marker":"page-state"}"#,
            )
            .await;
            let page = text(field(field(&opened, "structuredContent"), "page"));
            let args = format!(r#"{{"page":"{page}"}}"#);
            let wrong_session = call(&adapter, &peer, &fresh, "browser_inspect", &args).await;
            assert_eq!(field(&wrong_session, "isError"), &Value::Bool(true));
            // Release the lease, let worker runtimes disappear, and return later.
            actix::clock::sleep(Duration::from_millis(30)).await;
            let inspected = call(&adapter, &peer, &first, "browser_inspect", &args).await;
            assert_eq!(
                field(field(&inspected, "structuredContent"), "marker"),
                &Value::Text("page-state".into())
            );
            let unknown = call(
                &adapter,
                &peer,
                &first,
                "sandbox_inspect",
                r#"{"sandbox":"unknown"}"#,
            )
            .await;
            assert_eq!(field(&unknown, "isError"), &Value::Bool(true));
            let created = call(&adapter, &peer, &first, "sandbox_create", "{}").await;
            let sandbox = text(field(field(&created, "structuredContent"), "sandbox"));
            let args = format!(r#"{{"sandbox":"{sandbox}"}}"#);
            call(&adapter, &peer, &first, "sandbox_destroy", &args).await;
            let gone = call(&adapter, &peer, &first, "sandbox_inspect", &args).await;
            assert_eq!(field(&gone, "isError"), &Value::Bool(true));
            assert_eq!(methods(&peer, "initialize").len(), 2);
            first.cancellation.cancel();
            fresh.cancellation.cancel();
            wait_cleanup(&peer, 2).await;
        }
    });
}

#[test]
fn new_ownership_closes_old_session_and_stale_cleanup_cannot_close_new_state() {
    actix::System::new().block_on(async {
        let peer = Peer::new("http", "stateful");
        let adapter = peer.adapter();
        let old = context("takeover");
        call(&adapter, &peer, &old, "browser_open", r#"{"marker":"old"}"#).await;
        let mut new = context("takeover");
        new.fence = 2;
        let opened = call(&adapter, &peer, &new, "browser_open", r#"{"marker":"new"}"#).await;
        assert!(old.cancellation.is_cancelled());
        old.cancellation.cancel();
        let page = text(field(field(&opened, "structuredContent"), "page"));
        let inspected = call(
            &adapter,
            &peer,
            &new,
            "browser_inspect",
            &format!(r#"{{"page":"{page}"}}"#),
        )
        .await;
        assert_eq!(
            field(field(&inspected, "structuredContent"), "marker"),
            &Value::Text("new".into())
        );
        let binding = binding(&peer, "browser_open");
        assert!(
            adapter
                .connect(&binding, &old, &Limits::default())
                .await
                .is_err()
        );
        new.cancellation.cancel();
        wait_cleanup(&peer, 2).await;
    });
}

#[test]
fn expiry_and_ambiguous_timeout_never_renew_or_replay_a_session() {
    actix::System::new().block_on(async {
        for mode in ["stateful-expire", "stateful-hold"] {
            let peer = Peer::new("http", mode);
            let adapter = peer.adapter();
            let context = context("lost-session");
            let binding = binding(&peer, "browser_open");
            let limits = Limits::default();
            let mut lease = adapter.connect(&binding, &context, &limits).await.unwrap();
            let outcome = lease.invoke(request("browser_open", r#"{"marker":"hold"}"#, 100)).await;
            assert_eq!(outcome.delivery, McpDelivery::PossiblySent);
            assert_eq!(outcome.result.unwrap_err().code(), if mode == "stateful-expire" { FailureCode::McpTransport } else { FailureCode::McpTimeout });
            lease.close().await.unwrap();
            assert!(matches!(adapter.connect(&binding, &context, &limits).await, Err(e) if e.code() == FailureCode::McpTransport));
            assert_eq!(methods(&peer, "initialize").len(), 1);
            assert_eq!(peer.calls().len(), if mode == "stateful-expire" { 0 } else { 1 });
            context.cancellation.cancel();
            if mode == "stateful-hold" { wait_cleanup(&peer, 1).await; }
        }
    });
}

#[test]
fn cancelling_one_session_keeps_other_runs_live_and_shutdown_is_acknowledged() {
    actix::System::new().block_on(async {
        let peer = Peer::new("http", "stateful-hold");
        let adapter = peer.adapter();
        let held = context("held");
        let healthy = context("healthy");
        let binding = binding(&peer, "browser_open");
        let limits = Limits::default();
        let mut lease = adapter.connect(&binding, &held, &limits).await.unwrap();
        let held_request = lease.invoke(request("browser_open", r#"{"marker":"hold"}"#, 2000));
        let healthy_request = async {
            let deadline = Instant::now() + Duration::from_secs(2);
            while peer.calls().is_empty() {
                assert!(Instant::now() < deadline);
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            let opened = call(
                &adapter,
                &peer,
                &healthy,
                "browser_open",
                r#"{"marker":"healthy"}"#,
            )
            .await;
            assert!(matches!(
                field(field(&opened, "structuredContent"), "page"),
                Value::Text(_)
            ));
            held.cancellation.cancel();
        };
        let (outcome, _) = tokio::join!(held_request, healthy_request);
        assert_eq!(outcome.delivery, McpDelivery::PossiblySent);
        assert_eq!(methods(&peer, "notifications/cancelled").len(), 1);
        lease.close().await.unwrap();
        adapter.shutdown().await.unwrap();
        wait_cleanup(&peer, 2).await;
        assert!(adapter.connect(&binding, &healthy, &limits).await.is_err());
        adapter.shutdown().await.unwrap();
    });
}

#[test]
fn engine_failure_cancellation_and_shutdown_clean_up_owned_sessions() {
    actix::System::new().block_on(async {
        for stop in ["failure", "cancel", "shutdown"] {
            let peer = Peer::new(
                "http",
                if stop == "failure" {
                    "stateful-error"
                } else {
                    "stateful-hold"
                },
            );
            let harness = Harness::new(Some(&peer), 1).await;
            let graph = harness
                .compiler
                .compile_source(BROWSER, &catalogs(peer.transport))
                .unwrap();
            harness
                .runtime
                .register_graph(&caller(), &graph.executable_bytes, &[])
                .await
                .unwrap();
            let run = harness
                .runtime
                .start_run(
                    &caller(),
                    graph.fingerprint,
                    &BTreeMap::from([(
                        "marker".parse().unwrap(),
                        InputValue::Inline(Value::Text("hold".into())),
                    )]),
                    stop,
                    None,
                )
                .await
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while peer.calls().is_empty() {
                assert!(Instant::now() < deadline);
                actix::clock::sleep(Duration::from_millis(2)).await;
            }
            if stop == "cancel" {
                harness
                    .runtime
                    .cancel_run(&caller(), &run.run_id, "capability test", "cancel")
                    .await
                    .unwrap();
            }
            if stop == "shutdown" {
                harness.engine.shutdown().await.unwrap();
            } else {
                let view = terminal(&harness.runtime, &run.run_id).await;
                assert_ne!(view.header.status, RunStatus::Succeeded);
            }
            wait_cleanup(&peer, 1).await;
            assert_eq!(peer.calls().len(), 1);
            assert_eq!(methods(&peer, "initialize").len(), 1);
            if stop != "shutdown" {
                harness.engine.shutdown().await.unwrap();
            }
        }
    });
}
