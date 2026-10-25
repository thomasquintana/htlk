//! Opt-in live capability runner. Tool/argument mappings live in source and catalogs.
use htlk::{
    compiler::*,
    runtime::{
        Authorization, HostContext, NativeRegistry, Runtime, RuntimeConfig, RuntimePolicy,
        execution::{ExecutionConfig, ExecutionEngine},
        failure::{FailureCode, RuntimeFailure},
        mcp::{McpEndpoint, NativeMcpAdapter},
        records::{InputValue, RunStatus, SecurityLabels},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    cbor::{Limits, Value},
    digest::Digest,
    *,
};
use std::{
    collections::BTreeMap,
    error::Error,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        _: Authorization<'_>,
    ) -> std::result::Result<(), RuntimeFailure> {
        if context.principal() == "capability-spike" {
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
        Ok(SecurityLabels::default())
    }
}
fn object(value: &Value) -> Result<&cbor::Map> {
    match value {
        Value::Map(map) => Ok(map),
        _ => Err("expected object".into()),
    }
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    object(value)?
        .get(key)
        .ok_or_else(|| format!("missing {key}").into())
}
fn text(value: &Value) -> Result<&str> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err("expected string".into()),
    }
}
fn strings(value: &Value) -> Result<BTreeMap<String, String>> {
    object(value)?
        .iter()
        .map(|(key, value)| Ok((key.into(), text(value)?.into())))
        .collect()
}
fn compiler(limits: &Limits) -> Result<(Compiler, NativeRegistry)> {
    let policy = PolicyDocument::new(
        PolicyFields {
            cost_unit: "credits".into(),
            defaults: ExecutionLimits::new()
                .with_timeout_ms(30000)?
                .with_attempt_timeout_ms(10000)?
                .with_max_concurrency(1)?,
            evaluator_limits: EvaluatorLimits {
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
        limits,
    )?;
    let mut registry = NativeRegistry::new(limits)?;
    let profile = registry.link_policy(&policy)?;
    Ok((
        Compiler::new(CompilerConfig {
            profile,
            policy,
            libraries: BTreeMap::new(),
            limits: limits.clone(),
            frontend: FrontendLimits::default(),
            schema_options: analyzer::NativeSchemaOptions::default(),
            capabilities: CompilerCapabilities::default(),
        })?,
        registry,
    ))
}
async fn run(config_path: &Path) -> Result<()> {
    let limits = Limits::default();
    let config = JsonDocument::new(&std::fs::read(config_path)?, &limits)?;
    let config = config.value();
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let source = std::fs::read_to_string(parent.join(text(field(config, "source")?)?))?;
    let catalogs = MCPCatalogInputs::from_json(
        &std::fs::read(parent.join(text(field(config, "catalogs")?)?))?,
        &limits,
    )?;
    let mut endpoints = BTreeMap::new();
    for (deployment, endpoint) in object(field(config, "endpoints")?)?.iter() {
        let endpoint = match text(field(endpoint, "transport")?)? {
            "stdio" => {
                let Value::Array(arguments) = field(endpoint, "arguments")? else {
                    return Err("expected arguments array".into());
                };
                McpEndpoint::Stdio {
                    program: text(field(endpoint, "program")?)?.into(),
                    arguments: arguments
                        .iter()
                        .map(|arg| text(arg).map(String::from))
                        .collect::<Result<_>>()?,
                    environment: strings(field(endpoint, "environment")?)?,
                }
            }
            "streamable_http" => McpEndpoint::Http {
                url: text(field(endpoint, "url")?)?.into(),
                headers: strings(field(endpoint, "headers")?)?,
            },
            _ => return Err("unsupported endpoint transport".into()),
        };
        endpoints.insert(deployment.into(), endpoint);
    }
    let inputs: BTreeMap<Identifier, InputValue> = object(field(config, "inputs")?)?
        .iter()
        .map(|(key, value)| Ok((key.parse()?, InputValue::Inline(value.clone()))))
        .collect::<Result<_>>()?;
    let (compiler, registry) = compiler(&limits)?;
    let graph = compiler
        .compile_source(&source, &catalogs)
        .map_err(|diagnostics| format!("compilation failed: {diagnostics:?}"))?;
    let adapter = Arc::new(NativeMcpAdapter::new(endpoints, 10000)?);
    let runtime = Arc::new(
        Runtime::new(
            SqliteStore::in_memory(StoreConfig::default()).await?,
            Arc::new(registry),
            Arc::new(Policy),
            RuntimeConfig::default(),
        )?
        .with_mcp(adapter.clone()),
    );
    let context = HostContext::new("capability-spike".into(), &limits)?;
    runtime
        .register_graph(&context, &graph.executable_bytes, &[])
        .await?;
    let engine = ExecutionEngine::launch(runtime.clone(), ExecutionConfig::default())?;
    let result = async {
        let run = runtime
            .start_run(
                &context,
                graph.fingerprint,
                &inputs,
                "capability-spike",
                None,
            )
            .await?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let view = loop {
            match runtime.inspect_run(&context, &run.run_id).await {
                Ok(view) if view.header.status.is_terminal() => break view,
                Ok(_) => {}
                Err(error) if error.code() == FailureCode::Fenced => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err("capability spike exceeded 60s".into());
            }
            actix::clock::sleep(Duration::from_millis(20)).await;
        };
        println!(
            "run={} status={:?} fingerprint={}",
            run.run_id, view.header.status, graph.fingerprint
        );
        if view.header.status != RunStatus::Succeeded {
            return Err(format!("capability sequence failed: {view:#?}").into());
        }
        for (name, entry) in object(view.outputs.as_ref().ok_or("missing outputs")?)?.iter() {
            let artifact = text(field(entry, "artifact_id")?)?.parse()?;
            let value = runtime.read_artifact(&context, artifact).await?.value;
            let value = JsonDocument::from_value(&value, &limits)?;
            println!("{name}={}", std::str::from_utf8(value.as_bytes())?);
        }
        Ok(())
    }
    .await;
    engine.shutdown().await?;
    adapter.shutdown().await?;
    result
}
fn main() -> Result<()> {
    let config = std::env::args_os()
        .nth(1)
        .ok_or("usage: cargo run --example mcp_capability_spike -- <host-config.json>")?;
    actix::System::new().block_on(run(Path::new(&config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_config_runs_the_complete_sequence_through_the_live_runner() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let directory =
            std::env::temp_dir().join(format!("htlk-live-spike-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let limits = Limits::default();
        let Value::Array(tools) = JsonDocument::new(include_bytes!("tools.json"), &limits)
            .unwrap()
            .value()
            .clone()
        else {
            panic!()
        };
        let entries = tools
            .iter()
            .map(|tool| {
                let tool = JsonDocument::from_value(tool, &limits).unwrap();
                format!(
                    r#"{{"server":"fixture","descriptor":{}}}"#,
                    std::str::from_utf8(tool.as_bytes()).unwrap()
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let common = r#""catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"fixture":{"deployment_id":"test","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{}"#;
        std::fs::write(directory.join("catalogs.json"), format!(r#"{{"tools":{{{common},"tools":[{entries}]}},"resources":{{{common},"resources":[],"templates":[]}},"prompts":{{{common},"prompts":[]}}}}"#)).unwrap();
        // Encode paths through the strict JSON boundary (including spaces/escapes).
        let quote = |path: &Path| {
            let json =
                JsonDocument::from_value(&Value::Text(path.to_str().unwrap().into()), &limits)
                    .unwrap();
            std::str::from_utf8(json.as_bytes()).unwrap().to_owned()
        };
        let config = format!(
            r#"{{"source":{},"catalogs":"catalogs.json","inputs":{{"marker":"runner-smoke"}},"endpoints":{{"test":{{"transport":"stdio","program":"node","arguments":[{},"stdio","stateful",{}],"environment":{{}}}}}}}}"#,
            quote(&root.join("examples/mcp_capability_spike/sandbox.htlk")),
            quote(&root.join("tests/support/stateful_mcp_peer.mjs")),
            quote(&directory.join("requests.jsonl"))
        );
        std::fs::write(directory.join("host.json"), config).unwrap();
        actix::System::new()
            .block_on(run(&directory.join("host.json")))
            .unwrap();
        let log = std::fs::read_to_string(directory.join("requests.jsonl")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("\"method\":\"initialize\""))
                .count(),
            1
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("\"method\":\"tools/call\""))
                .count(),
            4
        );
    }
}
