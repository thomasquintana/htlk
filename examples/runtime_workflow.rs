//! Compile/register/run, a compiler-in-the-loop planning window, and exact approval.
use htlk::{
    compiler::*,
    runtime::{
        self, Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
        execution::{CloseWait, ExecutionConfig, ExecutionEngine, ExtensionRequest},
        failure::{FailureCode, RuntimeFailure},
        mcp::{McpAdapter, McpFuture, McpPolicy, McpRequest},
        records::{InputValue, RunStatus, SecurityLabels},
        source::{SourceEntry, SourceOrigin, SourceSidecar},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    self as executable, Identifier, JsonDocument, JsonPointer, McpBinding,
    cbor::{Limits, Value},
    digest::{Digest, hash_bytes},
};
use std::{collections::BTreeMap, error::Error, sync::Arc, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> std::result::Result<(), RuntimeFailure> {
        let allowed = match action {
            Authorization::Approve {
                authority,
                requesting_principal,
                ..
            } => {
                context.principal() == "reviewer"
                    && requesting_principal == "demo"
                    && authority == "demo-tool"
            }
            _ => context.principal() == "demo",
        };
        if allowed {
            Ok(())
        } else {
            Err(RuntimeFailure::new(FailureCode::Policy))
        }
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> std::result::Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::new(vec!["demo".into()], &Limits::default()).unwrap())
    }
    fn mcp_policy(
        &self,
        _: &HostContext,
        _: &McpBinding,
    ) -> std::result::Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            approval_authority: Some("demo-tool".into()),
            policy_version: "demo-v1".into(),
            ..Default::default()
        })
    }
}
struct ScriptedTool {
    descriptors: BTreeMap<Digest, JsonDocument>,
}
impl McpAdapter for ScriptedTool {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move { Ok(self.descriptors[&binding.descriptor()].clone()) })
    }
    fn invoke<'a>(&'a self, _: &'a McpBinding, _: McpRequest) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async {
            Ok(br#"{"content":[],"structuredContent":{"evidence_text":"A bounded, approved scripted result."}}"#.to_vec())
        })
    }
}
fn compiler() -> Result<(Compiler, Arc<NativeRegistry>)> {
    let limits = Limits::default();
    let policy = executable::PolicyDocument::new(
        executable::PolicyFields {
            cost_unit: "credits".into(),
            defaults: executable::ExecutionLimits::new()
                .with_timeout_ms(30000)?
                .with_attempt_timeout_ms(1000)?
                .with_max_concurrency(1)?,
            evaluator_limits: executable::EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 65536,
                max_collection_visits: 10000,
                max_regex_bytes: 4096,
                max_regex_compiled_bytes: 65536,
                max_output_bytes: 65536,
                max_steps: 100000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &limits,
    )?;
    let mut registry = NativeRegistry::new(&limits)?;
    let profile = registry.link_policy(&policy)?;
    let compiler = Compiler::new(CompilerConfig {
        profile,
        policy,
        libraries: BTreeMap::new(),
        limits,
        frontend: FrontendLimits::default(),
        schema_options: analyzer::NativeSchemaOptions::default(),
        capabilities: CompilerCapabilities::default(),
    })?;
    Ok((compiler, Arc::new(registry)))
}
fn sources(result: &CompilationSuccess, source: &str) -> Result<SourceSidecar> {
    let entries = result
        .source_map
        .iter()
        .map(|e| {
            Ok(SourceEntry {
                canonical_path: JsonPointer::new(&e.canonical_path, &Limits::default())?,
                origin: match &e.span.source {
                    SourceLocation::Inline => SourceOrigin::Inline,
                    SourceLocation::Module {
                        package_digest,
                        module_path,
                        source_path,
                    } => SourceOrigin::Module {
                        package_digest: *package_digest,
                        module_path: module_path.clone(),
                        source_path: source_path.clone(),
                    },
                },
                span: Some((e.span.start_byte as u64, e.span.end_byte as u64)),
                declaration_name: e.declaration_name.clone(),
                description: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SourceSidecar {
        provenance: hash_bytes(source.as_bytes()),
        entries,
    })
}
async fn waiting(
    runtime: &Runtime,
    context: &HostContext,
    run: &runtime::identity::RunId,
) -> Result<runtime::execution::WaitView> {
    for _ in 0..1000 {
        if let Ok(view) = runtime.inspect_run(context, run).await
            && let Some(wait) = view.waits.into_iter().next()
        {
            return Ok(wait);
        }
        actix::clock::sleep(Duration::from_millis(10)).await;
    }
    Err("request did not open".into())
}
async fn output(
    runtime: &Runtime,
    context: &HostContext,
    run: &runtime::identity::RunId,
    port: &str,
) -> Result<Value> {
    for _ in 0..1000 {
        if let Ok(view) = runtime.inspect_run(context, run).await
            && view.header.status.is_terminal()
        {
            if view.header.status != RunStatus::Succeeded {
                return Err(format!("run failed: {:?}", view.failure).into());
            }
            if let Some(Value::Map(outputs)) = view.outputs
                && let Some(Value::Map(value)) = outputs.get(port)
                && let Some(Value::Text(id)) = value.get("artifact_id")
            {
                return Ok(runtime.read_artifact(context, id.parse()?).await?.value);
            }
            return Err("required output absent".into());
        }
        actix::clock::sleep(Duration::from_millis(10)).await;
    }
    Err("run did not finish".into())
}
async fn planning(
    compiler: &Compiler,
    registry: Arc<NativeRegistry>,
    catalogs: &MCPCatalogInputs,
    context: &HostContext,
) -> Result<()> {
    const SOURCE: &str = r#"
        ir_version="0.1" graph example.planning{
            inputs={context=string}outputs={context=string}
            nodes{request=eval(json,{context=inputs.context}){inputs={context=string}}window=wait(boolean){topic="graph_extension" timeout_ms=20000}}
            edges{edge context{from=inputs.context to=request.inputs.context}edge request{from=request.outputs.value to=window.inputs.request}edge output{from=inputs.context to=outputs.context}}
            postconditions=status(@window)=="succeeded"
        }
    "#;
    let original = compiler.compile_source(SOURCE, catalogs)?;
    let service = Arc::new(Runtime::new(
        SqliteStore::in_memory(StoreConfig::default()).await?,
        registry,
        Arc::new(Policy),
        RuntimeConfig::default(),
    )?);
    service
        .register_graph(context, &original.executable_bytes, &[])
        .await?;
    service
        .attach_source(context, original.fingerprint, &sources(&original, SOURCE)?)
        .await?;
    let engine = ExecutionEngine::launch(service.clone(), ExecutionConfig::default())?;
    let run = service
        .start_run(
            context,
            original.fingerprint,
            &BTreeMap::from([(
                "context".parse()?,
                InputValue::Inline(Value::Text("Preserved planning context".into())),
            )]),
            "planning-start",
            None,
        )
        .await?;
    let window = waiting(&service, context, &run.run_id).await?;
    let next=compiler.compile_source(r#"ir_version="0.1" graph example.added{inputs={context=string}outputs={result=string}nodes{copy=eval(string,inputs.context){inputs={context=string}}}edges{edge input{from=inputs.context to=copy.inputs.context}edge result{from=copy.outputs.value to=outputs.result}}}"#,catalogs)?;
    let endpoint = |graph, port: &str| JoinEndpoint {
        graph,
        port: port.into(),
    };
    let joined = compiler.join_graphs(
        &original.canonical_document,
        &next.canonical_document,
        &JoinSpec {
            id: "example.extended".into(),
            left_alias: "preserved".into(),
            right_alias: "added".into(),
            edges: vec![JoinEdge {
                id: "context".into(),
                from: endpoint(JoinOperand::Left, "context"),
                to: endpoint(JoinOperand::Right, "context"),
            }],
            inputs: vec![JoinInput {
                name: "context".into(),
                to: vec![endpoint(JoinOperand::Left, "context")],
            }],
            outputs: vec![JoinOutput {
                name: "result".into(),
                from: endpoint(JoinOperand::Right, "result"),
            }],
        },
        catalogs,
    )?;
    service
        .register_graph(context, &joined.executable_bytes, &[])
        .await?;
    service
        .inherit_sources(context, original.fingerprint, joined.fingerprint)
        .await?;
    service
        .attach_source(
            context,
            joined.fingerprint,
            &SourceSidecar {
                provenance: hash_bytes(b"generated binary join"),
                entries: vec![SourceEntry {
                    canonical_path: JsonPointer::new(
                        &format!("/scopes/{}", joined.canonical_document.fields().root_scope),
                        &Limits::default(),
                    )?,
                    origin: SourceOrigin::Synthetic {
                        reason: "binary join wrapper".into(),
                    },
                    span: None,
                    declaration_name: None,
                    description: Some("Preserved instance plus added sibling.".into()),
                }],
            },
        )
        .await?;
    let installed = service
        .extend_run(
            context,
            &run.run_id,
            &ExtensionRequest {
                expected_fingerprint: original.fingerprint,
                composed_fingerprint: joined.fingerprint,
                inputs: BTreeMap::new(),
                close_wait: Some(CloseWait {
                    wait_id: window.wait_id,
                    value: Value::Bool(true),
                    idempotency_key: "close-planning".into(),
                }),
            },
            "install-extension",
        )
        .await?;
    println!(
        "extension {}: {:?}",
        installed.extension_number,
        output(&service, context, &run.run_id, "result").await?
    );
    engine.shutdown().await?;
    Ok(())
}
async fn approval(
    compiler: &Compiler,
    registry: Arc<NativeRegistry>,
    catalogs: &MCPCatalogInputs,
    context: &HostContext,
) -> Result<()> {
    let graph=compiler.compile_source(r#"ir_version="0.1" graph example.approval{inputs={}outputs={value=json}nodes{args=eval(json,{query="example"}) lookup=call(mcp.tool("research","lookup"))}edges{edge args{from=args.outputs.value to=lookup.inputs.arguments}edge output{from=lookup.outputs.value to=outputs.value}}}"#,catalogs)?;
    let descriptors = graph
        .canonical_document
        .fields()
        .bindings
        .values()
        .map(|binding| {
            (
                binding.descriptor(),
                graph.canonical_document.fields().documents[&binding.descriptor()].clone(),
            )
        })
        .collect();
    let service = Arc::new(
        Runtime::new(
            SqliteStore::in_memory(StoreConfig::default()).await?,
            registry,
            Arc::new(Policy),
            RuntimeConfig::default(),
        )?
        .with_mcp(Arc::new(ScriptedTool { descriptors })),
    );
    service
        .register_graph(context, &graph.executable_bytes, &[])
        .await?;
    let engine = ExecutionEngine::launch(service.clone(), ExecutionConfig::default())?;
    let run = service
        .start_run(
            context,
            graph.fingerprint,
            &BTreeMap::new(),
            "approval-start",
            None,
        )
        .await?;
    let request = waiting(&service, context, &run.run_id).await?;
    // A real transport authenticates this reviewer. JSON fields cannot impersonate it.
    let reviewer = HostContext::new("reviewer".into(), &Limits::default())?;
    service
        .submit_approval(
            &reviewer,
            request.wait_id,
            request.request_digest.ok_or("missing approval digest")?,
            true,
            "allow-exact-action",
        )
        .await?;
    println!(
        "approved result: {:?}",
        output(&service, context, &run.run_id, "value").await?
    );
    engine.shutdown().await?;
    Ok(())
}
fn main() -> Result<()> {
    actix::System::new().block_on(async {
        let (compiler, registry) = compiler()?;
        let catalogs = MCPCatalogInputs::from_json(
            include_bytes!("../specs/examples/decision_brief.catalogs.json"),
            &Limits::default(),
        )?;
        let context = HostContext::new("demo".into(), &Limits::default())?;
        planning(&compiler, registry.clone(), &catalogs, &context).await?;
        approval(&compiler, registry, &catalogs, &context).await?;
        Ok(())
    })
}
