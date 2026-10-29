//! Explicit live Chrome campaign using actual catalogs and ordinary authored graphs.

use htlk::{
    compiler::*,
    runtime::{
        Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
        execution::{ExecutionConfig, ExecutionEngine, McpInferenceSelection, RunView},
        failure::{FailureCode, RuntimeFailure},
        link_mcp_content_library,
        mcp::{
            McpAdapter, McpAttemptFuture, McpConnection, McpEndpoint, McpFuture, McpPolicy,
            McpRequest, McpRunContext, NativeMcpAdapter,
        },
        records::{InputValue, RunStatus, SecurityLabels},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    cbor::{Limits, Map, Value},
    digest::Digest,
    *,
};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const ONE: &str = "() => ({marker: document.getElementById('marker').textContent, price: 1})";
const TWO: &str = "() => ({marker: document.getElementById('marker').textContent, price: 2})";
const THREE: &str = "() => ({marker: document.getElementById('marker').textContent, price: 3})";
const DRIFT: &str = "() => ({marker: document.getElementById('marker').textContent, price: 'unknown', currency: 'USD'})";
type Grants = Arc<Mutex<BTreeMap<(Digest, Digest), ToolResultPolicy>>>;

struct Policy {
    url: String,
    deployment: String,
    grants: Grants,
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        caller: &HostContext,
        action: Authorization<'_>,
    ) -> std::result::Result<(), RuntimeFailure> {
        let denied = || RuntimeFailure::new(FailureCode::Policy);
        if caller.principal() != "live-acceptance" {
            return Err(denied());
        }
        if let Authorization::DispatchMcp {
            binding,
            parameters,
            ..
        } = action
        {
            if binding.server().deployment_id() != self.deployment {
                return Err(denied());
            }
            let McpBindingKind::Tool { name, .. } = binding.kind() else {
                return Err(denied());
            };
            let Value::Map(parameters) = parameters else {
                return Err(denied());
            };
            let Some(Value::Map(arguments)) = parameters.get("arguments") else {
                return Err(denied());
            };
            let permitted = match name.as_str() {
                "new_page" => arguments.get("url") == Some(&Value::Text(self.url.clone())),
                "evaluate_script" => {
                    matches!(arguments.get("function"), Some(Value::Text(script)) if [ONE,TWO,THREE,DRIFT].contains(&script.as_str()))
                }
                "close_page" => {
                    matches!(arguments.get("pageId"), Some(Value::Integer(id)) if (0..4096).contains(id) || *id == 99_999_999)
                }
                "take_snapshot" | "list_pages" => arguments.is_empty(),
                "take_screenshot" => {
                    arguments.get("format") == Some(&Value::Text("jpeg".into()))
                        && arguments.get("filePath").is_none()
                }
                _ => false,
            };
            if !permitted {
                return Err(denied());
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
    ) -> std::result::Result<SecurityLabels, RuntimeFailure> {
        Ok(
            SecurityLabels::new(vec!["live-evidence".into()], &Limits::default())
                .expect("fixed label"),
        )
    }
    fn mcp_policy(
        &self,
        _: &HostContext,
        binding: &McpBinding,
    ) -> std::result::Result<McpPolicy, RuntimeFailure> {
        let key = (
            binding
                .server()
                .digest(&Limits::default())
                .map_err(|_| RuntimeFailure::new(FailureCode::Policy))?,
            binding.descriptor(),
        );
        Ok(McpPolicy {
            result_policy: self
                .grants
                .lock()
                .map_err(|_| RuntimeFailure::new(FailureCode::Policy))?
                .get(&key)
                .cloned()
                .unwrap_or_default(),
            result_labels: SecurityLabels::new(vec!["live-evidence".into()], &Limits::default())
                .expect("fixed label"),
            policy_version: "live-campaign/1".into(),
            ..Default::default()
        })
    }
}
struct Audited {
    native: Arc<NativeMcpAdapter>,
    calls: Arc<AtomicUsize>,
}
struct Connection<'a> {
    inner: Box<dyn McpConnection + 'a>,
    calls: Arc<AtomicUsize>,
}
impl McpConnection for Connection<'_> {
    fn descriptor(&self) -> &JsonDocument {
        self.inner.descriptor()
    }
    fn invoke<'a>(&'a mut self, request: McpRequest) -> McpAttemptFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.invoke(request)
    }
    fn close(&mut self) -> McpFuture<'_, ()> {
        self.inner.close()
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
            let inner = self.native.connect(binding, context, limits).await?;
            Ok(Box::new(Connection {
                inner,
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
fn object(fields: Vec<(&str, Value)>) -> Result<Value> {
    Ok(Value::Map(Map::try_from_entries(
        fields.into_iter().map(|(key, value)| (key.into(), value)),
    )?))
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    let Value::Map(value) = value else {
        return Err("object required".into());
    };
    value.get(key).ok_or_else(|| "field absent".into())
}
fn text(value: &Value) -> Result<&str> {
    if let Value::Text(value) = value {
        Ok(value)
    } else {
        Err("text required".into())
    }
}
fn quoted(value: &str) -> String {
    String::from_utf8(
        JsonDocument::from_value(&Value::Text(value.into()), &Limits::default())
            .expect("bounded authored text")
            .as_bytes()
            .to_vec(),
    )
    .expect("UTF-8")
}
fn write_json(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    let document = JsonDocument::from_value(value, &Limits::default())?;
    std::fs::write(path, document.as_bytes())?;
    Ok(())
}
fn render(template: &str, content: Digest) -> String {
    template
        .replace("__CONTENT_DIGEST__", &content.to_string())
        .replace("__ONE_SCRIPT__", &quoted(ONE))
        .replace("__TWO_SCRIPT__", &quoted(TWO))
        .replace("__THREE_SCRIPT__", &quoted(THREE))
        .replace("__DRIFT_SCRIPT__", &quoted(DRIFT))
}
fn compiler(limits: &Limits) -> Result<(CompilerConfig, NativeRegistry, Digest)> {
    let policy = PolicyDocument::new(
        PolicyFields {
            cost_unit: "live-test-calls".into(),
            defaults: ExecutionLimits::new()
                .with_timeout_ms(60000)?
                .with_attempt_timeout_ms(15000)?
                .with_max_concurrency(1)?,
            evaluator_limits: EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 2 * 1024 * 1024,
                max_collection_visits: 100000,
                max_regex_bytes: 4096,
                max_regex_compiled_bytes: 65536,
                max_output_bytes: 2 * 1024 * 1024,
                max_steps: 50_000_000,
            },
            maximum_scope_depth: 16,
            maximum_expanded_nodes: 1000,
        },
        limits,
    )?;
    let mut registry = NativeRegistry::new(limits)?;
    let profile = registry.link_policy(&policy)?;
    let content = link_mcp_content_library(&mut registry, limits)?;
    let identity = content.implementation_digest();
    Ok((
        CompilerConfig {
            profile,
            policy,
            libraries: BTreeMap::from([(identity, content)]),
            mcp_result_policies: BTreeMap::new(),
            mcp_result_documents: BTreeMap::new(),
            limits: limits.clone(),
            schema_options: analyzer::NativeSchemaOptions::default(),
            frontend: FrontendLimits::default(),
            capabilities: CompilerCapabilities::default(),
        },
        registry,
        identity,
    ))
}
struct Campaign {
    runtime: Arc<Runtime>,
    config: CompilerConfig,
    catalogs: MCPCatalogInputs,
    caller: HostContext,
    marker: String,
    url: String,
    directory: PathBuf,
    grants: Grants,
    calls: Arc<AtomicUsize>,
    content: Digest,
}
impl Campaign {
    async fn execute(
        &self,
        template: &str,
        key: &str,
        policy: Option<((Digest, Digest), ToolResultPolicy)>,
        documents: &[JsonDocument],
    ) -> Result<RunView> {
        let mut config = self.config.clone();
        self.grants.lock().map_err(|_| "grant lock")?.clear();
        if let Some((selection, policy)) = policy {
            config.mcp_result_policies.insert(selection, policy.clone());
            self.grants
                .lock()
                .map_err(|_| "grant lock")?
                .insert(selection, policy);
        }
        for document in documents {
            config
                .mcp_result_documents
                .insert(document.digest(), document.clone());
        }
        let source = render(template, self.content);
        std::fs::write(self.directory.join(format!("{key}.htlk")), &source)?;
        let graph = Compiler::new(config)?.compile_source(&source, &self.catalogs)?;
        self.runtime
            .register_graph(&self.caller, &graph.executable_bytes, &[])
            .await?;
        let started = self
            .runtime
            .start_run(
                &self.caller,
                graph.fingerprint,
                &BTreeMap::from([
                    (
                        "url".parse()?,
                        InputValue::Inline(Value::Text(self.url.clone())),
                    ),
                    (
                        "marker".parse()?,
                        InputValue::Inline(Value::Text(self.marker.clone())),
                    ),
                ]),
                key,
                None,
            )
            .await?;
        let view = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                let view = self
                    .runtime
                    .inspect_run(&self.caller, &started.run_id)
                    .await?;
                if view.header.status.is_terminal() {
                    break Ok::<_, htlk::runtime::RuntimeError>(view);
                }
                actix::clock::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        write_json(
            self.directory.join(format!("{key}.run.json")),
            &object(vec![
                ("run_id", Value::Text(view.header.run_id.to_string())),
                ("fingerprint", Value::Text(graph.fingerprint.to_string())),
                ("status", Value::Text(format!("{:?}", view.header.status))),
                (
                    "calls_so_far",
                    Value::Integer(self.calls.load(Ordering::SeqCst) as i64),
                ),
                ("outputs", view.outputs.clone().unwrap_or(Value::Null)),
            ])?,
        )?;
        if view.header.status != RunStatus::Succeeded {
            return Err(format!("live phase {key} failed: {:?}", view.failure).into());
        }
        if self.output(&view, "cleanup").await? != Value::Bool(true) {
            return Err("test-owned page cleanup not verified".into());
        }
        Ok(view)
    }
    async fn output(&self, view: &RunView, name: &str) -> Result<Value> {
        let id = text(field(
            field(view.outputs.as_ref().ok_or("outputs absent")?, name)?,
            "artifact_id",
        )?)?
        .parse()?;
        Ok(self.runtime.read_artifact(&self.caller, id).await?.value)
    }
    fn selection(&self, view: &RunView, name: &str) -> Result<McpInferenceSelection> {
        Ok(McpInferenceSelection {
            run_id: view.header.run_id.clone(),
            invocation_id: htlk::runtime::identity::InvocationId::new(
                view.header.root_scope,
                &name.parse()?,
                &self.config.limits,
            )?,
        })
    }
    async fn run(&self) -> Result<Value> {
        let basic = self
            .execute(include_str!("basic.htlk"), "basic", None, &[])
            .await?;
        if self.output(&basic, "verified").await? != Value::Bool(true) {
            return Err("controlled marker/image verification failed".into());
        }
        let executable = self
            .runtime
            .executable(&self.caller, basic.header.fingerprint)
            .await?;
        let binding = executable.document().fields().bindings.values().find(|binding| matches!(binding.kind(), McpBindingKind::Tool { name, .. } if name == "evaluate_script")).ok_or("evaluate binding absent")?;
        let key = (
            binding.server().digest(&self.config.limits)?,
            binding.descriptor(),
        );
        let mut selections = vec![
            self.selection(&basic, "one")?,
            self.selection(&basic, "two")?,
            self.selection(&basic, "three")?,
        ];
        let before = self.calls.load(Ordering::SeqCst);
        let first = self
            .runtime
            .infer_mcp_results(
                &self.caller,
                &selections,
                &analyzer::InferencePolicy::default(),
            )
            .await?;
        if self.calls.load(Ordering::SeqCst) != before {
            return Err("inference dispatched an extra operation".into());
        }
        write_json(
            self.directory.join("revision-one.schema.json"),
            first.revision().schema().value(),
        )?;
        write_json(
            self.directory.join("revision-one.evidence.json"),
            first.revision().evidence().value(),
        )?;
        let documents = [
            first.revision().schema().clone(),
            first.revision().evidence().clone(),
        ];
        let observe = ToolResultPolicy {
            inferred_schema: Some(first.revision().adoption(InferredSchemaPolicy::Observe)),
            ..Default::default()
        };
        let observed = self
            .execute(
                include_str!("revision.htlk"),
                "observe-drift",
                Some((key, observe)),
                &documents,
            )
            .await?;
        let result = self.output(&observed, "result").await?;
        if text(field(
            field(field(&result, "validation")?, "inferred_schema")?,
            "status",
        )?)? != "invalid"
        {
            return Err("expected Observe mismatch absent".into());
        }
        if field(field(&result, "data")?, "marker")? != &Value::Text(self.marker.clone()) {
            return Err("drift marker absent".into());
        }
        let enforced_policy = ToolResultPolicy {
            inferred_schema: Some(first.revision().adoption(InferredSchemaPolicy::Enforce)),
            ..Default::default()
        };
        let enforced = self
            .execute(
                include_str!("revision.htlk"),
                "enforce-drift",
                Some((key, enforced_policy)),
                &documents,
            )
            .await?;
        if self.output(&enforced, "failure").await? != Value::Text("MCP_INFERRED_SCHEMA".into()) {
            return Err("required inferred mismatch not enforced".into());
        }
        selections.push(self.selection(&observed, "drift")?);
        let before = self.calls.load(Ordering::SeqCst);
        let second = self
            .runtime
            .infer_mcp_results(
                &self.caller,
                &selections,
                &analyzer::InferencePolicy::default(),
            )
            .await?;
        if self.calls.load(Ordering::SeqCst) != before {
            return Err("schema evolution repeated an effect".into());
        }
        write_json(
            self.directory.join("revision-two.schema.json"),
            second.revision().schema().value(),
        )?;
        write_json(
            self.directory.join("revision-two.evidence.json"),
            second.revision().evidence().value(),
        )?;
        write_json(
            self.directory.join("schema-difference.json"),
            first
                .revision()
                .difference(second.revision(), &Default::default(), &self.config.limits)?
                .value(),
        )?;
        let revised = ToolResultPolicy {
            inferred_schema: Some(second.revision().adoption(InferredSchemaPolicy::Enforce)),
            ..Default::default()
        };
        let adopted = self
            .execute(
                include_str!("revision.htlk"),
                "adopt-revision",
                Some((key, revised)),
                &[
                    second.revision().schema().clone(),
                    second.revision().evidence().clone(),
                ],
            )
            .await?;
        let result = self.output(&adopted, "result").await?;
        if text(field(
            field(field(&result, "validation")?, "inferred_schema")?,
            "status",
        )?)? != "valid"
        {
            return Err("adopted revision did not validate actual data".into());
        }
        let expected_error = self.selection(&basic, "expected_error")?;
        let node = self
            .runtime
            .inspect_node(
                &self.caller,
                &basic.header.run_id,
                expected_error.invocation_id,
            )
            .await?;
        let evidence = self
            .runtime
            .inspect_mcp_evidence(
                &self.caller,
                &basic.header.run_id,
                expected_error.invocation_id,
                node.attempts
                    .last()
                    .ok_or("error attempt absent")?
                    .attempt_id,
                65536,
            )
            .await?;
        if evidence.failure_code != Some(FailureCode::McpToolError)
            || !evidence.response_received
            || evidence.raw_response.is_none()
        {
            return Err("authorized live tool-error evidence absent".into());
        }
        if self.calls.load(Ordering::SeqCst) != 21 {
            return Err("unexpected live operation count".into());
        }
        object(vec![
            ("marker", Value::Text(self.marker.clone())),
            (
                "tool_calls",
                Value::Integer(self.calls.load(Ordering::SeqCst) as i64),
            ),
            ("verified", Value::Bool(true)),
            ("screenshots_preserved", Value::Bool(true)),
            ("tool_error_evidence", Value::Bool(true)),
            ("observe_mismatch", Value::Bool(true)),
            ("enforce_mismatch", Value::Bool(true)),
            ("explicit_revision_adoption", Value::Bool(true)),
            ("inference_extra_calls", Value::Integer(0)),
            ("test_owned_tabs_closed", Value::Bool(true)),
            (
                "revision_one",
                Value::Text(first.revision().schema().digest().to_string()),
            ),
            (
                "revision_two",
                Value::Text(second.revision().schema().digest().to_string()),
            ),
        ])
    }
}
async fn inspect_saved(directory: &Path) -> Result<()> {
    if !directory.join("runs.sqlite").is_file() {
        return Err("saved campaign store does not exist".into());
    }
    let limits = Limits::default();
    let (_, registry, _) = compiler(&limits)?;
    let runtime = Runtime::new(
        SqliteStore::open(directory.join("runs.sqlite"), StoreConfig::default()).await?,
        Arc::new(registry),
        Arc::new(Policy {
            url: String::new(),
            deployment: "chrome-local-pinned".into(),
            grants: Arc::new(Mutex::new(BTreeMap::new())),
        }),
        RuntimeConfig::default(),
    )?;
    let caller = HostContext::new("live-acceptance".into(), &limits)?;
    let basic = JsonDocument::new(&std::fs::read(directory.join("basic.run.json"))?, &limits)?;
    let mut summary = Vec::new();
    for port in ["sample_one", "sample_two", "sample_three", "screenshot"] {
        let id = text(field(
            field(field(basic.value(), "outputs")?, port)?,
            "artifact_id",
        )?)?
        .parse()?;
        let artifact = runtime.read_artifact(&caller, id).await?;
        let protocol = field(&artifact.value, "protocol_result")?;
        let mut kinds = Vec::new();
        let Value::Array(blocks) = field(protocol, "content")? else {
            return Err("content absent".into());
        };
        for block in blocks {
            kinds.push(field(block, "type")?.clone());
        }
        let extraction = field(&artifact.value, "extraction")?;
        let mut sources = Vec::new();
        let Value::Array(candidates) = field(extraction, "candidates")? else {
            return Err("candidates absent".into());
        };
        for candidate in candidates {
            sources.push(field(candidate, "sources")?.clone());
        }
        let Value::Map(protocol) = protocol else {
            return Err("protocol object absent".into());
        };
        summary.push(object(vec![
            ("port", Value::Text(port.into())),
            ("artifact_id", Value::Text(id.to_string())),
            ("content_kinds", Value::Array(kinds)),
            (
                "original_structured_content_present",
                Value::Bool(protocol.get("structuredContent").is_some()),
            ),
            ("extraction_status", field(extraction, "status")?.clone()),
            ("candidate_sources", Value::Array(sources)),
            ("validation", field(&artifact.value, "validation")?.clone()),
        ])?);
    }
    let run = htlk::runtime::identity::RunId::new(
        text(field(basic.value(), "run_id")?)?.to_owned(),
        &limits,
    )?;
    let header = runtime.inspect_run(&caller, &run).await?;
    let executable = runtime
        .executable(&caller, header.header.fingerprint)
        .await?;
    let mut bindings = Vec::new();
    for binding in executable.document().fields().bindings.values() {
        if let McpBindingKind::Tool {
            name,
            output_schema,
            result_policy,
            ..
        } = binding.kind()
        {
            bindings.push(object(vec![
                ("name", Value::Text(name.clone())),
                ("binding", Value::Text(binding.digest(&limits)?.to_string())),
                ("descriptor", Value::Text(binding.descriptor().to_string())),
                (
                    "server",
                    Value::Text(binding.server().digest(&limits)?.to_string()),
                ),
                (
                    "declared_schema_present",
                    Value::Bool(output_schema.is_some()),
                ),
                (
                    "result_policy",
                    Value::Text(digest::hash_bytes(&result_policy.encode(&limits)?).to_string()),
                ),
            ])?);
        }
    }
    let invocation = htlk::runtime::identity::InvocationId::new(
        header.header.root_scope,
        &"expected_error".parse()?,
        &limits,
    )?;
    let node = runtime.inspect_node(&caller, &run, invocation).await?;
    let error = runtime
        .inspect_mcp_evidence(
            &caller,
            &run,
            invocation,
            node.attempts.last().ok_or("attempt absent")?.attempt_id,
            65536,
        )
        .await?;
    let detail = object(vec![
        ("artifacts", Value::Array(summary)),
        ("bindings", Value::Array(bindings)),
        ("error_invocation", Value::Text(invocation.to_string())),
        (
            "error_attempt",
            Value::Text(
                node.attempts
                    .last()
                    .ok_or("attempt absent")?
                    .attempt_id
                    .to_string(),
            ),
        ),
        (
            "error_code",
            Value::Text(error.failure_code.ok_or("error absent")?.to_string()),
        ),
        (
            "error_response_received",
            Value::Bool(error.response_received),
        ),
        (
            "error_effect_uncertain",
            Value::Bool(error.effect_outcome_uncertain),
        ),
        (
            "error_raw_digest",
            Value::Text(error.raw_digest.ok_or("raw error absent")?.to_string()),
        ),
        ("provider_calls", Value::Integer(0)),
    ])?;
    write_json(directory.join("evidence-summary.json"), &detail)?;
    println!(
        "{}",
        std::str::from_utf8(JsonDocument::from_value(&detail, &limits)?.as_bytes())?
    );
    Ok(())
}
async fn run(arguments: &[String]) -> Result<()> {
    if arguments.len() == 3 && arguments[1] == "--inspect" {
        return inspect_saved(Path::new(&arguments[2])).await;
    }
    if arguments.len() != 4 {
        return Err(
            "Usage: cargo run --example mcp_live_acceptance -- HOST.json CATALOGS.json NEW_OUTPUT_DIR | --inspect OUTPUT_DIR"
                .into(),
        );
    }
    let limits = Limits::default();
    let host = JsonDocument::new(&std::fs::read(&arguments[1])?, &limits)?;
    let catalogs = MCPCatalogInputs::from_json(&std::fs::read(&arguments[2])?, &limits)?;
    let directory = PathBuf::from(&arguments[3]);
    std::fs::create_dir(&directory)?;
    let marker = format!(
        "HTLK_MCP_{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let html = format!(
        "<!doctype html><title>{marker}</title><h1 id='marker'>{marker}</h1><p>Controlled acceptance page</p>"
    );
    let url = "data:text/html;charset=utf-8,".to_owned()
        + &html
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect::<String>();
    let deployment = text(field(host.value(), "deployment_id")?)?.to_owned();
    let args = match field(host.value(), "arguments")? {
        Value::Array(args) => args
            .iter()
            .map(|value| text(value).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?,
        _ => return Err("arguments required".into()),
    };
    if !args.iter().any(|arg| arg == "--isolated") || !args.iter().any(|arg| arg == "--headless") {
        return Err("campaign requires a test-owned isolated headless browser".into());
    }
    let environment = match field(host.value(), "environment")? {
        Value::Map(env) => env
            .iter()
            .map(|(key, value)| Ok((key.to_owned(), text(value)?.to_owned())))
            .collect::<Result<BTreeMap<_, _>>>()?,
        _ => return Err("environment required".into()),
    };
    let native = Arc::new(NativeMcpAdapter::new(
        BTreeMap::from([(
            deployment.clone(),
            McpEndpoint::Stdio {
                program: text(field(host.value(), "program")?)?.into(),
                arguments: args,
                environment,
            },
        )]),
        15000,
    )?);
    let calls = Arc::new(AtomicUsize::new(0));
    let grants = Arc::new(Mutex::new(BTreeMap::new()));
    let (config, registry, content) = compiler(&limits)?;
    let runtime = Arc::new(
        Runtime::new(
            SqliteStore::open(directory.join("runs.sqlite"), StoreConfig::default()).await?,
            Arc::new(registry),
            Arc::new(Policy {
                url: url.clone(),
                deployment,
                grants: grants.clone(),
            }),
            RuntimeConfig::default(),
        )?
        .with_mcp(Arc::new(Audited {
            native: native.clone(),
            calls: calls.clone(),
        })),
    );
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            workers: 2,
            poll_interval: Duration::from_millis(5),
            ..Default::default()
        },
    )?;
    let campaign = Campaign {
        runtime,
        config,
        catalogs,
        caller: HostContext::new("live-acceptance".into(), &limits)?,
        marker,
        url,
        directory: directory.clone(),
        grants,
        calls,
        content,
    };
    let result = campaign.run().await;
    let engine_stopped = engine.shutdown().await;
    let adapter_stopped = native.shutdown().await;
    let report = result?;
    engine_stopped?;
    adapter_stopped?;
    write_json(directory.join("report.json"), &report)?;
    println!(
        "{}",
        std::str::from_utf8(JsonDocument::from_value(&report, &limits)?.as_bytes())?
    );
    Ok(())
}
fn main() -> Result<()> {
    actix::System::new().block_on(run(&std::env::args().collect::<Vec<_>>()))
}
