//! Durable receiver tasks, including independent-writer cancellation races.
#[path = "support/store.rs"]
mod test_store;
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Value},
    digest::hash_bytes,
};
use htlk_runtime::{
    HostContext,
    clock::CoordinatorClock,
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, McpFuture, client::*},
    records::SecurityLabels,
    store::{
        McpReceivedTask, McpReceivedTaskLimits, McpReceivedTaskStatus, McpStoredInteraction,
        McpStoredSession, McpWorkClaim, McpWorkState, StoreConfig, StoreError,
    },
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};
use test_store::TestStore as SqliteStore;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "htlk-receiver-tasks-{}-{}-{}.sqlite",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            )))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
struct Clock(AtomicI64);
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(self.0.load(Ordering::SeqCst))
    }
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
}
struct Installed;
impl McpClientHandler for Installed {
    fn handle<'a>(
        &'a self,
        _: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        // Task-control APIs must never dispatch the original handler themselves.
        Box::pin(async { panic!("task control redispatched host work") })
    }
}
fn representation() -> Limits {
    Limits {
        max_document_bytes: 64 * 1024,
        ..Default::default()
    }
}
fn store_config() -> StoreConfig {
    StoreConfig {
        limits: representation(),
        ..Default::default()
    }
}
fn doc(text: &str) -> JsonDocument {
    JsonDocument::new(text.as_bytes(), &representation()).unwrap()
}
fn host(principal: &str) -> HostContext {
    HostContext::new(principal.into(), &representation()).unwrap()
}
fn quota(records: usize, active: usize) -> McpReceivedTaskLimits {
    McpReceivedTaskLimits::new(
        records,
        active,
        500,
        1000,
        10,
        16 * 1024,
        records * (16 * 1024 + 1024),
    )
    .unwrap()
}
fn settings(records: usize, active: usize) -> McpTaskReceiverSettings {
    McpTaskReceiverSettings::new(
        McpTaskReceiverMethods {
            sampling: true,
            elicitation: true,
        },
        quota(records, active),
        McpTaskReceiverWait::new(500, 5, 2, 64 * 1024).unwrap(),
    )
    .unwrap()
}
fn config(store: SqliteStore, clock: Arc<Clock>) -> McpClientConfig {
    McpClientConfig::new(
        McpClientLimits {
            callbacks: 32,
            frame_bytes: 64 * 1024,
            retained_bytes: 1024 * 1024,
            timeout_ms: 1000,
            ..Default::default()
        },
        BTreeMap::from([
            (
                McpClientService::Sampling,
                Arc::new(Installed) as Arc<dyn McpClientHandler>,
            ),
            (
                McpClientService::ElicitationForm,
                Arc::new(Installed) as Arc<dyn McpClientHandler>,
            ),
            (
                McpClientService::ElicitationUrl,
                Arc::new(Installed) as Arc<dyn McpClientHandler>,
            ),
        ]),
        None,
    )
    .unwrap()
    .with_persistence(McpClientPersistence::new(store, clock))
    .with_authorizer(Arc::new(Policy))
}
fn request(id: &str, method: &str, ttl: Option<&str>) -> JsonDocument {
    let params = match method {
        "sampling/createMessage" => {
            r#""messages":[{"role":"user","content":{"type":"text","text":"private-question"}}],"maxTokens":10"#
        }
        "elicitation/create" => {
            r#""message":"Continue?","requestedSchema":{"type":"object","properties":{}}"#
        }
        _ => "",
    };
    let task = ttl.map_or(String::new(), |ttl| format!(r#", "task":{ttl}"#));
    doc(&format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{{params}{task}}}}}"#
    ))
}
fn control(id: &str, method: &str, task_id: &str) -> JsonDocument {
    doc(&format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{"taskId":"{task_id}","_meta":{{"io.modelcontextprotocol/related-task":{{"taskId":"ignored-conflict"}}}}}}}}"#
    ))
}
fn field<'a>(document: &'a JsonDocument, key: &str) -> &'a Value {
    let Value::Map(fields) = document.value() else {
        panic!()
    };
    fields.get(key).unwrap()
}
fn error_code(reply: &JsonDocument) -> i64 {
    let Value::Map(error) = field(reply, "error") else {
        panic!("{reply:?}")
    };
    let Some(Value::Integer(code)) = error.get("code") else {
        panic!()
    };
    *code
}

struct Fixture {
    _path: Database,
    first: SqliteStore,
    second: SqliteStore,
    owner: McpOwner,
    session: McpStoredSession,
    clock: Arc<Clock>,
    receiver: McpTaskReceiver,
}
impl Fixture {
    async fn new(records: usize, active: usize) -> Self {
        htlk_analyzer::mcp_schema_validators().unwrap();
        let path = Database::new();
        let first = SqliteStore::open(&path.0, store_config()).await.unwrap();
        let second = SqliteStore::open(&path.0, store_config()).await.unwrap();
        let owner = first
            .acquire_mcp_owner(&host("alice"), "receiver", 10, 10_000)
            .await
            .unwrap();
        let session = first
            .open_mcp_epoch(&owner, hash_bytes(b"server-a"), 10)
            .await
            .unwrap();
        let clock = Arc::new(Clock(AtomicI64::new(20)));
        let receiver = McpTaskReceiver::new(
            config(first.clone(), clock.clone()),
            settings(records, active),
        )
        .unwrap();
        Self {
            _path: path,
            first,
            second,
            owner,
            session,
            clock,
            receiver,
        }
    }
    fn now(&self) -> i64 {
        self.clock.0.load(Ordering::SeqCst)
    }
    async fn receive(&self, request: &JsonDocument, deadline: i64) -> McpStoredInteraction {
        self.first
            .receive_mcp_interaction(
                &self.owner,
                self.session.id,
                request.as_bytes().to_vec(),
                SecurityLabels::new(vec!["private".into()], &representation()).unwrap(),
                deadline,
                self.now(),
                128,
            )
            .await
            .unwrap()
    }
    async fn accept(
        &self,
        request: &JsonDocument,
        deadline: i64,
    ) -> (McpStoredInteraction, McpReceivedTask) {
        let record = self.receive(request, deadline).await;
        let accepted = self
            .receiver
            .accept(
                &self.owner,
                self.session.server,
                self.session.id,
                record.id,
                request,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accepted.task.status(), McpReceivedTaskStatus::Working);
        htlk_analyzer::validate_mcp_message(
            htlk_analyzer::McpMessageKind::CreateTaskResult,
            &accepted.result,
            &representation(),
        )
        .unwrap();
        assert!(matches!(field(&accepted.result, "task"), Value::Map(_)));
        assert_eq!(
            self.first
                .mcp_owner_usage(&self.owner)
                .await
                .unwrap()
                .calls_spent,
            0
        );
        (record, accepted.task)
    }
    async fn handle(&self, request: &JsonDocument) -> JsonDocument {
        self.receiver
            .handle_request(
                &self.owner,
                self.session.server,
                self.session.id,
                request,
                Arc::new(McpCancellation::new()),
            )
            .await
            .unwrap()
            .unwrap()
    }
    async fn complete(
        &self,
        record: &McpStoredInteraction,
        raw: Vec<u8>,
    ) -> Result<McpStoredInteraction, StoreError> {
        self.second
            .complete_mcp_work(
                &self.owner,
                record.id,
                self.owner.host(),
                &format!("finish:{}", record.id),
                raw,
                SecurityLabels::default(),
                None,
                McpWorkDelivery::Completed,
                self.now(),
            )
            .await
    }
}

#[test]
fn competing_acceptances_commit_one_opaque_identity_before_any_host_work() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16, 8).await;
        let original = request(
            "0",
            "sampling/createMessage",
            Some(r#"{"ttl":99999,"taskId":"requestor-chosen"}"#),
        );
        let record = f.receive(&original, 900).await;
        let (one, two) = tokio::join!(
            f.first.create_mcp_received_task(
                &f.owner,
                f.session.id,
                f.session.server,
                record.id,
                20,
                quota(16, 8)
            ),
            f.second.create_mcp_received_task(
                &f.owner,
                f.session.id,
                f.session.server,
                record.id,
                20,
                quota(16, 8)
            ),
        );
        let one = one.unwrap();
        let two = two.unwrap();
        assert_eq!(one.task_id(), two.task_id());
        assert_eq!(one.task_id().len(), 64);
        assert_ne!(one.task_id(), "requestor-chosen");
        assert_eq!(one.ttl_ms(), 1000);
        let facts = f.second.mcp_journal(&f.owner, 0, 64).await.unwrap();
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.kind == "receiver_task_accepted")
                .count(),
            1
        );
        let usage = f.second.mcp_owner_usage(&f.owner).await.unwrap();
        assert_eq!((usage.calls_spent, usage.outstanding), (0, 0));
        let string_id = request("\"0\"", "elicitation/create", Some("{}"));
        let record = f.receive(&string_id, 900).await;
        let other = f
            .receiver
            .accept(
                &f.owner,
                f.session.server,
                f.session.id,
                record.id,
                &string_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_ne!(one.task_id(), other.task.task_id());
        assert_eq!(other.task.ttl_ms(), 500);
    });
}

#[test]
fn result_polling_reads_saved_sampling_elicitation_and_error_payloads_without_redispatch() {
    actix::System::new().block_on(async {
        for (method, error) in [("sampling/createMessage", false), ("elicitation/create", false), ("sampling/createMessage", true)] {
            let f = Fixture::new(16, 8).await;
            let request = request("7", method, Some("{}"));
            let (record, task) = f.accept(&request, 900).await;
            let initial = task.acceptance(&representation()).unwrap();
            assert!(matches!(f.first.begin_mcp_work(&f.owner,record.id,20,16,8).await.unwrap(), McpWorkClaim::Started(_)));
            let underlying = if error {
                br#"{ "jsonrpc": "2.0", "id": 7, "error": { "code": -32050, "message": "private-original", "data": {"n":1.0} } }"#.to_vec()
            } else if method == "sampling/createMessage" {
                br#"{ "jsonrpc":"2.0", "id":7, "result":{ "role":"assistant", "model":"actual-model", "content":{"type":"text","text":"private-answer"}, "_meta":{"preserved":true} } }"#.to_vec()
            } else { br#"{"jsonrpc":"2.0","id":7,"result":{"action":"decline"}}"#.to_vec() };
            let retrieval = control("\"result-request\"", "tasks/result", task.task_id());
            let (reply, ()) = tokio::join!(f.receiver.handle_request(&f.owner, f.session.server, f.session.id, &retrieval, Arc::new(McpCancellation::new())), async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                f.complete(&record, underlying.clone()).await.unwrap();
            });
            let reply = reply.unwrap().unwrap();
            assert_eq!(field(&reply, "id"), &Value::Text("result-request".into()));
            let payload = field(&reply, if error { "error" } else { "result" });
            let Value::Map(payload) = payload else { panic!() };
            let Some(Value::Map(meta)) = payload.get("_meta") else { panic!() };
            let Some(Value::Map(related)) = meta.get("io.modelcontextprotocol/related-task") else { panic!() };
            assert_eq!(related.get("taskId"), Some(&Value::Text(task.task_id().into())));
            if error { assert_eq!(error_code(&reply), -32050); }
            let saved = f.second.mcp_interaction(&f.owner,record.id).await.unwrap();
            assert_eq!(saved.reply.unwrap(), underlying, "original bytes must stay separate from task projection");
            let current = f.second.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
            assert_eq!(current.status(), if error { McpReceivedTaskStatus::Failed } else { McpReceivedTaskStatus::Completed });
            assert_eq!(current.result().unwrap().source_reply_digest(), saved.reply_digest);
            let duplicate = f.receiver.accept(&f.owner,f.session.server,f.session.id,record.id,&request).await.unwrap().unwrap();
            assert_eq!(duplicate.result.as_bytes(), initial.as_bytes());
            let again = f.handle(&retrieval).await;
            assert_eq!(again.as_bytes(), reply.as_bytes());
            assert_eq!(error_code(&f.handle(&control("8","tasks/cancel",task.task_id())).await), -32602);
            assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().calls_spent,1);
        }
    });
}

#[test]
fn cancellation_commits_before_reply_and_preserves_uncertain_work_and_late_evidence() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16,8).await;
        let request = request("1","sampling/createMessage",Some("{}"));
        let (record, task) = f.accept(&request,900).await;
        f.first.begin_mcp_work(&f.owner,record.id,20,16,8).await.unwrap();
        let response = f.handle(&control("2","tasks/cancel",task.task_id())).await;
        let committed = f.second.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
        assert_eq!(committed.status(),McpReceivedTaskStatus::Cancelled);
        let Value::Map(result) = field(&response,"result") else { panic!() };
        assert_eq!(result.get("status"),Some(&Value::Text("cancelled".into())));
        let work = f.second.mcp_interaction(&f.owner,record.id).await.unwrap();
        assert_eq!(work.state,McpWorkState::Cancelled);
        assert_eq!(work.work_delivery,McpWorkDelivery::Uncertain);
        assert!(work.outstanding && work.call_charged);
        assert!(work.reply.is_none());
        let late = br#"{"jsonrpc":"2.0","id":1,"result":{"role":"assistant","model":"late-model","content":{"type":"text","text":"late"}}}"#.to_vec();
        assert_eq!(f.complete(&record,late.clone()).await.unwrap_err(),StoreError::Terminal);
        let evidence = f.second.retain_mcp_late_reply(&f.owner,record.id,late,SecurityLabels::default(),McpWorkDelivery::Completed,None,8).await.unwrap();
        let before = f.handle(&control("3","tasks/result",task.task_id())).await;
        assert_eq!(error_code(&before),-32800);
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,1);
        f.second.settle_mcp_work(&f.owner,record.id,evidence,20).await.unwrap();
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,0);
        let after = f.handle(&control("3","tasks/result",task.task_id())).await;
        assert_eq!(before.as_bytes(),after.as_bytes());
        assert_eq!(error_code(&f.handle(&control("4","tasks/cancel",task.task_id())).await),-32602);
    });
}

#[test]
fn independent_completion_and_cancellation_writers_choose_one_immutable_terminal_outcome() {
    actix::System::new().block_on(async {
        let f = Fixture::new(32,32).await;
        for id in 0..20 {
            let request = request(&id.to_string(),"sampling/createMessage",Some("{}"));
            let record = f.receive(&request,900).await;
            let task = f.receiver.accept(&f.owner,f.session.server,f.session.id,record.id,&request).await.unwrap().unwrap().task;
            f.first.begin_mcp_work(&f.owner,record.id,20,32,32).await.unwrap();
            let reply = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"role":"assistant","model":"race-model","content":{{"type":"text","text":"answer"}}}}}}"#).into_bytes();
            let (finished,cancelled) = tokio::join!(f.complete(&record,reply),
                f.first.cancel_mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20));
            assert_ne!(finished.is_ok(),cancelled.is_ok());
            let task = f.second.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
            match (finished,cancelled) {
                (Ok(_),Err(error)) => { assert_eq!(error,StoreError::Terminal); assert_eq!(task.status(),McpReceivedTaskStatus::Completed); }
                (Err(error),Ok(_)) => { assert_eq!(error,StoreError::Terminal); assert_eq!(task.status(),McpReceivedTaskStatus::Cancelled); }
                _ => panic!("exactly one terminal writer must win"),
            }
            let original = task.result().unwrap().payload().digest();
            assert_eq!(f.first.set_mcp_received_task_input(&f.owner,f.session.id,f.session.server,task.task_id(),true,20).await.unwrap_err(),StoreError::Terminal);
            assert_eq!(f.first.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap().result().unwrap().payload().digest(),original);
        }
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().calls_spent,20);
    });
}

#[test]
fn store_scope_isolation_cursor_paging_and_authorized_new_epoch_retrieval() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16, 8).await;
        let mut ids = vec![];
        for id in 0..3 {
            let request = request(&id.to_string(), "elicitation/create", Some("{}"));
            let (_, task) = f.accept(&request, 900).await;
            ids.push(task.task_id().to_owned());
        }
        let first = f
            .second
            .mcp_received_tasks(&f.owner, f.session.id, f.session.server, None, 2, 20)
            .await
            .unwrap();
        assert_eq!(first.tasks.len(), 2);
        let next = first.next_cursor.unwrap();
        let last = f
            .first
            .mcp_received_tasks(&f.owner, f.session.id, f.session.server, Some(&next), 2, 20)
            .await
            .unwrap();
        assert_eq!(last.tasks.len(), 1);
        assert!(last.next_cursor.is_none());
        let mut listed: Vec<_> = first
            .tasks
            .iter()
            .chain(&last.tasks)
            .map(|task| task.task_id().to_owned())
            .collect();
        listed.sort();
        ids.sort();
        assert_eq!(listed, ids);
        assert_eq!(
            f.second
                .mcp_received_tasks(&f.owner, f.session.id, f.session.server, Some(""), 2, 20)
                .await
                .unwrap_err(),
            StoreError::Missing
        );
        let bob = f
            .second
            .acquire_mcp_owner(&host("bob"), "receiver", 20, 10_000)
            .await
            .unwrap();
        let bob_session = f
            .second
            .open_mcp_epoch(&bob, f.session.server, 20)
            .await
            .unwrap();
        assert!(
            f.second
                .mcp_received_tasks(&bob, bob_session.id, bob_session.server, None, 2, 20)
                .await
                .unwrap()
                .tasks
                .is_empty()
        );
        assert_eq!(
            f.second
                .mcp_received_task(&bob, bob_session.id, bob_session.server, &ids[0], 20)
                .await
                .unwrap_err(),
            StoreError::Missing
        );
        let other = f
            .second
            .open_mcp_epoch(&f.owner, hash_bytes(b"server-b"), 20)
            .await
            .unwrap();
        assert_eq!(
            f.second
                .mcp_received_task(&f.owner, other.id, other.server, &ids[0], 20)
                .await
                .unwrap_err(),
            StoreError::Missing
        );
        assert_eq!(
            f.second
                .mcp_received_task(&f.owner, other.id, f.session.server, &ids[0], 20)
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
        f.first
            .close_mcp_epoch(&f.owner, f.session.id, 20)
            .await
            .unwrap();
        let fresh = f
            .second
            .open_mcp_epoch(&f.owner, f.session.server, 20)
            .await
            .unwrap();
        assert_eq!(
            f.first
                .mcp_received_task(&f.owner, fresh.id, fresh.server, &ids[0], 20)
                .await
                .unwrap()
                .task_id(),
            ids[0]
        );
        f.first.release_mcp_owner(&f.owner, 20).await.unwrap();
        let takeover = f
            .second
            .acquire_mcp_owner(&host("alice"), "receiver", 21, 10_000)
            .await
            .unwrap();
        let takeover_session = f
            .second
            .open_mcp_epoch(&takeover, f.session.server, 21)
            .await
            .unwrap();
        assert_eq!(
            f.first
                .mcp_received_task(
                    &takeover,
                    takeover_session.id,
                    takeover_session.server,
                    &ids[0],
                    21
                )
                .await
                .unwrap_err(),
            StoreError::Missing
        );
        assert_eq!(
            f.first
                .mcp_received_task(&f.owner, fresh.id, fresh.server, &ids[0], 21)
                .await
                .unwrap_err(),
            StoreError::Fenced
        );
    });
}

#[test]
fn ttl_and_admission_bounds_never_release_uncertain_work_or_recreate_expired_tasks() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16, 1).await;
        let original = request("1", "sampling/createMessage", Some(r#"{"ttl":10}"#));
        let (work, task) = f.accept(&original, 900).await;
        f.first
            .begin_mcp_work(&f.owner, work.id, 20, 16, 8)
            .await
            .unwrap();
        f.clock.0.store(31, Ordering::SeqCst);
        assert_eq!(
            f.second
                .mcp_received_task(&f.owner, f.session.id, f.session.server, task.task_id(), 31)
                .await
                .unwrap_err(),
            StoreError::Expired
        );
        assert_eq!(
            f.receiver
                .accept(&f.owner, f.session.server, f.session.id, work.id, &original)
                .await
                .unwrap_err()
                .code(),
            FailureCode::RetentionExpired
        );
        let second = request("2", "sampling/createMessage", Some("{}"));
        let next = f.receive(&second, 900).await;
        assert_eq!(
            f.first
                .create_mcp_received_task(
                    &f.owner,
                    f.session.id,
                    f.session.server,
                    next.id,
                    31,
                    quota(16, 1)
                )
                .await
                .unwrap_err(),
            StoreError::Budget
        );
        assert_eq!(
            f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,
            1
        );
        assert_eq!(
            f.second
                .mcp_interaction(&f.owner, work.id)
                .await
                .unwrap()
                .state,
            McpWorkState::Intent
        );

        let f = Fixture::new(16, 8).await;
        for (id, ttl) in [
            (1, r#"{"ttl":-1}"#),
            (2, r#"{"ttl":null}"#),
            (3, r#"{"ttl":0.5}"#),
            (4, "false"),
        ] {
            let request = request(&id.to_string(), "sampling/createMessage", Some(ttl));
            let work = f.receive(&request, 900).await;
            assert_eq!(
                f.first
                    .create_mcp_received_task(
                        &f.owner,
                        f.session.id,
                        f.session.server,
                        work.id,
                        20,
                        quota(16, 8)
                    )
                    .await
                    .unwrap_err(),
                StoreError::Invalid
            );
        }
        assert_eq!(
            f.first.mcp_owner_usage(&f.owner).await.unwrap().calls_spent,
            0
        );
        let zero = request("5", "sampling/createMessage", Some(r#"{"ttl":0}"#));
        let (_, task) = f.accept(&zero, 900).await;
        assert_eq!(
            f.second
                .mcp_received_task(&f.owner, f.session.id, f.session.server, task.task_id(), 20)
                .await
                .unwrap_err(),
            StoreError::Expired
        );
    });
}

#[test]
fn result_wait_cancellation_and_timeout_do_not_cancel_work_and_input_required_is_durable() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16, 8).await;
        let request = request("1", "sampling/createMessage", Some("{}"));
        let (work, task) = f.accept(&request, 900).await;
        f.first
            .begin_mcp_work(&f.owner, work.id, 20, 16, 8)
            .await
            .unwrap();
        let state = f
            .second
            .set_mcp_received_task_input(
                &f.owner,
                f.session.id,
                f.session.server,
                task.task_id(),
                true,
                20,
            )
            .await
            .unwrap();
        assert_eq!(state.status(), McpReceivedTaskStatus::InputRequired);
        let token = Arc::new(McpCancellation::new());
        token.cancel();
        let request = control("2", "tasks/result", task.task_id());
        let response = f
            .receiver
            .handle_request(&f.owner, f.session.server, f.session.id, &request, token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(error_code(&response), -32800);
        assert_eq!(
            f.second
                .mcp_interaction(&f.owner, work.id)
                .await
                .unwrap()
                .state,
            McpWorkState::Intent
        );
        let receiver = McpTaskReceiver::new(
            config(f.second.clone(), f.clock.clone()),
            McpTaskReceiverSettings::new(
                McpTaskReceiverMethods {
                    sampling: true,
                    elicitation: true,
                },
                quota(16, 8),
                McpTaskReceiverWait::new(25, 5, 2, 65536).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let response = receiver
            .handle_request(
                &f.owner,
                f.session.server,
                f.session.id,
                &request,
                Arc::new(McpCancellation::new()),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(error_code(&response), -32000);
        assert_eq!(
            f.first
                .mcp_received_task(&f.owner, f.session.id, f.session.server, task.task_id(), 20)
                .await
                .unwrap()
                .status(),
            McpReceivedTaskStatus::InputRequired
        );
        assert_eq!(
            f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,
            1
        );
        let state = f
            .second
            .set_mcp_received_task_input(
                &f.owner,
                f.session.id,
                f.session.server,
                task.task_id(),
                false,
                20,
            )
            .await
            .unwrap();
        assert_eq!(state.status(), McpReceivedTaskStatus::Working);
        f.clock.0.store(901, Ordering::SeqCst);
        // TTL 500 expires before execution deadline; expiration itself is not a
        // protocol failed/cancelled transition and must leave intent uncertain.
        assert_eq!(
            error_code(&f.handle(&control("3", "tasks/get", task.task_id())).await),
            -32602
        );
        assert_eq!(
            f.first
                .mcp_interaction(&f.owner, work.id)
                .await
                .unwrap()
                .work_delivery,
            McpWorkDelivery::Uncertain
        );
    });
}

#[test]
fn unsupported_metadata_is_ignored_and_capabilities_match_installed_receiver_methods() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16, 8).await;
        let selected = McpTaskReceiverSettings::new(
            McpTaskReceiverMethods {
                sampling: false,
                elicitation: true,
            },
            quota(16, 8),
            McpTaskReceiverWait::new(100, 5, 2, 65536).unwrap(),
        )
        .unwrap();
        let caps = selected.capabilities(&representation()).unwrap();
        let Value::Map(requests) = field(&caps, "requests") else {
            panic!()
        };
        assert!(requests.get("sampling").is_none());
        assert!(requests.get("elicitation").is_some());
        let receiver =
            McpTaskReceiver::new(config(f.first.clone(), f.clock.clone()), selected).unwrap();
        let request = request("1", "sampling/createMessage", Some("false"));
        // Unsupported augmentation is ignored before even looking up an
        // interaction or interpreting the malformed task field.
        assert!(
            receiver
                .accept(
                    &f.owner,
                    f.session.server,
                    f.session.id,
                    hash_bytes(b"no-record"),
                    &request
                )
                .await
                .unwrap()
                .is_none()
        );
        let view = mcp_without_task_augmentation(&request, &representation()).unwrap();
        validate_client_request(
            &view,
            &config(f.first.clone(), f.clock.clone()),
            &representation(),
        )
        .unwrap();
        assert_eq!(field(&view, "id"), field(&request, "id"));
        let Value::Map(params) = field(&view, "params") else {
            panic!()
        };
        assert!(params.get("task").is_none());
        let Value::Map(original) = field(&request, "params") else {
            panic!()
        };
        assert_eq!(original.get("task"), Some(&Value::Bool(false)));
        let params =
            doc(r#"{"name":"nested","_meta":{"progressToken":"lifetime","private":true}}"#);
        let associated =
            mcp_with_received_task_metadata(&params, "receiver-id", &representation()).unwrap();
        let Value::Map(meta) = field(&associated, "_meta") else {
            panic!()
        };
        assert_eq!(
            meta.get("progressToken"),
            Some(&Value::Text("lifetime".into()))
        );
        assert_eq!(meta.get("private"), Some(&Value::Bool(true)));
        assert!(
            mcp_with_received_task_metadata(&associated, "different-task", &representation())
                .is_err()
        );
        let no_persistence =
            McpClientConfig::new(McpClientLimits::default(), BTreeMap::new(), None).unwrap();
        assert!(McpTaskReceiver::new(no_persistence, selected).is_err());
    });
}

#[test]
fn receiver_state_and_original_reply_survive_all_connection_handles_closing() {
    actix::System::new().block_on(async {
        let path = Database::new();
        let (owner, task_id, initial, source) = {
            let store = SqliteStore::open(&path.0, store_config()).await.unwrap();
            let owner = store
                .acquire_mcp_owner(&host("alice"), "durable", 10, 10_000)
                .await
                .unwrap();
            let session = store
                .open_mcp_epoch(&owner, hash_bytes(b"server-a"), 10)
                .await
                .unwrap();
            let request = request("1", "elicitation/create", Some("{}"));
            let record = store
                .receive_mcp_interaction(
                    &owner,
                    session.id,
                    request.as_bytes().to_vec(),
                    SecurityLabels::default(),
                    900,
                    20,
                    16,
                )
                .await
                .unwrap();
            let task = store
                .create_mcp_received_task(
                    &owner,
                    session.id,
                    session.server,
                    record.id,
                    20,
                    quota(16, 8),
                )
                .await
                .unwrap();
            let initial = task.acceptance(&representation()).unwrap();
            store
                .begin_mcp_work(&owner, record.id, 20, 16, 8)
                .await
                .unwrap();
            let reply =
                br#"{ "jsonrpc":"2.0", "id":1, "result":{ "action":"accept", "content":{} } }"#
                    .to_vec();
            let saved = store
                .complete_mcp_work(
                    &owner,
                    record.id,
                    owner.host(),
                    "finished",
                    reply,
                    SecurityLabels::default(),
                    None,
                    McpWorkDelivery::Completed,
                    20,
                )
                .await
                .unwrap();
            let task = store
                .mcp_received_task(&owner, session.id, session.server, task.task_id(), 20)
                .await
                .unwrap();
            let facts = (
                owner,
                task.task_id().to_owned(),
                initial,
                saved.reply_digest.unwrap(),
            );
            store.close().await.unwrap();
            facts
        };
        let reopened = SqliteStore::open(&path.0, store_config()).await.unwrap();
        let session = reopened
            .open_mcp_epoch(&owner, hash_bytes(b"server-a"), 21)
            .await
            .unwrap();
        let task = reopened
            .mcp_received_task(&owner, session.id, session.server, &task_id, 21)
            .await
            .unwrap();
        assert_eq!(task.status(), McpReceivedTaskStatus::Completed);
        assert_eq!(task.result().unwrap().source_reply_digest(), Some(source));
        assert_eq!(
            task.acceptance(&representation()).unwrap().as_bytes(),
            initial.as_bytes()
        );
        assert_eq!(
            reopened.mcp_owner_usage(&owner).await.unwrap().calls_spent,
            1
        );
        let clock = Arc::new(Clock(AtomicI64::new(21)));
        let receiver =
            McpTaskReceiver::new(config(reopened.clone(), clock), settings(16, 8)).unwrap();
        let response = receiver
            .handle_request(
                &owner,
                session.server,
                session.id,
                &control("2", "tasks/result", &task_id),
                Arc::new(McpCancellation::new()),
            )
            .await
            .unwrap()
            .unwrap();
        let Value::Map(result) = field(&response, "result") else {
            panic!()
        };
        assert_eq!(result.get("action"), Some(&Value::Text("accept".into())));
    });
}

#[test]
fn record_and_reserved_byte_quotas_include_tombstones_and_deadline_failure_keeps_work_uncertain() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16,8).await;
        let bytes = McpReceivedTaskLimits::new(16,8,500,1000,10,16*1024,16*1024+1024).unwrap();
        let first = request("1","sampling/createMessage",Some("{}"));
        let record = f.receive(&first,900).await;
        let task = f.first.create_mcp_received_task(&f.owner,f.session.id,f.session.server,record.id,20,bytes).await.unwrap();
        // Pending cancellation frees active work capacity, but not retained reply
        // capacity or the stable deduplication tombstone.
        f.second.cancel_mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
        let second = request("2","sampling/createMessage",Some("{}"));
        let second = f.receive(&second,900).await;
        assert_eq!(f.first.create_mcp_received_task(&f.owner,f.session.id,f.session.server,second.id,20,bytes).await.unwrap_err(),StoreError::Budget);
        let records = McpReceivedTaskLimits::new(1,1,500,1000,10,16*1024,65536).unwrap();
        assert_eq!(f.first.create_mcp_received_task(&f.owner,f.session.id,f.session.server,second.id,20,records).await.unwrap_err(),StoreError::Budget);
        assert!(matches!(f.first.begin_mcp_work(&f.owner,record.id,20,16,8).await.unwrap(),McpWorkClaim::Existing(record) if record.state==McpWorkState::Cancelled));
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().calls_spent,0);

        let f = Fixture::new(16,8).await;
        let original = request("3","sampling/createMessage",Some(r#"{"ttl":1000}"#));
        let (record,task) = f.accept(&original,30).await;
        f.first.begin_mcp_work(&f.owner,record.id,20,16,8).await.unwrap();
        f.clock.0.store(31,Ordering::SeqCst);
        let state = f.second.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),31).await.unwrap();
        assert_eq!(state.status(),McpReceivedTaskStatus::Failed);
        assert_eq!(error_code(&f.handle(&control("4","tasks/result",task.task_id())).await),-32000);
        let work = f.first.mcp_interaction(&f.owner,record.id).await.unwrap();
        assert_eq!(work.state,McpWorkState::Cancelled);
        assert_eq!(work.work_delivery,McpWorkDelivery::Uncertain);
        assert!(work.outstanding);
        assert_eq!(error_code(&f.handle(&control("5","tasks/cancel",task.task_id())).await),-32602);
    });
}

#[test]
fn acceptance_delivery_is_separate_and_cancel_rpc_redelivery_reuses_committed_receipt() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16,8).await;
        let original = request("1","sampling/createMessage",Some(r#"{"ttl":10}"#));
        let (work,task) = f.accept(&original,900).await;
        assert_eq!(f.first.record_mcp_received_task_acceptance_delivery(&f.owner,f.session.id,f.session.server,task.task_id(),true,20).await.unwrap_err(),StoreError::Invalid);
        for accepted in [false,false,true,true] {
            f.second.record_mcp_received_task_acceptance_delivery(&f.owner,f.session.id,f.session.server,task.task_id(),accepted,20).await.unwrap();
        }
        let work = f.first.mcp_interaction(&f.owner,work.id).await.unwrap();
        assert_eq!(work.send_attempts,0);
        assert!(!work.transport_accepted && work.reply.is_none());
        f.first.begin_mcp_work(&f.owner,work.id,20,16,8).await.unwrap();
        let cancellation = control("\"cancel-once\"","tasks/cancel",task.task_id());
        let (one,two) = tokio::join!(f.handle(&cancellation),f.handle(&cancellation));
        assert_eq!(one.as_bytes(),two.as_bytes());
        let Value::Map(result) = field(&one,"result") else {panic!()};
        assert_eq!(result.get("status"),Some(&Value::Text("cancelled".into())));
        let journal = f.second.mcp_journal(&f.owner,0,64).await.unwrap();
        assert_eq!(journal.iter().filter(|fact|fact.kind=="receiver_task_cancel_receipt").count(),1);
        assert_eq!(journal.iter().filter(|fact|fact.kind=="receiver_task_acceptance_send").count(),2);
        assert_eq!(journal.iter().filter(|fact|fact.kind=="receiver_task_acceptance_accepted").count(),1);
        f.clock.0.store(31,Ordering::SeqCst);
        assert_eq!(f.handle(&cancellation).await.as_bytes(),one.as_bytes(),"expiry cannot rewrite an existing RPC receipt");
        assert_eq!(error_code(&f.handle(&control("\"new-cancel\"","tasks/cancel",task.task_id())).await),-32602);
        let changed = doc(&format!(r#"{{"jsonrpc":"2.0","id":"cancel-once","method":"tasks/cancel","params":{{"taskId":"{}"}}}}"#,task.task_id()));
        assert_eq!(f.second.cancel_mcp_received_task_request(&f.owner,f.session.id,f.session.server,&changed,31).await.unwrap_err(),StoreError::IdempotencyConflict);
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,1);
    });
}

#[test]
fn final_reply_capacity_is_reserved_and_existing_cancelled_errors_remain_original_errors() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16,8).await;
        let original = request("1","sampling/createMessage",Some("{}"));
        let work = f.receive(&original,900).await;
        let minimum = representation().max_document_bytes / 8 + 256;
        let reserved = McpReceivedTaskLimits::new(1,1,500,1000,10,minimum,minimum+1024).unwrap();
        let task = f.first.create_mcp_received_task(&f.owner,f.session.id,f.session.server,work.id,20,reserved).await.unwrap();
        f.first.begin_mcp_work(&f.owner,work.id,20,16,8).await.unwrap();
        let text = "x".repeat(7900);
        let result = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"role":"assistant","model":"bounded-model","content":{{"type":"text","text":"{text}"}}}}}}"#).into_bytes();
        assert!(result.len() <= representation().max_document_bytes/8);
        f.complete(&work,result.clone()).await.unwrap();
        let done = f.second.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
        assert_eq!(done.status(),McpReceivedTaskStatus::Completed);
        let Value::Map(payload) = done.result().unwrap().payload().value() else {panic!()};
        let Some(Value::Map(content)) = payload.get("content") else {panic!()};
        assert_eq!(content.get("text"),Some(&Value::Text(text)));
        assert_eq!(f.first.mcp_interaction(&f.owner,work.id).await.unwrap().reply.unwrap(),result);

        let f = Fixture::new(16,8).await;
        let original = request("1","elicitation/create",Some("{}"));
        let (work,task) = f.accept(&original,900).await;
        f.first.begin_mcp_work(&f.owner,work.id,20,16,8).await.unwrap();
        f.first.cancel_mcp_work(&f.owner,work.id,20).await.unwrap();
        let original_error = br#"{ "jsonrpc":"2.0", "id":1, "error": {"code":-32041,"message":"saved-original-cancellation","data":{"private":true}} }"#.to_vec();
        let saved = f.second.record_mcp_cancelled_reply(&f.owner,work.id,original_error.clone(),20).await.unwrap();
        let response = f.handle(&control("2","tasks/result",task.task_id())).await;
        assert_eq!(error_code(&response),-32041);
        let task = f.second.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
        assert_eq!(task.status(),McpReceivedTaskStatus::Cancelled);
        assert_eq!(task.result().unwrap().source_reply_digest(),saved.reply_digest);
        assert_eq!(f.first.mcp_interaction(&f.owner,work.id).await.unwrap().reply.unwrap(),original_error);
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,1);
    });
}

#[test]
fn failed_terminal_write_rolls_back_work_cancellation_and_the_rpc_receipt() {
    actix::System::new().block_on(async {
        let f = Fixture::new(16,8).await;
        let original = request("1","sampling/createMessage",Some("{}"));
        let (work,task) = f.accept(&original,900).await;
        f.first.begin_mcp_work(&f.owner,work.id,20,16,8).await.unwrap();
        if test_store::postgres_selected() {
            #[cfg(feature="postgres")]
            test_store::postgres_admin(&f._path.0, |db| db.batch_execute("CREATE FUNCTION reject_task_cancellation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.status='cancelled' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_task_cancellation BEFORE UPDATE ON mcp_received_tasks FOR EACH ROW EXECUTE FUNCTION reject_task_cancellation();").unwrap());
        } else {
            #[cfg(feature="sqlite")]
            rusqlite::Connection::open(&f._path.0).unwrap().execute_batch("CREATE TRIGGER reject_task_cancellation BEFORE UPDATE ON mcp_received_tasks WHEN NEW.status='cancelled' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
        }
        let cancel = control("2","tasks/cancel",task.task_id());
        assert_eq!(f.second.cancel_mcp_received_task_request(&f.owner,f.session.id,f.session.server,&cancel,20).await.unwrap_err(),StoreError::Database);
        let unchanged = f.first.mcp_received_task(&f.owner,f.session.id,f.session.server,task.task_id(),20).await.unwrap();
        assert_eq!(unchanged.status(),McpReceivedTaskStatus::Working);
        let unchanged = f.second.mcp_interaction(&f.owner,work.id).await.unwrap();
        assert_eq!(unchanged.state,McpWorkState::Intent);
        assert!(unchanged.outstanding && unchanged.reply.is_none());
        assert!(f.first.mcp_received_task_cancel_receipt(&f.owner,f.session.id,f.session.server,&cancel,20).await.unwrap().is_none());
        if test_store::postgres_selected() {
            #[cfg(feature="postgres")]
            test_store::postgres_admin(&f._path.0, |db| db.batch_execute("DROP TRIGGER reject_task_cancellation ON mcp_received_tasks; DROP FUNCTION reject_task_cancellation();").unwrap());
        } else {
            #[cfg(feature="sqlite")]
            rusqlite::Connection::open(&f._path.0).unwrap().execute_batch("DROP TRIGGER reject_task_cancellation;").unwrap();
        }
        let response = f.handle(&cancel).await;
        let Value::Map(result) = field(&response,"result") else {panic!()};
        assert_eq!(result.get("status"),Some(&Value::Text("cancelled".into())));
        assert_eq!(f.first.mcp_owner_usage(&f.owner).await.unwrap().outstanding,1);
    });
}

struct LiveServices {
    calls: AtomicU64,
    controls_observed: tokio::sync::Notify,
}
impl McpEventHandler for LiveServices {
    fn receive<'a>(&'a self, event: McpClientEvent) -> McpFuture<'a, ()> {
        Box::pin(async move {
            if event.method == "notifications/message"
                && field(&event.params, "data") == &Value::Text("receiver-controls-observed".into())
            {
                self.controls_observed.notify_one();
            }
            Ok(())
        })
    }
}
impl McpClientHandler for LiveServices {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        request: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(context.task_id.is_some());
            tokio::select! {
                _=context.cancellation.cancelled()=>return McpServiceOutcome::failure(McpWorkDelivery::Uncertain,McpRpcError::new(doc(r#"{"code":-32000,"message":"underlying work cancellation uncertain"}"#)).unwrap()),
                _=self.controls_observed.notified()=>{}
            }
            let result = if field(request, "method")
                == &Value::Text("sampling/createMessage".into())
            {
                doc(
                    r#"{"role":"assistant","model":"live-model","content":{"type":"text","text":"saved answer"}}"#,
                )
            } else {
                doc(r#"{"action":"decline"}"#)
            };
            McpServiceOutcome::completed(result)
        })
    }
}

#[test]
fn live_receiver_tasks_progress_on_one_worker_over_stdio_http_json_and_post_sse() {
    const PEER: &str = include_str!("../../../tests/support/mcp_receiver_tasks_peer.mjs");
    for mode in ["stdio", "http-json", "http-sse"] {
        for scenario in ["complete", "cancel", "lost-ack", "saturated", "wait-cancel"] {
            if scenario == "lost-ack" && mode != "http-json" {
                continue;
            }
            for method in ["sampling/createMessage", "elicitation/create"] {
                actix::System::new().block_on(async {
                    htlk_analyzer::mcp_schema_validators().unwrap();
                    let store = SqliteStore::in_memory(store_config()).await.unwrap();
                    let clock = Arc::new(Clock(AtomicI64::new(20)));
                    let persistence = McpClientPersistence::new(store.clone(), clock.clone());
                    let owner = persistence
                        .acquire_owner(&host("alice"), "live-receiver", 10_000)
                        .await
                        .unwrap();
                    let handler = Arc::new(LiveServices {
                        calls: AtomicU64::new(0),
                        controls_observed: tokio::sync::Notify::new(),
                    });
                    let config = McpClientConfig::new(
                        McpClientLimits {
                            callbacks: 1,
                            pending_requests: if scenario == "saturated" { 2 } else { 64 },
                            frame_bytes: 65536,
                            retained_bytes: if scenario == "saturated" {
                                12 * 65536
                            } else {
                                1024 * 1024
                            },
                            timeout_ms: test_store::io_budget_ms(2000),
                            ..Default::default()
                        },
                        BTreeMap::from([
                            (
                                McpClientService::Sampling,
                                handler.clone() as Arc<dyn McpClientHandler>,
                            ),
                            (
                                McpClientService::ElicitationForm,
                                handler.clone() as Arc<dyn McpClientHandler>,
                            ),
                        ]),
                        Some(handler.clone() as Arc<dyn McpEventHandler>),
                    )
                    .unwrap()
                    .with_persistence(persistence)
                    .with_authorizer(Arc::new(Policy))
                    .with_task_receiver(settings(16, 1))
                    .unwrap();
                    let client = NativeMcpClient::new(config).unwrap();
                    let mut child = None;
                    let endpoint = if mode == "stdio" {
                        htlk_runtime::mcp::McpEndpoint::Stdio {
                            program: "node".into(),
                            arguments: vec![
                                "--input-type=module".into(),
                                "-e".into(),
                                PEER.into(),
                                mode.into(),
                                scenario.into(),
                                method.into(),
                            ],
                            environment: BTreeMap::new(),
                        }
                    } else {
                        use std::io::BufRead;
                        let mut peer = std::process::Command::new("node")
                            .args(["--input-type=module", "-e", PEER, mode, scenario, method])
                            .stdout(std::process::Stdio::piped())
                            .spawn()
                            .unwrap();
                        let mut port = String::new();
                        std::io::BufReader::new(peer.stdout.take().unwrap())
                            .read_line(&mut port)
                            .unwrap();
                        child = Some(peer);
                        htlk_runtime::mcp::McpEndpoint::Http {
                            url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                            headers: BTreeMap::new(),
                        }
                    };
                    let server = htlk_executable::ServerIdentity::new(
                        "receiver-peer".into(),
                        if mode == "stdio" {
                            htlk_executable::McpTransport::Stdio
                        } else {
                            htlk_executable::McpTransport::StreamableHttp
                        },
                        "receiver-peer".into(),
                        "1".into(),
                        &representation(),
                    )
                    .unwrap();
                    let session = client
                        .open(
                            owner.clone(),
                            server.clone(),
                            endpoint,
                            representation(),
                            Arc::new(McpCancellation::new()),
                        )
                        .await
                        .unwrap();
                    let reply = session
                        .request(
                            "tools/call",
                            doc(r#"{"name":"start"}"#),
                            Arc::new(McpCancellation::new()),
                        )
                        .await;
                    let result = reply
                        .result
                        .unwrap_or_else(|error| panic!("{mode} {scenario} {method}: {error:?}"))
                        .unwrap_or_else(|error| panic!("{mode} {scenario} {method}: {error:?}"));
                    assert_eq!(
                        field(&result, "resultSeen"),
                        &Value::Bool(true),
                        "{mode} {scenario} {method}"
                    );
                    assert_eq!(
                        handler.calls.load(Ordering::SeqCst),
                        1,
                        "replayed acceptance must not recall the model"
                    );
                    let tasks = store
                        .mcp_received_tasks(
                            &owner,
                            session.durable_identity().unwrap(),
                            server.digest(&representation()).unwrap(),
                            None,
                            16,
                            20,
                        )
                        .await
                        .unwrap();
                    assert_eq!(tasks.tasks.len(), 1);
                    assert_eq!(
                        tasks.tasks[0].status(),
                        if scenario == "cancel" || scenario == "saturated" {
                            McpReceivedTaskStatus::Cancelled
                        } else {
                            McpReceivedTaskStatus::Completed
                        }
                    );
                    let usage = store.mcp_owner_usage(&owner).await.unwrap();
                    assert_eq!(usage.calls_spent, 1);
                    if scenario == "cancel" || scenario == "saturated" {
                        assert_eq!(usage.outstanding, 1);
                    }
                    client.shutdown().await.unwrap();
                    if let Some(mut child) = child {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                });
            }
        }
    }
}

#[test]
fn independent_outbound_claims_scope_identity_and_recheck_terminal_and_final_evidence() {
    actix::System::new().block_on(async{
        let f=Fixture::new(16,8).await;
        let key=hash_bytes(b"same-logical-key");
        let descriptor=doc(r#"{"name":"work","inputSchema":{"type":"object"},"execution":{"taskSupport":"optional"}}"#);
        let params=doc(r#"{"name":"work","arguments":{},"task":{"ttl":100}}"#);
        let (one,two)=tokio::join!(
            f.first.claim_mcp_remote_task(&f.owner,f.session.id,f.session.server,key,descriptor.clone(),params.clone(),None,20,900,16),
            f.second.claim_mcp_remote_task(&f.owner,f.session.id,f.session.server,key,descriptor.clone(),params.clone(),None,20,900,16),
        );
        assert_ne!(one.unwrap(),two.unwrap());
        let initial=doc(r#"{"taskId":"remote-private","status":"working","createdAt":"1970-01-01T00:00:00.020Z","lastUpdatedAt":"1970-01-01T00:00:00.020Z","ttl":100,"pollInterval":1}"#);
        let state=McpTaskState::from_document(initial.clone(),&representation()).unwrap();
        let acceptance=doc(&format!("{{\"task\":{}}}",std::str::from_utf8(initial.as_bytes()).unwrap()));
        f.first.accept_mcp_remote_task(&f.owner,f.session.id,f.session.server,key,state.clone(),acceptance.as_bytes().to_vec(),20).await.unwrap();
        let resolved=f.second.resolve_mcp_remote_task(&f.owner,f.session.id,f.session.server,state.task_id(),20).await.unwrap();
        assert!(resolved.origin.is_none());
        assert_eq!(resolved.expires_ms,Some(120));
        let bob=f.second.acquire_mcp_owner(&host("bob"),"receiver",20,1000).await.unwrap();
        let bob_session=f.second.open_mcp_epoch(&bob,f.session.server,20).await.unwrap();
        assert!(f.second.claim_mcp_remote_task(&bob,bob_session.id,bob_session.server,key,descriptor.clone(),params.clone(),None,20,900,16).await.unwrap());
        assert_eq!(f.second.resolve_mcp_remote_task(&bob,bob_session.id,bob_session.server,state.task_id(),20).await.unwrap_err(),StoreError::Missing);
        let final_result=doc(r#"{"content":[],"_meta":{"io.modelcontextprotocol/related-task":{"taskId":"remote-private"}}}"#);
        f.second.update_mcp_remote_task(&f.owner,f.session.id,f.session.server,key,None,Some((false,final_result.as_bytes().to_vec())),20).await.unwrap();
        let snapshot=f.first.inspect_mcp_remote_task_intent(&f.owner,f.session.server,key).await.unwrap();
        assert_eq!(snapshot.result_status,Some(McpTaskStatus::Completed));
        assert_eq!(snapshot.final_payload.unwrap().1,final_result.as_bytes());
        assert_eq!(f.first.update_mcp_remote_task(&f.owner,f.session.id,f.session.server,key,Some(state),None,20).await.unwrap_err(),StoreError::Terminal);
        assert_eq!(f.first.resolve_mcp_remote_task(&f.owner,f.session.id,f.session.server,"remote-private",20).await.unwrap_err(),StoreError::Terminal);
        if test_store::postgres_selected() {
            #[cfg(feature="postgres")]
            test_store::postgres_admin(&f._path.0, |db| db.batch_execute("UPDATE mcp_remote_tasks SET final_payload=decode('7b7d','hex') WHERE final_kind='result'").unwrap());
        } else {
            #[cfg(feature="sqlite")]
            rusqlite::Connection::open(&f._path.0).unwrap().execute_batch("UPDATE mcp_remote_tasks SET final_payload=x'7b7d' WHERE final_kind='result';").unwrap();
        }
        assert_eq!(f.first.inspect_mcp_remote_task_intent(&f.owner,f.session.server,key).await.unwrap_err(),StoreError::Corrupt);
    });
}
