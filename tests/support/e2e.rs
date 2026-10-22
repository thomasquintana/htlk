//! Shared full-stack fixtures; every external service is an isolated local peer.
use execution::{ExecutionConfig, ExecutionEngine, RunView};
use failure::{FailureCode, RuntimeFailure};
use htlk::{compiler::*, runtime::*};
use htlk_executable::{cbor::*, digest::Digest, *};
use identity::RunId;
use mcp::{McpEndpoint, NativeMcpAdapter};
use records::{InputValue, SecurityLabels};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use store::{RecordFamily, ReplayState, SqliteStore, StoreConfig};

pub struct TempDir(PathBuf);
impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "htlk-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Peer {
    child: Option<Child>,
    pub endpoint: McpEndpoint,
    pub transport: &'static str,
    pub log: PathBuf,
    _directory: TempDir,
}
impl Peer {
    pub fn new(transport: &'static str, mode: &str) -> Self {
        let directory = TempDir::new();
        let log = directory.path().join("requests.jsonl");
        std::fs::write(&log, b"").unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/mcp_peer.mjs");
        let arguments = vec![
            script.to_str().unwrap().into(),
            transport.into(),
            mode.into(),
            log.to_str().unwrap().into(),
        ];
        let (child, endpoint) = if transport == "stdio" {
            (
                None,
                McpEndpoint::Stdio {
                    program: "node".into(),
                    arguments,
                    environment: BTreeMap::new(),
                },
            )
        } else {
            let mut child = Command::new("node")
                .args(&arguments)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("Node.js is required for native E2E peers");
            let stdout = child.stdout.take().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout).read_line(&mut line);
                let _ = tx.send(result.map(|_| line));
            });
            let url = match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(url)) if url.starts_with("http://127.0.0.1:") => url.trim().to_string(),
                other => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("peer startup failed: {other:?}");
                }
            };
            (
                Some(child),
                McpEndpoint::Http {
                    url,
                    headers: BTreeMap::new(),
                },
            )
        };
        Self {
            child,
            endpoint,
            transport: if transport == "stdio" {
                "stdio"
            } else {
                "streamable_http"
            },
            log,
            _directory: directory,
        }
    }
    pub fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|line| {
                JsonDocument::new(line.as_bytes(), &Limits::default())
                    .unwrap()
                    .value()
                    .clone()
            })
            .collect()
    }
    pub fn calls(&self) -> Vec<Value> {
        self.requests().into_iter().filter(|value| matches!(value, Value::Map(fields) if matches!(fields.get("method"), Some(Value::Text(method)) if ["tools/call", "resources/read", "prompts/get"].contains(&method.as_str())))).collect()
    }
    pub fn adapter(&self) -> Arc<NativeMcpAdapter> {
        Arc::new(
            NativeMcpAdapter::new(
                BTreeMap::from([("test".into(), self.endpoint.clone())]),
                5000,
            )
            .unwrap(),
        )
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn catalogs(transport: &str) -> MCPCatalogInputs {
    let server = format!(
        r#""catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{{"fixture":{{"deployment_id":"test","transport":"{transport}","implementation_name":"fixture","implementation_version":"1"}}}},"schema_documents":{{}}"#
    );
    MCPCatalogInputs::from_json(format!(r#"{{
      "tools":{{{server},"tools":[{{"server":"fixture","descriptor":{{"name":"echo","inputSchema":{{"type":"object","properties":{{"value":{{"type":["string","null"]}}}},"required":["value"],"additionalProperties":false}},"outputSchema":{{"type":"object","properties":{{"value":{{"type":["string","null"]}}}},"required":["value"],"additionalProperties":false}}}}}}]}},
      "resources":{{{server},"resources":[{{"server":"fixture","descriptor":{{"name":"fixed","uri":"context://fixed"}}}}],"templates":[{{"server":"fixture","descriptor":{{"name":"topic","uriTemplate":"context://topics/{{topic}}"}}}}]}},
      "prompts":{{{server},"prompts":[{{"server":"fixture","descriptor":{{"name":"review","arguments":[{{"name":"subject","required":true}}]}}}}]}}
    }}"#).as_bytes(), &Limits::default()).unwrap()
}

pub struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if context.principal() == "viewer"
            && matches!(
                action,
                Authorization::ReadArtifact { .. } | Authorization::InspectRun { .. }
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
        Ok(SecurityLabels::new(vec!["confidential".into()], &Limits::default()).unwrap())
    }
}
pub fn caller() -> HostContext {
    HostContext::new("alice".into(), &Limits::default()).unwrap()
}

pub async fn inspect(runtime: &Runtime, run: &RunId) -> RunView {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match runtime.inspect_run(&caller(), run).await {
            Ok(view) => return view,
            // Inspection deliberately rejects a snapshot when a coordinator
            // commits between its reads. Retry that exact transient category.
            Err(error) if error.code() == FailureCode::Fenced => {
                assert!(
                    Instant::now() < deadline,
                    "inspection never settled: {error}"
                );
            }
            Err(error) => panic!("run inspection failed: {error:?}"),
        }
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
}

pub async fn terminal(runtime: &Runtime, run: &RunId) -> RunView {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let view = inspect(runtime, run).await;
        if view.header.status.is_terminal() {
            return view;
        }
        assert!(Instant::now() < deadline, "execution stalled: {view:#?}");
        actix::clock::sleep(Duration::from_millis(2)).await;
    }
}

pub async fn output(runtime: &Runtime, view: &RunView, port: &str) -> Value {
    let Value::Map(outputs) = view.outputs.as_ref().expect("output manifest") else {
        panic!("output map")
    };
    let Some(Value::Map(entry)) = outputs.get(port) else {
        panic!("missing {port}: {outputs:?}")
    };
    let Some(Value::Text(id)) = entry.get("artifact_id") else {
        panic!("artifact identity")
    };
    runtime
        .read_artifact(&caller(), id.parse().unwrap())
        .await
        .unwrap()
        .value
}

pub async fn assert_replay(store: &SqliteStore, run: &RunId) {
    let mut sequence = 0;
    let mut state: Option<ReplayState> = None;
    loop {
        let page = store.journal(run, sequence, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        for entry in page {
            assert_eq!(entry.sequence, sequence + 1);
            sequence = entry.sequence;
            if let Some(state) = &mut state {
                state.apply(&entry, &Limits::default()).unwrap();
            } else {
                state = Some(ReplayState::begin(&entry, &Limits::default()).unwrap());
            }
        }
    }
    let state = state.unwrap();
    assert_eq!(state.header(), &store.run_header(run).await.unwrap());
    for family in [
        RecordFamily::Scope,
        RecordFamily::Invocation,
        RecordFamily::Guard,
        RecordFamily::Binding,
        RecordFamily::Artifact,
        RecordFamily::Attempt,
        RecordFamily::Response,
        RecordFamily::Wait,
        RecordFamily::Usage,
        RecordFamily::Reservation,
        RecordFamily::Manifest,
        RecordFamily::Reply,
        RecordFamily::Approval,
        RecordFamily::Cancellation,
        RecordFamily::Extension,
        RecordFamily::Loop,
        RecordFamily::Timer,
        RecordFamily::Observation,
        RecordFamily::Tombstone,
    ] {
        let mut cursor: Option<String> = None;
        loop {
            let page = store
                .records(run, family, cursor.as_deref(), 2)
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            for record in page {
                cursor = Some(record.key.id.clone());
                assert_eq!(state.record(&record.key), Some(&record), "{family:?}");
            }
        }
    }
}

pub struct Harness {
    pub runtime: Arc<Runtime>,
    pub store: SqliteStore,
    pub compiler: Compiler,
    pub engine: ExecutionEngine,
    _directory: TempDir,
}
impl Harness {
    pub async fn new(peer: Option<&Peer>, workers: usize) -> Self {
        Self::with_adapter(peer.map(Peer::adapter), workers).await
    }
    pub async fn with_adapter(adapter: Option<Arc<NativeMcpAdapter>>, workers: usize) -> Self {
        Self::with_policy(adapter, workers, Arc::new(Policy)).await
    }
    pub async fn with_policy(
        adapter: Option<Arc<NativeMcpAdapter>>,
        workers: usize,
        policy: Arc<dyn RuntimePolicy>,
    ) -> Self {
        let directory = TempDir::new();
        let (compiler, registry) = crate::support::native_compiler_with_timeouts(30000, 10000);
        let store = SqliteStore::open(
            directory.path().join("runtime.sqlite"),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let runtime = Runtime::new(
            store.clone(),
            Arc::new(registry),
            policy,
            RuntimeConfig::default(),
        )
        .unwrap();
        let runtime = Arc::new(if let Some(adapter) = adapter {
            runtime.with_mcp(adapter)
        } else {
            runtime
        });
        let engine = ExecutionEngine::launch(
            runtime.clone(),
            ExecutionConfig {
                workers,
                max_runs: 3,
                lease_ms: 1000,
                poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        Self {
            runtime,
            store,
            compiler,
            engine,
            _directory: directory,
        }
    }
    pub async fn execute(
        &self,
        graph: &CompilationSuccess,
        inputs: BTreeMap<Identifier, InputValue>,
        key: &str,
    ) -> RunView {
        self.runtime
            .register_graph(&caller(), &graph.executable_bytes, &[])
            .await
            .unwrap();
        let run = self
            .runtime
            .start_run(&caller(), graph.fingerprint, &inputs, key, None)
            .await
            .unwrap();
        terminal(&self.runtime, &run.run_id).await
    }
    pub async fn close(self) {
        self.engine.shutdown().await.unwrap();
        self.store.close().await.unwrap();
    }
}
