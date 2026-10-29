//! Independent Node wire responses, not live Chrome/product evidence.
#[path = "support/e2e.rs"]
#[allow(dead_code)]
mod e2e;
#[allow(dead_code)]
mod support;
use e2e::*;
use htlk::runtime::{
    failure::FailureCode,
    identity::InvocationId,
    records::{InputValue, RunStatus},
};
use htlk_executable::{JsonDocument, McpBindingKind, McpToolResult, cbor::*};
use std::collections::BTreeMap;

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(map) = value else {
        panic!("expected map")
    };
    map.get(key).unwrap()
}
fn json(text: &str) -> Value {
    JsonDocument::new(text.as_bytes(), &Limits::default())
        .unwrap()
        .value()
        .clone()
}

#[test]
fn schema_less_native_transports_preserve_candidates_media_and_authoritative_errors() {
    actix::System::new().block_on(async {
        for transport in ["stdio", "http", "sse"] {
            let peer = Peer::new(transport, "interop");
            let h = Harness::new(Some(&peer), 2).await;
            let graph = h.compiler.compile_source(r#"
                ir_version="0.1" graph test.wire_results {
                  inputs={case=string} outputs={result=McpToolResult}
                  nodes{args=eval(json,{value=inputs.case}){inputs={case=string}} echo=call(mcp.tool("fixture","echo"))}
                  edges{edge input{from=inputs.case to=args.inputs.case}edge args{from=args.outputs.value to=echo.inputs.arguments}edge out{from=echo.outputs.value to=outputs.result}}
                }"#, &schema_less_catalogs(peer.transport)).unwrap();
            assert!(graph.canonical_document.fields().bindings.values().all(|binding| matches!(binding.kind(), McpBindingKind::Tool { output_schema: None, .. })));
            let cases = [
                ("prose", "selected", Some(json(r#"{"value":"braces } and escaped \"quote\""}"#)), 1),
                ("array", "selected", Some(json("[1,null,true]")), 1),
                ("scalar", "selected", Some(Value::Bool(false)), 1),
                ("null", "selected", Some(Value::Null), 1),
                ("ordinary", "none", None, 0),
                ("ambiguous", "ambiguous", None, 2),
                ("duplicate", "selected", Some(json(r#"{"value":1}"#)), 1),
                ("malformed", "selected", Some(json(r#"{"value":2}"#)), 1),
                ("preferred", "selected", Some(json(r#"{"value":1}"#)), 2),
                ("mixed", "none", None, 0),
            ];
            for (name, status, data, count) in cases {
                let view = h.execute(&graph, BTreeMap::from([("case".parse().unwrap(), InputValue::Inline(Value::Text(name.into())))]), name).await;
                assert_eq!(view.header.status, RunStatus::Succeeded, "{transport}/{name}: {view:?}");
                let value = output(&h.runtime, &view, "result").await;
                McpToolResult::from_value(&value, &Limits::default()).unwrap();
                let Value::Map(envelope) = &value else { panic!() };
                assert_eq!(envelope.get("data"), data.as_ref(), "{transport}/{name}");
                let extraction = field(&value, "extraction");
                assert_eq!(field(extraction, "status"), &Value::Text(status.into()));
                let Value::Array(candidates) = field(extraction, "candidates") else { panic!() };
                assert_eq!(candidates.len(), count);
                if name == "duplicate" {
                    let Value::Array(sources) = field(&candidates[0], "sources") else { panic!() };
                    assert_eq!(sources.len(), 2);
                }
                if name == "mixed" {
                    let Value::Array(content) = field(field(&value, "protocol_result"), "content") else { panic!() };
                    assert_eq!(content.len(), 5);
                    assert_eq!(field(&content[1], "data"), &Value::Text("AP8=".into()));
                }
                assert_replay(&h.store, &view.header.run_id).await;
            }
            for (case, code) in [("error", FailureCode::McpToolError), ("invalid", FailureCode::McpProtocol)] {
                let view = h.execute(&graph, BTreeMap::from([("case".parse().unwrap(), InputValue::Inline(Value::Text(case.into())))]), case).await;
                assert_eq!(view.header.status, RunStatus::Failed, "{transport}/{case}");
                assert!(view.outputs.is_none());
                let invocation = InvocationId::new(view.header.root_scope, &"echo".parse().unwrap(), &Limits::default()).unwrap();
                let node = h.runtime.inspect_node(&caller(), &view.header.run_id, invocation).await.unwrap();
                assert_eq!(node.failure.unwrap().code(), code);
                let evidence = h.runtime.inspect_mcp_evidence(&caller(), &view.header.run_id, invocation, node.attempts.last().unwrap().attempt_id, 65536).await.unwrap();
                assert!(evidence.response_received);
                assert!(evidence.raw_response.is_some());
                if case == "error" {
                    assert_eq!(evidence.failure_code, Some(FailureCode::McpToolError));
                }
            }
            assert_eq!(peer.calls().len(), 12, "no retries or inference probes");
            h.close().await;
        }
    });
}
