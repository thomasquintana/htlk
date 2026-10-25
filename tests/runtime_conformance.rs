//! Remaining runtime §17 boundaries exercised with compiler-produced actor graphs.
#[allow(dead_code)]
mod support;
use htlk::{
    compiler::MCPCatalogInputs,
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        clock::CoordinatorClock,
        execution::{ExecutionConfig, ExecutionEngine, RunView},
        failure::{FailureCode, RuntimeFailure},
        identity::InvocationId,
        mcp::{McpAdapter, McpFuture, McpRequest},
        records::{InputValue, ManifestEntry, RunStatus, SecurityLabels},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        let labels = match action {
            Authorization::InspectRun { labels, .. }
            | Authorization::ReadBoundary { labels, .. } => Some(labels),
            Authorization::ReadArtifact { metadata } => Some(metadata.security_labels()),
            _ => None,
        };
        if context.principal() == "viewer" && labels.is_some_and(|l| !l.as_slice().is_empty()) {
            return Err(RuntimeFailure::new(FailureCode::Policy));
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
        Ok(SecurityLabels::new(vec!["classified".into()], &Limits::default()).unwrap())
    }
}
struct Clock;
impl CoordinatorClock for Clock {
    fn now_ms(&self) -> Result<i64, RuntimeFailure> {
        Ok(1000)
    }
}
struct Echo {
    descriptors: BTreeMap<Digest, JsonDocument>,
    discovered: AtomicUsize,
    sent: AtomicUsize,
    error: bool,
}
impl McpAdapter for Echo {
    fn descriptor<'a>(
        &'a self,
        b: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            self.discovered.fetch_add(1, Ordering::SeqCst);
            Ok(self.descriptors[&b.descriptor()].clone())
        })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.sent.fetch_add(1, Ordering::SeqCst);
            assert!(
                matches!(&request.params,Value::Map(m) if matches!(m.get("arguments"),Some(Value::Map(a)) if a.get("value")==Some(&Value::Null)))
            );
            Ok(if self.error {
                br#"{"isError":true,"content":[],"structuredContent":{}}"#.to_vec()
            } else {
                br#"{"content":[],"structuredContent":{"value":null}}"#.to_vec()
            })
        })
    }
}
fn catalogs() -> MCPCatalogInputs {
    MCPCatalogInputs::from_json(br#"{
    "tools":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"fixture":{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{},"tools":[{"server":"fixture","descriptor":{"name":"echo","inputSchema":{"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"],"additionalProperties":false}}}]},
    "resources":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"fixture":{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{},"resources":[{"server":"fixture","descriptor":{"name":"zero","uri":"write://zero"}}],"templates":[]},
    "prompts":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"prompts":[]}
}"#,&Limits::default()).unwrap()
}
async fn execute(
    source: &str,
    inputs: BTreeMap<Identifier, InputValue>,
    error: bool,
) -> (Arc<Runtime>, RunView, Arc<Echo>) {
    let (compiler, registry) = support::native_compiler();
    let graph = compiler.compile_source(source, &catalogs()).unwrap();
    let echo = Arc::new(Echo {
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
        discovered: AtomicUsize::new(0),
        sent: AtomicUsize::new(0),
        error,
    });
    let runtime = Arc::new(
        Runtime::new(
            SqliteStore::in_memory(StoreConfig::default())
                .await
                .unwrap(),
            Arc::new(registry),
            Arc::new(Policy),
            RuntimeConfig::default(),
        )
        .unwrap()
        .with_clock(Arc::new(Clock))
        .with_mcp(echo.clone()),
    );
    let caller = HostContext::new("alice".into(), &Limits::default()).unwrap();
    runtime
        .register_graph(&caller, &graph.executable_bytes, &[])
        .await
        .unwrap();
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            poll_interval: Duration::from_millis(1),
            ..Default::default()
        },
    )
    .unwrap();
    let run = runtime
        .start_run(&caller, graph.fingerprint, &inputs, "start", None)
        .await
        .unwrap();
    let mut done = None;
    for _ in 0..2000 {
        if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await
            && view.header.status.is_terminal()
        {
            done = Some(view);
            break;
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
    let view = done.expect("conformance graph stalled");
    engine.shutdown().await.unwrap();
    (runtime, view, echo)
}
fn output(view: &RunView) -> Option<htlk::runtime::identity::ArtifactId> {
    let Some(Value::Map(outputs)) = &view.outputs else {
        return None;
    };
    let Some(Value::Map(value)) = outputs.get("value") else {
        return None;
    };
    let Some(Value::Text(id)) = value.get("artifact_id") else {
        return None;
    };
    Some(id.parse().unwrap())
}
#[test]
fn present_null_remains_a_value_through_mcp_projection_and_cbor() {
    actix::System::new().block_on(async {
        let (runtime,view,echo)=execute(r#"ir_version="0.1" graph test.null{inputs={}outputs={value=union(string,null)}nodes{args=eval(json,{value=null}) request=call(mcp.tool("fixture","echo")) project=eval(union(string,null),inputs.response.value){inputs={response=json}}}edges{edge args{from=args.outputs.value to=request.inputs.arguments}edge response{from=request.outputs.value to=project.inputs.response}edge value{from=project.outputs.value to=outputs.value}}}"#,BTreeMap::new(),false).await;assert_eq!(view.header.status,RunStatus::Succeeded);assert_eq!(echo.sent.load(Ordering::SeqCst),1);let value=runtime.read_artifact(&HostContext::new("alice".into(),&Limits::default()).unwrap(),output(&view).unwrap()).await.unwrap();assert_eq!(value.value,Value::Null);assert_eq!(executable_bytes(&value.value),vec![0xf6]);
    })
}
fn executable_bytes(value: &Value) -> Vec<u8> {
    htlk_executable::cbor::encode(value, &Limits::default()).unwrap()
}
#[test]
fn optional_absence_skipped_sources_and_zero_input_false_guards_do_not_dispatch() {
    actix::System::new().block_on(async {
        let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();
        let (runtime,view,_)=execute(r#"ir_version="0.1" graph test.absent{inputs={}outputs={value=optional(string)}nodes{copy=eval(optional(string),inputs.value){inputs={value=optional(string)}}}edges{edge value{from=copy.outputs.value to=outputs.value}}}"#,BTreeMap::new(),false).await;assert_eq!(view.header.status,RunStatus::Succeeded);assert!(output(&view).is_none());let node=runtime.inspect_node(&caller,&view.header.run_id,InvocationId::new(view.header.root_scope,&"copy".parse().unwrap(),&Limits::default()).unwrap()).await.unwrap();assert!(matches!(node.input_manifest.unwrap().entries().next().unwrap().1,ManifestEntry::Absent));
        let (_,view,_)=execute(r#"ir_version="0.1" graph test.skipped{inputs={}outputs={value=integer}nodes{source=eval(integer,1){when=false} consumer=eval(integer,inputs.value){inputs={value=integer}}}edges{edge source{from=source.outputs.value to=consumer.inputs.value}edge value{from=consumer.outputs.value to=outputs.value}}}"#,BTreeMap::new(),false).await;assert_eq!(view.failure.unwrap().code(),FailureCode::OutputMissing);
        let (_,view,echo)=execute(r#"ir_version="0.1" graph test.zero{inputs={}outputs={}nodes{write=read(mcp.resource("fixture","write://zero")){when=false}}edges{}postconditions=status(@write)=="skipped"}"#,BTreeMap::new(),false).await;assert_eq!(view.header.status,RunStatus::Succeeded);assert_eq!(echo.discovered.load(Ordering::SeqCst),0);assert_eq!(echo.sent.load(Ordering::SeqCst),0);
    })
}
#[test]
fn unexported_side_effect_outcome_is_required_only_by_the_explicit_scope_contract() {
    actix::System::new().block_on(async {
        for expected in ["succeeded","failed"]{
            let source=format!(r#"ir_version="0.1" graph test.effect{{inputs={{}}outputs={{}}nodes{{args=eval(json,{{value=null}}) write=call(mcp.tool("fixture","echo"))}}edges{{edge args{{from=args.outputs.value to=write.inputs.arguments}}}}postconditions=status(@write)=="{expected}"}}"#);let (_,view,echo)=execute(&source,BTreeMap::new(),true).await;assert_eq!(echo.sent.load(Ordering::SeqCst),1);assert_eq!(view.header.status,if expected=="failed"{RunStatus::Succeeded}else{RunStatus::Failed});if expected=="succeeded"{assert_eq!(view.failure.unwrap().code(),FailureCode::Postconditions);}
        }
    })
}
#[test]
fn protected_projection_and_ordinary_scope_transfer_keep_receiving_boundary_labels() {
    actix::System::new().block_on(async {
        let source=r#"ir_version="0.1" type Secret=record{secret=string} task leaf{inputs={value=Secret}outputs={value=string}nodes{project=eval(string,inputs.value.secret){inputs={value=Secret}}}edges{edge input{from=inputs.value to=project.inputs.value}edge output{from=project.outputs.value to=outputs.value}}}graph test.protected{inputs={value=Secret}outputs={value=string}nodes{leaf=use(leaf)}edges{edge input{from=inputs.value to=leaf.inputs.value}edge value{from=leaf.outputs.value to=outputs.value}}}"#;let input=Value::Map(Map::try_from_entries([("secret".into(),Value::Text("classified value".into()))]).unwrap());let (runtime,view,_)=execute(source,BTreeMap::from([("value".parse().unwrap(),InputValue::Inline(input))]),false).await;assert_eq!(view.header.status,RunStatus::Succeeded);let owner=HostContext::new("alice".into(),&Limits::default()).unwrap();let viewer=HostContext::new("viewer".into(),&Limits::default()).unwrap();let id=output(&view).unwrap();assert_eq!(runtime.read_artifact(&owner,id).await.unwrap().metadata.security_labels().as_slice(),["classified".to_string()]);assert_eq!(runtime.read_artifact(&viewer,id).await.unwrap_err().code(),FailureCode::Policy);assert_eq!(runtime.inspect_run(&viewer,&view.header.run_id).await.unwrap_err().code(),FailureCode::Policy);
    })
}
#[test]
fn task_definition_and_wrapper_contracts_are_conjunctive_and_precede_body_creation() {
    actix::System::new().block_on(async {
        for (definition_pre,wrapper_pre,definition_post,wrapper_post) in [(false,true,true,true),(true,false,true,true),(true,true,false,true),(true,true,true,false),(true,true,true,true)]{
            let source=r#"ir_version="0.1" task leaf{inputs={}outputs={value=integer}nodes{value=eval(integer,1)}edges{edge value{from=value.outputs.value to=outputs.value}}preconditions=DEF_PRE postconditions=DEF_POST}graph test.contracts{inputs={}outputs={value=integer}nodes{n=use(leaf){preconditions=WRAPPER_PRE postconditions=WRAPPER_POST}}edges{edge value{from=n.outputs.value to=outputs.value}}}"#.replace("DEF_PRE",&definition_pre.to_string()).replace("DEF_POST",&definition_post.to_string()).replace("WRAPPER_PRE",&wrapper_pre.to_string()).replace("WRAPPER_POST",&wrapper_post.to_string());let (runtime,view,_)=execute(&source,BTreeMap::new(),false).await;let all=definition_pre&&wrapper_pre&&definition_post&&wrapper_post;assert_eq!(view.header.status,if all{RunStatus::Succeeded}else{RunStatus::Failed});let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();let node=runtime.inspect_node(&caller,&view.header.run_id,InvocationId::new(view.header.root_scope,&"n".parse().unwrap(),&Limits::default()).unwrap()).await.unwrap();if !definition_pre||!wrapper_pre{assert_eq!(view.scopes.len(),1);assert!(node.child_scope.is_none());assert_eq!(node.failure.unwrap().code(),FailureCode::Preconditions);}else if !all{assert_eq!(node.failure.unwrap().code(),FailureCode::Postconditions);}
        }
    })
}
