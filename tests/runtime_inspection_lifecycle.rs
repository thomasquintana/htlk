//! Public pinned navigation over records written by actual MCP/wait execution.
#[allow(dead_code)]
mod support;

use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    clock::CoordinatorClock,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::RuntimeFailure,
    identity::{RunId, WaitId},
    inspection::{
        ContentKind, DiscoveryTarget, EntityId, EntityKind, InspectionContext, InspectionError,
        InspectionPolicy, InspectionTarget, Inspector, PageRequest, RelationKind, RunSelector,
    },
    mcp::{McpAdapter, McpFuture, McpRequest, McpRunContext},
    records::{RunStatus, SecurityLabels},
    store::{RecordFamily, RecordKey, StoreConfig},
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::store::TestStore;

struct Policy {
    hidden: Mutex<Option<RecordKey>>,
    unstructured: Mutex<Option<RecordKey>>,
}
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
}
impl InspectionPolicy for Policy {
    fn discover(&self, _: &InspectionContext, target: &DiscoveryTarget<'_>) -> bool {
        !matches!(target.reference.entity(), EntityId::Record(key) if Some(key)==self.hidden.lock().unwrap().as_ref())
    }
    fn structure(&self, _: &InspectionContext, target: &InspectionTarget<'_>) -> bool {
        !matches!(target.reference.entity(), EntityId::Record(key) if Some(key)==self.unstructured.lock().unwrap().as_ref())
    }
    fn content(&self, _: &InspectionContext, _: &InspectionTarget<'_>, _: ContentKind) -> bool {
        false
    }
}
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
struct Adapter {
    descriptors: BTreeMap<Digest, JsonDocument>,
    calls: AtomicUsize,
}
impl McpAdapter for Adapter {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&binding.descriptor()].clone()) })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a McpRunContext,
        _: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(
                br#"{"content":[],"structuredContent":{"evidence_text":"private-response-value"}}"#
                    .to_vec(),
            )
        })
    }
}

async fn open_wait(runtime: &Runtime, caller: &HostContext, run: &RunId) -> WaitId {
    for _ in 0..2000 {
        match runtime.inspect_run(caller, run).await {
            Ok(view) => {
                assert_eq!(view.header.status, RunStatus::Running);
                if let Some(wait) = view.waits.first() {
                    return wait.wait_id;
                }
            }
            Err(error) if error.code() == htlk::runtime::failure::FailureCode::Fenced => {}
            Err(error) => panic!("unexpected run inspection: {error}"),
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    panic!("production wait did not open")
}

#[test]
fn pinned_production_dispatch_wait_lifecycles_and_response_parent_chain() {
    actix::System::new().block_on(async {
        let (compiler, registry) = support::native_compiler_with_timeouts(10_000, 2000);
        let graph = compiler
            .compile_source(
                r#"
            ir_version="0.1" graph test.inspection_lifecycle {
                inputs={} outputs={value=string}
                nodes {
                    args=eval(json,{query="private-request-value"})
                    lookup=call(mcp.tool("research","lookup"))
                    review=wait(string){topic="review" timeout_ms=5000}
                }
                edges {
                    edge args {from=args.outputs.value to=lookup.inputs.arguments}
                    edge review {from=lookup.outputs.value to=review.inputs.request}
                    edge result {from=review.outputs.value to=outputs.value}
                }
            }
        "#,
                &support::catalogs(),
            )
            .unwrap();
        let adapter = Arc::new(Adapter {
            descriptors: graph
                .canonical_document
                .fields()
                .bindings
                .values()
                .map(|b| {
                    (
                        b.descriptor(),
                        graph.canonical_document.fields().documents[&b.descriptor()].clone(),
                    )
                })
                .collect(),
            calls: AtomicUsize::new(0),
        });
        let policy = Arc::new(Policy {
            hidden: Mutex::new(None),
            unstructured: Mutex::new(None),
        });
        let store = TestStore::in_memory(StoreConfig::default()).await.unwrap();
        let runtime = Arc::new(
            Runtime::new(
                store.clone(),
                Arc::new(registry),
                policy.clone(),
                RuntimeConfig::default(),
            )
            .unwrap()
            .with_clock(Arc::new(Clock))
            .with_mcp(adapter.clone()),
        );
        let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
        runtime
            .register_graph(&caller, &graph.executable_bytes, &[])
            .await
            .unwrap();
        let engine = ExecutionEngine::launch(
            runtime.clone(),
            ExecutionConfig {
                poll_interval: Duration::from_millis(2),
                ..ExecutionConfig::default()
            },
        )
        .unwrap();
        let run = runtime
            .start_run(&caller, graph.fingerprint, &BTreeMap::new(), "start", None)
            .await
            .unwrap();
        let wait = open_wait(&runtime, &caller, &run.run_id).await;
        let context = InspectionContext::new(caller.clone(), Some(run.run_id.clone()));
        let inspector = Inspector::new(store.clone().into(), policy.clone());
        let open = inspector
            .open_snapshot(&context, RunSelector::Caller, None)
            .await
            .unwrap();
        let attempts = inspector
            .list_entities(
                &context,
                &open,
                EntityKind::Records(RecordFamily::Attempt),
                PageRequest::new(8),
            )
            .await
            .unwrap();
        assert_eq!(attempts.items.len(), 1);
        assert_eq!(attempts.items[0].status, Some("response_recorded"));
        let EntityId::Record(attempt_key) = attempts.items[0].reference.entity() else {
            panic!("expected attempt key");
        };
        let attempt_key = attempt_key.clone();
        let response_key = RecordKey {
            family: RecordFamily::Response,
            id: attempt_key.id.clone(),
        };

        // The runtime is the writer/oracle. Recover its actual committed stage
        // boundaries rather than racing polling against the short prepared stage.
        let mut after = 0;
        let mut stages = Vec::new();
        loop {
            let changes = store
                .inspection_changes(after, Some(open.position()), 16)
                .await
                .unwrap();
            if changes.is_empty() {
                break;
            }
            after = changes.last().unwrap().cursor;
            stages.extend(
                changes
                    .into_iter()
                    .filter(|c| c.run_id == run.run_id && c.key.as_ref() == Some(&attempt_key))
                    .map(|c| c.position),
            );
        }
        assert_eq!(stages.len(), 3);
        let mut pinned = Vec::new();
        for (index, (position, expected)) in stages
            .iter()
            .zip(["prepared", "possibly_sent", "response_recorded"])
            .enumerate()
        {
            let snapshot = inspector
                .open_snapshot(&context, RunSelector::Caller, Some(position))
                .await
                .unwrap();
            let attempt = inspector
                .select(&context, &snapshot, EntityId::Record(attempt_key.clone()))
                .await
                .unwrap();
            assert_eq!(attempt.status, Some(expected));
            assert_eq!(attempt.attempt_number, Some(1));
            assert_eq!(attempt.final_record, Some(index == 2));
            if index < 2 {
                assert_eq!(
                    inspector
                        .select(&context, &snapshot, EntityId::Record(response_key.clone()))
                        .await
                        .unwrap_err(),
                    InspectionError::Unknown
                );
            }
            pinned.push((snapshot, attempt.reference));
        }
        let response = inspector
            .select(&context, &open, EntityId::Record(response_key.clone()))
            .await
            .unwrap()
            .reference;
        let raw_snapshot = store
            .open_inspection_snapshot(&run.run_id, Some(open.position()))
            .await
            .unwrap();
        let raw_response = store
            .inspection_record(&raw_snapshot, &response_key)
            .await
            .unwrap();
        let Value::Map(fields) = &raw_response.payload else {
            panic!("expected production response");
        };
        assert!(fields.get("invocation_id").is_none());
        assert!(matches!(fields.get("raw"), Some(Value::Bytes(_))));
        let raw_attempt = store
            .inspection_record(&raw_snapshot, &attempt_key)
            .await
            .unwrap();
        let Value::Map(fields) = &raw_attempt.payload else {
            panic!("expected production attempt");
        };
        let Some(Value::Text(invocation)) = fields.get("invocation_id") else {
            panic!("missing production owner");
        };
        assert!(fields.get("status").is_none() && fields.get("state").is_none());
        let invocation_key = RecordKey {
            family: RecordFamily::Invocation,
            id: invocation.clone(),
        };

        let parent = inspector
            .follow_relations(
                &context,
                &open,
                &response,
                RelationKind::ExecutionParent,
                PageRequest::new(1),
            )
            .await
            .unwrap();
        assert_eq!(parent.items.len(), 1);
        let parent = parent.items[0].target.clone();
        assert_eq!(parent.entity(), &EntityId::Record(attempt_key.clone()));
        assert_eq!(parent.position_number(), open.position().number());
        let owner = inspector
            .follow_relations(
                &context,
                &open,
                &parent,
                RelationKind::ExecutionParent,
                PageRequest::new(1),
            )
            .await
            .unwrap();
        assert_eq!(owner.items.len(), 1);
        assert_eq!(
            owner.items[0].target.entity(),
            &EntityId::Record(invocation_key.clone())
        );
        assert_eq!(
            owner.items[0].target.position_number(),
            open.position().number()
        );
        for private in ["private-request-value", "private-response-value"] {
            assert!(!format!("{attempts:?}{parent:?}{owner:?}").contains(private));
        }
        assert_eq!(
            inspector
                .read_content(&context, &open, &response, ContentKind::Payload, 65536)
                .await
                .unwrap_err(),
            InspectionError::Denied
        );
        assert_eq!(
            inspector
                .follow_relations(
                    &context,
                    &pinned[0].0,
                    &response,
                    RelationKind::ExecutionParent,
                    PageRequest::new(1)
                )
                .await
                .unwrap_err(),
            InspectionError::InvalidReference
        );

        // Current policy applies independently at both links, even to issued refs.
        for key in [&attempt_key, &invocation_key] {
            let source = if key == &attempt_key {
                &response
            } else {
                &parent
            };
            *policy.hidden.lock().unwrap() = Some(key.clone());
            assert!(
                inspector
                    .follow_relations(
                        &context,
                        &open,
                        source,
                        RelationKind::ExecutionParent,
                        PageRequest::new(1)
                    )
                    .await
                    .unwrap()
                    .items
                    .is_empty()
            );
            *policy.hidden.lock().unwrap() = None;
            *policy.unstructured.lock().unwrap() = Some(key.clone());
            assert!(
                inspector
                    .follow_relations(
                        &context,
                        &open,
                        source,
                        RelationKind::ExecutionParent,
                        PageRequest::new(1)
                    )
                    .await
                    .unwrap()
                    .items
                    .is_empty()
            );
            *policy.unstructured.lock().unwrap() = None;
        }

        let wait_id = EntityId::Record(RecordKey {
            family: RecordFamily::Wait,
            id: wait.to_string(),
        });
        let old_wait = inspector
            .select(&context, &open, wait_id.clone())
            .await
            .unwrap();
        assert_eq!(old_wait.status, Some("open"));
        runtime
            .submit_reply(
                &caller,
                wait,
                Value::Text("private-reply-value".into()),
                "reply",
            )
            .await
            .unwrap();
        for _ in 0..2000 {
            if store
                .run_header(&run.run_id)
                .await
                .unwrap()
                .status
                .is_terminal()
            {
                break;
            }
            actix::clock::sleep(Duration::from_millis(2)).await;
        }
        engine.shutdown().await.unwrap();
        let header = store.run_header(&run.run_id).await.unwrap();
        assert_eq!(header.status, RunStatus::Succeeded);
        let terminal = inspector.refresh_snapshot(&context, &open).await.unwrap();
        let accepted_wait = inspector
            .select(&context, &terminal, wait_id)
            .await
            .unwrap();
        assert_eq!(accepted_wait.status, Some("accepted"));
        assert_eq!(accepted_wait.final_record, Some(true));
        assert!(!format!("{accepted_wait:?}").contains("private-reply-value"));
        assert_eq!(
            inspector
                .inspect_entity(&context, &open, &old_wait.reference)
                .await
                .unwrap()
                .status,
            Some("open")
        );
        for ((snapshot, reference), expected) in
            pinned
                .iter()
                .zip(["prepared", "possibly_sent", "response_recorded"])
        {
            assert_eq!(
                inspector
                    .inspect_entity(&context, snapshot, reference)
                    .await
                    .unwrap()
                    .status,
                Some(expected)
            );
        }
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.run_header(&run.run_id).await.unwrap(),
            header,
            "inspection dispatched or committed work"
        );
        store.close().await.unwrap();
    });
}
