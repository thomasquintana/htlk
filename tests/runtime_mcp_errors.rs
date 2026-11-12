//! Native correlated RPC errors survive as protected evidence, never graph data.
#[allow(dead_code)]
mod support;
use htlk::{
    compiler::MCPCatalogInputs,
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        execution::{ExecutionConfig, ExecutionEngine, McpEvidencePayload},
        failure::{FailureCode, RuntimeFailure},
        identity::{AttemptId, InvocationId},
        mcp::{McpEndpoint, McpPolicy, NativeMcpAdapter, client::McpRpcError},
        records::{RunStatus, SecurityLabels},
        store::StoreConfig,
    },
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use support::store::TestStore as SqliteStore;

struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if context.principal() == "viewer"
            && matches!(action, Authorization::InspectRun { labels, .. } if !labels.as_slice().is_empty())
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
    fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            result_labels: SecurityLabels::new(vec!["private".into()], &Limits::default()).unwrap(),
            ..Default::default()
        })
    }
}

#[test]
fn url_required_error_is_inspectable_after_shutdown_without_an_adapter_or_recall() {
    actix::System::new().block_on(async {
        let limits = Limits::default();
        let (compiler, registry) = support::native_compiler_with_timeouts(10000, 1000);
        let catalogs = MCPCatalogInputs::from_json(br#"{
            "tools":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"server":{"deployment_id":"test","transport":"stdio","implementation_name":"peer","implementation_version":"1"}},"schema_documents":{},"tools":[{"server":"server","descriptor":{"name":"tool","inputSchema":{"type":"object"}}}]},
            "resources":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"resources":[],"templates":[]},
            "prompts":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"prompts":[]}
        }"#, &limits).unwrap();
        let compiled = compiler.compile_source(r#"ir_version="0.1" graph test.rpc{inputs={}outputs={value=McpToolResult}nodes{args=eval(json,{})work=call(mcp.tool("server","tool"))}edges{edge args{from=args.outputs.value to=work.inputs.arguments}edge output{from=work.outputs.value to=outputs.value}}}"#, &catalogs).unwrap();
        let script = r#"
            import {createInterface} from 'node:readline';
            let calls=0;
            const send=value=>process.stdout.write(JSON.stringify(value)+'\n');
            for await(const line of createInterface({input:process.stdin})) {
              const r=JSON.parse(line);
              if(r.method==='initialize') send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{}}}});
              else if(r.method==='tools/list') send({jsonrpc:'2.0',id:r.id,result:{tools:[{name:'tool',inputSchema:{type:'object'}}]}});
              else if(r.method==='tools/call') send({jsonrpc:'2.0',id:r.id,error:{code:-32042,message:'private-message',data:{calls:++calls,elicitations:[{mode:'url',elicitationId:'external',url:'https://example.invalid/authorize',message:'Continue'}]}}});
              else if(r.method!=='notifications/initialized') process.exit(31);
            }
        "#;
        let adapter = Arc::new(NativeMcpAdapter::new(BTreeMap::from([("test".into(), McpEndpoint::Stdio {
            program: "node".into(), arguments: vec!["--input-type=module".into(), "-e".into(), script.into()], environment: BTreeMap::new(),
        })]), 1000).unwrap());
        let store = SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
        let registry = Arc::new(registry);
        let runtime = Arc::new(Runtime::new(store.clone(), registry.clone(), Arc::new(Policy), RuntimeConfig::default()).unwrap().with_mcp(adapter.clone()));
        let caller = HostContext::new("alice".into(), &limits).unwrap();
        runtime.register_graph(&caller, &compiled.executable_bytes, &[]).await.unwrap();
        let engine = ExecutionEngine::launch(runtime.clone(), ExecutionConfig { poll_interval: Duration::from_millis(1), ..Default::default() }).unwrap();
        let run = runtime.start_run(&caller, compiled.fingerprint, &BTreeMap::new(), "start", None).await.unwrap();
        let view = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await && view.header.status.is_terminal() { break view }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }).await.unwrap();
        assert_eq!(view.header.status, RunStatus::Failed);
        assert!(view.outputs.is_none());
        assert!(!format!("{:?}", view.failure).contains("private-message"));
        engine.shutdown().await.unwrap(); adapter.shutdown().await.unwrap();
        let recovered = Runtime::new(store, registry, Arc::new(Policy), RuntimeConfig::default()).unwrap();
        let invocation = InvocationId::new(view.header.root_scope, &"work".parse().unwrap(), &limits).unwrap();
        let attempt = AttemptId::new(invocation, 1, &limits).unwrap();
        let evidence = recovered.inspect_mcp_evidence(&caller, &run.run_id, invocation, attempt, 4096).await.unwrap();
        assert_eq!(evidence.payload_kind, Some(McpEvidencePayload::ProtocolError));
        assert!(evidence.response_received); assert!(evidence.normalized.is_none());
        assert_eq!(evidence.failure_code, Some(FailureCode::McpProtocol));
        let raw = evidence.raw_response.unwrap();
        assert!(raw.starts_with(br#"{"code":-32042,"message":"private-message","data":"#));
        let error = McpRpcError::new(JsonDocument::new(&raw, &limits).unwrap()).unwrap();
        assert_ne!(raw, error.document().as_bytes());
        assert_eq!(error.code(), -32042);
        let Value::Map(fields) = error.document().value() else { panic!() };
        let Some(Value::Map(data)) = fields.get("data") else { panic!() };
        assert_eq!(data.get("calls"), Some(&Value::Integer(1)));
        assert!(data.get("elicitations").is_some());
        let prefix = recovered.inspect_mcp_evidence(&caller, &run.run_id, invocation, attempt, 3).await.unwrap();
        assert!(prefix.raw_truncated); assert_eq!(prefix.raw_response.as_deref(), Some(&raw[..3]));
        assert_eq!(prefix.raw_digest, evidence.raw_digest);
        assert!(!prefix.normalized_truncated);
        let viewer = HostContext::new("viewer".into(), &limits).unwrap();
        assert_eq!(recovered.inspect_mcp_evidence(&viewer, &run.run_id, invocation, attempt, 4096).await.unwrap_err().code(), FailureCode::Policy);
    });
}
