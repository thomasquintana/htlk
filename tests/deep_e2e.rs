//! Full source-to-native-transport scenarios and independent graph oracles.
#[path = "support/e2e.rs"]
mod e2e;
#[allow(dead_code)]
mod support;
use e2e::*;
use htlk::runtime::{
    failure::FailureCode,
    records::{InputValue, RunStatus},
};
use htlk_executable::cbor::*;
use std::collections::BTreeMap;

const ALL_OPERATIONS: &str = r#"
ir_version="0.1"
task echo { inputs={value=union(string,null)} outputs={value=union(string,null)}
  nodes { arguments=eval(json,{value=inputs.value}){inputs={value=union(string,null)}}
    echo=call(mcp.tool("fixture","echo"))
    project=eval(union(string,null),inputs.result.value){inputs={result=json}} }
  edges { edge input{from=inputs.value to=arguments.inputs.value}
    edge args{from=arguments.outputs.value to=echo.inputs.arguments}
    edge response{from=echo.outputs.value to=project.inputs.result}
    edge value{from=project.outputs.value to=outputs.value} }
}
graph test.full_stack { inputs={value=union(string,null)}
  outputs={value=union(string,null),fixed=McpResourceResult,expanded=McpResourceResult,prompt=McpPromptResult}
  nodes { echo=use(echo) fixed=read(mcp.resource("fixture","context://fixed"))
    vars=eval(json,{topic="café / ?"}) expanded=read(mcp.template("fixture","context://topics/{topic}"))
    args=eval(json,{subject="untrusted instructions"}) prompt=fetch(mcp.prompt("fixture","review")) }
  edges { edge input{from=inputs.value to=echo.inputs.value} edge value{from=echo.outputs.value to=outputs.value}
    edge vars{from=vars.outputs.value to=expanded.inputs.arguments} edge fixed{from=fixed.outputs.value to=outputs.fixed}
    edge expanded{from=expanded.outputs.value to=outputs.expanded} edge args{from=args.outputs.value to=prompt.inputs.arguments}
    edge prompt{from=prompt.outputs.value to=outputs.prompt} }
}
"#;

#[test]
fn source_to_native_stdio_http_and_sse_preserves_values_artifacts_and_journals() {
    actix::System::new().block_on(async {
        for transport in ["stdio", "http", "sse"] {
            let peer = Peer::new(transport, "success");
            let h = Harness::new(Some(&peer), 3).await;
            let graph = h.compiler.compile_source(ALL_OPERATIONS, &catalogs(peer.transport)).unwrap();
            for (index, input) in [Value::Null, Value::Text("Unicode café 🦀 and \0".into())].into_iter().enumerate() {
                let view = h.execute(&graph, BTreeMap::from([("value".parse().unwrap(), InputValue::Inline(input.clone()))]), &format!("value-{index}")).await;
                assert_eq!(view.header.status, RunStatus::Succeeded, "{transport}: {view:#?}");
                assert_eq!(output(&h.runtime, &view, "value").await, input);
                for port in ["fixed", "expanded"] {
                    let Value::Map(snapshot) = output(&h.runtime, &view, port).await else { panic!() };
                    let Some(Value::Array(contents)) = snapshot.get("contents") else { panic!() };
                    assert_eq!(contents.len(), 2);
                    assert!(matches!(&contents[1], Value::Map(fields) if fields.get("data") == Some(&Value::Bytes(vec![0,255]))));
                }
                let Value::Map(prompt) = output(&h.runtime, &view, "prompt").await else { panic!() };
                assert!(matches!(prompt.get("messages"), Some(Value::Array(messages)) if messages.len()==2));
                assert_replay(&h.store, &view.header.run_id).await;
                let replayed = h.runtime.start_run(&caller(), graph.fingerprint, &BTreeMap::from([("value".parse().unwrap(),InputValue::Inline(input))]), &format!("value-{index}"), None).await.unwrap();
                assert_eq!(replayed.run_id, view.header.run_id);
            }
            let calls = peer.calls();
            assert_eq!(calls.len(), 8, "{calls:#?}");
            assert_eq!(calls.iter().filter(|v| matches!(v,Value::Map(m) if m.get("method")==Some(&Value::Text("tools/call".into())))).count(),2);
            assert!(calls.iter().any(|v| matches!(v,Value::Map(m) if matches!(m.get("params"),Some(Value::Map(p)) if p.get("uri")==Some(&Value::Text("context://topics/caf%C3%A9%20%2F%20%3F".into()))))));
            h.close().await;
        }
    });
}

const TOOL: &str = r#"ir_version="0.1" graph test.tool{inputs={}outputs={value=json}
nodes{args=eval(json,{value=null}) echo=call(mcp.tool("fixture","echo"))}
edges{edge args{from=args.outputs.value to=echo.inputs.arguments}edge value{from=echo.outputs.value to=outputs.value}}}"#;

#[test]
fn native_protocol_failures_have_durable_classifications_and_never_publish_outputs() {
    actix::System::new().block_on(async {
        for transport in ["stdio", "http", "sse"] {
            for (mode, expected, sent) in [
                ("identity", FailureCode::McpIdentity, 0),
                ("drift", FailureCode::McpDescriptor, 0),
                ("missing", FailureCode::McpDescriptor, 0),
                ("cursor", FailureCode::McpProtocol, 0),
                ("malformed", FailureCode::EffectUncertain, 1),
                ("wrong-id", FailureCode::EffectUncertain, 1),
                ("rpc-error", FailureCode::McpProtocol, 1),
                ("tool-error", FailureCode::McpToolError, 1),
                ("schema", FailureCode::OutputSchema, 1),
            ] {
                let peer = Peer::new(transport, mode);
                let h = Harness::new(Some(&peer), 2).await;
                let graph = h
                    .compiler
                    .compile_source(TOOL, &catalogs(peer.transport))
                    .unwrap();
                let view = h.execute(&graph, BTreeMap::new(), "fault").await;
                assert_eq!(
                    view.header.status,
                    RunStatus::Failed,
                    "{transport}/{mode}: {view:#?}"
                );
                assert!(view.outputs.is_none());
                let node = h
                    .runtime
                    .inspect_node(
                        &caller(),
                        &view.header.run_id,
                        htlk::runtime::identity::InvocationId::new(
                            view.header.root_scope,
                            &"echo".parse().unwrap(),
                            &Limits::default(),
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(node.failure.unwrap().code(), expected, "{transport}/{mode}");
                assert_eq!(peer.calls().len(), sent, "{transport}/{mode}");
                assert_replay(&h.store, &view.header.run_id).await;
                h.close().await;
            }
        }
    });
}

#[test]
fn generated_branch_graphs_match_an_independent_oracle_across_worker_counts() {
    actix::System::new().block_on(async {
        for workers in [1,2,4] {
            let h = Harness::new(None,workers).await;
            for mask in 0u32..16 {
                let mut nodes=String::new(); let mut edges=String::new();
                for i in 0..4 { nodes+=&format!("n{i}=eval(integer,{i}){{when={}}}",mask&(1<<i)!=0); edges+=&format!("edge e{i}{{from=n{i}.outputs.value to=outputs.p{i}}}"); }
                let source=format!(r#"ir_version="0.1" graph test.generated{{inputs={{}}outputs={{p0=optional(integer),p1=optional(integer),p2=optional(integer),p3=optional(integer)}}nodes{{{nodes}}}edges{{{edges}}}}}"#);
                let graph=h.compiler.compile_source(&source,&support::catalogs()).unwrap();
                let view=h.execute(&graph,BTreeMap::new(),&format!("mask-{mask}")).await;
                assert_eq!(view.header.status,RunStatus::Succeeded);
                let Value::Map(outputs)=view.outputs.as_ref().unwrap() else{panic!()};
                for i in 0..4 { let entry=outputs.get(&format!("p{i}")).unwrap(); if mask&(1<<i)==0 { assert!(matches!(entry,Value::Map(m) if m.get("state")==Some(&Value::Text("absent".into()))),"{entry:?}"); } else {assert_eq!(output(&h.runtime,&view,&format!("p{i}")).await,Value::Integer(i));} }
                assert_replay(&h.store,&view.header.run_id).await;
            }
            h.close().await;
        }
    });
}

#[test]
fn standalone_and_modular_decision_briefs_execute_through_native_http_and_sse() {
    use htlk::{
        compiler::SourceBundle,
        runtime::mcp::{McpEndpoint, NativeMcpAdapter},
    };
    use std::sync::Arc;
    actix::System::new().block_on(async {
        let catalogs = support::catalogs();
        let bundle = SourceBundle::from_json(
            include_bytes!("../specs/examples/modules/source_bundle.json"),
            &Limits::default(),
        )
        .unwrap();
        for transport in ["http", "sse"] {
            for accept_at in 1..=4 {
                for modular in [false, true] {
                    let peer = Peer::new(transport, &format!("brief-{accept_at}"));
                    let McpEndpoint::Http { url, .. } = &peer.endpoint else {
                        unreachable!()
                    };
                    let adapter = Arc::new(
                        NativeMcpAdapter::new(
                            [
                                ("example-research", "research"),
                                ("example-models", "models"),
                            ]
                            .into_iter()
                            .map(|(deployment, server)| {
                                (
                                    deployment.into(),
                                    McpEndpoint::Http {
                                        url: url.clone(),
                                        headers: BTreeMap::from([(
                                            "x-fixture-server".into(),
                                            server.into(),
                                        )]),
                                    },
                                )
                            })
                            .collect(),
                            5000,
                        )
                        .unwrap(),
                    );
                    let h = Harness::with_adapter(Some(adapter), 4).await;
                    let standalone = h
                        .compiler
                        .compile_source(
                            include_str!("../specs/examples/decision_brief.htlk"),
                            &catalogs,
                        )
                        .unwrap();
                    let graph = if modular {
                        h.compiler.compile_bundle(&bundle, &catalogs).unwrap()
                    } else {
                        standalone.clone()
                    };
                    assert_eq!(graph.executable_bytes, standalone.executable_bytes);
                    let graph = h
                        .compiler
                        .compile_ir(graph.canonical_document.clone(), &catalogs)
                        .unwrap();
                    let view = h
                        .execute(
                            &graph,
                            BTreeMap::from([(
                                "question".parse().unwrap(),
                                InputValue::Inline(Value::Text(
                                    "Should we run a bounded pilot?".into(),
                                )),
                            )]),
                            "brief",
                        )
                        .await;
                    assert_eq!(
                        view.header.status,
                        RunStatus::Succeeded,
                        "{transport}/{accept_at}/{modular}: {:?}",
                        view.failure
                    );
                    let Value::Map(result) = output(&h.runtime, &view, "result").await else {
                        panic!()
                    };
                    assert_eq!(
                        result.get("status"),
                        Some(&Value::Text(
                            if accept_at <= 3 {
                                "accepted"
                            } else {
                                "needs_attention"
                            }
                            .into()
                        ))
                    );
                    assert_eq!(peer.calls().len(), 2 + 2 * accept_at.min(3));
                    assert_replay(&h.store, &view.header.run_id).await;
                    h.close().await;
                }
            }
        }
    });
}

#[test]
fn repeated_public_port_joins_execute_the_preserved_operand_implementations() {
    use htlk::compiler::*;
    actix::System::new().block_on(async {
        let h=Harness::new(None,3).await;
        let operand=h.compiler.compile_source(r#"ir_version="0.1"prompt decorate(value=string)="[{value}]"graph test.decorate{inputs={value=string}outputs={value=string}nodes{n=eval(string,render(&decorate,{value=inputs.value})){inputs={value=string}}}edges{edge input{from=inputs.value to=n.inputs.value}edge value{from=n.outputs.value to=outputs.value}}}"#,&support::catalogs()).unwrap();
        let spec=JoinSpec::from_json(br#"{"id":"test.composed","left_alias":"existing","right_alias":"added","edges":[{"id":"pipe","from":{"graph":"left","port":"value"},"to":{"graph":"right","port":"value"}}],"inputs":[{"name":"value","to":[{"graph":"left","port":"value"}]}],"outputs":[{"name":"value","from":{"graph":"right","port":"value"}}]}"#,&Limits::default()).unwrap();
        let mut graph=operand.clone();
        for depth in 1..=4 {
            if depth>1{graph=h.compiler.join_graphs(&graph.canonical_document,&operand.canonical_document,&spec,&support::catalogs()).unwrap();}
            let view=h.execute(&graph,BTreeMap::from([("value".parse().unwrap(),InputValue::Inline(Value::Text("seed".into())))]),&format!("join-{depth}")).await;
            assert_eq!(view.header.status,RunStatus::Succeeded);
            assert_eq!(output(&h.runtime,&view,"value").await,Value::Text(format!("{}seed{}","[".repeat(depth),"]".repeat(depth))));
            assert_eq!(view.scopes.len(),2*depth-1);
            assert_replay(&h.store,&view.header.run_id).await;
        }
        h.close().await;
    });
}

#[test]
fn native_dispatch_requires_authenticated_exact_action_approval_and_keeps_disposition_history() {
    use htlk::runtime::{failure::RuntimeFailure, mcp::McpPolicy, records::SecurityLabels, *};
    use htlk_executable::{Identifier, McpBinding, digest::Digest};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    struct ApprovalPolicy;
    impl RuntimePolicy for ApprovalPolicy {
        fn authorize(
            &self,
            context: &HostContext,
            action: Authorization<'_>,
        ) -> Result<(), RuntimeFailure> {
            if let Authorization::Approve {
                authority,
                requesting_principal,
                ..
            } = action
            {
                assert_eq!(authority, "reviewer");
                assert_eq!(requesting_principal, "alice");
                if context.principal() != "approver" {
                    return Err(RuntimeFailure::new(FailureCode::Policy));
                }
            }
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
        fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
            Ok(McpPolicy {
                approval_authority: Some("reviewer".into()),
                policy_version: "review-1".into(),
                ..Default::default()
            })
        }
        fn approval_labels(
            &self,
            _: &HostContext,
            _: &identity::RunId,
            _: identity::WaitId,
            _: &Value,
            _: bool,
        ) -> Result<SecurityLabels, RuntimeFailure> {
            Ok(SecurityLabels::new(vec!["review-protected".into()], &Limits::default()).unwrap())
        }
    }
    actix::System::new().block_on(async {
        for transport in ["stdio", "http", "sse"] {
            for allow in [false, true] {
                let peer = Peer::new(transport, "success");
                let h =
                    Harness::with_policy(Some(peer.adapter()), 2, Arc::new(ApprovalPolicy)).await;
                let graph = h
                    .compiler
                    .compile_source(TOOL, &catalogs(peer.transport))
                    .unwrap();
                h.runtime
                    .register_graph(&caller(), &graph.executable_bytes, &[])
                    .await
                    .unwrap();
                let run = h
                    .runtime
                    .start_run(
                        &caller(),
                        graph.fingerprint,
                        &BTreeMap::new(),
                        "approval",
                        None,
                    )
                    .await
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(20);
                let request = loop {
                    let view = inspect(&h.runtime, &run.run_id).await;
                    if let Some(wait) = view.waits.into_iter().find(|w| w.request_digest.is_some())
                    {
                        break wait;
                    }
                    assert!(Instant::now() < deadline, "approval stalled");
                    actix::clock::sleep(Duration::from_millis(2)).await;
                };
                assert!(
                    peer.requests().is_empty(),
                    "approval precedes native discovery"
                );
                let digest = request.request_digest.unwrap();
                let approver = HostContext::new("approver".into(), &Limits::default()).unwrap();
                assert_eq!(
                    h.runtime
                        .submit_approval(&caller(), request.wait_id, digest, allow, "unauthorized")
                        .await
                        .unwrap_err()
                        .code(),
                    FailureCode::Policy
                );
                assert!(
                    h.runtime
                        .submit_approval(
                            &approver,
                            request.wait_id,
                            Digest::from_bytes([0; 32]),
                            allow,
                            "wrong-request"
                        )
                        .await
                        .is_err()
                );
                assert!(
                    h.runtime
                        .submit_reply(&approver, request.wait_id, Value::Bool(allow), "wrong-kind")
                        .await
                        .is_err()
                );
                let accepted = h
                    .runtime
                    .submit_approval(&approver, request.wait_id, digest, allow, "decision")
                    .await
                    .unwrap();
                let view = terminal(&h.runtime, &run.run_id).await;
                assert_eq!(
                    view.header.status,
                    if allow {
                        RunStatus::Succeeded
                    } else {
                        RunStatus::Failed
                    }
                );
                assert_eq!(peer.calls().len(), usize::from(allow));
                let duplicate = h
                    .runtime
                    .submit_approval(&approver, request.wait_id, digest, allow, "decision")
                    .await
                    .unwrap();
                assert_eq!(duplicate, accepted);
                assert_eq!(
                    h.runtime
                        .submit_approval(
                            &approver,
                            request.wait_id,
                            digest,
                            allow,
                            "fresh-history-key"
                        )
                        .await
                        .unwrap(),
                    accepted
                );
                assert_eq!(
                    h.runtime
                        .submit_approval(
                            &approver,
                            request.wait_id,
                            digest,
                            !allow,
                            "conflicting-history-key"
                        )
                        .await
                        .unwrap_err()
                        .code(),
                    FailureCode::IdempotencyConflict
                );
                assert_eq!(
                    h.runtime
                        .submit_approval(&approver, request.wait_id, digest, !allow, "decision")
                        .await
                        .unwrap_err()
                        .code(),
                    FailureCode::IdempotencyConflict
                );
                if allow {
                    let Value::Map(outputs) = view.outputs.as_ref().unwrap() else {
                        panic!()
                    };
                    let Some(Value::Map(entry)) = outputs.get("value") else {
                        panic!()
                    };
                    let Some(Value::Text(id)) = entry.get("artifact_id") else {
                        panic!()
                    };
                    let value = h
                        .runtime
                        .read_artifact(&caller(), id.parse().unwrap())
                        .await
                        .unwrap();
                    assert!(
                        value
                            .metadata
                            .security_labels()
                            .as_slice()
                            .contains(&"review-protected".into())
                    );
                }
                assert_replay(&h.store, &run.run_id).await;
                h.close().await;
            }
        }
    });
}

#[test]
fn cancellation_reaches_live_native_requests_and_retains_ambiguous_accounting() {
    use std::time::{Duration, Instant};
    actix::System::new().block_on(async {
        for transport in ["stdio","http","sse"]{
            let peer=Peer::new(transport,"hold");let h=Harness::new(Some(&peer),2).await;let graph=h.compiler.compile_source(TOOL,&catalogs(peer.transport)).unwrap();h.runtime.register_graph(&caller(),&graph.executable_bytes,&[]).await.unwrap();let run=h.runtime.start_run(&caller(),graph.fingerprint,&BTreeMap::new(),"cancel",None).await.unwrap();
            let deadline=Instant::now()+Duration::from_secs(20);while peer.calls().is_empty(){assert!(Instant::now()<deadline,"native request never reached peer");actix::clock::sleep(Duration::from_millis(2)).await;}
            let receipt=h.runtime.cancel_run(&caller(),&run.run_id,"host stop","stop").await.unwrap();assert_eq!(receipt.status,RunStatus::Cancelled);let view=terminal(&h.runtime,&run.run_id).await;assert_eq!(view.header.status,RunStatus::Cancelled);assert!(view.outputs.is_none());
            assert!(matches!(&view.usage,Value::Map(m)if m.get("calls_spent")==Some(&Value::Integer(1))&&m.get("concurrency_reserved")==Some(&Value::Integer(1))));
            let duplicate=h.runtime.cancel_run(&caller(),&run.run_id,"host stop","stop").await.unwrap();assert_eq!(duplicate,receipt);
            // Wait for the actual protocol cancellation, not a scheduler sleep.
            while !peer.requests().iter().any(|v|matches!(v,Value::Map(m)if m.get("method")==Some(&Value::Text("notifications/cancelled".into())))){assert!(Instant::now()<deadline,"native cancellation was not observed");actix::clock::sleep(Duration::from_millis(2)).await;}
            assert_eq!(peer.calls().len(),1);assert_replay(&h.store,&run.run_id).await;h.close().await;
        }
    });
}
