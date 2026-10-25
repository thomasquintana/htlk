//! Explicit compiler configuration, authenticated demo policy and runtime lifecycle.
use crate::Result;
use htlk::{
    compiler::{analyzer, *},
    runtime::{
        Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
        execution::{ExecutionConfig, ExecutionEngine, RunView, WaitView},
        failure::{FailureCode, RuntimeFailure},
        identity::RunId,
        mcp::{McpAdapter, McpFuture, McpPolicy, McpRequest},
        records::{InputValue, RunStatus, SecurityLabels},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    self as executable, Identifier, JsonDocument, McpBinding,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct GuidePolicy;
impl RuntimePolicy for GuidePolicy {
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
                context.principal() == "guide-reviewer"
                    && requesting_principal == "guide-learner"
                    && authority == "guide-tool"
            }
            _ => context.principal() == "guide-learner",
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
        SecurityLabels::new(vec!["guide".into()], &Limits::default())
            .map_err(|_| RuntimeFailure::new(FailureCode::Policy))
    }

    fn mcp_policy(
        &self,
        _: &HostContext,
        _: &McpBinding,
    ) -> std::result::Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            approval_authority: Some("guide-tool".into()),
            policy_version: "guide-v1".into(),
            ..Default::default()
        })
    }
}

pub struct ScriptedTool {
    descriptors: BTreeMap<Digest, JsonDocument>,
    pub calls: AtomicUsize,
}
impl McpAdapter for ScriptedTool {
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            self.descriptors
                .get(&binding.descriptor())
                .cloned()
                .ok_or_else(|| RuntimeFailure::new(FailureCode::McpDescriptor))
        })
    }
    fn invoke<'a>(
        &'a self,
        _: &'a McpBinding,
        _: &'a htlk::runtime::mcp::McpRunContext,
        _: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(br#"{"content":[],"structuredContent":{"text":"Hello from the local scripted tool!"}}"#.to_vec())
        })
    }
}

pub struct Host {
    pub compiler: Compiler,
    pub catalogs: MCPCatalogInputs,
    registry: Arc<NativeRegistry>,
}
impl Host {
    pub fn new() -> Result<Self> {
        let limits = Limits::default();
        let policy = executable::PolicyDocument::new(
            executable::PolicyFields {
                cost_unit: "credits".into(),
                defaults: executable::ExecutionLimits::new()
                    .with_timeout_ms(1_800_000)?
                    .with_attempt_timeout_ms(10_000)?
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
        let catalogs = MCPCatalogInputs::from_json(include_bytes!("catalogs.json"), &limits)?;
        let compiler = Compiler::new(CompilerConfig {
            profile,
            policy,
            libraries: BTreeMap::new(),
            limits,
            frontend: FrontendLimits::default(),
            schema_options: analyzer::NativeSchemaOptions::default(),
            capabilities: CompilerCapabilities::default(),
        })?;
        Ok(Self {
            compiler,
            catalogs,
            registry: Arc::new(registry),
        })
    }

    pub fn compile(
        &self,
        source: &str,
    ) -> std::result::Result<CompilationSuccess, CompilationFailure> {
        self.compiler.compile_source(source, &self.catalogs)
    }

    pub async fn session(&self, graph: &CompilationSuccess) -> Result<Session> {
        let tool = Arc::new(ScriptedTool {
            descriptors: graph
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
                .collect(),
            calls: AtomicUsize::new(0),
        });
        let runtime = Arc::new(
            Runtime::new(
                SqliteStore::in_memory(StoreConfig::default()).await?,
                self.registry.clone(),
                Arc::new(GuidePolicy),
                RuntimeConfig::default(),
            )?
            .with_mcp(tool.clone()),
        );
        let context = HostContext::new("guide-learner".into(), &Limits::default())?;
        runtime
            .register_graph(&context, &graph.executable_bytes, &[])
            .await?;
        let engine = ExecutionEngine::launch(runtime.clone(), ExecutionConfig::default())?;
        Ok(Session {
            runtime,
            context,
            engine,
            fingerprint: graph.fingerprint,
            run: None,
            tool,
        })
    }
}

pub struct Session {
    pub runtime: Arc<Runtime>,
    pub context: HostContext,
    engine: ExecutionEngine,
    fingerprint: Digest,
    pub run: Option<RunId>,
    pub tool: Arc<ScriptedTool>,
}
impl Session {
    pub async fn start(&mut self, values: &[(&str, Value)]) -> Result<()> {
        let inputs = values
            .iter()
            .map(|(name, value)| Ok((name.parse()?, InputValue::Inline(value.clone()))))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let receipt = self
            .runtime
            .start_run(
                &self.context,
                self.fingerprint,
                &inputs,
                "guide-start",
                None,
            )
            .await?;
        self.run = Some(receipt.run_id);
        Ok(())
    }

    pub async fn view(&self) -> Result<RunView> {
        Ok(self
            .runtime
            .inspect_run(&self.context, self.run.as_ref().ok_or("run not started")?)
            .await?)
    }

    pub async fn wait(&self) -> Result<WaitView> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let view = self.view().await?;
            if let Some(wait) = view.waits.into_iter().next() {
                return Ok(wait);
            }
            if view.header.status.is_terminal() {
                return Err(format!("run ended before opening a wait: {:?}", view.failure).into());
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for the request to open".into());
            }
            actix::clock::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn terminal(&self) -> Result<RunView> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let view = self.view().await?;
            if view.header.status.is_terminal() {
                return Ok(view);
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for the run to finish".into());
            }
            actix::clock::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn output(&self, port: &str) -> Result<Value> {
        let view = self.terminal().await?;
        if view.header.status != RunStatus::Succeeded {
            return Err(format!("run did not succeed: {:?}", view.failure).into());
        }
        if let Some(Value::Map(outputs)) = view.outputs
            && let Some(Value::Map(value)) = outputs.get(port)
            && let Some(Value::Text(id)) = value.get("artifact_id")
        {
            return Ok(self
                .runtime
                .read_artifact(&self.context, id.parse()?)
                .await?
                .value);
        }
        Err(format!("required output '{port}' is absent").into())
    }

    pub async fn approve(&self, wait: &WaitView, allow: bool) -> Result<()> {
        // This local tutorial supplies both identities. A real host authenticates its reviewer.
        let reviewer = HostContext::new("guide-reviewer".into(), &Limits::default())?;
        self.runtime
            .submit_approval(
                &reviewer,
                wait.wait_id,
                wait.request_digest.ok_or("missing approval digest")?,
                allow,
                "guide-decision",
            )
            .await?;
        Ok(())
    }

    pub async fn finish<T>(self, result: Result<T>) -> Result<T> {
        // Always stop supervision, including when a prompt returns menu/quit/EOF.
        let cleanup: Result<()> = async {
            if let Some(run) = &self.run
                && !self.view().await?.header.status.is_terminal()
            {
                self.runtime
                    .cancel_run(&self.context, run, "left tutorial lesson", "guide-cancel")
                    .await?;
            }
            Ok(())
        }
        .await;
        let shutdown = self.engine.shutdown().await;
        let value = result?;
        cleanup?;
        shutdown?;
        Ok(value)
    }
}
