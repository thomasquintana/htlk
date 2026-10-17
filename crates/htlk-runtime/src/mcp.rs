//! Bounded raw MCP wire adapters. JSON ingress is checked before an SDK/Serde map
//! could erase duplicate keys or unsafe mathematical numeric tokens.
use crate::{
    failure::{FailureCode, RuntimeFailure},
    identity::InvocationId,
    records::SecurityLabels,
};
use htlk_executable::{
    JsonDocument, McpBinding, McpBindingKind, McpTransport, ServerIdentity,
    cbor::{Limits, Map, Value},
};
use std::{collections::BTreeMap, future::Future, pin::Pin, process::Stdio, time::Duration};
/// An externally enforced, trusted-adapter-verified duplicate suppression contract.
#[derive(Clone, Debug, PartialEq)]
pub enum McpDeduplication {
    /// Endpoint enforces the invocation-stable protocol/header key.
    Protocol,
    /// Endpoint enforces this already-authored argument, whose scope and uniqueness
    /// the trusted adapter has verified. Runtime checks that its value is unchanged.
    Argument {
        /// Exact argument member name.
        name: String,
        /// Exact frozen value; never inserted into graph inputs by the runtime.
        value: Value,
    },
}
use tokio::io::{AsyncReadExt, AsyncWriteExt};
/// Sendable bounded external operation future.
pub type McpFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RuntimeFailure>> + Send + 'a>>;
/// A pinned protocol request; frozen authored arguments are never mutated.
#[derive(Clone)]
pub struct McpRequest {
    /// Exact MCP method.
    pub method: String,
    /// Exact protocol parameters, separately audited.
    pub params: Value,
    /// Finite total attempt timeout.
    pub timeout_ms: u64,
    /// Raw response byte ceiling before parsing.
    pub max_response_bytes: usize,
    /// Verified invocation-stable protocol key if supported.
    pub idempotency_key: Option<String>,
}
/// Trusted adapter contract. Descriptors and raw replies remain independently
/// checked by runtime; adapters cannot claim replay safety from tool annotations.
pub trait McpAdapter: Send + Sync {
    /// Verifies actual endpoint deduplication, including authored-key scope and
    /// uniqueness. The default grants none; MCP annotations do not grant it.
    fn deduplication(
        &self,
        _binding: &McpBinding,
        _invocation: InvocationId,
        _params: &Value,
        _limits: &Limits,
    ) -> Result<Option<McpDeduplication>, RuntimeFailure> {
        Ok(None)
    }
    /// Retrieves the selected exact advertised descriptor under trusted deployment.
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument>;
    /// Sends one pinned request; returns raw result-object JSON before acceptance.
    fn invoke<'a>(&'a self, binding: &'a McpBinding, request: McpRequest)
    -> McpFuture<'a, Vec<u8>>;
}
/// Trusted replay/approval/result-protection facts, separate from executable pins.
#[derive(Clone, Debug, Default)]
pub struct McpPolicy {
    /// Host vouches replay safe for this compound operation.
    pub replay_safe: bool,
    /// Trusted external-result protection.
    pub result_labels: SecurityLabels,
    /// Exact-action approval authority, if required.
    pub approval_authority: Option<String>,
    /// Deployment security policy version.
    pub policy_version: String,
}
/// Host connection configuration, never part of executable semantic identity.
#[derive(Clone)]
pub enum McpEndpoint {
    /// Launched local server; environment is explicit trusted host configuration.
    Stdio {
        /// Program to launch.
        program: String,
        /// Exact arguments.
        arguments: Vec<String>,
        /// Additional explicit environment variables.
        environment: BTreeMap<String, String>,
    },
    /// Streamable HTTP server with operator-supplied authentication headers.
    Http {
        /// Trusted deployment URL.
        url: String,
        /// Secrets/configuration supplied only by the host.
        headers: BTreeMap<String, String>,
    },
}
/// Native bounded stdio/Streamable-HTTP implementation. Each operation negotiates
/// a fresh finite session and rechecks the exact descriptor before invoking it.
pub struct NativeMcpAdapter {
    endpoints: BTreeMap<String, McpEndpoint>,
    timeout_ms: u64,
    idempotent_bindings: std::collections::BTreeSet<htlk_executable::digest::Digest>,
}
impl NativeMcpAdapter {
    /// Constructs trusted deployment routing with a finite discovery deadline.
    ///
    /// # Errors
    /// Returns zero/unsupported timeout configuration.
    pub fn new(
        endpoints: BTreeMap<String, McpEndpoint>,
        timeout_ms: u64,
    ) -> Result<Self, RuntimeFailure> {
        if timeout_ms == 0 {
            return Err(fail(FailureCode::Profile));
        }
        Ok(Self {
            endpoints,
            timeout_ms,
            idempotent_bindings: Default::default(),
        })
    }
    /// Configures exact compound operation digests whose HTTP endpoint is known by
    /// the operator to enforce `Idempotency-Key`. This is trusted deployment policy,
    /// never a promise inferred from tool annotations or graph input data.
    pub fn with_protocol_idempotency(
        mut self,
        bindings: std::collections::BTreeSet<htlk_executable::digest::Digest>,
    ) -> Self {
        self.idempotent_bindings = bindings;
        self
    }
    async fn session(&self, binding: &McpBinding, max: usize) -> Result<Session, RuntimeFailure> {
        let endpoint = self
            .endpoints
            .get(binding.server().deployment_id())
            .ok_or_else(|| fail(FailureCode::McpIdentity))?;
        let mut session = Session::new(endpoint, binding.server(), max).await?;
        session.initialize(binding.server()).await?;
        Ok(session)
    }
}
impl McpAdapter for NativeMcpAdapter {
    fn deduplication(
        &self,
        binding: &McpBinding,
        _: InvocationId,
        _: &Value,
        limits: &Limits,
    ) -> Result<Option<McpDeduplication>, RuntimeFailure> {
        if binding.server().transport() == McpTransport::StreamableHttp
            && self.idempotent_bindings.contains(
                &binding
                    .digest(limits)
                    .map_err(|_| fail(FailureCode::McpIdentity))?,
            )
        {
            Ok(Some(McpDeduplication::Protocol))
        } else {
            Ok(None)
        }
    }
    fn descriptor<'a>(
        &'a self,
        binding: &'a McpBinding,
        limits: &'a Limits,
    ) -> McpFuture<'a, JsonDocument> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_millis(self.timeout_ms), async {
                let mut session = self.session(binding, limits.max_document_bytes).await?;
                session.descriptor(binding).await
            })
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
        })
    }
    fn invoke<'a>(
        &'a self,
        binding: &'a McpBinding,
        request: McpRequest,
    ) -> McpFuture<'a, Vec<u8>> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_millis(request.timeout_ms), async {
                let mut session = self.session(binding, request.max_response_bytes).await?;
                let descriptor = session.descriptor(binding).await?;
                if descriptor.digest() != binding.descriptor() {
                    return Err(fail(FailureCode::McpDescriptor));
                }
                let result = session
                    .rpc(
                        &request.method,
                        request.params,
                        request.idempotency_key.as_deref(),
                    )
                    .await?;
                Ok(JsonDocument::from_value(&result, &session.limits)
                    .map_err(|_| fail(FailureCode::McpProtocol))?
                    .as_bytes()
                    .to_vec())
            })
            .await
            .map_err(|_| fail(FailureCode::McpTimeout))?
        })
    }
}
enum Transport {
    Stdio {
        child: tokio::process::Child,
        input: tokio::process::ChildStdin,
        output: tokio::process::ChildStdout,
    },
    Http {
        client: reqwest::Client,
        url: String,
        headers: BTreeMap<String, String>,
        session: Option<String>,
    },
}
struct Session {
    transport: Transport,
    next: i64,
    limits: Limits,
}
impl Session {
    async fn new(
        endpoint: &McpEndpoint,
        server: &ServerIdentity,
        max: usize,
    ) -> Result<Self, RuntimeFailure> {
        let transport = match endpoint {
            McpEndpoint::Stdio {
                program,
                arguments,
                environment,
            } if server.transport() == McpTransport::Stdio => {
                let mut child = tokio::process::Command::new(program)
                    .args(arguments)
                    .envs(environment)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                let input = child
                    .stdin
                    .take()
                    .ok_or_else(|| fail(FailureCode::McpTransport))?;
                let output = child
                    .stdout
                    .take()
                    .ok_or_else(|| fail(FailureCode::McpTransport))?;
                Transport::Stdio {
                    child,
                    input,
                    output,
                }
            }
            McpEndpoint::Http { url, headers }
                if server.transport() == McpTransport::StreamableHttp =>
            {
                Transport::Http {
                    client: reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .map_err(|_| fail(FailureCode::McpTransport))?,
                    url: url.clone(),
                    headers: headers.clone(),
                    session: None,
                }
            }
            _ => return Err(fail(FailureCode::McpIdentity)),
        };
        Ok(Self {
            transport,
            next: 0,
            limits: Limits {
                max_document_bytes: max,
                ..Default::default()
            },
        })
    }
    async fn initialize(&mut self, server: &ServerIdentity) -> Result<(), RuntimeFailure> {
        let params = obj([
            ("protocolVersion", Value::Text("2025-11-25".into())),
            ("capabilities", obj([])?),
            (
                "clientInfo",
                obj([
                    ("name", Value::Text("htlk".into())),
                    ("version", Value::Text("0.1".into())),
                ])?,
            ),
        ])?;
        let result = self.rpc("initialize", params, None).await?;
        let map = as_map(&result)?;
        if as_text(get(map, "protocolVersion")?)? != "2025-11-25" {
            return Err(fail(FailureCode::McpIdentity));
        }
        let info = as_map(get(map, "serverInfo")?)?;
        if as_text(get(info, "name")?)? != server.implementation_name()
            || as_text(get(info, "version")?)? != server.implementation_version()
        {
            return Err(fail(FailureCode::McpIdentity));
        }
        self.notify("notifications/initialized").await
    }
    async fn descriptor(&mut self, binding: &McpBinding) -> Result<JsonDocument, RuntimeFailure> {
        let (method, collection, key, name) = match binding.kind() {
            McpBindingKind::Tool { name, .. } => ("tools/list", "tools", "name", name),
            McpBindingKind::Resource { uri } => ("resources/list", "resources", "uri", uri),
            McpBindingKind::Template { uri_template } => (
                "resources/templates/list",
                "resourceTemplates",
                "uriTemplate",
                uri_template,
            ),
            McpBindingKind::Prompt { name } => ("prompts/list", "prompts", "name", name),
        };
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1024 {
            let params = if let Some(cursor) = &cursor {
                obj([("cursor", Value::Text(cursor.clone()))])?
            } else {
                obj([])?
            };
            let result = self.rpc(method, params, None).await?;
            let fields = as_map(&result)?;
            let Value::Array(entries) = get(fields, collection)? else {
                return Err(fail(FailureCode::McpProtocol));
            };
            for descriptor in entries {
                if as_map(descriptor)?
                    .get(key)
                    .is_some_and(|v| matches!(v,Value::Text(s)if s==name))
                {
                    return JsonDocument::from_value(descriptor, &self.limits)
                        .map_err(|_| fail(FailureCode::McpDescriptor));
                }
            }
            cursor = match fields.get("nextCursor") {
                None => return Err(fail(FailureCode::McpDescriptor)),
                Some(Value::Text(cursor)) if seen.insert(cursor.clone()) => Some(cursor.clone()),
                _ => return Err(fail(FailureCode::McpProtocol)),
            };
        }
        Err(fail(FailureCode::McpProtocol))
    }
    async fn notify(&mut self, method: &str) -> Result<(), RuntimeFailure> {
        let message = obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("method", Value::Text(method.into())),
        ])?;
        self.exchange(message, None, None).await.map(|_| ())
    }
    async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        key: Option<&str>,
    ) -> Result<Value, RuntimeFailure> {
        self.next += 1;
        let id = self.next;
        let message = obj([
            ("jsonrpc", Value::Text("2.0".into())),
            ("id", Value::Integer(id)),
            ("method", Value::Text(method.into())),
            ("params", params),
        ])?;
        let reply = self
            .exchange(message, Some(id), key)
            .await?
            .ok_or_else(|| fail(FailureCode::McpProtocol))?;
        let fields = as_map(&reply)?;
        if fields.get("jsonrpc") != Some(&Value::Text("2.0".into()))
            || fields.get("id") != Some(&Value::Integer(id))
        {
            return Err(fail(FailureCode::McpProtocol));
        }
        if fields.get("error").is_some() {
            return Err(fail(FailureCode::McpProtocol));
        }
        Ok(get(fields, "result")?.clone())
    }
    async fn exchange(
        &mut self,
        message: Value,
        id: Option<i64>,
        key: Option<&str>,
    ) -> Result<Option<Value>, RuntimeFailure> {
        let bytes = JsonDocument::from_value(&message, &self.limits)
            .map_err(|_| fail(FailureCode::Input))?
            .as_bytes()
            .to_vec();
        match &mut self.transport {
            Transport::Stdio {
                child,
                input,
                output,
            } => {
                let _ = child.id();
                input
                    .write_all(&bytes)
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                input
                    .write_all(b"\n")
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                input
                    .flush()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                if id.is_none() {
                    return Ok(None);
                }
                for _ in 0..1024 {
                    let mut line = Vec::new();
                    loop {
                        let mut byte = [0];
                        if output
                            .read(&mut byte)
                            .await
                            .map_err(|_| fail(FailureCode::McpTransport))?
                            == 0
                        {
                            return Err(fail(FailureCode::McpTransport));
                        }
                        if byte[0] == b'\n' {
                            break;
                        }
                        if line.len() >= self.limits.max_document_bytes {
                            return Err(fail(FailureCode::McpProtocol));
                        }
                        line.push(byte[0]);
                    }
                    let document = JsonDocument::new(&line, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?;
                    let fields = as_map(document.value())?;
                    if fields.get("id") == id.map(Value::Integer).as_ref() {
                        return Ok(Some(document.value().clone()));
                    }
                }
                Err(fail(FailureCode::McpProtocol))
            }
            Transport::Http {
                client,
                url,
                headers,
                session,
            } => {
                let mut request = client
                    .post(url.as_str())
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .header("mcp-protocol-version", "2025-11-25")
                    .body(bytes);
                for (k, v) in headers.iter() {
                    request = request.header(k, v);
                }
                if let Some(session) = session.as_ref() {
                    request = request.header("mcp-session-id", session)
                }
                if let Some(key) = key {
                    request = request.header("idempotency-key", key)
                }
                let mut response = request
                    .send()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?;
                if !response.status().is_success() {
                    return Err(fail(FailureCode::McpTransport));
                }
                if let Some(value) = response.headers().get("mcp-session-id") {
                    *session = Some(
                        value
                            .to_str()
                            .map_err(|_| fail(FailureCode::McpProtocol))?
                            .into(),
                    )
                }
                if id.is_none() {
                    return Ok(None);
                }
                let sse = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| s.starts_with("text/event-stream"));
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| fail(FailureCode::McpTransport))?
                {
                    if body
                        .len()
                        .checked_add(chunk.len())
                        .is_none_or(|n| n > self.limits.max_document_bytes)
                    {
                        return Err(fail(FailureCode::McpProtocol));
                    }
                    body.extend_from_slice(&chunk);
                    if sse && let Some(value) = sse_reply(&body, id, &self.limits)? {
                        return Ok(Some(value));
                    }
                }
                if sse {
                    return Err(fail(FailureCode::McpProtocol));
                }
                Ok(Some(
                    JsonDocument::new(&body, &self.limits)
                        .map_err(|_| fail(FailureCode::McpProtocol))?
                        .value()
                        .clone(),
                ))
            }
        }
    }
}
fn sse_reply(
    bytes: &[u8],
    id: Option<i64>,
    limits: &Limits,
) -> Result<Option<Value>, RuntimeFailure> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => return Ok(None),
        Err(_) => return Err(fail(FailureCode::McpProtocol)),
    };
    let normalized = text.replace("\r\n", "\n");
    for event in normalized
        .split("\n\n")
        .take(normalized.matches("\n\n").count())
    {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let document = JsonDocument::new(data.as_bytes(), limits)
            .map_err(|_| fail(FailureCode::McpProtocol))?;
        if as_map(document.value())?.get("id") == id.map(Value::Integer).as_ref() {
            return Ok(Some(document.value().clone()));
        }
    }
    Ok(None)
}
fn obj<const N: usize>(entries: [(&str, Value); N]) -> Result<Value, RuntimeFailure> {
    Ok(Value::Map(
        Map::try_from_entries(entries.into_iter().map(|(k, v)| (k.into(), v)))
            .map_err(|_| fail(FailureCode::McpProtocol))?,
    ))
}
fn as_map(value: &Value) -> Result<&Map, RuntimeFailure> {
    if let Value::Map(m) = value {
        Ok(m)
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn get<'a>(m: &'a Map, key: &str) -> Result<&'a Value, RuntimeFailure> {
    m.get(key).ok_or_else(|| fail(FailureCode::McpProtocol))
}
fn as_text(value: &Value) -> Result<&str, RuntimeFailure> {
    if let Value::Text(s) = value {
        Ok(s)
    } else {
        Err(fail(FailureCode::McpProtocol))
    }
}
fn fail(code: FailureCode) -> RuntimeFailure {
    RuntimeFailure::new(code)
}
