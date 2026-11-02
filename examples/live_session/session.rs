//! Durable ordinary graph execution; host commands never bypass dispatch policy.

use super::{
    graph,
    host::{self, Grant, Policy},
    model::{self, Result, field, object, text},
};
use htlk::{
    compiler::*,
    runtime::{
        HostContext, NativeRegistry, Runtime, RuntimeConfig,
        execution::{ExecutionConfig, ExecutionEngine, WaitView},
        failure::FailureCode,
        identity::{InvocationId, RunId},
        link_mcp_content_library,
        mcp::{
            McpAdapter, McpAttemptFuture, McpConnection, McpFuture, McpRequest, McpRunContext,
            NativeMcpAdapter,
        },
        records::RunStatus,
        store::{RecordFamily, SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    JsonDocument, McpBinding,
    cbor::{Limits, Value},
    digest::Digest,
    *,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Audited {
    native: Arc<NativeMcpAdapter>,
    calls: Arc<AtomicUsize>,
}
struct Connection<'a> {
    native: Box<dyn McpConnection + 'a>,
    calls: Arc<AtomicUsize>,
}
impl McpConnection for Connection<'_> {
    fn descriptor(&self) -> &JsonDocument {
        self.native.descriptor()
    }
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.native.invoke(request)
    }
    fn close(&mut self) -> McpFuture<'_, ()> {
        self.native.close()
    }
}
impl McpAdapter for Audited {
    fn connect<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, Box<dyn McpConnection + 'a>> {
        Box::pin(async move {
            Ok(Box::new(Connection {
                native: self.native.connect(binding, context, limits).await?,
                calls: self.calls.clone(),
            }) as Box<dyn McpConnection + 'a>)
        })
    }
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        self.native.descriptor(binding, context, limits)
    }
    fn invoke<'a>(
        &'a self,
        binding: &'a McpBinding,
        context: &'a McpRunContext,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.native.invoke(binding, context, request)
    }
}

fn compiler(attempt_ms: u64) -> Result<(Compiler, NativeRegistry, Digest)> {
    let limits = Limits::default();
    let policy = PolicyDocument::new(
        PolicyFields {
            cost_unit: "live-test-calls".into(),
            defaults: ExecutionLimits::new()
                .with_timeout_ms(3_600_000)?
                .with_attempt_timeout_ms(attempt_ms)?
                .with_max_concurrency(1)?,
            evaluator_limits: EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 12 * 1024 * 1024,
                max_collection_visits: 1_000_000,
                max_regex_bytes: 4096,
                max_regex_compiled_bytes: 65536,
                max_output_bytes: 12 * 1024 * 1024,
                max_steps: 100_000_000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 100_000,
        },
        &limits,
    )?;
    let mut registry = NativeRegistry::new(&limits)?;
    let profile = registry.link_policy(&policy)?;
    let library = link_mcp_content_library(&mut registry, &limits)?;
    let content = library.implementation_digest();
    Ok((
        Compiler::new(CompilerConfig {
            profile,
            policy,
            libraries: BTreeMap::from([(content, library)]),
            mcp_result_policies: BTreeMap::new(),
            mcp_result_documents: BTreeMap::new(),
            limits,
            frontend: FrontendLimits::default(),
            capabilities: CompilerCapabilities::default(),
            schema_options: analyzer::NativeSchemaOptions::default(),
        })?,
        registry,
        content,
    ))
}

pub(super) struct Session {
    runtime: Arc<Runtime>,
    store: SqliteStore,
    native: Arc<NativeMcpAdapter>,
    engine: Option<ExecutionEngine>,
    owner: HostContext,
    run: RunId,
    wait: Option<WaitView>,
    grant: Grant,
    calls: Arc<AtomicUsize>,
    tools: Vec<String>,
    directory: PathBuf,
    maximum: usize,
    deadline_ms: u64,
    sequence: usize,
}

impl Session {
    pub(super) async fn open(
        host_path: &Path,
        catalog_path: &Path,
        directory: &Path,
    ) -> Result<Self> {
        let host = model::read(host_path)?;
        let catalog = model::read(catalog_path)?;
        let alias = text(field(host.value(), "alias")?)?;
        let deployment = text(field(host.value(), "deployment_id")?)?.to_owned();
        let maximum = number(host.value(), "max_calls", 200)? as usize;
        let attempt_ms = number(host.value(), "attempt_timeout_ms", 180_000)?;
        if !(1000..=600_000).contains(&attempt_ms) {
            return Err("invalid attempt timeout".into());
        }
        let Value::Array(entries) = field(field(catalog.value(), "tools")?, "tools")? else {
            return Err("tool catalog array required".into());
        };
        let mut tools = Vec::new();
        for entry in entries {
            if text(field(entry, "server")?)? == alias {
                tools.push(text(field(field(entry, "descriptor")?, "name")?)?.to_owned());
            }
        }
        tools.sort();
        tools.dedup();
        if let Ok(Value::Array(selected)) = field(host.value(), "selected_tools") {
            let selected = selected
                .iter()
                .map(|value| text(value).map(str::to_owned))
                .collect::<Result<Vec<_>>>()?;
            if selected.iter().any(|name| !tools.contains(name)) {
                return Err("host selected a tool absent from the actual catalog".into());
            }
            tools.retain(|name| selected.contains(name));
        }
        let (compiler, registry, content) = compiler(attempt_ms)?;
        let source = graph::source(alias, &tools, content, maximum)?;
        let catalogs = MCPCatalogInputs::from_json(catalog.as_bytes(), &Limits::default())?;
        std::fs::DirBuilder::new().create(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::write(directory.join("program.htlk"), &source)?;
        let graph = compiler.compile_source(&source, &catalogs)?;
        std::fs::write(directory.join("program.cbor"), &graph.executable_bytes)?;
        std::fs::write(directory.join("catalogs.json"), catalog.as_bytes())?;
        let native = Arc::new(NativeMcpAdapter::new(
            BTreeMap::from([(deployment.clone(), host::endpoint(host.value())?)]),
            30000,
        )?);
        let grant = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let store =
            SqliteStore::open(directory.join("runs.sqlite"), StoreConfig::default()).await?;
        let runtime = Arc::new(
            Runtime::new(
                store.clone(),
                Arc::new(registry),
                Arc::new(Policy {
                    deployment: deployment.clone(),
                    grant: grant.clone(),
                }),
                RuntimeConfig::default(),
            )?
            .with_mcp(Arc::new(Audited {
                native: native.clone(),
                calls: calls.clone(),
            })),
        );
        let owner = HostContext::new("live-session-owner".into(), &Limits::default())?;
        runtime
            .register_graph(&owner, &graph.executable_bytes, &[])
            .await?;
        let engine = ExecutionEngine::launch(
            runtime.clone(),
            ExecutionConfig {
                workers: 2,
                poll_interval: Duration::from_millis(5),
                ..Default::default()
            },
        )?;
        let run = runtime
            .start_run(
                &owner,
                graph.fingerprint,
                &BTreeMap::new(),
                "live-session",
                None,
            )
            .await?
            .run_id;
        model::write(
            &directory.join("session.json"),
            &object(vec![
                ("run_id", Value::Text(run.to_string())),
                ("fingerprint", Value::Text(graph.fingerprint.to_string())),
                ("deployment_id", Value::Text(deployment)),
                ("attempt_timeout_ms", Value::Integer(attempt_ms as i64)),
                (
                    "tools",
                    Value::Array(tools.iter().cloned().map(Value::Text).collect()),
                ),
            ])?,
        )?;
        let mut session = Self {
            runtime,
            store,
            native,
            engine: Some(engine),
            owner,
            run,
            wait: None,
            grant,
            calls,
            tools,
            directory: directory.into(),
            maximum,
            deadline_ms: attempt_ms + 30000,
            sequence: 0,
        };
        let wait = session.boundary(None).await?;
        session.wait = wait;
        let viewer = HostContext::new("live-session-viewer".into(), &Limits::default())?;
        if session
            .runtime
            .inspect_run(&viewer, &session.run)
            .await
            .is_ok()
        {
            return Err("unauthorized inspection unexpectedly succeeded".into());
        }
        Ok(session)
    }

    async fn boundary(&self, previous: Option<&WaitView>) -> Result<Option<WaitView>> {
        let deadline = Instant::now() + Duration::from_millis(self.deadline_ms);
        loop {
            let header = self.store.run_header(&self.run).await?;
            if header.status.is_terminal() {
                if header.status != RunStatus::Succeeded {
                    return Err(
                        "live graph terminated without success; private store retained".into(),
                    );
                }
                self.runtime.inspect_run(&self.owner, &self.run).await?;
                return Ok(None);
            }
            // Use the trusted host's bounded metadata index to locate a wait, then
            // authorize/inspect that exact node. Avoid repeatedly materializing the
            // entire growing run history just to discover the next input request.
            let mut cursor = None;
            loop {
                let records = self
                    .store
                    .records(&self.run, RecordFamily::Wait, cursor.as_deref(), 100)
                    .await?;
                if records.is_empty() {
                    break;
                }
                for record in records {
                    cursor = Some(record.key.id.clone());
                    if text(field(&record.payload, "state")?)? != "open"
                        || previous.is_some_and(|old| old.wait_id.to_string() == record.key.id)
                    {
                        continue;
                    }
                    let invocation = text(field(&record.payload, "invocation_id")?)?.parse()?;
                    match self
                        .runtime
                        .inspect_node(&self.owner, &self.run, invocation)
                        .await
                    {
                        Ok(node) => {
                            if let Some(wait) = node.waits.into_iter().find(|wait| {
                                wait.state == "open" && wait.wait_id.to_string() == record.key.id
                            }) {
                                return Ok(Some(wait));
                            }
                        }
                        Err(error) if error.code() == FailureCode::Fenced => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err("live step exceeded its finite deadline".into());
            }
            actix::clock::sleep(Duration::from_millis(100)).await;
        }
    }

    pub(super) async fn step(&mut self, command: &Value) -> Result<Value> {
        if self.sequence >= self.maximum {
            return Err("live call bound exhausted".into());
        }
        let tool = text(field(command, "tool")?)?;
        let index = self
            .tools
            .iter()
            .position(|name| name == tool)
            .ok_or("tool not in actual catalog")?;
        let arguments = field(command, "arguments")?.clone();
        let wait = self.wait.take().ok_or("session has no open command wait")?;
        *self.grant.lock().map_err(|_| "grant lock")? = Some((
            tool.into(),
            JsonDocument::from_value(&arguments, &Limits::default())?.digest(),
        ));
        self.sequence += 1;
        let reply = object(vec![
            ("tool", Value::Text(tool.into())),
            ("arguments", arguments),
            ("done", Value::Bool(false)),
        ])?;
        self.runtime
            .submit_reply(
                &self.owner,
                wait.wait_id,
                reply,
                &format!("command-{}", self.sequence),
            )
            .await?;
        let next = self.boundary(Some(&wait)).await?;
        let next = next.ok_or("live graph ended before next command")?;
        let artifact = next
            .request_artifact_id
            .ok_or("command request artifact missing")?;
        let result = self
            .runtime
            .read_artifact(&self.owner, artifact)
            .await?
            .value;
        let invocation = InvocationId::new(
            wait.scope_instance_id,
            &format!("call_{index}").parse()?,
            &Limits::default(),
        )?;
        let node = self
            .runtime
            .inspect_node(&self.owner, &self.run, invocation)
            .await?;
        if let Some(attempt) = node.attempts.last() {
            let evidence = self
                .runtime
                .inspect_mcp_evidence(
                    &self.owner,
                    &self.run,
                    invocation,
                    attempt.attempt_id,
                    12 * 1024 * 1024,
                )
                .await?;
            let metadata = object(vec![
                ("attempt_id", Value::Text(attempt.attempt_id.to_string())),
                ("invocation_id", Value::Text(invocation.to_string())),
                ("response_received", Value::Bool(evidence.response_received)),
                (
                    "effect_outcome_uncertain",
                    Value::Bool(evidence.effect_outcome_uncertain),
                ),
                (
                    "normalization",
                    evidence.recorded_normalization.unwrap_or(Value::Null),
                ),
            ])?;
            if let Some(raw) = evidence.raw_response {
                std::fs::write(
                    self.directory
                        .join(format!("raw-{:03}.json", self.sequence)),
                    raw,
                )?;
            }
            model::write(
                &self
                    .directory
                    .join(format!("evidence-{:03}.json", self.sequence)),
                &metadata,
            )?;
        }
        model::write(
            &self
                .directory
                .join(format!("response-{:03}.json", self.sequence)),
            &result,
        )?;
        let metadata = object(vec![
            ("event", Value::Text("result".into())),
            ("tool", Value::Text(tool.into())),
            ("status", field(&result, "status")?.clone()),
            (
                "failure_code",
                node.failure.as_ref().map_or(Value::Null, |failure| {
                    Value::Text(failure.code().to_string())
                }),
            ),
            ("sequence", Value::Integer(self.sequence as i64)),
            (
                "provider_calls",
                Value::Integer(self.calls.load(Ordering::SeqCst) as i64),
            ),
            ("result_artifact", Value::Text(artifact.to_string())),
        ])?;
        model::write(
            &self
                .directory
                .join(format!("step-{:03}.json", self.sequence)),
            &metadata,
        )?;
        *self.grant.lock().map_err(|_| "grant lock")? = None;
        self.wait = Some(next);
        Ok(metadata)
    }

    pub(super) async fn close(mut self, success: bool) -> Result<()> {
        let result = async {
            if success {
                let wait = self.wait.take().ok_or("final wait missing")?;
                self.runtime
                    .submit_reply(
                        &self.owner,
                        wait.wait_id,
                        object(vec![
                            ("done", Value::Bool(true)),
                            ("tool", Value::Text(String::new())),
                            ("arguments", object(vec![])?),
                        ])?,
                        "finish",
                    )
                    .await?;
                self.boundary(Some(&wait)).await?;
            } else {
                self.runtime
                    .cancel_run(&self.owner, &self.run, "live campaign aborted", "abort")
                    .await?;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        }
        .await;
        let stopped = self.engine.take().expect("engine present").shutdown().await;
        let native = self.native.shutdown().await;
        result?;
        stopped?;
        native?;
        Ok(())
    }
}

fn number(value: &Value, key: &str, default: u64) -> Result<u64> {
    match field(value, key) {
        Ok(Value::Integer(value)) if *value > 0 => Ok(*value as u64),
        Err(_) => Ok(default),
        _ => Err("positive configuration integer required".into()),
    }
}

pub(super) async fn inspect(directory: &Path) -> Result<Value> {
    if !directory.join("runs.sqlite").is_file() {
        return Err("saved store required".into());
    }
    let session = model::read(&directory.join("session.json"))?;
    let (_, registry, _) = compiler(number(session.value(), "attempt_timeout_ms", 180000)?)?;
    let runtime = Runtime::new(
        SqliteStore::open(directory.join("runs.sqlite"), StoreConfig::default()).await?,
        Arc::new(registry),
        Arc::new(Policy {
            deployment: text(field(session.value(), "deployment_id")?)?.into(),
            grant: Arc::new(Mutex::new(None)),
        }),
        RuntimeConfig::default(),
    )?;
    let owner = HostContext::new("live-session-owner".into(), &Limits::default())?;
    let run = RunId::new(
        text(field(session.value(), "run_id")?)?.into(),
        &Limits::default(),
    )?;
    let view = runtime.inspect_run(&owner, &run).await?;
    let mut count = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("step-") {
            let step = model::read(&entry.path())?;
            let artifact = text(field(step.value(), "result_artifact")?)?.parse()?;
            let result = runtime.read_artifact(&owner, artifact).await?.value;
            let name = entry
                .file_name()
                .to_string_lossy()
                .replace("step-", "response-");
            if model::read(&directory.join(name))?.value() != &result {
                return Err("saved response differs from durable result artifact".into());
            }
            count += 1;
        }
        if entry.file_name().to_string_lossy().starts_with("evidence-") {
            let saved = model::read(&entry.path())?;
            let invocation = text(field(saved.value(), "invocation_id")?)?.parse()?;
            let attempt = text(field(saved.value(), "attempt_id")?)?.parse()?;
            let evidence = runtime
                .inspect_mcp_evidence(&owner, &run, invocation, attempt, 12 * 1024 * 1024)
                .await?;
            if let Some(raw) = evidence.raw_response {
                let name = entry
                    .file_name()
                    .to_string_lossy()
                    .replace("evidence-", "raw-");
                if std::fs::read(directory.join(name))? != raw {
                    return Err("saved raw response differs from durable evidence".into());
                }
            }
        }
    }
    object(vec![
        ("run_id", Value::Text(run.to_string())),
        ("status", Value::Text(format!("{:?}", view.header.status))),
        ("verified_steps", Value::Integer(count)),
        ("provider_calls", Value::Integer(0)),
    ])
}
