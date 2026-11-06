//! Independent Node peer evidence for catalog/utility boundaries. Peers count
//! content/effect methods independently of host results; no network is required.
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
const mode=process.argv[1];
const send=value=>process.stdout.write(JSON.stringify(value)+'\n');
const reply=(r,result)=>send({jsonrpc:'2.0',id:r.id,result});
const event=(method,params={})=>send({jsonrpc:'2.0',method,params});
let counts={pages:0,content:0,complete:0,subscribe:0,unsubscribe:0,levels:[],cursors:[],references:[]};
const capabilities=mode==='unsupported'?{}:{tools:{},prompts:{},resources:{subscribe:mode!=='no-subscribe',listChanged:true},completions:{},logging:{}};
for await(const line of createInterface({input:process.stdin})) {
  const r=JSON.parse(line);
  if(r.method==='initialize') reply(r,{protocolVersion:'2025-11-25',serverInfo:{name:'services',version:'1'},capabilities});
  else if(r.method==='notifications/initialized') {
    if(mode==='events') {
      event('notifications/resources/updated',{uri:'test://idle'});
      event('notifications/tools/list_changed');
      event('notifications/prompts/list_changed');
      event('notifications/resources/list_changed');
      event('notifications/message',{level:'notice',data:{message:'idle'}});
    }
  } else if(['tools/list','prompts/list','resources/list','resources/templates/list'].includes(r.method)) {
    counts.pages++;
    const has=Object.hasOwn(r.params,'cursor');
    counts.cursors.push(has?r.params.cursor:null);
    const field={'tools/list':'tools','prompts/list':'prompts','resources/list':'resources','resources/templates/list':'resourceTemplates'}[r.method];
    let item=field==='tools'?{name:'tool',inputSchema:{type:'object'}}:
      field==='prompts'?{name:'prompt',arguments:[{name:'arg',required:true}]}:
      field==='resources'?{name:'resource',uri:'test://resource'}:
      {name:'template',uriTemplate:'test://resource/{arg}'};
    if(mode==='invalid-descriptor'||(mode==='invalid-second'&&has)) item=field==='tools'?{name:'tool',inputSchema:42}:{name:42};
    let result={[field]:[item]};
    if(mode==='invalid-page') result={[field]:[item],nextCursor:42};
    else if(mode==='items') result[field]=[item,item,item];
    else if(mode==='bytes') result.padding='x'.repeat(2000);
    else if(mode==='cycle') result.nextCursor=!has?'a':r.params.cursor==='a'?'b':'a';
    else if(mode==='empty-cycle') { result[field]=[];result.nextCursor=''; }
    else if(mode!=='single'&&mode!=='invalid-descriptor') {
      if(!has) result.nextCursor='';
      else if(r.params.cursor!=='') process.exit(31);
    }
    if(mode==='deadline'&&has) setTimeout(()=>reply(r,result),500);
    else reply(r,result);
  } else if(r.method==='completion/complete') {
    counts.complete++;
    counts.references.push(r.params);
    if(mode==='remote-error') send({jsonrpc:'2.0',id:r.id,error:{code:-32042,message:'private-service-message',data:{private:true}}});
    else reply(r,{completion:mode==='completion-many'?{values:Array(101).fill('x')}:
      mode==='completion-total'?{values:['one','two'],total:1}:
      mode==='completion-negative'?{values:[],total:-1}:
      {values:['alpha','beta'],total:3,hasMore:true}});
  } else if(r.method==='logging/setLevel') {
    counts.levels.push(r.params.level);
    reply(r,mode==='bad-ack'?{_meta:7}:{});
  } else if(r.method==='resources/subscribe') {
    counts.subscribe++;
    if(mode==='events') event('notifications/resources/updated',{uri:r.params.uri});
    reply(r,mode==='bad-ack'?{_meta:7}:{});
  } else if(r.method==='resources/unsubscribe') {
    counts.unsubscribe++;
    if(mode==='events') event('notifications/resources/updated',{uri:r.params.uri,phase:'before-ack'});
    reply(r,{});
    if(mode==='events') event('notifications/resources/updated',{uri:r.params.uri,phase:'after-ack'});
  } else if(['tools/call','resources/read','prompts/get'].includes(r.method)) {
    counts.content++;
    send({jsonrpc:'2.0',id:r.id,error:{code:-32601,message:'Forbidden content/effect method'}});
  } else if(r.method==='ping') reply(r,counts);
  else if(r.method==='notifications/cancelled') {}
  else process.exit(32);
}
"#;

fn cancellation() -> Arc<McpCancellation> {
    Arc::new(McpCancellation::new())
}
fn service_limits() -> McpServiceLimits {
    McpServiceLimits::new(16_384, 2_000).unwrap()
}
fn catalog_limits() -> McpCatalogLimits {
    McpCatalogLimits::new(10, 100, 16_384, 65_536, 2_000).unwrap()
}
fn document(text: &str) -> JsonDocument {
    JsonDocument::new(text.as_bytes(), &Limits::default()).unwrap()
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
async fn open(
    mode: &str,
    limits: McpClientLimits,
    events: Option<Arc<dyn McpEventHandler>>,
) -> (NativeMcpClient, McpClientSession) {
    // Preparing the pinned offline validator is a process-wide one-time cost.
    // Start peer timing campaigns after preparation so short deadline assertions
    // measure protocol traversal, independently of cold schema compilation.
    htlk_analyzer::mcp_schema_validators().unwrap();
    let representation = Limits::default();
    let owner = McpOwner::new(
        HostContext::new("catalog-reader".into(), &representation).unwrap(),
        McpOwnerScope::Host("catalog".into()),
        1,
        &representation,
    )
    .unwrap();
    let client =
        NativeMcpClient::new(McpClientConfig::new(limits, BTreeMap::new(), events).unwrap())
            .unwrap();
    let session = client
        .open(
            owner,
            ServerIdentity::new(
                "services".into(),
                McpTransport::Stdio,
                "services".into(),
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
                    mode.into(),
                ],
                environment: BTreeMap::new(),
            },
            representation,
            cancellation(),
        )
        .await
        .unwrap();
    (client, session)
}
async fn stats(session: &McpClientSession) -> JsonDocument {
    session
        .request("ping", document("{}"), cancellation())
        .await
        .result
        .unwrap()
        .unwrap()
}
fn field<'a>(document: &'a JsonDocument, key: &str) -> &'a Value {
    let Value::Map(fields) = document.value() else {
        panic!("expected object")
    };
    fields.get(key).unwrap()
}
fn count(document: &JsonDocument, key: &str) -> i64 {
    let Value::Integer(n) = field(document, key) else {
        panic!("expected count")
    };
    *n
}
fn incomplete(catalog: &McpCatalog) -> &McpCatalogStop {
    let McpCatalogStatus::Incomplete(stop) = catalog.status() else {
        panic!("expected explicit incomplete catalog")
    };
    stop
}

#[test]
fn all_inventories_enumerate_empty_opaque_cursors_without_content_or_adoption() {
    actix::System::new().block_on(async {
        let (client, session) = open("normal", McpClientLimits::default(), None).await;
        for kind in [
            McpCatalogKind::Tools,
            McpCatalogKind::Prompts,
            McpCatalogKind::Resources,
            McpCatalogKind::ResourceTemplates,
        ] {
            let catalog = session
                .enumerate_catalog(kind, None, catalog_limits(), cancellation())
                .await
                .unwrap();
            assert!(matches!(catalog.status(), McpCatalogStatus::Complete));
            assert_eq!(catalog.kind(), kind);
            assert_eq!(catalog.pages(), 2);
            assert_eq!(
                catalog.descriptors().len(),
                2,
                "changing catalogs may contain duplicates"
            );
            assert!(catalog.next_cursor().is_none());
            assert!(catalog.response_bytes() > catalog.descriptors()[0].as_bytes().len() * 2);
        }
        let observed = stats(&session).await;
        assert_eq!(count(&observed, "pages"), 8);
        assert_eq!(count(&observed, "content"), 0);
        assert_eq!(
            field(&observed, "cursors"),
            &Value::Array(
                (0..4)
                    .flat_map(|_| [Value::Null, Value::Text(String::new())])
                    .collect()
            )
        );
        client.shutdown().await.unwrap();

        let (client, session) = open("invalid-second", McpClientLimits::default(), None).await;
        let catalog = session
            .enumerate_tools(catalog_limits(), cancellation())
            .await
            .unwrap();
        assert!(matches!(
            incomplete(&catalog),
            McpCatalogStop::Failed(McpServiceError::InvalidResponse)
        ));
        assert_eq!(catalog.pages(), 1);
        assert_eq!(catalog.descriptors().len(), 1);
        assert_eq!(catalog.next_cursor().unwrap().as_str(), "");
        assert_eq!(count(&stats(&session).await, "pages"), 2);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn explicit_page_apis_preserve_resume_cursor_and_validate_every_category() {
    actix::System::new().block_on(async {
        let (client, session) = open("normal", McpClientLimits::default(), None).await;
        let page = session
            .list_tools(None, service_limits(), cancellation())
            .await
            .unwrap();
        let cursor = page.next_cursor().unwrap();
        assert_eq!(cursor.as_str(), "");
        assert_eq!(
            McpCatalogCursor::new("".into(), &Limits::default()).unwrap(),
            *cursor
        );
        let page = session
            .list_tools(Some(cursor), service_limits(), cancellation())
            .await
            .unwrap();
        assert!(page.next_cursor().is_none());
        assert_eq!(page.descriptors().len(), 1);
        assert_eq!(
            session
                .list_prompts(None, service_limits(), cancellation())
                .await
                .unwrap()
                .kind(),
            McpCatalogKind::Prompts
        );
        assert_eq!(
            session
                .list_resources(None, service_limits(), cancellation())
                .await
                .unwrap()
                .kind(),
            McpCatalogKind::Resources
        );
        assert_eq!(
            session
                .list_resource_templates(None, service_limits(), cancellation())
                .await
                .unwrap()
                .kind(),
            McpCatalogKind::ResourceTemplates
        );
        client.shutdown().await.unwrap();
        for mode in ["invalid-descriptor", "invalid-page"] {
            let (client, session) = open(mode, McpClientLimits::default(), None).await;
            for kind in [
                McpCatalogKind::Tools,
                McpCatalogKind::Prompts,
                McpCatalogKind::Resources,
                McpCatalogKind::ResourceTemplates,
            ] {
                let result = session
                    .list_catalog(kind, None, service_limits(), cancellation())
                    .await;
                assert!(
                    matches!(result, Err(McpServiceError::InvalidResponse)),
                    "{mode}"
                );
            }
            assert_eq!(count(&stats(&session).await, "content"), 0);
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn page_item_and_byte_budgets_stop_at_complete_page_boundaries() {
    actix::System::new().block_on(async {
        let (client, session) = open("normal", McpClientLimits::default(), None).await;
        let catalog = session
            .enumerate_tools(
                McpCatalogLimits::new(1, 100, 16_384, 65_536, 2_000).unwrap(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::PageLimit));
        assert_eq!(catalog.pages(), 1);
        assert_eq!(catalog.next_cursor().unwrap().as_str(), "");
        assert_eq!(
            count(&stats(&session).await, "pages"),
            1,
            "no speculative next-page fetch"
        );
        let first_bytes = catalog.response_bytes();
        let catalog = session
            .enumerate_tools(
                McpCatalogLimits::new(10, 100, 16_384, first_bytes + 1, 2_000).unwrap(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::ByteLimit));
        assert_eq!(catalog.pages(), 1);
        assert_eq!(catalog.response_bytes(), first_bytes);
        let resumed = session
            .enumerate_catalog(
                McpCatalogKind::Tools,
                catalog.next_cursor(),
                catalog_limits(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(resumed.status(), McpCatalogStatus::Complete));
        assert_eq!(resumed.pages(), 1);
        client.shutdown().await.unwrap();

        let (client, session) = open("items", McpClientLimits::default(), None).await;
        let catalog = session
            .enumerate_resources(
                McpCatalogLimits::new(10, 2, 16_384, 65_536, 2_000).unwrap(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::ItemLimit));
        assert_eq!(catalog.pages(), 0);
        assert!(
            catalog.descriptors().is_empty(),
            "never silently truncate a page"
        );
        assert_eq!(count(&stats(&session).await, "pages"), 1);
        client.shutdown().await.unwrap();

        let (client, session) = open("bytes", McpClientLimits::default(), None).await;
        let catalog = session
            .enumerate_resource_templates(
                McpCatalogLimits::new(10, 100, 16_384, 100, 2_000).unwrap(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::ByteLimit));
        assert_eq!(
            catalog.response_bytes(),
            0,
            "unknown padding counts toward admission"
        );
        assert_eq!(count(&stats(&session).await, "content"), 0);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn configured_caps_cycles_and_cancelled_enumeration_never_dispatch_extra_pages() {
    actix::System::new().block_on(async {
        for (mode, pages) in [("cycle", 3), ("empty-cycle", 2)] {
            let (client, session) = open(mode, McpClientLimits::default(), None).await;
            let catalog = session
                .enumerate_tools(catalog_limits(), cancellation())
                .await
                .unwrap();
            assert!(matches!(incomplete(&catalog), McpCatalogStop::CursorCycle));
            assert_eq!(catalog.pages(), pages);
            assert_eq!(count(&stats(&session).await, "pages"), pages as i64);
            client.shutdown().await.unwrap();
        }
        let (client, session) = open("cycle", McpClientLimits::default(), None).await;
        let initial = McpCatalogCursor::new("a".into(), &Limits::default()).unwrap();
        let catalog = session
            .enumerate_catalog(
                McpCatalogKind::Tools,
                Some(&initial),
                catalog_limits(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::CursorCycle));
        assert_eq!(count(&stats(&session).await, "pages"), 2);
        client.shutdown().await.unwrap();
        let (client, session) = open(
            "normal",
            McpClientLimits {
                catalog_pages: 1,
                ..Default::default()
            },
            None,
        )
        .await;
        let catalog = session
            .enumerate_prompts(catalog_limits(), cancellation())
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::PageLimit));
        assert_eq!(count(&stats(&session).await, "pages"), 1);
        let cancelled = cancellation();
        cancelled.cancel();
        let catalog = session
            .enumerate_prompts(catalog_limits(), cancelled)
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::Cancelled));
        assert_eq!(count(&stats(&session).await, "pages"), 1);
        client.shutdown().await.unwrap();

        let (client, session) = open(
            "items",
            McpClientLimits {
                catalog_items: 2,
                ..Default::default()
            },
            None,
        )
        .await;
        let catalog = session
            .enumerate_tools(catalog_limits(), cancellation())
            .await
            .unwrap();
        assert!(matches!(
            incomplete(&catalog),
            McpCatalogStop::Failed(McpServiceError::ResponseLimit)
        ));
        assert!(catalog.descriptors().is_empty());
        client.shutdown().await.unwrap();
    });
}

#[test]
fn absolute_time_and_raw_page_byte_limits_bound_the_peer() {
    actix::System::new().block_on(async {
        let (client, session) = open("deadline", McpClientLimits::default(), None).await;
        let started = std::time::Instant::now();
        let catalog = session
            .enumerate_tools(
                McpCatalogLimits::new(10, 100, 16_384, 65_536, 150).unwrap(),
                cancellation(),
            )
            .await
            .unwrap();
        assert!(matches!(incomplete(&catalog), McpCatalogStop::Deadline));
        assert_eq!(catalog.pages(), 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        client.shutdown().await.unwrap();
        let (client, session) = open("bytes", McpClientLimits::default(), None).await;
        let result = session
            .list_tools(
                None,
                McpServiceLimits::new(512, 2_000).unwrap(),
                cancellation(),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(McpServiceError::Local { .. }) | Err(McpServiceError::ResponseLimit)
            ),
            "response must fail before adopting a large page"
        );
        client.shutdown().await.unwrap();
    });
}

fn completion(reference: McpCompletionReference) -> McpCompletionRequest {
    McpCompletionRequest::new(
        reference,
        "arg".into(),
        "a".into(),
        Some(BTreeMap::from([("resolved".into(), "kept".into())])),
        &Limits::default(),
    )
    .unwrap()
}
#[test]
fn completion_and_logging_are_capability_gated_and_preserve_structured_results() {
    actix::System::new().block_on(async {
        let (client, session) = open("normal", McpClientLimits::default(), None).await;
        for reference in [
            McpCompletionReference::Prompt("prompt".into()),
            McpCompletionReference::ResourceTemplate("test://resource/{arg}".into()),
        ] {
            let result = session
                .complete(&completion(reference), service_limits(), cancellation())
                .await
                .unwrap();
            assert_eq!(result.values(), &["alpha", "beta"]);
            assert_eq!(result.total(), Some(3));
            assert_eq!(result.has_more(), Some(true));
        }
        for level in [
            McpLoggingLevel::Debug,
            McpLoggingLevel::Info,
            McpLoggingLevel::Notice,
            McpLoggingLevel::Warning,
            McpLoggingLevel::Error,
            McpLoggingLevel::Critical,
            McpLoggingLevel::Alert,
            McpLoggingLevel::Emergency,
        ] {
            session
                .set_logging_level(level, service_limits(), cancellation())
                .await
                .unwrap();
        }
        let observed = stats(&session).await;
        assert_eq!(count(&observed, "complete"), 2);
        assert_eq!(count(&observed, "content"), 0);
        assert_eq!(
            field(&observed, "levels"),
            &Value::Array(
                [
                    "debug",
                    "info",
                    "notice",
                    "warning",
                    "error",
                    "critical",
                    "alert",
                    "emergency"
                ]
                .into_iter()
                .map(|s| Value::Text(s.into()))
                .collect()
            )
        );
        let Value::Array(references) = field(&observed, "references") else {
            panic!()
        };
        for request in references {
            let Value::Map(request) = request else {
                panic!()
            };
            let Value::Map(context) = request.get("context").unwrap() else {
                panic!()
            };
            let Value::Map(arguments) = context.get("arguments").unwrap() else {
                panic!()
            };
            assert_eq!(arguments.get("resolved"), Some(&Value::Text("kept".into())));
        }
        client.shutdown().await.unwrap();

        for mode in ["unsupported", "no-subscribe"] {
            let (client, session) = open(mode, McpClientLimits::default(), None).await;
            let subscription = session
                .subscribe_resource("test://resource", service_limits(), cancellation())
                .await;
            assert!(matches!(subscription, Err(McpServiceError::Unsupported)));
            assert!(matches!(
                session
                    .unsubscribe_resource("test://resource", service_limits(), cancellation())
                    .await,
                Err(McpServiceError::Unsupported)
            ));
            if mode == "unsupported" {
                assert!(matches!(
                    session
                        .enumerate_tools(catalog_limits(), cancellation())
                        .await,
                    Err(McpServiceError::Unsupported)
                ));
                assert!(matches!(
                    session
                        .complete(
                            &completion(McpCompletionReference::Prompt("prompt".into())),
                            service_limits(),
                            cancellation()
                        )
                        .await,
                    Err(McpServiceError::Unsupported)
                ));
                assert!(matches!(
                    session
                        .set_logging_level(McpLoggingLevel::Info, service_limits(), cancellation())
                        .await,
                    Err(McpServiceError::Unsupported)
                ));
            }
            let observed = stats(&session).await;
            for key in ["pages", "content", "complete", "subscribe", "unsubscribe"] {
                assert_eq!(count(&observed, key), 0);
            }
            client.shutdown().await.unwrap();
        }
    });
}

#[test]
fn completion_semantics_and_remote_errors_are_checked_without_disclosing_private_text() {
    actix::System::new().block_on(async {
        for mode in [
            "completion-many",
            "completion-total",
            "completion-negative",
            "remote-error",
        ] {
            let (client, session) = open(mode, McpClientLimits::default(), None).await;
            let result = session
                .complete(
                    &completion(McpCompletionReference::Prompt("prompt".into())),
                    service_limits(),
                    cancellation(),
                )
                .await;
            match result {
                Err(error @ McpServiceError::Remote(_)) if mode == "remote-error" => {
                    assert_eq!(error.delivery(), McpDelivery::ResponseReceived);
                    assert!(!format!("{error:?}").contains("private-service-message"));
                    let McpServiceError::Remote(error) = error else {
                        unreachable!()
                    };
                    assert_eq!(error.code(), -32042);
                    assert!(
                        std::str::from_utf8(error.as_bytes())
                            .unwrap()
                            .contains("private-service-message")
                    );
                }
                Err(McpServiceError::InvalidResponse) if mode != "remote-error" => {}
                _ => panic!("unexpected result in {mode}"),
            }
            assert_eq!(count(&stats(&session).await, "content"), 0);
            client.shutdown().await.unwrap();
        }
        let (client, session) = open("bad-ack", McpClientLimits::default(), None).await;
        assert!(matches!(
            session
                .subscribe_resource("test://resource", service_limits(), cancellation())
                .await,
            Err(McpServiceError::InvalidResponse)
        ));
        assert!(matches!(
            session
                .set_logging_level(McpLoggingLevel::Info, service_limits(), cancellation())
                .await,
            Err(McpServiceError::InvalidResponse)
        ));
        client.shutdown().await.unwrap();
    });
}

#[test]
fn idle_notifications_and_unsubscribe_races_never_read_resources_or_invoke_tools() {
    actix::System::new().block_on(async {
        let (events, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let (client, session) = open(
            "events",
            McpClientLimits::default(),
            Some(Arc::new(Events(events))),
        )
        .await;
        for method in [
            "notifications/resources/updated",
            "notifications/tools/list_changed",
            "notifications/prompts/list_changed",
            "notifications/resources/list_changed",
            "notifications/message",
        ] {
            let event = tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.method, method);
            assert_eq!(event.epoch, session.epoch());
        }
        let subscribed = session
            .subscribe_resource("test://resource", service_limits(), cancellation())
            .await
            .unwrap();
        assert_eq!(
            subscribed.action(),
            McpResourceSubscriptionAction::Subscribe
        );
        assert_eq!(subscribed.uri(), "test://resource");
        assert_eq!(subscribed.epoch(), session.epoch());
        let unsubscribed = session
            .unsubscribe_resource("test://resource", service_limits(), cancellation())
            .await
            .unwrap();
        assert_eq!(
            unsubscribed.action(),
            McpResourceSubscriptionAction::Unsubscribe
        );
        for _ in 0..3 {
            let event = tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.method, "notifications/resources/updated");
            assert_eq!(
                field(&event.params, "uri"),
                &Value::Text("test://resource".into())
            );
        }
        let observed = stats(&session).await;
        assert_eq!(count(&observed, "subscribe"), 1);
        assert_eq!(count(&observed, "unsubscribe"), 1);
        assert_eq!(count(&observed, "content"), 0);
        assert_eq!(
            count(&observed, "pages"),
            0,
            "catalog invalidation does not refresh/adopt"
        );
        assert_eq!(count(&observed, "complete"), 0);
        client.shutdown().await.unwrap();
    });
}

#[test]
fn checked_limits_and_cursor_representation_reject_invalid_construction() {
    assert!(McpServiceLimits::new(0, 1).is_err());
    assert!(McpServiceLimits::new(1, 0).is_err());
    assert!(McpServiceLimits::new(1, u64::MAX).is_err());
    assert!(McpCatalogLimits::new(0, 1, 1, 1, 1).is_err());
    assert!(McpCatalogLimits::new(1, 0, 1, 1, 1).is_err());
    assert!(McpCatalogLimits::new(1, 1, 1, 0, 1).is_err());
    assert!(
        McpCatalogCursor::new(
            "oversized".into(),
            &Limits {
                max_document_bytes: 2,
                ..Default::default()
            }
        )
        .is_err()
    );
    let cursor = McpCatalogCursor::new("secret opaque cursor".into(), &Limits::default()).unwrap();
    assert!(!format!("{cursor:?}").contains("secret"));
}
