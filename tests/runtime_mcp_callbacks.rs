//! Run callbacks charge every actual ancestor and remain live with one graph worker.
#[allow(dead_code)]
mod support;
use htlk::{
    compiler::MCPCatalogInputs,
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        clock::SystemClock,
        execution::{ExecutionConfig, ExecutionEngine},
        failure::{FailureCode, RuntimeFailure},
        identity::InvocationId,
        mcp::{McpEndpoint, McpPolicy, NativeMcpAdapter, client::*},
        records::{RunStatus, SecurityLabels},
        store::{RecordFamily, SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    ExecutionLimits, Identifier, JsonDocument, McpBinding,
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

struct Policy;
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
    fn mcp_policy(&self, _: &HostContext, _: &McpBinding) -> Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            result_labels: SecurityLabels::new(vec!["private-parent".into()], &Limits::default())
                .unwrap(),
            ..Default::default()
        })
    }
}
impl McpClientAuthorizer for Policy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if actor.principal() != action.owner.host().principal() {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        if action.action == McpClientAction::Dispatch {
            assert!(action.origin.is_some());
            assert!(action.labels.as_slice().contains(&"private-parent".into()));
        }
        Ok(())
    }
}
struct Model {
    calls: AtomicUsize,
    origins: Mutex<Vec<McpOperationOrigin>>,
    related: AtomicUsize,
}
struct HttpTaskPeer(std::process::Child);
impl Drop for HttpTaskPeer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl McpClientHandler for Model {
    fn handle<'a>(
        &'a self,
        context: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if context.attribution == McpAttribution::RelatedTask {
                self.related.fetch_add(1, Ordering::SeqCst);
            }
            self.origins
                .lock()
                .unwrap()
                .push(context.origin.clone().expect("runtime origin"));
            assert!(context.labels.as_slice().contains(&"private-parent".into()));
            let mut outcome = McpServiceOutcome::completed(JsonDocument::new(br#"{"role":"assistant","model":"fixture","content":{"type":"text","text":"Answer"}}"#, &Limits::default()).unwrap());
            outcome.labels =
                SecurityLabels::new(vec!["model-private".into()], &Limits::default()).unwrap();
            outcome
        })
    }
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(fields) = value else { panic!() };
    fields.get(key).unwrap()
}

#[test]
fn runtime_callbacks_have_real_attempt_origins_and_atomic_ancestor_budgets() {
    actix::System::new().block_on(async {
        for (calls, concurrency, expected_models) in [(2, 2, 1), (1, 2, 0), (2, 1, 0)] {
          for (deferred,mode) in [(false,"stdio"),(true,"stdio"),(true,"json"),(true,"sse"),(false,"optional-no-cap")] {
            let limits = Limits::default();
            let defaults = ExecutionLimits::new().with_timeout_ms(15000).unwrap().with_attempt_timeout_ms(5000).unwrap()
                .with_max_mcp_calls(calls).unwrap().with_max_concurrency(concurrency).unwrap();
            let (compiler, registry) = support::native_compiler_with_defaults(defaults);
            let catalogs_json = r#"{
                "tools":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"server":{"deployment_id":"test","transport":"stdio","implementation_name":"peer","implementation_version":"1"}},"schema_documents":{},"tools":[{"server":"server","descriptor":{"name":"tool","inputSchema":{"type":"object"}}}]},
                "resources":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"resources":[],"templates":[]},
                "prompts":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"prompts":[]}
            }"#;
            let catalogs_json=if deferred {catalogs_json.replace(r#""descriptor":{"name":"tool","inputSchema":{"type":"object"}}"#,r#""descriptor":{"name":"tool","inputSchema":{"type":"object"},"execution":{"taskSupport":"required"}}"#)}else{catalogs_json.into()};
            let catalogs_json=if mode=="optional-no-cap"{catalogs_json.replace(r#""descriptor":{"name":"tool","inputSchema":{"type":"object"}}"#,r#""descriptor":{"name":"tool","inputSchema":{"type":"object"},"execution":{"taskSupport":"optional"}}"#)}else{catalogs_json};
            let catalogs_json=if matches!(mode,"json"|"sse") {catalogs_json.replace(r#""transport":"stdio""#,r#""transport":"streamable_http""#)}else{catalogs_json};
            let catalogs = MCPCatalogInputs::from_json(catalogs_json.as_bytes(), &limits).unwrap();
            let compiled = compiler.compile_source(r#"ir_version="0.1"
                task nested{inputs={}outputs={value=McpToolResult}nodes{args=eval(json,{})work=call(mcp.tool("server","tool"))}edges{edge args{from=args.outputs.value to=work.inputs.arguments}edge value{from=work.outputs.value to=outputs.value}}}
                graph test.callback{inputs={}outputs={value=McpToolResult}nodes{nested=use(nested)}edges{edge value{from=nested.outputs.value to=outputs.value}}}"#, &catalogs).unwrap();
            let store = SqliteStore::in_memory(StoreConfig::default()).await.unwrap();
            let model = Arc::new(Model { calls: AtomicUsize::new(0), origins: Mutex::new(Vec::new()), related: AtomicUsize::new(0) });
            let policy = Arc::new(Policy);
            let config = McpClientConfig::new(McpClientLimits { timeout_ms: 5000, ..Default::default() },
                BTreeMap::from([(McpClientService::Sampling, model.clone() as Arc<dyn McpClientHandler>)]), None).unwrap()
                .with_persistence(McpClientPersistence::new(store.clone(), Arc::new(SystemClock))).with_authorizer(policy.clone());
            let config=if deferred {config.with_graph_tasks(McpGraphTaskPolicy::Required{ttl_ms:60_000}).unwrap()}else{config};
            let config=if mode=="optional-no-cap"{config.with_graph_tasks(McpGraphTaskPolicy::Prefer{ttl_ms:60_000}).unwrap()}else{config};
            let host = McpInteractionHost::new(config.clone()).unwrap();
            let ordinary_script = r#"
                import {createInterface} from 'node:readline';const send=v=>process.stdout.write(JSON.stringify(v)+'\n');let parent;
                for await(const line of createInterface({input:process.stdin})) {
                  const r=JSON.parse(line);
                  if(r.method==='initialize') send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{}}}});
                  else if(r.method==='notifications/initialized') {}
                  else if(r.method==='tools/list') send({jsonrpc:'2.0',id:r.id,result:{tools:[{name:'tool',inputSchema:{type:'object'}}]}});
                  else if(r.method==='tools/call') {parent=r.id;send({jsonrpc:'2.0',id:'sample',method:'sampling/createMessage',params:{messages:[],maxTokens:10}});}
                  else if(r.id==='sample') send({jsonrpc:'2.0',id:parent,result:{content:[],structuredContent:{sampled:!r.error}}});
                  else if(r.method!=='notifications/cancelled') process.exit(31);
                }
            "#;
            let deferred_script=r#"
                import {createInterface} from 'node:readline';const send=v=>process.stdout.write(JSON.stringify(v)+'\n');let result,creates=0;
                const related='io.modelcontextprotocol/related-task';
                const task={taskId:'graph-task',status:'working',createdAt:new Date().toISOString(),lastUpdatedAt:new Date().toISOString(),ttl:60000,pollInterval:1};
                for await(const line of createInterface({input:process.stdin})) {
                  const r=JSON.parse(line);
                  if(r.method==='initialize') send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{},tasks:{requests:{tools:{call:{}}},cancel:{}}}}});
                  else if(r.method==='notifications/initialized') {}
                  else if(r.method==='tools/list') send({jsonrpc:'2.0',id:r.id,result:{tools:[{name:'tool',inputSchema:{type:'object'},execution:{taskSupport:'required'}}]}});
                  else if(r.method==='tools/call') {if(!r.params.task || ++creates!==1 || JSON.stringify(r.params.arguments)!=='{}')process.exit(41);send({jsonrpc:'2.0',id:r.id,result:{task}});}
                  else if(r.method==='tasks/get') send({jsonrpc:'2.0',id:r.id,result:{...task,status:'input_required'}});
                  else if(r.method==='tasks/result') {result=r.id;send({jsonrpc:'2.0',id:'sample',method:'sampling/createMessage',params:{messages:[],maxTokens:10,_meta:{[related]:{taskId:task.taskId}}}});}
                  else if(r.id==='sample') {if((r.result??r.error)?._meta?.[related]?.taskId!==task.taskId)process.exit(43);send({jsonrpc:'2.0',id:result,result:{content:[],structuredContent:{sampled:!r.error},_meta:{[related]:{taskId:task.taskId}}}});}
                  else if(r.method!=='notifications/cancelled') process.exit(42);
                }
            "#;
            let script=if mode=="optional-no-cap"{ordinary_script.replace("tools:[{name:'tool',inputSchema:{type:'object'}}]","tools:[{name:'tool',inputSchema:{type:'object'},execution:{taskSupport:'optional'}}]")}else if deferred{deferred_script.into()}else{ordinary_script.into()};
            let mut _peer=None;
            let endpoint=if !matches!(mode,"json"|"sse"){McpEndpoint::Stdio {
                program: "node".into(), arguments: vec!["--input-type=module".into(), "-e".into(), script], environment: BTreeMap::new(),
            }}else{
                use std::io::BufRead;
                let mut child=std::process::Command::new("node").args(["--input-type=module","-e",include_str!("support/mcp_graph_tasks_peer.mjs"),mode]).stdout(std::process::Stdio::piped()).spawn().unwrap();
                let mut port=String::new();std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut port).unwrap();
                _peer=Some(HttpTaskPeer(child));
                McpEndpoint::Http{url:format!("http://127.0.0.1:{}/mcp",port.trim()),headers:BTreeMap::new()}
            };
            let adapter = Arc::new(NativeMcpAdapter::with_client_config(BTreeMap::from([("test".into(), endpoint)]), config).unwrap());
            let runtime = Arc::new(Runtime::new(store.clone(), Arc::new(registry), policy, RuntimeConfig::default()).unwrap().with_mcp(adapter.clone()));
            let caller = HostContext::new("alice".into(), &limits).unwrap();
            runtime.register_graph(&caller, &compiled.executable_bytes, &[]).await.unwrap();
            let engine = ExecutionEngine::launch(runtime.clone(), ExecutionConfig { workers: 1, poll_interval: Duration::from_millis(1), ..Default::default() }).unwrap();
            let run = runtime.start_run(&caller, compiled.fingerprint, &BTreeMap::new(), "start", None).await.unwrap();
            let view = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Ok(view) = runtime.inspect_run(&caller, &run.run_id).await && view.header.status.is_terminal() { break view; }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }).await.unwrap();
            assert_eq!(view.header.status, RunStatus::Succeeded, "{calls}/{concurrency}: {:?}", view.failure);
            assert_eq!(model.calls.load(Ordering::SeqCst), expected_models);
            assert_eq!(model.related.load(Ordering::SeqCst),if deferred {expected_models}else{0});
            if expected_models != 0 { assert!(view.labels.as_slice().contains(&"model-private".into())); }
            let expected_calls = 1 + expected_models as i64;
            assert_eq!(field(&view.usage, "calls_spent"), &Value::Integer(expected_calls));
            assert_eq!(field(&view.usage, "concurrency_reserved"), &Value::Integer(0));
            let usage = store.records(&run.run_id, RecordFamily::Usage, None, 32).await.unwrap();
            assert!(usage.len() >= 5, "run, both scopes, wrapper and tool node");
            for record in &usage {
                assert_eq!(field(&record.payload, "calls_spent"), &Value::Integer(expected_calls), "{}", record.key.id);
                assert_eq!(field(&record.payload, "concurrency_reserved"), &Value::Integer(0));
            }
            let owner = McpOwner::new(caller.clone(), McpOwnerScope::Run(run.run_id.clone()), view.header.fence, &limits).unwrap();
            let page = host.list(&caller, &owner, None, 16).await.unwrap();
            assert_eq!(page.interactions.len(), 1);
            let interaction = host.inspect(&caller, &owner, page.interactions[0], 8192).await.unwrap();
            let origin = interaction.record.origin.unwrap();
            assert_eq!(origin.run_id, run.run_id);
            let parent = runtime.inspect_node(&caller, &run.run_id, origin.invocation_id).await.unwrap();
            assert_eq!(parent.local_id, "work");
            assert_eq!(parent.attempts[0].attempt_id, origin.attempt_id);
            assert_eq!(parent.client_interactions, page.interactions);
            let wrapper = InvocationId::new(view.header.root_scope, &"nested".parse().unwrap(), &limits).unwrap();
            assert_ne!(origin.invocation_id, wrapper);
            engine.shutdown().await.unwrap(); adapter.shutdown().await.unwrap();
          }
        }
    });
}
