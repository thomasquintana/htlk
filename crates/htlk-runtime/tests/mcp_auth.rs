//! OAuth interoperability against an independently implemented Node HTTP peer.
use htlk_executable::{
    JsonDocument, McpTransport, ServerIdentity,
    cbor::{Limits, Value},
};
use htlk_runtime::mcp::client::auth::*;
use htlk_runtime::{
    HostContext,
    failure::{FailureCode, RuntimeFailure},
    mcp::{
        McpCancellation, McpDelivery, McpEndpoint,
        client::{
            McpClientConfig, McpClientLimits, McpClientSession, McpOwner, McpOwnerScope,
            McpSessionEnd, NativeMcpClient,
        },
    },
};
use reqwest::{
    Client, Method, StatusCode, Url,
    header::{HeaderMap, HeaderValue},
};
use std::{
    io::BufRead,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct Peer {
    child: Child,
    base: Url,
}
impl Peer {
    fn start(mode: &str, method: &str) -> Self {
        let mut child = Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/support/mcp_auth_peer.mjs"
            ))
            .env("AUTH_MODE", mode)
            .env("AUTH_METHOD", method)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        Self {
            child,
            base: Url::parse(&format!("http://127.0.0.1:{}/", port.trim())).unwrap(),
        }
    }
    fn url(&self, path: &str) -> Url {
        self.base.join(path).unwrap()
    }
    fn binding(&self, principal: &str) -> CredentialBinding {
        CredentialBinding::new(
            "trusted-compound-server".into(),
            principal.into(),
            self.url("public/mcp"),
        )
        .unwrap()
    }
    fn policy(&self) -> Arc<Policy> {
        Arc::new(Policy {
            origin: self.base.clone(),
            deny: None,
            calls: Mutex::new(vec![]),
        })
    }
    fn oauth(&self, method: &str) -> OAuthClient {
        self.with_limits(method, self.policy(), local_limits())
    }
    fn with_limits(&self, method: &str, policy: Arc<Policy>, limits: OAuthLimits) -> OAuthClient {
        let authentication = match method {
            "none" => OAuthClientAuthentication::None,
            "client_secret_basic" => {
                OAuthClientAuthentication::ClientSecretBasic("client-secret: +".into())
            }
            "client_secret_post" => {
                OAuthClientAuthentication::ClientSecretPost("client-secret: +".into())
            }
            _ => unreachable!(),
        };
        OAuthClient::new(
            self.binding("alice"),
            OAuthRegistration::new(
                self.url("tenant"),
                "registered-client".into(),
                Url::parse("http://127.0.0.1:32123/callback").unwrap(),
                authentication,
            )
            .unwrap(),
            policy,
            limits,
        )
        .unwrap()
    }
    async fn challenge(&self) -> OAuthChallenge {
        let response = http().get(self.url("public/mcp")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        OAuthChallenge::from_headers(response.status(), response.headers(), &local_limits())
            .unwrap()
            .unwrap()
    }
    async fn stats(&self) -> JsonDocument {
        let response = http()
            .get(self.url("stats"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        JsonDocument::new(&response, &Limits::default()).unwrap()
    }
    async fn paths(&self) -> Vec<String> {
        let stats = self.stats().await;
        let Value::Map(map) = stats.value() else {
            panic!()
        };
        let Some(Value::Array(requests)) = map.get("requests") else {
            panic!()
        };
        requests
            .iter()
            .map(|r| {
                let Value::Map(map) = r else { panic!() };
                let Some(Value::Text(path)) = map.get("path") else {
                    panic!()
                };
                path.clone()
            })
            .collect()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Policy {
    origin: Url,
    deny: Option<OAuthEndpointKind>,
    calls: Mutex<Vec<(OAuthEndpointKind, Url)>>,
}
impl OAuthEndpointPolicy for Policy {
    fn allows(&self, _: &CredentialBinding, kind: OAuthEndpointKind, url: &Url) -> bool {
        self.calls.lock().unwrap().push((kind, url.clone()));
        self.deny != Some(kind) && url.origin() == self.origin.origin()
    }
}
fn local_limits() -> OAuthLimits {
    OAuthLimits {
        allow_http_loopback: true,
        ..OAuthLimits::default()
    }
}
fn http() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
fn entropy(seed: u8) -> OAuthEntropy {
    OAuthEntropy::new([seed; 32], [seed.wrapping_add(1); 32]).unwrap()
}
async fn navigate(pending: &OAuthPendingAuthorization) -> Url {
    let response = http()
        .get(pending.authorization_url().clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FOUND,
        "independent AS rejected authorization parameters"
    );
    Url::parse(
        response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
}
async fn grant(client: &OAuthClient, challenge: &OAuthChallenge, seed: u8) -> OAuthTokens {
    let discovery = client.discover(challenge).await.unwrap();
    let pending = client
        .begin_authorization(discovery, entropy(seed))
        .unwrap();
    let callback = navigate(&pending).await;
    client
        .finish_authorization(pending, &callback)
        .await
        .unwrap()
}
fn challenge(raw: &str, status: StatusCode) -> Result<Option<OAuthChallenge>, OAuthError> {
    let mut headers = HeaderMap::new();
    headers.insert("www-authenticate", HeaderValue::from_str(raw).unwrap());
    OAuthChallenge::from_headers(status, &headers, &local_limits())
}

#[tokio::test]
async fn independent_pkce_flow_refresh_scope_step_up_and_all_http_methods() {
    let peer = Peer::start("oauth", "none");
    let client = peer.oauth("none");
    let initial = peer.challenge().await;
    assert_eq!(initial.scopes().unwrap(), ["read"]);
    let mut tokens = grant(&client, &initial, 1).await;
    assert_eq!(tokens.binding(), &peer.binding("alice"));
    assert_eq!(tokens.scopes().unwrap(), ["read"]);
    assert_eq!(tokens.expires_in(), Some(Duration::from_secs(3600)));
    for method in [Method::POST, Method::GET, Method::DELETE] {
        let provider: &dyn McpCredentialProvider = &tokens;
        let authorization = provider
            .authorization(&peer.binding("alice"), &method, &peer.url("public/mcp"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            format!("{authorization:?}"),
            "OAuthAuthorization([redacted])"
        );
        let request = authorization
            .apply(http().request(method, peer.url("public/mcp")))
            .build()
            .unwrap();
        assert!(
            request
                .headers()
                .get("authorization")
                .unwrap()
                .is_sensitive()
        );
        let response = http().execute(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let old_access = tokens.access_token().to_owned();
    let old_refresh = tokens.refresh_token().unwrap().to_owned();
    let discovery = client.discover(&initial).await.unwrap();
    tokens = client.refresh(&discovery, tokens).await.unwrap();
    assert_ne!(old_access, tokens.access_token());
    assert_ne!(old_refresh, tokens.refresh_token().unwrap());
    let response = tokens
        .authorization(&peer.binding("alice"), &peer.url("public/mcp"))
        .unwrap()
        .apply(
            http()
                .post(peer.url("public/mcp"))
                .header("x-require-write", "true"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let step_up =
        OAuthChallenge::from_headers(response.status(), response.headers(), &local_limits())
            .unwrap()
            .unwrap();
    assert!(step_up.is_insufficient_scope());
    assert_eq!(step_up.scopes().unwrap(), ["read", "write"]);
    // The challenge is authoritative even though metadata only advertises read.
    let upgraded = grant(&client, &step_up, 3).await;
    assert_eq!(upgraded.scopes().unwrap(), ["read", "write"]);
    let response = upgraded
        .authorization(&peer.binding("alice"), &peer.url("public/mcp"))
        .unwrap()
        .apply(
            http()
                .post(peer.url("public/mcp"))
                .header("x-require-write", "true"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        peer.paths().await.iter().filter(|p| *p == "/token").count(),
        3
    );
}

#[tokio::test]
async fn well_known_resource_and_both_oidc_path_conventions_use_required_priority() {
    for (mode, expected) in [
        (
            "fallback",
            vec![
                "/public/mcp",
                "/.well-known/oauth-protected-resource/public/mcp",
                "/.well-known/oauth-protected-resource",
                "/.well-known/oauth-authorization-server/tenant",
            ],
        ),
        (
            "oidc-insert",
            vec![
                "/public/mcp",
                "/.well-known/oauth-protected-resource/public/mcp",
                "/.well-known/oauth-authorization-server/tenant",
                "/.well-known/openid-configuration/tenant",
            ],
        ),
        (
            "oidc-append",
            vec![
                "/public/mcp",
                "/.well-known/oauth-protected-resource/public/mcp",
                "/.well-known/oauth-authorization-server/tenant",
                "/.well-known/openid-configuration/tenant",
                "/tenant/.well-known/openid-configuration",
            ],
        ),
    ] {
        let peer = Peer::start(mode, "none");
        let client = peer.oauth("none");
        let discovery = client.discover(&peer.challenge().await).await.unwrap();
        assert_eq!(discovery.scopes().unwrap(), ["read"]);
        assert_eq!(discovery.issuer(), &peer.url("tenant"));
        assert_eq!(peer.paths().await, expected, "{mode}");
        // OIDC metadata really supports exchange, not just successful parsing.
        let pending = client.begin_authorization(discovery, entropy(10)).unwrap();
        let callback = navigate(&pending).await;
        client
            .finish_authorization(pending, &callback)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn preregistered_confidential_client_authentication_is_interoperable() {
    for method in ["client_secret_basic", "client_secret_post"] {
        let peer = Peer::start("oauth", method);
        let client = peer.oauth(method);
        let challenge = peer.challenge().await;
        let tokens = grant(&client, &challenge, 13).await;
        assert!(tokens.refresh_token().is_some());
        client
            .refresh(&client.discover(&challenge).await.unwrap(), tokens)
            .await
            .unwrap();
        assert_eq!(
            peer.paths().await.iter().filter(|p| *p == "/token").count(),
            2
        );
    }
}

#[tokio::test]
async fn invalid_callbacks_are_rejected_before_token_dispatch() {
    let peer = Peer::start("oauth", "none");
    let client = peer.oauth("none");
    let challenge = peer.challenge().await;
    for mutation in [
        "state",
        "missing-state",
        "duplicate-state",
        "redirect",
        "fragment",
        "issuer",
        "missing-issuer",
        "resource",
        "code-and-error",
        "denied",
    ] {
        let pending = client
            .begin_authorization(client.discover(&challenge).await.unwrap(), entropy(20))
            .unwrap();
        let mut callback = navigate(&pending).await;
        match mutation {
            "state" => {
                callback
                    .query_pairs_mut()
                    .clear()
                    .append_pair("state", "wrong")
                    .append_pair("code", "private-code");
            }
            "missing-state" => {
                callback.set_query(Some("code=private-code"));
            }
            "duplicate-state" => {
                callback.query_pairs_mut().append_pair("state", "another");
            }
            "redirect" => {
                callback.set_path("/different");
            }
            "fragment" => {
                callback.set_fragment(Some("private-fragment"));
            }
            "issuer" => {
                let pairs: Vec<_> = callback
                    .query_pairs()
                    .filter(|(k, _)| k != "iss")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                callback
                    .query_pairs_mut()
                    .clear()
                    .extend_pairs(pairs)
                    .append_pair("iss", "https://evil.test");
            }
            "resource" => {
                let pairs: Vec<_> = callback
                    .query_pairs()
                    .filter(|(k, _)| k != "resource")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                callback
                    .query_pairs_mut()
                    .clear()
                    .extend_pairs(pairs)
                    .append_pair("resource", "https://evil.test");
            }
            "missing-issuer" => {
                let pairs: Vec<_> = callback
                    .query_pairs()
                    .filter(|(k, _)| k != "iss")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                callback.query_pairs_mut().clear().extend_pairs(pairs);
            }
            "code-and-error" => {
                callback
                    .query_pairs_mut()
                    .append_pair("error", "access_denied");
            }
            "denied" => {
                let pairs: Vec<_> = callback
                    .query_pairs()
                    .filter(|(k, _)| k != "code")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                callback
                    .query_pairs_mut()
                    .clear()
                    .extend_pairs(pairs)
                    .append_pair("error", "access_denied");
            }
            _ => unreachable!(),
        }
        assert_eq!(
            client
                .finish_authorization(pending, &callback)
                .await
                .unwrap_err(),
            if mutation == "denied" {
                OAuthError::Denied
            } else {
                OAuthError::Callback
            },
            "{mutation}"
        );
    }
    assert!(!peer.paths().await.iter().any(|p| p == "/token"));
}

#[tokio::test]
async fn independent_server_rejects_tampered_pkce_and_token_exchange_is_not_retried() {
    let peer = Peer::start("oauth", "none");
    let client = peer.oauth("none");
    let pending = client
        .begin_authorization(
            client.discover(&peer.challenge().await).await.unwrap(),
            entropy(30),
        )
        .unwrap();
    // Intercept the browser URL and replace its challenge. A correctly validating
    // client still sends its original verifier, which the independent AS rejects.
    let mut url = pending.authorization_url().clone();
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| k != "code_challenge")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(pairs)
        .append_pair("code_challenge", &"a".repeat(43));
    let response = http().get(url).send().await.unwrap();
    let callback = Url::parse(
        response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        client
            .finish_authorization(pending, &callback)
            .await
            .unwrap_err(),
        OAuthError::TokenRejected
    );
    assert_eq!(
        peer.paths().await.iter().filter(|p| *p == "/token").count(),
        1
    );

    let peer = Peer::start("token-disconnect", "none");
    let client = peer.oauth("none");
    let pending = client
        .begin_authorization(
            client.discover(&peer.challenge().await).await.unwrap(),
            entropy(31),
        )
        .unwrap();
    let callback = navigate(&pending).await;
    assert_eq!(
        client
            .finish_authorization(pending, &callback)
            .await
            .unwrap_err(),
        OAuthError::Transport
    );
    // Server redeemed the code before dropping the connection; no second exchange
    // or protected operation is dispatched by the library after uncertain delivery.
    let paths = peer.paths().await;
    assert_eq!(paths.iter().filter(|p| *p == "/token").count(), 1);
    assert_eq!(paths.iter().filter(|p| *p == "/public/mcp").count(), 1);
}

#[tokio::test]
async fn discovery_fails_closed_for_bad_metadata_limits_and_redirects() {
    for (mode, expected) in [
        ("no-pkce", OAuthError::Metadata),
        ("issuer-mismatch", OAuthError::Metadata),
        ("resource-mismatch", OAuthError::Metadata),
        ("duplicate-json", OAuthError::Metadata),
        ("large", OAuthError::Limit),
        ("metadata-redirect", OAuthError::Redirect),
    ] {
        let peer = Peer::start(mode, "none");
        let client = peer.oauth("none");
        let error = client.discover(&peer.challenge().await).await.unwrap_err();
        assert_eq!(error, expected, "{mode}");
        let paths = peer.paths().await;
        assert!(
            !paths
                .iter()
                .any(|p| p == "/trap" || p == "/token" || p == "/authorize")
        );
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.starts_with("/.well-known/oauth-protected"))
                .count(),
            0,
            "authoritative header must not silently fall back"
        );
    }
    for mode in ["slow", "slow-body"] {
        let peer = Peer::start(mode, "none");
        let client = peer.with_limits(
            "none",
            peer.policy(),
            OAuthLimits {
                timeout: Duration::from_millis(40),
                ..local_limits()
            },
        );
        assert_eq!(
            client.discover(&peer.challenge().await).await.unwrap_err(),
            OAuthError::Limit,
            "{mode}"
        );
    }
    let peer = Peer::start("oidc-append", "none");
    let client = peer.with_limits(
        "none",
        peer.policy(),
        OAuthLimits {
            discovery_requests: 2,
            ..local_limits()
        },
    );
    assert_eq!(
        client.discover(&peer.challenge().await).await.unwrap_err(),
        OAuthError::Limit
    );
    assert_eq!(peer.paths().await.len(), 3); // Initial 401 plus exactly two probes.
}

#[tokio::test]
async fn endpoint_roles_are_independently_trusted_and_no_credentials_cross_bindings() {
    let peer = Peer::start("oauth", "none");
    let challenge = peer.challenge().await;
    for role in [
        OAuthEndpointKind::ResourceMetadata,
        OAuthEndpointKind::AuthorizationServerMetadata,
        OAuthEndpointKind::Authorization,
        OAuthEndpointKind::Token,
    ] {
        let policy = Arc::new(Policy {
            origin: peer.base.clone(),
            deny: Some(role),
            calls: Mutex::new(vec![]),
        });
        let client = peer.with_limits("none", policy.clone(), local_limits());
        assert_eq!(
            client.discover(&challenge).await.unwrap_err(),
            OAuthError::UntrustedEndpoint
        );
        assert!(
            policy
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(kind, _)| kind == &role)
        );
    }
    let client = peer.oauth("none");
    let tokens = grant(&client, &challenge, 40).await;
    assert_eq!(
        tokens
            .authorization(&peer.binding("bob"), &peer.url("public/mcp"))
            .unwrap_err(),
        OAuthError::Binding
    );
    assert_eq!(
        tokens
            .authorization(&peer.binding("alice"), &peer.url("other"))
            .unwrap_err(),
        OAuthError::Binding
    );
    let other = CredentialBinding::new(
        "other-server".into(),
        "alice".into(),
        peer.url("public/mcp"),
    )
    .unwrap();
    assert_eq!(
        tokens
            .authorization(&other, &peer.url("public/mcp"))
            .unwrap_err(),
        OAuthError::Binding
    );
    // A foreign metadata location is blocked before any attempted request.
    let foreign = challenge_from_url("https://untrusted.invalid/private");
    assert_eq!(
        client.discover(&foreign).await.unwrap_err(),
        OAuthError::UntrustedEndpoint
    );
}
fn challenge_from_url(url: &str) -> OAuthChallenge {
    challenge(
        &format!("Bearer resource_metadata=\"{url}\""),
        StatusCode::UNAUTHORIZED,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn bearer_parser_and_per_operation_loop_budget_are_bounded() {
    let c = challenge("Digest realm=\"a,b\", Basic realm=\"other\", Bearer realm=\"escaped \\\"text\\\"\", scope=\"read write\"", StatusCode::UNAUTHORIZED).unwrap().unwrap();
    assert_eq!(c.scopes().unwrap(), ["read", "write"]);
    let mut budget = OAuthChallengeBudget::new(&local_limits()).unwrap();
    budget.admit(&c).unwrap();
    assert_eq!(budget.admit(&c), Err(OAuthError::Limit));
    let other = challenge(
        "Bearer error=insufficient_scope, scope=write",
        StatusCode::FORBIDDEN,
    )
    .unwrap()
    .unwrap();
    budget.admit(&other).unwrap();
    let third = challenge("Bearer scope=admin", StatusCode::UNAUTHORIZED)
        .unwrap()
        .unwrap();
    assert_eq!(budget.admit(&third), Err(OAuthError::Limit));
    for raw in [
        "Bearer scope=\"read\", SCOPE=\"write\"",
        "Bearer scope=read, Bearer scope=write",
        "Bearer scope=\"unterminated",
        "Bearer scope=\"read  write\"",
        "Bearer scope=\"read\\\\write\"",
        "Bearer error=unsupported",
        "Bearer resource_metadata=\"not a url\"",
    ] {
        assert_eq!(
            challenge(raw, StatusCode::UNAUTHORIZED).unwrap_err(),
            OAuthError::Challenge,
            "{raw}"
        );
    }
    assert!(
        challenge("Basic realm=private", StatusCode::FORBIDDEN)
            .unwrap()
            .is_none()
    );
    assert!(
        OAuthChallenge::from_headers(StatusCode::UNAUTHORIZED, &HeaderMap::new(), &local_limits())
            .unwrap()
            .is_some()
    );
    let mut headers = HeaderMap::new();
    headers.append(
        "www-authenticate",
        HeaderValue::from_static("Basic realm=other"),
    );
    headers.append(
        "www-authenticate",
        HeaderValue::from_static("Bearer scope=read"),
    );
    assert_eq!(
        OAuthChallenge::from_headers(StatusCode::UNAUTHORIZED, &headers, &local_limits())
            .unwrap()
            .unwrap()
            .scopes()
            .unwrap(),
        ["read"]
    );
    assert_eq!(
        OAuthChallenge::from_headers(
            StatusCode::UNAUTHORIZED,
            &headers,
            &OAuthLimits {
                bytes: 4,
                ..local_limits()
            }
        )
        .unwrap_err(),
        OAuthError::Limit
    );
}

#[tokio::test]
async fn expiry_restore_refresh_rejection_and_secret_redaction() {
    let peer = Peer::start("expired", "none");
    let client = peer.oauth("none");
    let tokens = grant(&client, &peer.challenge().await, 50).await;
    assert_eq!(
        tokens
            .authorization(&peer.binding("alice"), &peer.url("public/mcp"))
            .unwrap_err(),
        OAuthError::Expired
    );
    let restored = client
        .restore_tokens(
            tokens.access_token().into(),
            tokens.refresh_token().map(str::to_owned),
            tokens.scopes().map(<[String]>::to_vec),
            Some(Duration::from_secs(60)),
        )
        .unwrap();
    assert!(
        restored
            .authorization(&peer.binding("alice"), &peer.url("public/mcp"))
            .is_ok()
    );
    let unknown_expiry = client
        .restore_tokens("opaque-token".into(), None, None, None)
        .unwrap();
    assert_eq!(
        unknown_expiry
            .authorization(&peer.binding("alice"), &peer.url("public/mcp"))
            .unwrap_err(),
        OAuthError::Expired
    );
    let peer = Peer::start("refresh-rejected", "none");
    let client = peer.oauth("none");
    let challenge = peer.challenge().await;
    let tokens = grant(&client, &challenge, 51).await;
    for debug in [
        format!("{client:?}"),
        format!("{tokens:?}"),
        format!("{challenge:?}"),
        format!("{:?}", peer.binding("alice")),
    ] {
        for secret in [
            tokens.access_token(),
            tokens.refresh_token().unwrap(),
            "alice",
            "127.0.0.1",
        ] {
            assert!(!debug.contains(secret));
        }
    }
    let error = client
        .refresh(&client.discover(&challenge).await.unwrap(), tokens)
        .await
        .unwrap_err();
    assert_eq!(error, OAuthError::RefreshRejected);
    assert!(!error.to_string().contains("private-remote-diagnostic"));
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(
        peer.paths().await.iter().filter(|p| *p == "/token").count(),
        2
    );
}

#[tokio::test]
async fn authorization_expiry_and_token_redirect_do_not_dispatch_again() {
    let peer = Peer::start("oauth", "none");
    let client = peer.with_limits(
        "none",
        peer.policy(),
        OAuthLimits {
            authorization_lifetime: Duration::from_millis(1),
            ..local_limits()
        },
    );
    let pending = client
        .begin_authorization(
            client.discover(&peer.challenge().await).await.unwrap(),
            entropy(60),
        )
        .unwrap();
    let callback = navigate(&pending).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(
        client
            .finish_authorization(pending, &callback)
            .await
            .unwrap_err(),
        OAuthError::Callback
    );
    assert!(!peer.paths().await.iter().any(|p| p == "/token"));
    let peer = Peer::start("token-redirect", "none");
    let client = peer.oauth("none");
    let pending = client
        .begin_authorization(
            client.discover(&peer.challenge().await).await.unwrap(),
            entropy(61),
        )
        .unwrap();
    let callback = navigate(&pending).await;
    assert_eq!(
        client
            .finish_authorization(pending, &callback)
            .await
            .unwrap_err(),
        OAuthError::Redirect
    );
    let paths = peer.paths().await;
    assert_eq!(paths.iter().filter(|p| *p == "/token").count(), 1);
    assert!(!paths.iter().any(|p| p == "/trap"));
}

#[tokio::test]
async fn interrupted_refresh_does_not_reuse_rotated_secret_or_replay_resource_request() {
    let peer = Peer::start("refresh-disconnect", "none");
    let client = peer.oauth("none");
    let challenge = peer.challenge().await;
    let tokens = grant(&client, &challenge, 70).await;
    let discovery = client.discover(&challenge).await.unwrap();
    assert_eq!(
        client.refresh(&discovery, tokens).await.unwrap_err(),
        OAuthError::Transport
    );
    let paths = peer.paths().await;
    assert_eq!(paths.iter().filter(|p| *p == "/token").count(), 2);
    assert_eq!(paths.iter().filter(|p| *p == "/public/mcp").count(), 1);
}

#[test]
fn imported_tokens_enforce_bearer_grammar_and_binding_accessors() {
    let peer = Peer::start("oauth", "none");
    let client = peer.oauth("none");
    let binding = peer.binding("alice");
    assert_eq!(binding.server(), "trusted-compound-server");
    assert_eq!(binding.principal(), "alice");
    assert_eq!(binding.resource(), &peer.url("public/mcp"));
    for token in ["", "=", "ab=c", "private token", "private\r\nsecret"] {
        assert_eq!(
            client
                .restore_tokens(token.into(), None, None, Some(Duration::from_secs(60)))
                .unwrap_err(),
            OAuthError::Token
        );
    }
    assert!(
        client
            .restore_tokens("abc+/==".into(), None, None, Some(Duration::from_secs(60)))
            .is_ok()
    );
}

#[test]
fn invalid_local_configuration_fails_before_network() {
    let resource = Url::parse("https://example.test/mcp").unwrap();
    assert!(
        CredentialBinding::new(
            "s".into(),
            "p".into(),
            Url::parse("https://u:p@example.test/mcp").unwrap()
        )
        .is_err()
    );
    assert!(
        CredentialBinding::new(
            "s".into(),
            "p".into(),
            Url::parse("https://example.test/mcp?token=private").unwrap()
        )
        .is_err()
    );
    assert!(
        OAuthRegistration::new(
            resource.clone(),
            "id".into(),
            Url::parse("http://untrusted.test/callback").unwrap(),
            OAuthClientAuthentication::None
        )
        .is_err()
    );
    assert!(OAuthEntropy::new([0; 32], [1; 32]).is_err());
    assert!(OAuthEntropy::new([1; 32], [1; 32]).is_err());
    assert_eq!(
        OAuthChallengeBudget::new(&OAuthLimits {
            challenges: 0,
            ..OAuthLimits::default()
        })
        .unwrap_err(),
        OAuthError::Configuration
    );
    let peer = Peer::start("oauth", "none");
    let registration = OAuthRegistration::new(
        peer.url("tenant"),
        "registered-client".into(),
        Url::parse("http://127.0.0.1:32123/callback").unwrap(),
        OAuthClientAuthentication::None,
    )
    .unwrap();
    assert_eq!(
        OAuthClient::new(
            peer.binding("alice"),
            registration,
            peer.policy(),
            OAuthLimits::default()
        )
        .unwrap_err(),
        OAuthError::UntrustedEndpoint
    );
}

fn native_server() -> ServerIdentity {
    ServerIdentity::new(
        "auth-deployment".into(),
        McpTransport::StreamableHttp,
        "auth-peer".into(),
        "1".into(),
        &Limits::default(),
    )
    .unwrap()
}
fn native_owner() -> McpOwner {
    McpOwner::new(
        HostContext::new("alice".into(), &Limits::default()).unwrap(),
        McpOwnerScope::Host("auth-session".into()),
        1,
        &Limits::default(),
    )
    .unwrap()
}
struct RefreshingProvider {
    oauth: OAuthClient,
    binding: CredentialBinding,
    state: tokio::sync::Mutex<Option<(OAuthDiscovery, OAuthTokens)>>,
    calls: Mutex<Vec<Method>>,
    fail: AtomicBool,
    stall: AtomicBool,
}
impl RefreshingProvider {
    fn new(peer: &Peer) -> Self {
        let binding = CredentialBinding::for_server(
            &native_server(),
            "alice".into(),
            peer.url("public/mcp"),
            &Limits::default(),
        )
        .unwrap();
        let oauth = OAuthClient::new(
            binding.clone(),
            OAuthRegistration::new(
                peer.url("tenant"),
                "registered-client".into(),
                Url::parse("http://127.0.0.1:32123/callback").unwrap(),
                OAuthClientAuthentication::None,
            )
            .unwrap(),
            peer.policy(),
            local_limits(),
        )
        .unwrap();
        Self {
            oauth,
            binding,
            state: tokio::sync::Mutex::new(None),
            calls: Mutex::new(vec![]),
            fail: AtomicBool::new(false),
            stall: AtomicBool::new(false),
        }
    }
    async fn authorize(&self, challenge: &OAuthChallenge, seed: u8) {
        let tokens = grant(&self.oauth, challenge, seed).await;
        let discovery = self.oauth.discover(challenge).await.unwrap();
        *self.state.lock().await = Some((discovery, tokens));
    }
}
impl McpCredentialProvider for RefreshingProvider {
    fn authorization<'a>(
        &'a self,
        binding: &'a CredentialBinding,
        method: &'a Method,
        destination: &'a Url,
    ) -> OAuthCredentialFuture<'a> {
        Box::pin(async move {
            assert_eq!(binding, &self.binding);
            assert_eq!(destination, binding.resource());
            self.calls.lock().unwrap().push(method.clone());
            if self.fail.load(Ordering::Acquire) {
                return Err(OAuthError::Denied);
            }
            if self.stall.load(Ordering::Acquire) {
                std::future::pending::<()>().await;
            }
            let mut state = self.state.lock().await;
            let Some((discovery, tokens)) = state.take() else {
                return Ok(None);
            };
            // Rotate on every acquisition to independently prove that the wire
            // never caches credentials across HTTP methods or resumes.
            let tokens = self.oauth.refresh(&discovery, tokens).await?;
            let authorization = tokens.authorization(binding, destination)?;
            *state = Some((discovery, tokens));
            Ok(Some(authorization))
        })
    }
}
fn native_client(provider: Arc<RefreshingProvider>, idle: bool) -> NativeMcpClient {
    NativeMcpClient::new(
        McpClientConfig::new(McpClientLimits::default(), Default::default(), None)
            .unwrap()
            .with_server_event_stream(idle)
            .with_credential_provider(provider),
    )
    .unwrap()
}
async fn native_open(
    client: &NativeMcpClient,
    peer: &Peer,
    headers: std::collections::BTreeMap<String, String>,
) -> Result<McpClientSession, RuntimeFailure> {
    client
        .open(
            native_owner(),
            native_server(),
            McpEndpoint::Http {
                url: peer.url("public/mcp").to_string(),
                headers,
            },
            Limits::default(),
            Arc::new(McpCancellation::new()),
        )
        .await
}
fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
async fn native_call(
    session: &McpClientSession,
    name: &str,
) -> htlk_runtime::mcp::client::McpClientReply {
    session
        .request(
            "tools/call",
            doc(&format!(r#"{{"name":"{name}","arguments":{{}}}}"#)),
            Arc::new(McpCancellation::new()),
        )
        .await
}
fn native_observations(stats: &JsonDocument) -> &[Value] {
    let Value::Map(map) = stats.value() else {
        panic!()
    };
    let Some(Value::Array(requests)) = map.get("nativeRequests") else {
        panic!()
    };
    requests
}
fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Map(map) = value else { panic!() };
    map.get(name).unwrap()
}

#[tokio::test]
async fn native_http_rotates_oauth_credentials_on_control_posts_get_resumes_and_delete() {
    let peer = Peer::start("native", "none");
    let provider = Arc::new(RefreshingProvider::new(&peer));
    let client = native_client(provider.clone(), true);
    let concerns = client.authentication_concerns();
    assert_eq!(
        native_open(&client, &peer, Default::default())
            .await
            .err()
            .unwrap()
            .code(),
        FailureCode::Policy
    );
    let concern = concerns.borrow().clone().unwrap();
    assert_eq!(concern.binding, provider.binding);
    assert_eq!(concern.method, Method::POST);
    assert!(concern.request_id.is_some());
    let McpAuthenticationReason::Challenge {
        status: 401,
        challenge: Some(challenge),
    } = &concern.reason
    else {
        panic!("{concern:?}")
    };
    provider.authorize(challenge, 80).await;
    // Host explicitly reopens after authorization. The failed initialization was
    // not retried in the background and no old operation is replayed.
    assert_eq!(native_observations(&peer.stats().await).len(), 1);
    let session = native_open(&client, &peer, Default::default())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stats = peer.stats().await;
            if native_observations(&stats)
                .iter()
                .any(|r| field(r, "cursor") == &Value::Text("idle-1".into()))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        native_call(&session, "resume").await.delivery,
        McpDelivery::ResponseReceived
    );
    assert!(native_call(&session, "ping").await.result.unwrap().is_ok());
    session.close().await.unwrap();
    client.shutdown().await.unwrap();
    let stats = peer.stats().await;
    let observations = native_observations(&stats);
    let mut credentials = std::collections::BTreeSet::new();
    for request in observations.iter().skip(1) {
        assert_eq!(field(request, "valid"), &Value::Bool(true));
        assert!(
            credentials.insert(match field(request, "credential") {
                Value::Integer(n) => *n,
                _ => panic!(),
            }),
            "wire reused credentials"
        );
    }
    assert!(
        observations
            .iter()
            .any(|r| field(r, "method") == &Value::Text("DELETE".into()))
    );
    assert!(
        observations
            .iter()
            .any(|r| field(r, "rpc") == &Value::Text("response".into()))
    );
    assert!(
        observations
            .iter()
            .any(|r| matches!(field(r, "cursor"), Value::Text(s) if s.starts_with("resume-")))
    );
    assert_eq!(provider.calls.lock().unwrap().len(), observations.len());
    let Value::Map(map) = stats.value() else {
        panic!()
    };
    assert_eq!(
        map.get("nativeCalls"),
        Some(&Value::Integer(2)),
        "resumption must not repeat effectful POST"
    );
}

#[tokio::test]
async fn native_authentication_failures_are_structured_before_send_and_never_replay_scope_challenges()
 {
    let peer = Peer::start("native", "none");
    let provider = Arc::new(RefreshingProvider::new(&peer));
    provider.authorize(&peer.challenge().await, 90).await;
    let client = native_client(provider.clone(), false);
    let concerns = client.authentication_concerns();
    let session = native_open(&client, &peer, Default::default())
        .await
        .unwrap();
    let before = native_observations(&peer.stats().await).len();
    provider.fail.store(true, Ordering::Release);
    let reply = native_call(&session, "scope").await;
    assert_eq!(reply.delivery, McpDelivery::NotSent);
    assert_eq!(reply.result.unwrap_err().code(), FailureCode::Policy);
    assert_eq!(native_observations(&peer.stats().await).len(), before);
    let concern = concerns.borrow().clone().unwrap();
    assert!(matches!(
        concern.reason,
        McpAuthenticationReason::Credential(OAuthError::Denied)
    ));
    provider.fail.store(false, Ordering::Release);
    let reply = native_call(&session, "scope").await;
    assert_eq!(reply.delivery, McpDelivery::PossiblySent);
    assert_eq!(reply.result.unwrap_err().code(), FailureCode::Policy);
    let next = concerns.borrow().clone().unwrap();
    assert_eq!(next.sequence, concern.sequence + 1);
    let McpAuthenticationReason::Challenge {
        status: 403,
        challenge: Some(challenge),
    } = &next.reason
    else {
        panic!()
    };
    assert!(challenge.is_insufficient_scope());
    assert_eq!(challenge.scopes().unwrap(), ["read", "write"]);
    let stats = peer.stats().await;
    let Value::Map(map) = stats.value() else {
        panic!()
    };
    assert_eq!(map.get("nativeCalls"), Some(&Value::Integer(1)));
    provider.authorize(challenge, 92).await;
    // A fresh grant does not dispatch anything. Only the explicit host call below
    // sends the second request, under the normal operation authorization contract.
    assert_eq!(
        native_observations(&peer.stats().await).len(),
        native_observations(&stats).len()
    );
    assert!(native_call(&session, "scope").await.result.unwrap().is_ok());
    session.close().await.unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_conflicting_static_headers_and_initial_provider_failures_send_nothing() {
    let peer = Peer::start("native", "none");
    let provider = Arc::new(RefreshingProvider::new(&peer));
    let client = native_client(provider.clone(), false);
    assert_eq!(
        native_open(
            &client,
            &peer,
            [("aUtHoRiZaTiOn".into(), "Bearer static-secret".into())].into()
        )
        .await
        .err()
        .unwrap()
        .code(),
        FailureCode::McpIdentity
    );
    assert!(provider.calls.lock().unwrap().is_empty());
    assert!(native_observations(&peer.stats().await).is_empty());
    client.shutdown().await.unwrap();
    let config = McpClientConfig::new(McpClientLimits::default(), Default::default(), None)
        .unwrap()
        .with_credential_provider(provider.clone());
    let concerns = config.authentication_concerns();
    let client = NativeMcpClient::new(config).unwrap();
    provider.fail.store(true, Ordering::Release);
    assert_eq!(
        native_open(&client, &peer, Default::default())
            .await
            .err()
            .unwrap()
            .code(),
        FailureCode::Policy
    );
    assert!(native_observations(&peer.stats().await).is_empty());
    assert!(matches!(
        concerns.borrow().as_ref().unwrap().reason,
        McpAuthenticationReason::Credential(OAuthError::Denied)
    ));
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_get_control_delete_and_malformed_challenges_remain_host_visible() {
    for mode in [
        "native-get-challenge",
        "native-control-challenge",
        "native-delete-challenge",
        "native-invalid-challenge",
    ] {
        let peer = Peer::start(mode, "none");
        let provider = Arc::new(RefreshingProvider::new(&peer));
        if mode != "native-invalid-challenge" {
            provider.authorize(&peer.challenge().await, 100).await;
        }
        let client = native_client(provider, mode == "native-get-challenge");
        let concerns = client.authentication_concerns();
        let opened = native_open(&client, &peer, Default::default()).await;
        if mode == "native-get-challenge" {
            let session = opened.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while session.ended().is_none() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(session.ended(), Some(McpSessionEnd::Authentication));
            session.close().await.unwrap();
        } else if mode == "native-delete-challenge" {
            opened.unwrap().close().await.unwrap();
        } else {
            assert_eq!(opened.err().unwrap().code(), FailureCode::Policy);
        }
        let concern = concerns.borrow().clone().unwrap();
        let expected = if mode == "native-get-challenge" {
            Method::GET
        } else if mode == "native-delete-challenge" {
            Method::DELETE
        } else {
            Method::POST
        };
        assert_eq!(concern.method, expected, "{mode}");
        if mode == "native-invalid-challenge" {
            assert!(matches!(
                concern.reason,
                McpAuthenticationReason::InvalidChallenge {
                    status: 401,
                    error: OAuthError::Challenge
                }
            ));
        } else {
            assert!(matches!(
                concern.reason,
                McpAuthenticationReason::Challenge { .. }
            ));
        }
        client.shutdown().await.unwrap();
        let stats = peer.stats().await;
        let observations = native_observations(&stats);
        if mode == "native-control-challenge" {
            assert_eq!(
                observations
                    .iter()
                    .filter(|r| field(r, "rpc") == &Value::Text("notifications/initialized".into()))
                    .count(),
                1
            );
        } else if mode == "native-get-challenge" || mode == "native-delete-challenge" {
            assert_eq!(
                observations
                    .iter()
                    .filter(|r| field(r, "valid") == &Value::Bool(true)
                        && field(r, "method") == &Value::Text(expected.to_string()))
                    .count(),
                1
            );
        } else {
            assert_eq!(observations.len(), 1);
        }
    }
}

#[tokio::test]
async fn native_interrupted_credential_acquisition_is_bounded_visible_and_not_sent() {
    let peer = Peer::start("native", "none");
    let provider = Arc::new(RefreshingProvider::new(&peer));
    provider.stall.store(true, Ordering::Release);
    let config = McpClientConfig::new(
        McpClientLimits {
            timeout_ms: 80,
            cancellation_grace_ms: 10,
            ..McpClientLimits::default()
        },
        Default::default(),
        None,
    )
    .unwrap()
    .with_credential_provider(provider);
    let mut concerns = config.authentication_concerns();
    let client = NativeMcpClient::new(config).unwrap();
    assert!(
        native_open(&client, &peer, Default::default())
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(1), concerns.wait_for(|c| c.is_some()))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        concerns.borrow().as_ref().unwrap().reason,
        McpAuthenticationReason::AcquisitionInterrupted
            | McpAuthenticationReason::Credential(OAuthError::Limit)
    ));
    assert!(native_observations(&peer.stats().await).is_empty());
    client.shutdown().await.unwrap();
}
