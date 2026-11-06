//! Independent stdio/HTTP peers exercise protocol tasks without SDKs or hooks.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use htlk_runtime::{
    HostContext,
    mcp::{McpCancellation, McpDelivery, McpEndpoint, client::*},
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const PEER: &str = include_str!("../../../tests/support/mcp_tasks_peer.mjs");
fn document(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn cancellation() -> Arc<McpCancellation> {
    Arc::new(McpCancellation::new())
}
fn limits() -> McpTaskLimits {
    McpTaskLimits::new(16 * 1024, 2000, 16, 16, 128 * 1024, 5).unwrap()
}
fn descriptor(support: Option<&str>) -> JsonDocument {
    document(&format!(
        r#"{{"name":"work","inputSchema":{{"type":"object"}}{}}}"#,
        support.map_or(String::new(), |support| format!(
            r#", "execution":{{"taskSupport":"{support}"}}"#
        ))
    ))
}
fn params() -> JsonDocument {
    document(r#"{"name":"work","arguments":{},"_meta":{"progressToken":"whole-task-lifetime"}}"#)
}
fn native() -> NativeMcpClient {
    // Offline schema preparation is deliberately outside each measured campaign.
    htlk_analyzer::mcp_schema_validators().unwrap();
    NativeMcpClient::new(
        McpClientConfig::new(
            McpClientLimits {
                timeout_ms: 2000,
                ..Default::default()
            },
            BTreeMap::new(),
            None,
        )
        .unwrap(),
    )
    .unwrap()
}
async fn open(client: &NativeMcpClient, scenario: &str) -> McpClientSession {
    open_endpoint(
        client,
        McpEndpoint::Stdio {
            program: "node".into(),
            arguments: vec![
                "--input-type=module".into(),
                "-e".into(),
                PEER.into(),
                scenario.into(),
                "stdio".into(),
            ],
            environment: BTreeMap::new(),
        },
        McpTransport::Stdio,
    )
    .await
}
async fn open_endpoint(
    client: &NativeMcpClient,
    endpoint: McpEndpoint,
    transport: McpTransport,
) -> McpClientSession {
    let representation = Limits::default();
    client
        .open(
            McpOwner::new(
                HostContext::new("alice".into(), &representation).unwrap(),
                McpOwnerScope::Host("protocol-tasks".into()),
                1,
                &representation,
            )
            .unwrap(),
            ServerIdentity::new(
                "tasks-peer".into(),
                transport,
                "tasks-peer".into(),
                "1".into(),
                &representation,
            )
            .unwrap(),
            endpoint,
            representation,
            cancellation(),
        )
        .await
        .unwrap()
}
async fn create(session: &McpClientSession) -> McpTaskReply<McpToolTaskAcceptance> {
    session
        .prepare_tool_task_call(
            &descriptor(Some("required")),
            params(),
            McpToolTaskMode::Task { ttl_ms: Some(1000) },
        )
        .unwrap()
        .dispatch(limits(), cancellation())
        .await
        .unwrap()
}
fn accepted(reply: McpTaskReply<McpToolTaskAcceptance>) -> McpRemoteTask {
    assert_eq!(reply.reply.delivery, McpDelivery::ResponseReceived);
    let bytes = reply.reply.raw_result.as_ref().unwrap();
    assert!(std::str::from_utf8(bytes).unwrap().contains("taskId"));
    assert!(
        !std::str::from_utf8(bytes)
            .unwrap()
            .contains("actual-operation-result")
    );
    match reply.value.unwrap().unwrap() {
        McpToolTaskAcceptance::Accepted(task) => *task,
        McpToolTaskAcceptance::Inline => panic!("acceptance must not become tool output"),
    }
}
async fn stats(session: &McpClientSession) -> BTreeMap<String, i64> {
    let reply = session
        .request("test/stats", document("{}"), cancellation())
        .await;
    let doc = reply.result.unwrap().unwrap();
    let Value::Map(fields) = doc.value() else {
        panic!()
    };
    fields
        .iter()
        .map(|(key, value)| {
            let Value::Integer(value) = value else {
                panic!()
            };
            (key.into(), *value)
        })
        .collect()
}

#[test]
fn descriptor_and_server_capability_matrix_is_checked_before_any_create() {
    actix::System::new().block_on(async {
        for scenario in ["normal", "no-cap"] {
            let client = native();
            let session = open(&client, scenario).await;
            for support in [None, Some("forbidden"), Some("optional"), Some("required")] {
                for mode in [
                    McpToolTaskMode::Inline,
                    McpToolTaskMode::Task { ttl_ms: None },
                ] {
                    let expected = match mode {
                        McpToolTaskMode::Inline => support != Some("required"),
                        McpToolTaskMode::Task { .. } => {
                            scenario != "no-cap" && matches!(support, Some("required" | "optional"))
                        }
                    };
                    assert_eq!(
                        session
                            .prepare_tool_task_call(&descriptor(support), params(), mode)
                            .is_ok(),
                        expected,
                        "{scenario} {support:?} {mode:?}"
                    );
                }
            }
            for invalid in [
                r#"{"name":"another"}"#,
                r#"{"name":"work","task":{}}"#,
                r#"{"name":"work","arguments":[]}"#,
                r#"{"name":"work","_meta":{"progressToken":true}}"#,
            ] {
                assert!(
                    session
                        .prepare_tool_task_call(
                            &descriptor(Some("optional")),
                            document(invalid),
                            McpToolTaskMode::Inline
                        )
                        .is_err()
                );
            }
            let prepared = session
                .prepare_tool_task_call(
                    &descriptor(Some("optional")),
                    params(),
                    McpToolTaskMode::Inline,
                )
                .unwrap();
            assert_eq!(prepared.mode(), McpToolTaskMode::Inline);
            assert!(!String::from_utf8_lossy(prepared.params().as_bytes()).contains("\"task\""));
            let reply = prepared.dispatch(limits(), cancellation()).await.unwrap();
            assert!(matches!(
                reply.value,
                Ok(Some(McpToolTaskAcceptance::Inline))
            ));
            drop(reply);
            assert_eq!(stats(&session).await["creates"], 0);
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn bounded_polling_completes_without_status_notifications_and_keeps_acceptance_separate() {
    actix::System::new().block_on(async {
        for scenario in ["normal", "input-required", "null-ttl", "tool-error", "remote-error"] {
            let client = native();
            let session = open(&client, scenario).await;
            let mut task = accepted(create(&session).await);
            assert_eq!(task.state().status(), McpTaskStatus::Working);
            assert_eq!(task.state().ttl_ms(), if scenario == "null-ttl" { None } else { Some(60000) });
            assert_eq!(task.state().poll_interval_ms(), Some(25));
            let waited = task.wait(limits(), cancellation()).await.unwrap();
            let McpTaskWait::Result(reply) = waited else { panic!("{waited:?}") };
            assert_eq!(reply.reply.delivery, McpDelivery::ResponseReceived);
            if scenario == "remote-error" {
                assert!(matches!(reply.value, Ok(Some(()))));
                let error = reply.reply.result.unwrap().unwrap_err();
                assert_eq!(error.code(), -32050);
                assert_eq!(error.as_bytes(), br#"{"code":-32050,"message":"private-underlying-error","data":{"protected":true},"_meta":{"io.modelcontextprotocol/related-task":{"taskId":"opaque/private/task"}}}"#);
            } else {
                assert!(matches!(reply.value, Ok(Some(()))));
                let raw = String::from_utf8(reply.reply.raw_result.unwrap()).unwrap();
                assert!(raw.contains("actual-operation-result"));
                assert!(raw.contains(MCP_RELATED_TASK));
                assert!(!raw.contains("\"task\":"));
            }
            let counters = stats(&session).await;
            assert_eq!(counters["creates"], 1);
            assert_eq!(counters["results"], 1);
            assert_eq!(counters["earlyPoll"], 0);
            assert_eq!(counters["gets"], if scenario == "input-required" { 1 } else { 2 });
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn invalid_acceptance_and_task_lifecycle_are_rejected_with_original_wire_evidence() {
    actix::System::new().block_on(async {
        for scenario in [
            "bad-initial",
            "negative-ttl",
            "fractional-ttl",
            "negative-poll",
            "missing-ttl",
            "bad-date",
            "backward-date",
        ] {
            let client = native();
            let session = open(&client, scenario).await;
            let reply = create(&session).await;
            assert!(reply.value.is_err(), "{scenario}: {reply:?}");
            assert_eq!(reply.reply.delivery, McpDelivery::ResponseReceived);
            assert!(reply.reply.raw_result.is_some());
            drop(reply);
            assert_eq!(stats(&session).await["creates"], 1);
            client.shutdown().await.unwrap();
        }
        for scenario in [
            "identity-drift",
            "ttl-drift",
            "time-regression",
            "terminal-regression",
            "wrong-related",
        ] {
            let client = native();
            let session = open(&client, scenario).await;
            let mut task = accepted(create(&session).await);
            if scenario == "wrong-related" {
                let McpTaskWait::Result(reply) = task.wait(limits(), cancellation()).await.unwrap()
                else {
                    panic!()
                };
                assert_eq!(reply.value.unwrap_err(), McpTaskError::InvalidResponse);
                assert!(reply.reply.result.unwrap().is_ok());
            } else {
                if scenario == "terminal-regression" {
                    let first = task.get(limits(), cancellation()).await.unwrap();
                    assert_eq!(
                        first.value.unwrap().unwrap().status(),
                        McpTaskStatus::Completed
                    );
                }
                let reply = task.get(limits(), cancellation()).await.unwrap();
                assert!(reply.value.is_err(), "{scenario}: {reply:?}");
                assert_eq!(reply.reply.delivery, McpDelivery::ResponseReceived);
                assert_eq!(task.state().task_id(), "opaque/private/task");
            }
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn actual_task_cancel_is_sent_even_when_local_wait_or_result_rpc_is_cancelled() {
    actix::System::new().block_on(async {
        for scenario in ["cancel", "cancel-in-result"] {
            let client = native();
            let session = open(&client, scenario).await;
            let mut task = accepted(create(&session).await);
            let signal = cancellation();
            let cancel = signal.clone();
            let (waited, ()) = tokio::join!(task.wait(limits(), signal), async {
                tokio::time::sleep(Duration::from_millis(if scenario == "cancel" {
                    5
                } else {
                    120
                }))
                .await;
                cancel.cancel();
            });
            let McpTaskWait::Cancellation { reply, interrupted } = waited.unwrap() else {
                panic!()
            };
            let reply = reply.unwrap();
            assert_eq!(
                reply.value.unwrap().unwrap().status(),
                McpTaskStatus::Cancelled
            );
            assert_eq!(task.state().status(), McpTaskStatus::Cancelled);
            if scenario == "cancel-in-result" {
                let interrupted = interrupted.unwrap();
                assert_eq!(interrupted.delivery, McpDelivery::ResponseReceived);
                assert_eq!(interrupted.result.unwrap().unwrap_err().code(), -32800);
            }
            drop(reply.reply);
            let counters = stats(&session).await;
            assert_eq!(counters["cancels"], 1);
            assert_eq!(counters["creates"], 1);
            if scenario == "cancel" {
                assert_eq!(counters["rpcCancels"], 0);
            }
            let rejected = task.cancel(limits(), cancellation()).await.unwrap();
            assert!(matches!(rejected.value, Ok(None)));
            assert_eq!(rejected.reply.result.unwrap().unwrap_err().code(), -32602);
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn ttl_and_whole_polling_budget_are_finite_and_never_recreate_work() {
    actix::System::new().block_on(async {
        for scenario in ["expired", "infinite"] {
            let client = native();
            let session = open(&client, scenario).await;
            let mut task = accepted(create(&session).await);
            let bound = McpTaskLimits::new(16384, 80, 2, 8, 32768, 5).unwrap();
            let error = task.wait(bound, cancellation()).await.unwrap_err();
            assert_eq!(
                error,
                if scenario == "expired" {
                    McpTaskError::Expired
                } else {
                    McpTaskError::Limit
                }
            );
            let counters = stats(&session).await;
            assert_eq!(counters["creates"], 1);
            assert_eq!(counters["results"], 0);
            if scenario == "expired" {
                assert_eq!(counters["gets"], 0);
            }
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn task_listing_preserves_empty_cursors_and_reports_partial_bounds_and_repeats() {
    actix::System::new().block_on(async {
        for scenario in ["normal", "list-repeat", "list-duplicate"] {
            let client = native();
            let session = open(&client, scenario).await;
            let inventory = session
                .list_remote_tasks(limits(), cancellation())
                .await
                .unwrap();
            if scenario == "normal" {
                assert!(inventory.complete, "{inventory:?}");
                assert_eq!(inventory.tasks.len(), 2);
                assert!(inventory.stop.is_none());
            } else if scenario == "list-repeat" {
                assert!(!inventory.complete);
                assert_eq!(inventory.tasks.len(), 2);
                assert_eq!(inventory.stop, Some(McpTaskError::InvalidResponse));
            } else {
                assert!(!inventory.complete);
                assert!(inventory.last_reply.as_ref().unwrap().value.is_err());
            }
            drop(inventory);
            assert_eq!(stats(&session).await["creates"], 0);
            let small = McpTaskLimits::new(16384, 1000, 1, 1, 32768, 5).unwrap();
            let inventory = session
                .list_remote_tasks(small, cancellation())
                .await
                .unwrap();
            assert!(!inventory.complete);
            assert!(inventory.tasks.len() <= 1);
            client.shutdown().await.unwrap();
        }
        let client = native();
        let session = open(&client, "no-list-cancel").await;
        let mut task = accepted(create(&session).await);
        assert_eq!(
            session
                .list_remote_tasks(limits(), cancellation())
                .await
                .unwrap_err(),
            McpTaskError::Unsupported
        );
        assert_eq!(
            task.cancel(limits(), cancellation()).await.unwrap_err(),
            McpTaskError::Unsupported
        );
        let counters = stats(&session).await;
        assert_eq!(counters["lists"], 0);
        assert_eq!(counters["cancels"], 0);
        client.shutdown().await.unwrap();
    });
}

struct HttpPeer {
    child: std::process::Child,
    port: u16,
}
impl HttpPeer {
    fn start(scenario: &str, mode: &str) -> Self {
        use std::io::BufRead;
        let mut child = std::process::Command::new("node")
            .args(["--input-type=module", "-e", PEER, scenario, mode])
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
        open_endpoint(
            client,
            McpEndpoint::Http {
                url: format!("http://127.0.0.1:{}/mcp", self.port),
                headers: BTreeMap::new(),
            },
            McpTransport::StreamableHttp,
        )
        .await
    }
}
impl Drop for HttpPeer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn task_operations_execute_over_http_json_and_sse_and_lost_acceptance_is_never_reposted() {
    for mode in ["http-json", "http-sse"] {
        for scenario in ["normal", "lost-ack"] {
            let peer = HttpPeer::start(scenario, mode);
            actix::System::new().block_on(async {
                let client = native();
                let session = peer.open(&client).await;
                let reply = create(&session).await;
                if scenario == "lost-ack" {
                    assert_eq!(reply.reply.delivery, McpDelivery::PossiblySent);
                    assert!(reply.reply.result.is_err());
                    assert!(matches!(reply.value, Ok(None)));
                    drop(reply);
                    client.shutdown().await.unwrap();
                    // Explicit new transport owner may inspect/reconcile known
                    // identities, but must never recreate an unacknowledged call.
                    let fresh_client = native();
                    let fresh = peer.open(&fresh_client).await;
                    assert_eq!(stats(&fresh).await["creates"], 1);
                    fresh_client.shutdown().await.unwrap();
                } else {
                    let mut task = accepted(reply);
                    let McpTaskWait::Result(reply) =
                        task.wait(limits(), cancellation()).await.unwrap()
                    else {
                        panic!()
                    };
                    assert!(matches!(reply.value, Ok(Some(()))));
                    drop(reply);
                    assert_eq!(stats(&session).await["creates"], 1);
                    client.shutdown().await.unwrap();
                }
            });
        }
    }
}

#[test]
fn task_timestamp_and_related_metadata_semantics_are_independent_of_schema_strings() {
    for (created, updated, valid) in [
        (
            "2024-02-29T01:02:03.123456789Z",
            "2024-02-29T02:02:03.123456789+01:00",
            true,
        ),
        ("2025-02-29T00:00:00Z", "2025-03-01T00:00:00Z", false),
        ("2026-01-01T24:00:00Z", "2026-01-02T00:00:00Z", false),
        ("2026-01-01T00:00:00+25:00", "2026-01-02T00:00:00Z", false),
        ("2026-01-01T00:00:00Z", "2025-12-31T23:59:59Z", false),
        (
            "2026-01-01T00:00:00.1234567892Z",
            "2026-01-01T00:00:00.1234567891Z",
            false,
        ),
        (
            "2026-01-01T00:00:00.123456789100Z",
            "2026-01-01T01:00:00.1234567891+01:00",
            true,
        ),
    ] {
        let document = document(&format!(
            r#"{{"taskId":"","status":"working","createdAt":"{created}","lastUpdatedAt":"{updated}","ttl":null}}"#
        ));
        assert_eq!(
            McpTaskState::from_document(document, &Limits::default()).is_ok(),
            valid,
            "{created} {updated}"
        );
    }
    let valid = document(&format!(
        r#"{{"_meta":{{"{MCP_RELATED_TASK}":{{"taskId":"opaque"}}}}}}"#
    ));
    assert_eq!(mcp_related_task_id(valid.value()).unwrap(), Some("opaque"));
    let invalid = document(&format!(
        r#"{{"_meta":{{"{MCP_RELATED_TASK}":{{"taskId":7}}}}}}"#
    ));
    assert_eq!(
        mcp_related_task_id(invalid.value()).unwrap_err(),
        McpTaskError::InvalidResponse
    );
}

#[test]
fn final_result_identity_terminal_observations_and_restored_retention_do_not_reset() {
    actix::System::new().block_on(async {
        let client = native();
        let session = open(&client, "input-required").await;
        let mut task = accepted(create(&session).await);
        assert_eq!(
            session
                .resume_remote_task(task.state().clone(), None)
                .unwrap_err(),
            McpTaskError::InvalidInput
        );
        assert_eq!(
            session
                .resume_remote_task(task.state().clone(), Some(0))
                .unwrap_err(),
            McpTaskError::Expired
        );
        let mut short = session
            .resume_remote_task(task.state().clone(), Some(10))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(
            short.get(limits(), cancellation()).await.unwrap_err(),
            McpTaskError::Expired
        );
        let waited = task.wait(limits(), cancellation()).await.unwrap();
        let McpTaskWait::Result(reply) = waited else {
            panic!("{waited:?}")
        };
        assert!(matches!(reply.value, Ok(Some(()))));
        drop(reply);
        let observed = task.get(limits(), cancellation()).await.unwrap();
        assert_eq!(
            observed.value.unwrap().unwrap().status(),
            McpTaskStatus::Completed
        );
        drop(observed.reply);
        // The independent peer includes a fresh result-request counter, so this
        // deliberately changed final result must not replace the accepted one.
        let changed = task.result(limits(), cancellation()).await.unwrap();
        assert_eq!(changed.value.unwrap_err(), McpTaskError::InvalidTransition);
        assert_eq!(changed.reply.delivery, McpDelivery::ResponseReceived);
        client.shutdown().await.unwrap();

        let client = native();
        let session = open(&client, "terminal-regression").await;
        let mut task = accepted(create(&session).await);
        let cancelled = task.cancel(limits(), cancellation()).await.unwrap();
        assert_eq!(
            cancelled.value.unwrap().unwrap().status(),
            McpTaskStatus::Cancelled
        );
        drop(cancelled.reply);
        // The peer reports late completion after cancellation. Cancellation is
        // terminal even when the underlying operation continues to execute.
        let late = task.get(limits(), cancellation()).await.unwrap();
        assert_eq!(late.value.unwrap_err(), McpTaskError::InvalidTransition);
        assert_eq!(task.state().status(), McpTaskStatus::Cancelled);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn retrieval_rpc_errors_are_retained_without_installing_an_underlying_final_result() {
    use htlk_executable::digest::hash_bytes;
    use htlk_runtime::{
        clock::SystemClock,
        store::{SqliteStore, StoreError},
    };
    for mode in ["stdio", "http-json", "http-sse"] {
        let peer = (mode != "stdio").then(|| HttpPeer::start("retrieval-error", mode));
        actix::System::new().block_on(async {
            let store = SqliteStore::in_memory(Default::default()).await.unwrap();
            let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
            let owner = persistence
                .acquire_owner(
                    &HostContext::new("alice".into(), &Limits::default()).unwrap(),
                    "protocol-tasks",
                    60_000,
                )
                .await
                .unwrap();
            let config = McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), None)
                .unwrap()
                .with_persistence(persistence);
            let client = NativeMcpClient::new(config).unwrap();
            let session = if let Some(peer) = &peer {
                peer.open(&client).await
            } else {
                open(&client, "retrieval-error").await
            };
            let key = hash_bytes(b"retrieval-is-not-execution");
            let creation = session
                .prepare_tool_task_call(
                    &descriptor(Some("required")),
                    params(),
                    McpToolTaskMode::Task {
                        ttl_ms: Some(60_000),
                    },
                )
                .unwrap()
                .with_logical_key(key)
                .dispatch(limits(), cancellation())
                .await
                .unwrap();
            let mut task = accepted(creation);
            let failed = task.result(limits(), cancellation()).await.unwrap();
            assert_eq!(failed.value, Err(McpTaskError::Retrieval));
            assert_eq!(
                failed.reply.delivery,
                McpDelivery::ResponseReceived,
                "delivery describes only the retrieval RPC"
            );
            let error = failed.reply.result.as_ref().unwrap().as_ref().unwrap_err();
            assert_eq!(error.code(), -32603);
            let server = session.server().digest(&Limits::default()).unwrap();
            let saved = store
                .inspect_mcp_remote_task_intent(&owner, server, key)
                .await
                .unwrap();
            assert!(saved.final_payload.is_none() && saved.result_status.is_none());
            assert_eq!(saved.task.unwrap().status(), McpTaskStatus::Working);
            let facts = store.mcp_journal(&owner, 0, 64).await.unwrap();
            let observed = facts
                .iter()
                .find(|fact| fact.kind == "remote_task_retrieval_error")
                .unwrap();
            let Value::Array(parts) = &observed.payload else {
                panic!()
            };
            assert_eq!(parts.last(), Some(&Value::Bytes(error.as_bytes().to_vec())));
            assert_eq!(
                store
                    .update_mcp_remote_task(
                        &owner,
                        session.durable_identity().unwrap(),
                        server,
                        key,
                        None,
                        Some((true, error.as_bytes().to_vec())),
                        htlk_runtime::clock::CoordinatorClock::now_ms(&SystemClock).unwrap()
                    )
                    .await
                    .unwrap_err(),
                StoreError::Invalid,
                "transactional boundary also refuses to make a retrieval error final"
            );
            drop(failed);
            let completed = task.result(limits(), cancellation()).await.unwrap();
            assert_eq!(completed.value, Ok(Some(())));
            let saved = store
                .inspect_mcp_remote_task_intent(&owner, server, key)
                .await
                .unwrap();
            assert_eq!(saved.result_status, Some(McpTaskStatus::Completed));
            assert_eq!(
                saved.final_payload.unwrap().1,
                completed.reply.raw_result.unwrap()
            );
            assert_eq!(stats(&session).await["creates"], 1);
            client.shutdown().await.unwrap();
        });
    }
}

#[test]
fn related_error_requires_terminal_observation_and_then_persists_exact_execution_error() {
    use htlk_executable::digest::hash_bytes;
    use htlk_runtime::{clock::SystemClock, store::SqliteStore};
    actix::System::new().block_on(async {
        let store = SqliteStore::in_memory(Default::default()).await.unwrap();
        let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
        let owner = persistence
            .acquire_owner(
                &HostContext::new("alice".into(), &Limits::default()).unwrap(),
                "protocol-tasks",
                60_000,
            )
            .await
            .unwrap();
        let client = NativeMcpClient::new(
            McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), None)
                .unwrap()
                .with_persistence(persistence),
        )
        .unwrap();
        let session = open(&client, "remote-error").await;
        let key = hash_bytes(b"known-execution-error");
        let creation = session
            .prepare_tool_task_call(
                &descriptor(Some("required")),
                params(),
                McpToolTaskMode::Task {
                    ttl_ms: Some(60_000),
                },
            )
            .unwrap()
            .with_logical_key(key)
            .dispatch(limits(), cancellation())
            .await
            .unwrap();
        let mut task = accepted(creation);
        let unproven = task.result(limits(), cancellation()).await.unwrap();
        assert_eq!(
            unproven.value,
            Err(McpTaskError::Retrieval),
            "related metadata alone does not prove terminal execution"
        );
        let raw = unproven
            .reply
            .result
            .unwrap()
            .unwrap_err()
            .as_bytes()
            .to_vec();
        let server = session.server().digest(&Limits::default()).unwrap();
        assert!(
            store
                .inspect_mcp_remote_task_intent(&owner, server, key)
                .await
                .unwrap()
                .final_payload
                .is_none()
        );
        let observed = task.get(limits(), cancellation()).await.unwrap();
        assert_eq!(
            observed.value.unwrap().unwrap().status(),
            McpTaskStatus::Failed
        );
        drop(observed.reply);
        let known = task.result(limits(), cancellation()).await.unwrap();
        assert_eq!(known.value, Ok(Some(())));
        assert_eq!(known.reply.result.unwrap().unwrap_err().as_bytes(), raw);
        let saved = store
            .inspect_mcp_remote_task_intent(&owner, server, key)
            .await
            .unwrap();
        assert_eq!(saved.final_payload, Some((true, raw)));
        assert_eq!(saved.task.unwrap().status(), McpTaskStatus::Failed);
        assert_eq!(stats(&session).await["creates"], 1);
        client.shutdown().await.unwrap();
    });
}

struct OrdinaryServices(std::sync::atomic::AtomicUsize);
impl McpClientHandler for OrdinaryServices {
    fn handle<'a>(
        &'a self,
        _: &'a McpInteractionContext,
        request: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let Value::Map(fields) = request.value() else {
                panic!()
            };
            let result = if fields.get("method")
                == Some(&Value::Text("sampling/createMessage".into()))
            {
                document(
                    r#"{"role":"assistant","model":"fixture-model","content":{"type":"text","text":"answer"}}"#,
                )
            } else {
                document(r#"{"action":"decline"}"#)
            };
            McpServiceOutcome::completed(result)
        })
    }
}
struct SamePrincipal;
impl McpClientAuthorizer for SamePrincipal {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), htlk_runtime::failure::RuntimeFailure> {
        assert_eq!(actor.principal(), action.owner.host().principal());
        Ok(())
    }
}

#[test]
fn uninstalled_client_direction_task_services_ignore_augmentation_and_execute_normally() {
    actix::System::new().block_on(async {
        let store = htlk_runtime::store::SqliteStore::in_memory(Default::default())
            .await
            .unwrap();
        let persistence =
            McpClientPersistence::new(store, Arc::new(htlk_runtime::clock::SystemClock));
        let representation = Limits::default();
        let owner = persistence
            .acquire_owner(
                &HostContext::new("alice".into(), &representation).unwrap(),
                "task-receiver",
                60_000,
            )
            .await
            .unwrap();
        let handlers = Arc::new(OrdinaryServices(std::sync::atomic::AtomicUsize::new(0)));
        let config = McpClientConfig::new(
            McpClientLimits {
                timeout_ms: 2000,
                ..Default::default()
            },
            BTreeMap::from([
                (
                    McpClientService::Sampling,
                    handlers.clone() as Arc<dyn McpClientHandler>,
                ),
                (
                    McpClientService::ElicitationForm,
                    handlers.clone() as Arc<dyn McpClientHandler>,
                ),
            ]),
            None,
        )
        .unwrap()
        .with_persistence(persistence)
        .with_authorizer(Arc::new(SamePrincipal));
        let client = NativeMcpClient::new(config).unwrap();
        let session = client
            .open(
                owner,
                ServerIdentity::new(
                    "tasks-peer".into(),
                    McpTransport::Stdio,
                    "tasks-peer".into(),
                    "1".into(),
                    &representation,
                )
                .unwrap(),
                McpEndpoint::Stdio {
                    program: "node".into(),
                    arguments: vec![
                        "--input-type=module".into(),
                        "-e".into(),
                        PEER.into(),
                        "receiving-unsupported".into(),
                        "stdio".into(),
                    ],
                    environment: BTreeMap::new(),
                },
                representation,
                cancellation(),
            )
            .await
            .unwrap();
        let prepared = session
            .prepare_tool_task_call(
                &descriptor(Some("optional")),
                params(),
                McpToolTaskMode::Inline,
            )
            .unwrap();
        let reply = prepared.dispatch(limits(), cancellation()).await.unwrap();
        assert!(
            matches!(reply.value, Ok(Some(McpToolTaskAcceptance::Inline))),
            "{reply:?}"
        );
        assert_eq!(handlers.0.load(std::sync::atomic::Ordering::SeqCst), 2);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn durable_remote_intent_precedes_send_and_survives_lost_ack_or_transport_owner_restart() {
    use htlk_executable::digest::hash_bytes;
    use htlk_runtime::{
        clock::SystemClock,
        store::{SqliteStore, StoreConfig},
    };
    for mode in ["http-json", "http-sse"] {
        for scenario in ["normal", "lost-ack", "create-error"] {
            let peer = HttpPeer::start(scenario, mode);
            actix::System::new().block_on(async {
                htlk_analyzer::mcp_schema_validators().unwrap();
                let path = std::env::temp_dir().join(format!(
                    "htlk-remote-task-{}-{}-{}.sqlite",
                    std::process::id(),
                    mode,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                let store = SqliteStore::open(&path, StoreConfig::default())
                    .await
                    .unwrap();
                let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
                let owner = persistence
                    .acquire_owner(
                        &HostContext::new("alice".into(), &Limits::default()).unwrap(),
                        "durable-outbound",
                        60_000,
                    )
                    .await
                    .unwrap();
                let config = McpClientConfig::new(
                    McpClientLimits {
                        timeout_ms: 2000,
                        ..Default::default()
                    },
                    BTreeMap::new(),
                    None,
                )
                .unwrap()
                .with_persistence(persistence.clone());
                let client = NativeMcpClient::new(config.clone()).unwrap();
                let server = ServerIdentity::new(
                    "tasks-peer".into(),
                    McpTransport::StreamableHttp,
                    "tasks-peer".into(),
                    "1".into(),
                    &Limits::default(),
                )
                .unwrap();
                let endpoint = McpEndpoint::Http {
                    url: format!("http://127.0.0.1:{}/mcp", peer.port),
                    headers: BTreeMap::new(),
                };
                let session = client
                    .open(
                        owner.clone(),
                        server.clone(),
                        endpoint.clone(),
                        Limits::default(),
                        cancellation(),
                    )
                    .await
                    .unwrap();
                let key = hash_bytes(b"stable-logical-work");
                let raw = session
                    .request(
                        "tools/call",
                        document(r#"{"name":"work","arguments":{},"task":{}}"#),
                        cancellation(),
                    )
                    .await;
                assert_eq!(raw.delivery, McpDelivery::NotSent);
                assert_eq!(
                    raw.result.unwrap_err().code(),
                    htlk_runtime::failure::FailureCode::Profile
                );
                assert_eq!(stats(&session).await["creates"], 0);
                let result = session
                    .prepare_tool_task_call(
                        &descriptor(Some("required")),
                        params(),
                        McpToolTaskMode::Task { ttl_ms: Some(1000) },
                    )
                    .unwrap()
                    .with_logical_key(key)
                    .dispatch(limits(), cancellation())
                    .await
                    .unwrap();
                let server_key = server.digest(&Limits::default()).unwrap();
                let saved = store
                    .inspect_mcp_remote_task_intent(&owner, server_key, key)
                    .await
                    .unwrap();
                if scenario == "normal" {
                    assert_eq!(
                        result
                            .reply
                            .task_registration
                            .as_ref()
                            .unwrap()
                            .as_ref()
                            .unwrap(),
                        &()
                    );
                    assert!(saved.task.is_some() && saved.acceptance.is_some());
                    let mut task = accepted(result);
                    let McpTaskWait::Result(final_reply) =
                        task.wait(limits(), cancellation()).await.unwrap()
                    else {
                        panic!()
                    };
                    assert!(matches!(final_reply.value, Ok(Some(()))));
                    let saved = store
                        .inspect_mcp_remote_task_intent(&owner, server_key, key)
                        .await
                        .unwrap();
                    assert_eq!(
                        saved.final_payload.unwrap().1,
                        final_reply.reply.raw_result.unwrap()
                    );
                    assert!(saved.origin.is_none());
                } else if scenario == "create-error" {
                    assert_eq!(result.reply.delivery, McpDelivery::ResponseReceived);
                    let error = result.reply.result.unwrap().unwrap_err();
                    assert_eq!(error.code(), -32051);
                    assert_eq!(saved.create_error.as_deref(), Some(error.as_bytes()));
                    assert!(
                        saved.task.is_none()
                            && saved.acceptance.is_none()
                            && saved.final_payload.is_none()
                    );
                } else {
                    assert_eq!(result.reply.delivery, McpDelivery::PossiblySent);
                    assert!(saved.task.is_none() && saved.acceptance.is_none());
                }
                client.shutdown().await.unwrap();
                let fresh_client = NativeMcpClient::new(config).unwrap();
                let fresh = fresh_client
                    .open(
                        owner.clone(),
                        server.clone(),
                        endpoint,
                        Limits::default(),
                        cancellation(),
                    )
                    .await
                    .unwrap();
                let again = fresh
                    .prepare_tool_task_call(
                        &descriptor(Some("required")),
                        params(),
                        McpToolTaskMode::Task { ttl_ms: Some(1000) },
                    )
                    .unwrap()
                    .with_logical_key(key)
                    .dispatch(limits(), cancellation())
                    .await
                    .unwrap_err();
                assert_eq!(
                    again,
                    McpTaskError::Persistence(htlk_runtime::failure::FailureCode::EffectUncertain)
                );
                assert_eq!(stats(&fresh).await["creates"], 1);
                fresh_client.shutdown().await.unwrap();
                drop(fresh);
                drop(session);
                drop(persistence);
                store.close().await.unwrap();
                let reopened = SqliteStore::open(&path, StoreConfig::default())
                    .await
                    .unwrap();
                let saved = reopened
                    .inspect_mcp_remote_task_intent(&owner, server_key, key)
                    .await
                    .unwrap();
                assert_eq!(saved.task.is_some(), scenario == "normal");
                reopened.close().await.unwrap();
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
                }
            });
        }
    }
}
