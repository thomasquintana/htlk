//! Independent stdio, HTTP JSON/idle SSE and coalesced POST SSE URL lifecycles.
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
    store::{SqliteStore, StoreConfig},
};
use std::{
    collections::BTreeMap,
    io::BufRead,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
const PEER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/support/mcp_url_peer.mjs"
);
fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(fields) = value else {
        panic!("map")
    };
    fields.get(key).unwrap()
}
fn number(value: &Value) -> i64 {
    let Value::Integer(n) = value else {
        panic!("integer")
    };
    *n
}
fn actor(name: &str) -> HostContext {
    HostContext::new(name.into(), &Limits::default()).unwrap()
}
fn cancel() -> Arc<McpCancellation> {
    Arc::new(McpCancellation::new())
}
struct Policy {
    deny: bool,
}
impl McpClientAuthorizer for Policy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if actor.principal() != action.owner.host().principal()
            || (self.deny && action.action == McpClientAction::Dispatch)
        {
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
        Ok(SecurityLabels::new(vec!["private-request".into()], &Limits::default()).unwrap())
    }
    fn result_labels(
        &self,
        _: &HostContext,
        _: &McpOwner,
        _: &JsonDocument,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::new(vec!["private-response".into()], &Limits::default()).unwrap())
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
struct IsolatedHuman(McpAwaitReply);
impl McpClientHandler for IsolatedHuman {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        request: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        assert_eq!(
            context.attribution,
            McpAttribution::Session,
            "an idle HTTP callback has no proven relation to the sole pending RPC"
        );
        assert!(context.originating_request_id.is_none());
        assert!(context.origin.is_none());
        self.0.handle(context, request)
    }
}
struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn endpoint(mode: &str, scenario: &str) -> (McpEndpoint, Option<Peer>) {
    if mode == "stdio" {
        (
            McpEndpoint::Stdio {
                program: "node".into(),
                arguments: vec![PEER.into(), mode.into(), scenario.into()],
                environment: BTreeMap::new(),
            },
            None,
        )
    } else {
        let mut child = Command::new("node")
            .args([PEER, mode, scenario])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        (
            McpEndpoint::Http {
                url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                headers: BTreeMap::new(),
            },
            Some(Peer(child)),
        )
    }
}
struct Fixture {
    mode: String,
    _peer: Option<Peer>,
    store: SqliteStore,
    owner: McpOwner,
    host: McpInteractionHost,
    client: NativeMcpClient,
    session: McpClientSession,
    events: tokio::sync::mpsc::Receiver<McpClientEvent>,
}
impl Fixture {
    async fn open(mode: &str, scenario: &str, deny: bool) -> Self {
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
        let owner = persistence
            .acquire_owner(&actor("alice"), "url-live", 120_000)
            .await
            .unwrap();
        let human: Arc<dyn McpClientHandler> = if scenario == "idle-callback" {
            Arc::new(IsolatedHuman(McpAwaitReply::new(persistence.clone())))
        } else {
            Arc::new(McpAwaitReply::new(persistence.clone()))
        };
        let (events, receive) = tokio::sync::mpsc::channel(32);
        let config = McpClientConfig::new(
            McpClientLimits {
                timeout_ms: 5000,
                frame_bytes: 64 * 1024,
                retained_bytes: 2 * 1024 * 1024,
                ..Default::default()
            },
            BTreeMap::from([
                (McpClientService::ElicitationForm, human.clone()),
                (McpClientService::ElicitationUrl, human),
            ]),
            Some(Arc::new(Events(events))),
        )
        .unwrap()
        .with_persistence(persistence)
        .with_authorizer(Arc::new(Policy { deny }));
        let host = McpInteractionHost::new(config.clone()).unwrap();
        let client = NativeMcpClient::new(config).unwrap();
        let (endpoint, peer) = endpoint(mode, scenario);
        let session = client
            .open(
                owner.clone(),
                ServerIdentity::new(
                    "url-peer".into(),
                    if mode == "stdio" {
                        McpTransport::Stdio
                    } else {
                        McpTransport::StreamableHttp
                    },
                    "url-peer".into(),
                    "1".into(),
                    &Limits::default(),
                )
                .unwrap(),
                endpoint,
                Limits::default(),
                cancel(),
            )
            .await
            .unwrap();
        if mode == "json" {
            tokio::time::timeout(Duration::from_secs(2), async {
                while session.server_events_available() != Some(true) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        Self {
            mode: mode.into(),
            _peer: peer,
            store,
            owner,
            host,
            client,
            session,
            events: receive,
        }
    }
    async fn event(&mut self) -> McpClientEvent {
        tokio::time::timeout(Duration::from_secs(2), self.events.recv())
            .await
            .unwrap_or_else(|_| panic!("{} event timeout: {:?}", self.mode, self.session.ended()))
            .unwrap()
    }
    async fn barrier(&mut self) -> Vec<McpClientEvent> {
        let mut observed = Vec::new();
        loop {
            let event = self.event().await;
            if event.method == "test/url_barrier" {
                break;
            }
            if event.method.starts_with("htlk/url_") {
                let raw = std::str::from_utf8(event.params.as_bytes()).unwrap();
                for secret in [
                    "http",
                    "private",
                    "ready",
                    "waiting",
                    "callback-url",
                    "form-only",
                    "unknown",
                ] {
                    assert!(!raw.contains(secret), "event leaked {secret}: {raw}");
                }
                assert_eq!(event.session_id, self.session.durable_identity());
                assert_eq!(event.server, *self.session.server());
            }
            assert_ne!(
                event.method, "notifications/elicitation/complete",
                "raw URL completion must never reach generic consumers"
            );
            observed.push(event);
        }
        observed
    }
    async fn stats(&self) {
        let reply = self
            .session
            .request("test/stats", doc("{}"), cancel())
            .await;
        let result = reply.result.unwrap().unwrap();
        assert_eq!(
            number(field(result.value(), "tools")),
            1,
            "no automatic operation retry"
        );
        assert_eq!(
            number(field(result.value(), "fetches")),
            0,
            "no URL/metadata navigation or prefetch"
        );
    }
    async fn close(self) {
        self.session.close().await.unwrap();
        self.client.shutdown().await.unwrap();
        self.store.close().await.unwrap();
    }
}

#[test]
fn required_errors_open_addressed_interactions_before_same_stream_completions_and_keep_original_outcome()
 {
    for mode in ["stdio", "sse", "json"] {
        actix::System::new().block_on(async {
            let mut f = Fixture::open(mode, "required", false).await;
            let reply = f
                .session
                .request(
                    "tools/call",
                    doc(r#"{"name":"protected","arguments":{}}"#),
                    cancel(),
                )
                .await;
            assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
            let ids = reply.url_interactions.unwrap().unwrap();
            let request_id = reply.request_id.unwrap();
            assert_eq!(ids.len(), 2);
            let original = reply.result.unwrap().unwrap_err();
            assert_eq!(original.code(), -32042);
            let before = original.as_bytes().to_vec();
            let page = f
                .host
                .list_urls(&actor("alice"), &f.owner, None, 8)
                .await
                .unwrap();
            assert_eq!(page.interactions.len(), 2);
            assert!(
                page.interactions
                    .iter()
                    .all(|a| ids.contains(&a.interaction_id))
            );
            assert!(
                f.host
                    .list_urls(&actor("bob"), &f.owner, None, 8)
                    .await
                    .is_err()
            );
            let mut ready = None;
            for address in page.interactions {
                let view = f
                    .host
                    .inspect_url(&actor("alice"), &f.owner, address)
                    .await
                    .unwrap();
                assert_eq!(
                    view.consent,
                    McpUrlConsent::Pending,
                    "server completion cannot manufacture consent"
                );
                assert!(view.request.url().contains("private=original-query"));
                assert!(view.origin.is_none());
                assert_eq!(
                    view.labels,
                    SecurityLabels::new(vec!["private-request".into()], &Limits::default())
                        .unwrap()
                );
                assert!(
                    f.host
                        .inspect_url(&actor("bob"), &f.owner, address)
                        .await
                        .is_err()
                );
                if view.request.elicitation_id() == "ready" {
                    ready = Some(address);
                }
                assert!(
                    f.host
                        .respond_url(
                            &actor("bob"),
                            &f.owner,
                            address,
                            view.request.params().digest(),
                            doc(r#"{"action":"accept"}"#),
                            "forbidden"
                        )
                        .await
                        .is_err()
                );
                assert!(
                    f.host
                        .respond_url(
                            &actor("alice"),
                            &f.owner,
                            address,
                            view.request.params().digest(),
                            doc(r#"{"action":"accept","content":{"secret":"forbidden"}}"#),
                            "invalid"
                        )
                        .await
                        .is_err()
                );
                let receipt = f
                    .host
                    .respond_url(
                        &actor("alice"),
                        &f.owner,
                        address,
                        view.request.params().digest(),
                        doc(r#"{"action":"accept"}"#),
                        &address.interaction_id.to_string(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    f.host
                        .respond_url(
                            &actor("alice"),
                            &f.owner,
                            address,
                            view.request.params().digest(),
                            doc(r#"{"action":"accept"}"#),
                            &address.interaction_id.to_string()
                        )
                        .await
                        .unwrap(),
                    receipt
                );
                let after = f
                    .host
                    .inspect_url(&actor("alice"), &f.owner, address)
                    .await
                    .unwrap();
                assert_eq!(after.consent, McpUrlConsent::Accepted);
                if view.request.elicitation_id() == "waiting" {
                    assert_eq!(after.completed_ms, None);
                }
            }
            if mode == "json" {
                f.session
                    .request("test/pulse", doc("{}"), cancel())
                    .await
                    .result
                    .unwrap()
                    .unwrap();
            }
            let events = f.barrier().await;
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.method == "htlk/url_interaction")
                    .count(),
                2
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.method == "htlk/url_completed")
                    .count(),
                1,
                "unknown, form, and duplicate IDs are ignored"
            );
            let ready = ready.unwrap();
            assert!(
                f.host
                    .inspect_url(&actor("alice"), &f.owner, ready)
                    .await
                    .unwrap()
                    .completed_ms
                    .is_some()
            );
            let error = f
                .host
                .inspect_url_required_error(
                    &actor("alice"),
                    &f.owner,
                    ready.server,
                    ready.session,
                    request_id.clone(),
                )
                .await
                .unwrap();
            assert_eq!(error.error.as_bytes(), before);
            assert_eq!(original.as_bytes(), before);
            assert_eq!(error.interactions, ids);
            assert!(
                f.host
                    .inspect_url_required_error(
                        &actor("bob"),
                        &f.owner,
                        ready.server,
                        ready.session,
                        request_id
                    )
                    .await
                    .is_err()
            );
            f.stats().await;
            f.close().await;
        });
    }
}

#[test]
fn form_and_url_frontends_share_addressed_replies_but_acceptance_is_not_completion() {
    for mode in ["stdio", "sse", "json"] {
        for headless in [false, true] {
            actix::System::new().block_on(async {
                let mut f = Fixture::open(mode, "callback", false).await;
                let session = f.session.clone();
                let operation = session.request(
                    "tools/call",
                    doc(r#"{"name":"nested","arguments":{}}"#),
                    cancel(),
                );
                let front = async {
                    let mut answered = Vec::new();
                    let mut url_address = None;
                    while answered.len() < 2 {
                        let id = if headless {
                            let page = f
                                .host
                                .list(&actor("alice"), &f.owner, None, 8)
                                .await
                                .unwrap();
                            let Some(id) = page
                                .interactions
                                .into_iter()
                                .find(|id| !answered.contains(id))
                            else {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                                continue;
                            };
                            id
                        } else {
                            let event = f.event().await;
                            if event.method != "htlk/interaction" {
                                continue;
                            }
                            let Value::Text(id) = field(event.params.value(), "interaction_id")
                            else {
                                panic!("id")
                            };
                            id.parse().unwrap()
                        };
                        let view = f
                            .host
                            .inspect(&actor("alice"), &f.owner, id, 8192)
                            .await
                            .unwrap();
                        if view.record.state == htlk_runtime::store::McpWorkState::Pending {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                            continue;
                        }
                        let request =
                            JsonDocument::new(&view.record.request, &Limits::default()).unwrap();
                        let Value::Map(params) = field(request.value(), "params") else {
                            panic!("params")
                        };
                        if params.get("mode") == Some(&Value::Text("url".into())) {
                            let page = f
                                .host
                                .list_urls(&actor("alice"), &f.owner, None, 8)
                                .await
                                .unwrap();
                            assert_eq!(page.interactions.len(), 1);
                            let address = page.interactions[0];
                            let url = f
                                .host
                                .inspect_url(&actor("alice"), &f.owner, address)
                                .await
                                .unwrap();
                            assert_eq!(url.callback_id, Some(id));
                            assert_eq!(url.completed_ms, None);
                            f.host
                                .respond_url(
                                    &actor("alice"),
                                    &f.owner,
                                    address,
                                    url.request.params().digest(),
                                    doc(r#"{"action":"accept"}"#),
                                    "url-consent",
                                )
                                .await
                                .unwrap();
                            url_address = Some(address);
                        } else {
                            assert!(
                                f.host
                                    .list_urls(&actor("alice"), &f.owner, None, 8)
                                    .await
                                    .unwrap()
                                    .interactions
                                    .is_empty()
                            );
                            assert!(
                                f.host
                                    .respond(
                                        &actor("alice"),
                                        &f.owner,
                                        id,
                                        view.record.request_digest,
                                        doc(
                                            r#"{"action":"accept","content":{"choice":"invalid"}}"#
                                        ),
                                        "bad-form"
                                    )
                                    .await
                                    .is_err()
                            );
                            f.host
                                .respond(
                                    &actor("alice"),
                                    &f.owner,
                                    id,
                                    view.record.request_digest,
                                    doc(r#"{"action":"accept","content":{"choice":"ok"}}"#),
                                    "form-consent",
                                )
                                .await
                                .unwrap();
                        }
                        answered.push(id);
                    }
                    url_address.unwrap()
                };
                let (reply, address) = tokio::join!(operation, front);
                assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
                assert!(reply.result.unwrap().is_ok());
                let url = f
                    .host
                    .inspect_url(&actor("alice"), &f.owner, address)
                    .await
                    .unwrap();
                assert_eq!(url.consent, McpUrlConsent::Accepted);
                assert_eq!(url.completed_ms, None);
                f.session
                    .request("test/pulse", doc("{}"), cancel())
                    .await
                    .result
                    .unwrap()
                    .unwrap();
                let events = f.barrier().await;
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.method == "htlk/url_completed")
                        .count(),
                    1
                );
                assert!(
                    f.host
                        .inspect_url(&actor("alice"), &f.owner, address)
                        .await
                        .unwrap()
                        .completed_ms
                        .is_some()
                );
                f.stats().await;
                f.close().await;
            });
        }
    }
}

#[test]
fn registration_failures_are_redacted_and_separate_from_the_original_rpc_error() {
    for (scenario, denied, code) in [
        ("malformed", false, FailureCode::McpProtocol),
        ("required", true, FailureCode::Policy),
    ] {
        actix::System::new().block_on(async {
            let mut f = Fixture::open("stdio", scenario, denied).await;
            let reply = f
                .session
                .request(
                    "tools/call",
                    doc(r#"{"name":"denied","arguments":{}}"#),
                    cancel(),
                )
                .await;
            assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
            assert_eq!(reply.url_interactions.unwrap().unwrap_err().code(), code);
            let original = reply.result.unwrap().unwrap_err();
            assert_eq!(original.code(), -32042);
            assert!(
                std::str::from_utf8(original.as_bytes())
                    .unwrap()
                    .contains("original-error-metadata")
            );
            assert!(
                f.host
                    .list_urls(&actor("alice"), &f.owner, None, 8)
                    .await
                    .unwrap()
                    .interactions
                    .is_empty()
            );
            let event = f.event().await;
            assert_eq!(event.method, "htlk/url_registration_failed");
            let raw = std::str::from_utf8(event.params.as_bytes()).unwrap();
            assert!(raw.contains(code.as_str()));
            assert!(!raw.contains("private"));
            f.stats().await;
            f.close().await;
        });
    }
}

#[test]
fn racing_authenticated_frontends_share_one_url_consent_receipt_without_retrying_the_rpc() {
    actix::System::new().block_on(async {
        let mut f = Fixture::open("stdio", "required", false).await;
        let reply = f
            .session
            .request(
                "tools/call",
                doc(r#"{"name":"racing","arguments":{}}"#),
                cancel(),
            )
            .await;
        let original = reply.result.unwrap().unwrap_err();
        let addresses = f
            .host
            .list_urls(&actor("alice"), &f.owner, None, 8)
            .await
            .unwrap();
        let mut waiting = None;
        for address in addresses.interactions {
            let record = f
                .host
                .inspect_url(&actor("alice"), &f.owner, address)
                .await
                .unwrap();
            if record.request.elicitation_id() == "waiting" {
                waiting = Some((address, record));
            }
        }
        let (address, record) = waiting.unwrap();
        let caller = actor("alice");
        let (interactive, headless) = tokio::join!(
            f.host.respond_url(
                &caller,
                &f.owner,
                address,
                record.request.params().digest(),
                doc(r#"{"action":"accept"}"#),
                "interactive"
            ),
            f.host.respond_url(
                &caller,
                &f.owner,
                address,
                record.request.params().digest(),
                doc(r#"{"action":"decline"}"#),
                "headless"
            ),
        );
        assert_eq!(
            [interactive.as_ref(), headless.as_ref()]
                .iter()
                .filter(|result| result.is_ok())
                .count(),
            1
        );
        let (key, result, receipt) = if let Ok(receipt) = interactive {
            ("interactive", doc(r#"{"action":"accept"}"#), receipt)
        } else {
            (
                "headless",
                doc(r#"{"action":"decline"}"#),
                headless.unwrap(),
            )
        };
        assert_eq!(
            f.host
                .respond_url(
                    &caller,
                    &f.owner,
                    address,
                    record.request.params().digest(),
                    result.clone(),
                    key
                )
                .await
                .unwrap(),
            receipt
        );
        let saved = f
            .host
            .inspect_url(&caller, &f.owner, address)
            .await
            .unwrap();
        assert_eq!(saved.consent, McpUrlConsent::from_result(&result).unwrap());
        assert_eq!(saved.completed_ms, None);
        assert_eq!(
            saved.labels,
            SecurityLabels::new(
                vec!["private-request".into(), "private-response".into()],
                &Limits::default()
            )
            .unwrap()
        );
        assert_eq!(original.code(), -32042);
        f.barrier().await;
        f.stats().await;
        f.close().await;
    });
}

#[test]
fn idle_http_url_callback_does_not_guess_the_only_pending_operation_as_its_parent() {
    actix::System::new().block_on(async {
        let mut f = Fixture::open("json", "idle-callback", false).await;
        let session = f.session.clone();
        let operation = session.request(
            "tools/call",
            doc(r#"{"name":"unrelated","arguments":{}}"#),
            cancel(),
        );
        let front = async {
            let event = f.event().await;
            assert_eq!(event.method, "htlk/interaction");
            let page = f
                .host
                .list_urls(&actor("alice"), &f.owner, None, 8)
                .await
                .unwrap();
            let address = page.interactions[0];
            let record = f
                .host
                .inspect_url(&actor("alice"), &f.owner, address)
                .await
                .unwrap();
            assert!(record.origin.is_none());
            f.host
                .respond_url(
                    &actor("alice"),
                    &f.owner,
                    address,
                    record.request.params().digest(),
                    doc(r#"{"action":"accept"}"#),
                    "independent-consent",
                )
                .await
                .unwrap();
        };
        let (reply, ()) = tokio::join!(operation, front);
        assert_eq!(reply.delivery, McpDelivery::ResponseReceived);
        assert!(reply.result.unwrap().is_ok());
        f.stats().await;
        f.close().await;
    });
}
