//! A host-owned MCP catalog browser and optional bounded resource monitor.
//!
//! Run `cargo run -p htlk-runtime --example mcp_client -- --help` for endpoint
//! arguments. The expected implementation name/version are explicit identity pins.
//! Discovery visits all four inventories but never calls tools, gets prompts,
//! reads resources, expands templates, or adopts compiler contracts.
//! `--connect-timeout-ms` sets the native client's startup/default timeout; raising
//! it leaves explicit catalog and subscription budgets at five seconds.
//!
//! # Host callback setup
//!
//! `Roots` returns only the operator-selected `--root` (or an empty roots array).
//! A root is advisory; supplying it performs no filesystem access. This immutable
//! provider does not advertise roots changes. A mutable host can also install
//! `RootsChanged` and explicitly call `session.roots_changed` after a real change.
//!
//! Form and URL services use `McpAwaitReply`. A separate bounded event consumer
//! inspects each addressed interaction and submits a digest-bound, idempotent
//! **decline**. This headless policy never supplies form content, accepts consent,
//! navigates URLs, or retries the operation that requested elicitation. Replace
//! that decision with authenticated presentation and explicit human input in an
//! interactive host; keep the same inspect/respond boundary.
//!
//! Sampling is intentionally unadvertised. To install a real model integration,
//! implement `McpClientHandler` and route the complete structured sampling request
//! under host policy. Use `context.work.as_ref()` and `McpHostWork::run` with a
//! protected routing/input descriptor, then `McpHostWorkReceipt::invoke` for one
//! actual native model RPC on the callback session. Map its genuine result to a
//! sampling response naming the actual selected model and measured usage; preserve
//! uncertain delivery on failure. An independent `Runtime::start_run` does not
//! inherit callback budgets. See `tests/mcp_sampling.rs` for an exercised native
//! operation mapper, recovery, and same-session ancestry.
//!
//! `--bearer-env` supplies an operator-managed static HTTP credential. It does not
//! perform OAuth or refresh. A host needing those services should install
//! `McpClientConfig::with_credential_provider` with exact server/principal/resource
//! binding; see the MCP guide's authorization section. Authentication concerns are
//! observed even when open fails, and never cause automatic operation replay.

use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use htlk_runtime::{
    HostContext,
    clock::SystemClock,
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpCancellation, McpEndpoint, McpFuture, client::*},
    store::{McpSubscriptionLimits, SqliteStore, StoreConfig},
};
use std::{collections::BTreeMap, error::Error, path::Path, sync::Arc, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const PAGE_BYTES: usize = 256 * 1024;
const RPC_MS: u64 = 5_000;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5_000;
const HELP: &str = "Host-owned MCP discovery (protocol 2025-11-25)

Usage:
  mcp_client [OPTIONS] stdio NAME VERSION /absolute/program [ARG ...]
  mcp_client [OPTIONS] http NAME VERSION URL

NAME and VERSION must match the server's initialize implementation identity.
Options must precede stdio/http; all remaining stdio arguments go to the child.

Options:
  --connect-timeout-ms N  Native startup/default timeout: 1000..120000 ms (default: 5000)
  --store PATH         SQLite evidence/intent store (default: in-memory)
  --root FILE_URI      One explicit file:// advisory root (default: no roots)
  --subscribe URI      Monitor this exact resource; never read its content
  --watch-seconds N    Idle reception for 0..60 seconds (default: 0)
  --bearer-env NAME    Static HTTP bearer token from this environment variable
  --help              Print help without starting a child or contacting a server

HTTP URLs must use HTTPS or numeric-loopback HTTP, without userinfo/query/fragment.
Stdio inherits the host environment; use an absolute executable path.
Each inventory is bounded to 16 pages, 512 items, 256 KiB/page, 2 MiB, 5 seconds.
All four inventories share a 20-second discovery deadline. Incomplete discovery
fails explicitly. Resource monitoring requires --subscribe and --watch-seconds > 0.
Form/URL requests are inspected and declined; sampling is not advertised.
Shutdown closes the exact epoch, stops the client, and releases its owner lease.";

struct Options {
    store: Option<String>,
    root: Option<String>,
    subscribe: Option<String>,
    watch_seconds: u64,
    connect_timeout_ms: u64,
    server: ServerIdentity,
    endpoint: McpEndpoint,
}

fn input() -> Box<dyn Error> {
    "invalid arguments or host configuration (values redacted); use --help".into()
}

fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Option<Options>> {
    let mut args = args.into_iter();
    let mut flags = BTreeMap::new();
    let transport = loop {
        let arg = args.next().ok_or_else(input)?;
        let arg = arg.into_string().map_err(|_| input())?;
        if arg == "--help" && flags.is_empty() && args.next().is_none() {
            return Ok(None);
        }
        if arg == "stdio" || arg == "http" {
            break arg;
        }
        if !matches!(
            arg.as_str(),
            "--store"
                | "--root"
                | "--subscribe"
                | "--watch-seconds"
                | "--bearer-env"
                | "--connect-timeout-ms"
        ) {
            return Err(input());
        }
        let value = args
            .next()
            .ok_or_else(input)?
            .into_string()
            .map_err(|_| input())?;
        if value.is_empty() || value.len() > 8192 || flags.insert(arg, value).is_some() {
            return Err(input());
        }
    };
    let rest = args
        .map(|arg| arg.into_string().map_err(|_| input()))
        .collect::<Result<Vec<_>>>()?;
    if rest.len() < 3 || rest.len() > 128 || rest.iter().any(|s| s.len() > 8192) {
        return Err(input());
    }
    let limits = Limits::default();
    let connect_timeout_ms = flags
        .remove("--connect-timeout-ms")
        .map(|n| n.parse::<u64>().map_err(|_| input()))
        .transpose()?
        .unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS);
    if !(1_000..=120_000).contains(&connect_timeout_ms) {
        return Err(input());
    }
    let watch_seconds = flags
        .remove("--watch-seconds")
        .map(|n| n.parse::<u64>().map_err(|_| input()))
        .transpose()?
        .unwrap_or(0);
    if watch_seconds > 60 || (flags.contains_key("--subscribe") && watch_seconds == 0) {
        return Err(input());
    }
    let root = flags.remove("--root");
    if let Some(root) = &root {
        let url = reqwest::Url::parse(root).map_err(|_| input())?;
        if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
            return Err(input());
        }
    }
    let bearer = flags.remove("--bearer-env");
    let endpoint = if transport == "stdio" {
        if bearer.is_some() || !Path::new(&rest[2]).is_absolute() {
            return Err(input());
        }
        McpEndpoint::Stdio {
            program: rest[2].clone(),
            arguments: rest[3..].to_vec(),
            environment: BTreeMap::new(),
        }
    } else {
        if rest.len() != 3 {
            return Err(input());
        }
        let url = reqwest::Url::parse(&rest[2]).map_err(|_| input())?;
        let loopback = url
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback());
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(input());
        }
        let mut headers = BTreeMap::new();
        if let Some(name) = bearer {
            let token = std::env::var(name).map_err(|_| input())?;
            if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_whitespace) {
                return Err(input());
            }
            let value = format!("Bearer {token}");
            reqwest::header::HeaderValue::from_str(&value).map_err(|_| input())?;
            headers.insert("Authorization".into(), value);
        }
        McpEndpoint::Http {
            url: url.to_string(),
            headers,
        }
    };
    Ok(Some(Options {
        store: flags.remove("--store"),
        root,
        subscribe: flags.remove("--subscribe"),
        watch_seconds,
        connect_timeout_ms,
        server: ServerIdentity::new(
            "host-example".into(),
            if transport == "stdio" {
                McpTransport::Stdio
            } else {
                McpTransport::StreamableHttp
            },
            rest[0].clone(),
            rest[1].clone(),
            &limits,
        )?,
        endpoint,
    }))
}

// This single-principal embedding host grants only its pinned server and purpose.
// A multi-user application must authenticate actors and enforce its own label policy.
struct Policy(Digest);
impl McpClientAuthorizer for Policy {
    fn authorize(
        &self,
        actor: &HostContext,
        action: McpClientAuthorization<'_>,
    ) -> std::result::Result<(), RuntimeFailure> {
        if actor.principal() != action.owner.host().principal()
            || action.server.is_some_and(|server| server != self.0)
            || !matches!(action.owner.scope(), McpOwnerScope::Host(name) if name == "catalog-monitor")
            || !matches!(
                action.action,
                McpClientAction::Dispatch
                    | McpClientAction::Inspect
                    | McpClientAction::Reply
                    | McpClientAction::Deliver
            )
        {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
}

struct Roots(JsonDocument);
impl McpClientHandler for Roots {
    fn handle<'a>(
        &'a self,
        _context: &'a McpInteractionContext,
        _request: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        Box::pin(async { McpServiceOutcome::completed(self.0.clone()) })
    }
}

struct Events(tokio::sync::mpsc::Sender<McpClientEvent>);
impl McpEventHandler for Events {
    fn receive<'a>(&'a self, event: McpClientEvent) -> McpFuture<'a, ()> {
        Box::pin(async move {
            self.0
                .try_send(event)
                .map_err(|_| RuntimeFailure::new(FailureCode::Budget))
        })
    }
}

fn text_field<'a>(document: &'a JsonDocument, key: &str) -> Option<&'a str> {
    let Value::Map(map) = document.value() else {
        return None;
    };
    match map.get(key)? {
        Value::Text(text) => Some(text),
        _ => None,
    }
}

async fn consume_events(
    mut events: tokio::sync::mpsc::Receiver<McpClientEvent>,
    host: McpInteractionHost,
) -> Result<()> {
    while let Some(event) = events.recv().await {
        // Never log protected params, URLs, form values, or arbitrary method text.
        if event.method == "htlk/interaction" {
            let kind = text_field(&event.params, "kind").ok_or_else(input)?;
            if kind != "form" && kind != "url" {
                continue;
            }
            let id: Digest = text_field(&event.params, "interaction_id")
                .ok_or_else(input)?
                .parse()?;
            let view = host
                .inspect(event.owner.host(), &event.owner, id, PAGE_BYTES)
                .await?;
            if view.request_truncated || view.session_closed {
                return Err("interaction cannot be inspected completely".into());
            }
            // Keys are owner-scoped: use a distinct stable key per interaction.
            let receipt = host
                .respond(
                    event.owner.host(),
                    &event.owner,
                    id,
                    view.record.request_digest,
                    JsonDocument::new(br#"{"action":"decline"}"#, &Limits::default())?,
                    &format!("headless-decline-v1:{id}"),
                )
                .await?;
            eprintln!("elicitation declined: {}", receipt.interaction_id);
        } else if event.notification.is_some() {
            eprintln!(
                "notification received: {} (no content acquisition)",
                event.event_id
            );
        } else {
            eprintln!("lifecycle event received: {}", event.event_id);
        }
    }
    Ok(())
}

fn cancellation() -> Arc<McpCancellation> {
    Arc::new(McpCancellation::new())
}

async fn browse(session: &McpClientSession) -> Result<()> {
    let bounds = McpCatalogLimits::new(16, 512, PAGE_BYTES, 2 * 1024 * 1024, RPC_MS)?;
    let mut incomplete = false;
    for kind in [
        McpCatalogKind::Tools,
        McpCatalogKind::Prompts,
        McpCatalogKind::Resources,
        McpCatalogKind::ResourceTemplates,
    ] {
        match session
            .enumerate_catalog(kind, None, bounds, cancellation())
            .await
        {
            Ok(catalog) => {
                println!(
                    "{kind:?}: {:?}, {} pages, {} items",
                    catalog.status(),
                    catalog.pages(),
                    catalog.descriptors().len()
                );
                // JSON serialization escapes server-controlled terminal characters.
                for descriptor in catalog.descriptors() {
                    println!("{}", std::str::from_utf8(descriptor.as_bytes())?);
                }
                incomplete |= !matches!(catalog.status(), McpCatalogStatus::Complete);
            }
            Err(McpServiceError::Unsupported) => println!("{kind:?}: unsupported"),
            Err(error) => {
                eprintln!("{kind:?}: {error}");
                incomplete = true;
            }
        }
    }
    if incomplete {
        Err("catalog discovery is incomplete".into())
    } else {
        Ok(())
    }
}

async fn monitor(
    session: &McpClientSession,
    subscriptions: &McpSubscriptionHost,
    options: &Options,
) -> Result<()> {
    let consumer = options
        .subscribe
        .as_ref()
        .map(|uri| {
            McpSubscriptionConsumer::new(uri.clone(), "terminal-monitor".into(), &Limits::default())
        })
        .transpose()?;
    let bounds = McpServiceLimits::new(PAGE_BYTES, RPC_MS)?;
    let result = async {
        if let Some(consumer) = &consumer {
            let report = subscriptions
                .subscribe(session, consumer, bounds, cancellation())
                .await?;
            eprintln!(
                "subscribe: {:?}, converged={}",
                report.record().state(),
                report.record().converged()
            );
            if report.failure().is_some() || !report.record().converged() {
                return Err("subscription did not converge; inspect retained intent".into());
            }
        }
        let until = tokio::time::Instant::now() + Duration::from_secs(options.watch_seconds);
        while tokio::time::Instant::now() < until {
            if session.ended().is_some() {
                return Err("session ended during monitoring".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        eprintln!(
            "idle server events available: {:?}",
            session.server_events_available()
        );
        Ok(())
    }
    .await;
    // Release desired presence even after failed/uncertain subscribe. This does not
    // blindly retry an uncertain RPC; inspect the report for retained uncertainty.
    let release = if let Some(consumer) = &consumer {
        let report = subscriptions
            .unsubscribe(session, consumer, bounds, cancellation())
            .await?;
        eprintln!(
            "unsubscribe: {:?}, converged={}",
            report.record().state(),
            report.record().converged()
        );
        if report.failure().is_some() || !report.record().converged() {
            Err("subscription release is unconfirmed; intent/evidence retained".into())
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
    result.and(release)
}

async fn run(options: Options) -> Result<()> {
    let limits = Limits::default();
    let store = if let Some(path) = &options.store {
        SqliteStore::open(path, StoreConfig::default()).await?
    } else {
        SqliteStore::in_memory(StoreConfig::default()).await?
    };
    let persistence = McpClientPersistence::new(store.clone(), Arc::new(SystemClock));
    let roots = options
        .root
        .as_ref()
        .map(|uri| {
            Map::try_from_entries(vec![("uri".into(), Value::Text(uri.clone()))]).map(Value::Map)
        })
        .transpose()?
        .into_iter()
        .collect();
    let roots = JsonDocument::from_value(
        &Value::Map(Map::try_from_entries(vec![(
            "roots".into(),
            Value::Array(roots),
        )])?),
        &limits,
    )?;
    let human: Arc<dyn McpClientHandler> = Arc::new(McpAwaitReply::new(persistence.clone()));
    let (send, receive) = tokio::sync::mpsc::channel(32);
    let config = McpClientConfig::new(
        McpClientLimits {
            owner_identities: 1,
            pending_requests: 4,
            callbacks: 4,
            events: 32,
            frame_bytes: PAGE_BYTES,
            retained_bytes: 4 * 1024 * 1024,
            requests_per_session: 128,
            messages_per_session: 1024,
            callback_depth: 4,
            catalog_pages: 16,
            catalog_items: 512,
            timeout_ms: options.connect_timeout_ms,
            cancellation_grace_ms: 100,
        },
        BTreeMap::from([
            (
                McpClientService::Roots,
                Arc::new(Roots(roots)) as Arc<dyn McpClientHandler>,
            ),
            (McpClientService::ElicitationForm, human.clone()),
            (McpClientService::ElicitationUrl, human),
        ]),
        Some(Arc::new(Events(send))),
    )?
    .with_persistence(persistence.clone())
    .with_authorizer(Arc::new(Policy(options.server.digest(&limits)?)));
    let concerns = config.authentication_concerns();
    let host = McpInteractionHost::new(config.clone())?;
    let client = NativeMcpClient::new(config)?;
    let actor = HostContext::new("local-operator".into(), &limits)?;
    // Covers at most 120s startup + 20s discovery + 60s watch + bounded
    // subscribe/unsubscribe and cleanup, with margin. Long-lived hosts renew leases.
    let subscriptions = McpSubscriptionHost::new(
        persistence.clone(),
        McpSubscriptionLimits::new(8, 8, 32, 1024 * 1024, 2)?,
    );
    let owner = persistence
        .acquire_owner(&actor, "catalog-monitor", 300_000)
        .await?;
    let mut consumer = actix::spawn(consume_events(receive, host));
    let result = async {
        let session = client
            .open(
                owner.clone(),
                options.server.clone(),
                options.endpoint.clone(),
                limits,
                cancellation(),
            )
            .await?;
        let work = async {
            tokio::time::timeout(Duration::from_secs(20), browse(&session)).await??;
            monitor(&session, &subscriptions, &options).await
        };
        let result = tokio::select! {
            result = work => result,
            event_result = &mut consumer => match event_result {
                Ok(Err(error)) => Err(error),
                _ => Err("event consumer ended; session is closing".into()),
            },
        };
        let health = if let Some(reason) = session.ended() {
            eprintln!("session ended: {reason:?}");
            Err("session ended before explicit close".into())
        } else {
            Ok(())
        };
        let closed = subscriptions.disconnect(&session).await;
        result.and(health).and(closed.map_err(Into::into))
    }
    .await;
    let stopped = client.shutdown().await;
    consumer.abort();
    // Release only after transport shutdown was acknowledged.
    let released = if stopped.is_ok() {
        persistence.release_owner(&owner).await
    } else {
        stopped
    };
    if concerns.borrow().is_some() {
        eprintln!("HTTP authentication needs host attention; no operation was replayed");
    }
    let closed = store.close().await;
    result
        .and(released.map_err(Into::into))
        .and(closed.map_err(Into::into))
}

fn main() -> std::process::ExitCode {
    let result = match parse(std::env::args_os().skip(1)) {
        Ok(None) => {
            println!("{HELP}");
            return std::process::ExitCode::SUCCESS;
        }
        Ok(Some(options)) => actix::System::new().block_on(run(options)),
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<RuntimeFailure>() {
                eprintln!("mcp_client: {}", failure.code());
            }
            eprintln!(
                "mcp_client: arguments, configuration, or session failed (values redacted); use --help"
            );
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, path::PathBuf};

    fn arguments(timeout: Option<&str>) -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(timeout) = timeout {
            args.extend(["--connect-timeout-ms".into(), timeout.into()]);
        }
        args.extend(["stdio", "timeout-peer", "1", "/unused-server"].map(OsString::from));
        args
    }

    #[test]
    fn startup_timeout_cli_default_and_inclusive_bounds() {
        assert_eq!(
            parse(arguments(None)).unwrap().unwrap().connect_timeout_ms,
            5000
        );
        for (text, expected) in [("1000", 1000), ("30000", 30000), ("120000", 120000)] {
            assert_eq!(
                parse(arguments(Some(text)))
                    .unwrap()
                    .unwrap()
                    .connect_timeout_ms,
                expected
            );
        }
        assert!(parse([OsString::from("--help")]).unwrap().is_none());
    }

    #[test]
    fn invalid_startup_timeout_arguments_are_redacted() {
        for timeout in [
            "0",
            "999",
            "120001",
            "-1",
            "1.5",
            "18446744073709551616",
            "private-secret",
            "",
        ] {
            let error = parse(arguments(Some(timeout)))
                .err()
                .expect("invalid timeout");
            assert_eq!(error.to_string(), input().to_string());
        }
        let missing = parse([OsString::from("--connect-timeout-ms")])
            .err()
            .unwrap();
        assert_eq!(missing.to_string(), input().to_string());
        let mut duplicate = vec!["--connect-timeout-ms".into(), "30000".into()];
        duplicate.extend(arguments(Some("private-secret")));
        assert_eq!(
            parse(duplicate).err().unwrap().to_string(),
            input().to_string()
        );
    }

    // This independent peer records actual wire methods rather than inferring
    // discovery/no-dispatch from the Rust client's result. Delays exercise the
    // example's parsed configuration through its real open/browse/close path.
    const DELAYED_PEER: &str = r#"
        const assert = require('node:assert/strict');
        const {appendFileSync} = require('node:fs');
        const {createInterface} = require('node:readline');
        const [log, initializeDelay, catalogDelay] = process.argv.slice(1);
        const reply = (r, result) => process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\n');
        createInterface({input:process.stdin}).on('line', line => {
            const r = JSON.parse(line);
            appendFileSync(log, r.method+'\n');
            if (r.method === 'initialize') {
                assert.equal(r.params.capabilities.sampling, undefined);
                setTimeout(() => reply(r, {protocolVersion:'2025-11-25',serverInfo:{name:'timeout-peer',version:'1'},capabilities:{tools:{},prompts:{},resources:{}}}), Number(initializeDelay));
            } else if (r.method === 'tools/list') {
                setTimeout(() => reply(r, {tools:[]}), Number(catalogDelay));
            } else if (r.method === 'prompts/list') reply(r, {prompts:[]});
            else if (r.method === 'resources/list') reply(r, {resources:[]});
            else if (r.method === 'resources/templates/list') reply(r, {resourceTemplates:[]});
            else assert(['notifications/initialized','notifications/cancelled'].includes(r.method), 'Unexpected content/effect method');
        });
    "#;

    struct Logs(PathBuf);
    impl Logs {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "htlk-example-timeout-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn methods(&self, name: &str) -> Vec<String> {
            std::fs::read_to_string(self.0.join(name))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }
    impl Drop for Logs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn delayed_peer(
        node: &Path,
        log: &Path,
        timeout: Option<&str>,
        initialize_ms: &str,
        catalog_ms: &str,
    ) -> Options {
        let mut args = arguments(timeout);
        args.pop();
        args.extend([
            node.as_os_str().to_owned(),
            "-e".into(),
            DELAYED_PEER.into(),
            log.as_os_str().to_owned(),
            initialize_ms.into(),
            catalog_ms.into(),
        ]);
        parse(args).unwrap().unwrap()
    }

    #[test]
    fn delayed_stdio_startup_uses_cli_timeout_without_extending_catalog_budget() {
        let node = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("node"))
            .find(|path| path.is_absolute() && path.is_file())
            .expect("Node.js is a repository test prerequisite");
        let logs = Logs::new();
        let short = delayed_peer(&node, &logs.0.join("default"), None, "6500", "0");
        let long = delayed_peer(&node, &logs.0.join("extended"), Some("10000"), "6500", "0");
        let catalog = delayed_peer(&node, &logs.0.join("catalog"), Some("10000"), "0", "6500");
        actix::System::new().block_on(async {
            let (short, long, catalog) = tokio::time::timeout(Duration::from_secs(20), async {
                tokio::join!(run(short), run(long), run(catalog))
            })
            .await
            .expect("startup, discovery, and cleanup remain bounded");
            assert_eq!(
                short
                    .err()
                    .unwrap()
                    .downcast_ref::<RuntimeFailure>()
                    .unwrap()
                    .code(),
                FailureCode::McpTimeout
            );
            long.unwrap();
            assert_eq!(
                catalog.err().unwrap().to_string(),
                "catalog discovery is incomplete"
            );
        });
        let default = logs.methods("default");
        assert_eq!(
            default
                .iter()
                .filter(|method| *method == "initialize")
                .count(),
            1
        );
        assert!(
            default
                .iter()
                .all(|method| matches!(method.as_str(), "initialize" | "notifications/cancelled"))
        );
        assert_eq!(
            logs.methods("extended"),
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "prompts/list",
                "resources/list",
                "resources/templates/list",
            ]
        );
        let catalog = logs.methods("catalog");
        assert_eq!(
            catalog
                .iter()
                .filter(|method| *method == "tools/list")
                .count(),
            1
        );
        assert!(catalog.iter().all(|method| matches!(
            method.as_str(),
            "initialize"
                | "notifications/initialized"
                | "notifications/cancelled"
                | "tools/list"
                | "prompts/list"
                | "resources/list"
                | "resources/templates/list"
        )));
    }
}
