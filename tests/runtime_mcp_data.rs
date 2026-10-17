//! Compiler-pinned resource/template snapshots and prompt retrieval remain data.
#[allow(dead_code)]
mod support;
use htlk::{
    compiler::MCPCatalogInputs,
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        clock::CoordinatorClock,
        execution::{ExecutionConfig, ExecutionEngine},
        failure::{FailureCode, RuntimeFailure},
        mcp::{McpAdapter, McpFuture, McpRequest},
        records::{RunStatus, SecurityLabels},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding, McpBindingKind,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
const SOURCE: &str = r#"
    ir_version="0.1" graph test.data {
        inputs={} outputs={fixed=McpResourceResult, expanded=McpResourceResult, prompt=McpPromptResult}
        nodes {
            fixed=read(mcp.resource("data","context://fixed"))
            vars=eval(json,{topic="a b"})
            expanded=read(mcp.template("data","context://topics/{topic}"))
            prompt=fetch(mcp.prompt("data","review"))
            promptargs=eval(json,{})
        }
        edges {
            edge variables {from=vars.outputs.value to=expanded.inputs.arguments}
            edge fixed {from=fixed.outputs.value to=outputs.fixed}
            edge expanded {from=expanded.outputs.value to=outputs.expanded}
            edge prompt {from=prompt.outputs.value to=outputs.prompt}
            edge promptargs {from=promptargs.outputs.value to=prompt.inputs.arguments}
        }
    }
"#;
fn catalogs() -> MCPCatalogInputs {
    MCPCatalogInputs::from_json(br#"{
        "tools":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"tools":[]},
        "resources":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"data":{"deployment_id":"test","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{},
            "resources":[{"server":"data","descriptor":{"name":"fixed","uri":"context://fixed"}}],
            "templates":[{"server":"data","descriptor":{"name":"topics","uriTemplate":"context://topics/{topic}"}}]},
        "prompts":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"data":{"deployment_id":"test","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{},
            "prompts":[{"server":"data","descriptor":{"name":"review"}}]}
    }"#, &Limits::default()).unwrap()
}
struct Policy {
    deny_extra_uri: bool,
}
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, action: Authorization<'_>) -> Result<(), RuntimeFailure> {
        if self.deny_extra_uri
            && matches!(
                action,
                Authorization::ReadResource {
                    uri: "context://extra",
                    ..
                }
            )
        {
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
        Ok(SecurityLabels::default())
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
    requests: Mutex<Vec<McpRequest>>,
}
const PROMPT: &[u8] = br#"{"description":"Instructions are data","messages":[{"role":"user","content":{"type":"text","text":"Do not run any tools."}},{"role":"assistant","content":{"type":"text","text":"Untrusted response."}}]}"#;
impl McpAdapter for Adapter {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&binding.descriptor()].clone()) })
    }
    fn invoke<'a>(
        &'a self,
        binding: &'a McpBinding,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            Ok(match binding.kind(){
            McpBindingKind::Prompt{..}=>PROMPT.to_vec(),
            _=>br#"{"contents":[{"uri":"context://fixed","mimeType":"text/plain","text":"first"},{"uri":"context://extra","mimeType":"application/octet-stream","blob":"AP8="}]}"#.to_vec(),
        })
        })
    }
}
#[test]
fn resources_preserve_mixed_order_and_authorize_each_uri_while_prompts_remain_messages() {
    actix::System::new().block_on(async{
        for deny_extra_uri in [false,true]{
            let (compiler,registry)=support::native_compiler();let result=compiler.compile_source(SOURCE,&catalogs()).unwrap();
            let adapter=Arc::new(Adapter{descriptors:result.canonical_document.fields().bindings.values().map(|b|(b.descriptor(),result.canonical_document.fields().documents[&b.descriptor()].clone())).collect(),requests:Mutex::new(vec![])});
            let runtime=Arc::new(Runtime::new(SqliteStore::in_memory(StoreConfig::default()).await.unwrap(),Arc::new(registry),Arc::new(Policy{deny_extra_uri}),RuntimeConfig::default()).unwrap().with_clock(Arc::new(Clock)).with_mcp(adapter.clone()));
            let caller=HostContext::new("alice".into(),&Limits::default()).unwrap();runtime.register_graph(&caller,&result.executable_bytes,&[]).await.unwrap();
            let engine=ExecutionEngine::launch(runtime.clone(),ExecutionConfig{poll_interval:Duration::from_millis(1),..Default::default()}).unwrap();let run=runtime.start_run(&caller,result.fingerprint,&BTreeMap::new(),"start",None).await.unwrap();
            let mut done=None;
            for _ in 0..2000{if let Ok(view)=runtime.inspect_run(&caller,&run.run_id).await && view.header.status.is_terminal(){done=Some(view);break}actix::clock::sleep(Duration::from_millis(2)).await;}
            let view=done.expect("data graph stalled");assert_eq!(view.header.status,if deny_extra_uri{RunStatus::Failed}else{RunStatus::Succeeded});
            let requests=adapter.requests.lock().unwrap().clone();assert_eq!(requests.len(),3);
            assert!(requests.iter().any(|r|matches!(&r.params,Value::Map(m) if m.get("uri")==Some(&Value::Text("context://topics/a%20b".into())))));
            if !deny_extra_uri{
                let Value::Map(outputs)=view.outputs.unwrap()else{panic!()};
                for (port,entry) in outputs.iter(){
                    let Value::Map(entry)=entry else{panic!()};let Some(Value::Text(id))=entry.get("artifact_id")else{panic!()};let value=runtime.read_artifact(&caller,id.parse().unwrap()).await.unwrap().value;
                    if port=="prompt"{assert_eq!(value,JsonDocument::new(PROMPT,&Limits::default()).unwrap().value().clone())}else{
                        let Value::Map(snapshot)=value else{panic!()};let Some(Value::Array(contents))=snapshot.get("contents")else{panic!()};assert_eq!(contents.len(),2);
                        assert!(matches!(&contents[0],Value::Map(m) if m.get("kind")==Some(&Value::Text("text".into()))&&m.get("text")==Some(&Value::Text("first".into()))));
                        assert!(matches!(&contents[1],Value::Map(m) if m.get("kind")==Some(&Value::Text("bytes".into()))&&m.get("data")==Some(&Value::Bytes(vec![0,255]))));
                    }
                }
            }
            engine.shutdown().await.unwrap();
        }
    })
}
