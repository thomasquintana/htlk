//! Bounded MCP HTTP OAuth authorization-code/PKCE implementation.
//!
//! The host supplies CSPRNG entropy, presents the authorization URL, receives the
//! callback, and persists returned secrets. This module performs discovery, request
//! construction, callback validation, code exchange, and refresh. It never navigates
//! automatically or retries an MCP operation. In particular, acquiring a token is
//! not evidence that replaying an interrupted effectful POST is safe.
//!
//! This profile supports pre-registered public clients (`none`) and confidential
//! clients (`client_secret_basic` / `client_secret_post`). Dynamic registration,
//! Client ID Metadata Documents, ID-token authentication, and independently
//! versioned MCP authorization extensions are deliberately not implemented. OIDC
//! discovery is supported as an OAuth metadata source, not as a login protocol.
//! Stdio peers continue to obtain credentials through host environment configuration.
//!
//! Pre-registration provides a pinned issuer and an already registered exact
//! redirect. CIMD is deferred because publishing and maintaining a conforming
//! HTTPS client document is a separate host capability and draft-version contract.
//! Optional dynamic registration is deferred because it creates remote client
//! records and requires host registration/secret-lifecycle policy. Neither is
//! silently substituted for missing pre-registration or advertised as supported.

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use htlk_executable::{
    JsonDocument, ServerIdentity,
    cbor::{Limits, Map, Value},
};
use reqwest::{
    Client, Method, StatusCode, Url,
    header::{HeaderMap, HeaderValue},
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

/// Redacted, stable failure categories. Remote response bodies, URLs, codes,
/// states, client secrets, and tokens never appear in errors or their sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthError {
    /// Invalid local registration, binding, entropy shape, or limits.
    Configuration,
    /// Endpoint was not allowed by both URL security and host trust policy.
    UntrustedEndpoint,
    /// Malformed, ambiguous, or unsupported bearer challenge.
    Challenge,
    /// Invalid, mismatched, or unsupported discovery metadata.
    Metadata,
    /// No supported discovery document was found.
    DiscoveryUnavailable,
    /// Request count, byte, challenge, or time budget exhausted.
    Limit,
    /// Transport failure, including an interrupted token exchange. No retry occurs.
    Transport,
    /// Redirect responses are never followed by the protocol client.
    Redirect,
    /// Callback does not match the registered redirect, state, issuer, or resource.
    Callback,
    /// User or authorization server rejected authorization.
    Denied,
    /// Token endpoint rejected the request; remote diagnostics remain private.
    TokenRejected,
    /// Refresh was rejected with invalid_grant; new host authorization is required.
    RefreshRejected,
    /// Invalid or unsupported token response.
    Token,
    /// Credentials do not belong to the exact requested binding/client/issuer.
    Binding,
    /// Access token has expired or no expiry was supplied; refresh/authorize first.
    Expired,
}
impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP OAuth {self:?}")
    }
}
impl std::error::Error for OAuthError {}

/// Exact trusted scope for a credential. `server` is the host's compound server
/// identity and `principal` is its authenticated principal, not peer-provided text.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialBinding {
    server: String,
    principal: String,
    resource: Url,
}
impl CredentialBinding {
    /// Derives the server key from the exact compound identity using the same
    /// canonical digest as native sessions. Principal comes from trusted ownership.
    pub fn for_server(
        server: &ServerIdentity,
        principal: String,
        resource: Url,
        limits: &Limits,
    ) -> Result<Self, OAuthError> {
        let server = server
            .digest(limits)
            .map_err(|_| OAuthError::Configuration)?
            .to_string();
        Self::new(server, principal, resource)
    }
    /// Constructs a bounded exact scope. Resource URLs must not contain userinfo,
    /// fragments, or query parameters. No parent-path audience widening is inferred.
    pub fn new(server: String, principal: String, resource: Url) -> Result<Self, OAuthError> {
        if server.is_empty()
            || principal.is_empty()
            || server.len() > 4096
            || principal.len() > 4096
            || !http_url(&resource)
            || resource.query().is_some()
        {
            return Err(OAuthError::Configuration);
        }
        Ok(Self {
            server,
            principal,
            resource,
        })
    }
    /// Exact host server identity.
    pub fn server(&self) -> &str {
        &self.server
    }
    /// Exact host-authenticated principal.
    pub fn principal(&self) -> &str {
        &self.principal
    }
    /// Exact protected resource indicator and credential destination.
    pub fn resource(&self) -> &Url {
        &self.resource
    }
}
impl fmt::Debug for CredentialBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialBinding([redacted])")
    }
}

/// Trust roles are separate: trusting an MCP resource does not authorize fetching
/// arbitrary metadata URLs or sending client secrets to advertised token endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthEndpointKind {
    /// Protected-resource metadata, including header-provided URLs.
    ResourceMetadata,
    /// OAuth/OIDC issuer discovery document.
    AuthorizationServerMetadata,
    /// URL handed to the host for explicit browser navigation.
    Authorization,
    /// Destination of authorization codes, verifiers, and client/refresh secrets.
    Token,
}

/// Trusted host network policy. Called before each request or navigation URL is
/// produced. Implementations must authorize the complete URL for its role and
/// binding; DNS/egress restrictions belong to the host's network environment.
pub trait OAuthEndpointPolicy: Send + Sync {
    /// Whether this specific URL may be used in this role for this binding.
    fn allows(&self, binding: &CredentialBinding, kind: OAuthEndpointKind, url: &Url) -> bool;
}

/// Finite bounds for a discovery/exchange/refresh operation. Host interaction time
/// is separately bounded by `authorization_lifetime`, not by the network timeout.
#[derive(Clone, Debug)]
pub struct OAuthLimits {
    /// Maximum bytes per JSON document, callback, or challenge header collection.
    pub bytes: usize,
    /// Total network time per public operation, including body reads and fallbacks.
    pub timeout: Duration,
    /// Maximum discovery requests, cumulative across resource and AS fallbacks.
    pub discovery_requests: usize,
    /// Maximum bearer challenges for one original MCP operation.
    pub challenges: usize,
    /// Maximum lifetime of a pending authorization (monotonic, process-local).
    pub authorization_lifetime: Duration,
    /// Explicit test/development exception, restricted to numeric loopback IPs.
    /// Host endpoint policy must still approve each HTTP URL.
    pub allow_http_loopback: bool,
}
impl Default for OAuthLimits {
    fn default() -> Self {
        Self {
            bytes: 64 * 1024,
            timeout: Duration::from_secs(15),
            discovery_requests: 5,
            challenges: 2,
            authorization_lifetime: Duration::from_secs(600),
            allow_http_loopback: false,
        }
    }
}
impl OAuthLimits {
    fn validate(&self) -> Result<(), OAuthError> {
        if self.bytes == 0
            || self.bytes > 1024 * 1024
            || self.discovery_requests == 0
            || self.discovery_requests > 16
            || self.challenges == 0
            || self.challenges > 8
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(300)
            || self.authorization_lifetime.is_zero()
            || self.authorization_lifetime > Duration::from_secs(3600)
        {
            return Err(OAuthError::Configuration);
        }
        Ok(())
    }
    fn json(&self) -> Limits {
        Limits {
            max_document_bytes: self.bytes,
            ..Limits::default()
        }
    }
}

/// Host-provided pre-registration. Debug never reveals the client identifier or
/// secret. Client authentication is explicit rather than inferred from a secret.
pub struct OAuthRegistration {
    issuer: Url,
    client_id: String,
    redirect: Url,
    authentication: OAuthClientAuthentication,
}
/// Token endpoint client authentication for pre-registered clients.
pub enum OAuthClientAuthentication {
    /// Public client; PKCE remains mandatory.
    None,
    /// HTTP Basic with OAuth form-encoded identifier and secret.
    ClientSecretBasic(String),
    /// Secret sent in the token request body, only when advertised by metadata.
    ClientSecretPost(String),
}
impl fmt::Debug for OAuthClientAuthentication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "None",
            Self::ClientSecretBasic(_) => "ClientSecretBasic([redacted])",
            Self::ClientSecretPost(_) => "ClientSecretPost([redacted])",
        })
    }
}
impl OAuthRegistration {
    /// Validates exact registered issuer and redirect. Redirects may use HTTPS or
    /// numeric loopback HTTP; callback query/fragment and issuer query are forbidden.
    pub fn new(
        issuer: Url,
        client_id: String,
        redirect: Url,
        authentication: OAuthClientAuthentication,
    ) -> Result<Self, OAuthError> {
        let secret = match &authentication {
            OAuthClientAuthentication::None => None,
            OAuthClientAuthentication::ClientSecretBasic(s)
            | OAuthClientAuthentication::ClientSecretPost(s) => Some(s),
        };
        if !http_url(&issuer)
            || issuer.query().is_some()
            || !http_url(&redirect)
            || redirect.query().is_some()
            || !(redirect.scheme() == "https" || loopback(&redirect))
            || client_id.is_empty()
            || client_id.len() > 4096
            || client_id.chars().any(char::is_control)
            || secret.is_some_and(|s| s.is_empty() || s.len() > 4096)
        {
            return Err(OAuthError::Configuration);
        }
        Ok(Self {
            issuer,
            client_id,
            redirect,
            authentication,
        })
    }
    fn auth_method(&self) -> &str {
        match self.authentication {
            OAuthClientAuthentication::None => "none",
            OAuthClientAuthentication::ClientSecretBasic(_) => "client_secret_basic",
            OAuthClientAuthentication::ClientSecretPost(_) => "client_secret_post",
        }
    }
}
impl fmt::Debug for OAuthRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthRegistration([redacted])")
    }
}

/// Parsed bearer challenge. Unknown diagnostics are discarded rather than logged.
#[derive(Clone)]
pub struct OAuthChallenge {
    metadata: Option<Url>,
    scopes: Option<Vec<String>>,
    insufficient_scope: bool,
}

/// Protected reason that native HTTP needs host authentication attention.
#[derive(Clone, Debug)]
pub enum McpAuthenticationReason {
    /// Credential acquisition failed before this HTTP request was sent.
    Credential(OAuthError),
    /// Acquisition was dropped by the caller/owner deadline or cancellation
    /// before this HTTP request was sent. No credential result was obtained.
    AcquisitionInterrupted,
    /// Resource returned 401/403. The parsed challenge may be absent for a
    /// non-OAuth 403; this never grants authority to replay an MCP operation.
    Challenge {
        /// HTTP status code, independent of JSON-RPC errors.
        status: u16,
        /// Bounded bearer challenge; remote diagnostic text is discarded.
        challenge: Option<OAuthChallenge>,
    },
    /// Resource returned a malformed, ambiguous, or over-budget challenge.
    InvalidChallenge {
        /// HTTP status code.
        status: u16,
        /// Redacted parser failure.
        error: OAuthError,
    },
}

/// Bounded process-local authentication concern. Contains protected host identity
/// and challenge data; consumers must authorize presentation/persistence. Debug
/// excludes identities, URLs and remote values. No concern retries an operation.
#[derive(Clone)]
pub struct McpAuthenticationConcern {
    /// Monotonic configuration-scoped sequence. Gaps mean overwritten concerns.
    pub sequence: u64,
    /// Exact trusted credential scope.
    pub binding: CredentialBinding,
    /// Process-local session epoch, including failed initialization attempts.
    pub epoch: u64,
    /// HTTP method that failed acquisition or received a challenge.
    pub method: Method,
    /// Client-origin RPC ID when this request belongs to an outgoing operation.
    pub request_id: Option<super::McpRequestId>,
    /// Structured local acquisition failure or HTTP challenge.
    pub reason: McpAuthenticationReason,
}
impl fmt::Debug for McpAuthenticationConcern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpAuthenticationConcern")
            .field("sequence", &self.sequence)
            .field("epoch", &self.epoch)
            .field("method", &self.method)
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}
/// Latest-value authentication subscription. Slow consumers detect loss through
/// sequence gaps. The latest concern survives failed open/close and event overflow.
pub type McpAuthenticationConcerns =
    tokio::sync::watch::Receiver<Option<Arc<McpAuthenticationConcern>>>;

#[derive(Clone)]
pub(super) struct AuthenticationMonitor(
    tokio::sync::watch::Sender<Option<Arc<McpAuthenticationConcern>>>,
);
impl AuthenticationMonitor {
    pub fn new() -> Self {
        Self(tokio::sync::watch::channel(None).0)
    }
    pub fn subscribe(&self) -> McpAuthenticationConcerns {
        self.0.subscribe()
    }
    pub fn record(&self, mut concern: McpAuthenticationConcern) {
        self.0.send_modify(|latest| {
            concern.sequence = latest.as_ref().map_or(1, |c| c.sequence.saturating_add(1));
            *latest = Some(Arc::new(concern));
        });
    }
}
impl OAuthChallenge {
    /// Parses all WWW-Authenticate fields, including quoted commas/escapes and
    /// other schemes. Ambiguous bearer challenges and duplicate parameters fail.
    /// An unadorned 401 yields well-known discovery; unrelated statuses yield None.
    pub fn from_headers(
        status: StatusCode,
        headers: &HeaderMap,
        limits: &OAuthLimits,
    ) -> Result<Option<Self>, OAuthError> {
        limits.validate()?;
        if status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN {
            return Ok(None);
        }
        let mut bytes = 0usize;
        let mut bearer: Option<BTreeMap<String, String>> = None;
        for header in headers.get_all("www-authenticate") {
            bytes = bytes
                .checked_add(header.as_bytes().len())
                .ok_or(OAuthError::Limit)?;
            if bytes > limits.bytes {
                return Err(OAuthError::Limit);
            }
            parse_challenges(
                header.to_str().map_err(|_| OAuthError::Challenge)?,
                &mut bearer,
            )?;
        }
        let params = bearer.unwrap_or_default();
        let insufficient_scope = params
            .get("error")
            .is_some_and(|e| e == "insufficient_scope");
        if status == StatusCode::FORBIDDEN && !insufficient_scope {
            return Ok(None);
        }
        if params
            .get("error")
            .is_some_and(|e| e != "invalid_token" && e != "insufficient_scope")
        {
            return Err(OAuthError::Challenge);
        }
        let metadata = params
            .get("resource_metadata")
            .map(|s| Url::parse(s).map_err(|_| OAuthError::Challenge))
            .transpose()?;
        let scopes = params
            .get("scope")
            .map(|s| parse_scopes(s, OAuthError::Challenge))
            .transpose()?;
        Ok(Some(Self {
            metadata,
            scopes,
            insufficient_scope,
        }))
    }
    /// Authoritative requested scopes, which need not be a subset of metadata.
    pub fn scopes(&self) -> Option<&[String]> {
        self.scopes.as_deref()
    }
    /// Whether the resource specifically requested a scope step-up.
    pub fn is_insufficient_scope(&self) -> bool {
        self.insufficient_scope
    }
}
impl fmt::Debug for OAuthChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthChallenge([redacted])")
    }
}

/// Per-original-operation challenge budget. Reuse it across refresh and scope
/// upgrades; do not reset it just because a new token was acquired. This type
/// grants no permission to retry or replay an MCP request.
#[derive(Debug)]
pub struct OAuthChallengeBudget {
    remaining: usize,
    seen: BTreeSet<[u8; 32]>,
}
impl OAuthChallengeBudget {
    /// Creates a finite budget from validated limits.
    pub fn new(limits: &OAuthLimits) -> Result<Self, OAuthError> {
        limits.validate()?;
        Ok(Self {
            remaining: limits.challenges,
            seen: BTreeSet::new(),
        })
    }
    /// Consumes a challenge; repeated identical challenges fail immediately.
    pub fn admit(&mut self, challenge: &OAuthChallenge) -> Result<(), OAuthError> {
        let mut h = Sha256::new();
        h.update([u8::from(challenge.insufficient_scope)]);
        if let Some(url) = &challenge.metadata {
            h.update(url.as_str());
        }
        h.update([0]);
        if let Some(scopes) = &challenge.scopes {
            for scope in scopes {
                h.update(scope);
                h.update([0]);
            }
        }
        if self.remaining == 0 || !self.seen.insert(h.finalize().into()) {
            return Err(OAuthError::Limit);
        }
        self.remaining -= 1;
        Ok(())
    }
}

/// Validated discovery result, pinned to the exact binding and registration.
pub struct OAuthDiscovery {
    binding: CredentialBinding,
    issuer: Url,
    issuer_identifier: String,
    authorization: Url,
    token: Url,
    scopes: Option<Vec<String>>,
    refresh: bool,
    issuer_in_callback: bool,
    client_id: String,
}
impl OAuthDiscovery {
    /// Scopes selected from the challenge, otherwise protected-resource metadata.
    pub fn scopes(&self) -> Option<&[String]> {
        self.scopes.as_deref()
    }
    /// Exact trusted issuer, validated against both registration and AS metadata.
    pub fn issuer(&self) -> &Url {
        &self.issuer
    }
}
impl fmt::Debug for OAuthDiscovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthDiscovery([redacted])")
    }
}

/// Independent 256-bit random values supplied by the host's cryptographic RNG.
/// Never reuse either value, derive them from user input, or log them. Supplying
/// entropy explicitly keeps RNG/platform dependencies at the host boundary.
pub struct OAuthEntropy {
    state: [u8; 32],
    verifier: [u8; 32],
}
impl OAuthEntropy {
    /// Takes two independently generated CSPRNG outputs. Shape checks do not prove
    /// randomness; the host is responsible for their cryptographic generation.
    pub fn new(state: [u8; 32], verifier: [u8; 32]) -> Result<Self, OAuthError> {
        if state == verifier || state == [0; 32] || verifier == [0; 32] {
            return Err(OAuthError::Configuration);
        }
        Ok(Self { state, verifier })
    }
}
impl fmt::Debug for OAuthEntropy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthEntropy([redacted])")
    }
}

/// Single-use authorization transaction. No Clone/serialization: consuming it
/// prevents code-exchange replay, including after uncertain exchange delivery.
pub struct OAuthPendingAuthorization {
    discovery: OAuthDiscovery,
    url: Url,
    redirect: Url,
    state: String,
    verifier: String,
    expires: Instant,
}
impl OAuthPendingAuthorization {
    /// Protected URL for host-controlled consent/navigation. Do not log it.
    pub fn authorization_url(&self) -> &Url {
        &self.url
    }
}
impl fmt::Debug for OAuthPendingAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthPendingAuthorization([redacted])")
    }
}

/// Validated, resource/principal/registration-bound token secrets. The host may
/// persist explicit secret accessors in its own protected store. No automatic
/// persistence or Debug disclosure occurs, and refresh consumes this value.
pub struct OAuthTokens {
    binding: CredentialBinding,
    issuer: Url,
    client_id: String,
    access: String,
    refresh: Option<String>,
    scopes: Option<Vec<String>>,
    expires: Option<Instant>,
    expires_in: Option<Duration>,
}
impl OAuthTokens {
    /// Protected access-token secret, solely for host secret persistence.
    pub fn access_token(&self) -> &str {
        &self.access
    }
    /// Protected refresh-token secret, solely for host secret persistence.
    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh.as_deref()
    }
    /// Granted scopes. Omitted scope inherits the request's scopes.
    pub fn scopes(&self) -> Option<&[String]> {
        self.scopes.as_deref()
    }
    /// AS-reported lifetime, measured conservatively from request dispatch.
    pub fn expires_in(&self) -> Option<Duration> {
        self.expires_in
    }
    /// Exact binding to use as part of the host persistence key.
    pub fn binding(&self) -> &CredentialBinding {
        &self.binding
    }
    /// Builds a sensitive Authorization header only for the exact binding and
    /// exact resource URL. No prefix/origin-only matching or token-in-URL occurs.
    /// Unknown expiry requires explicit refresh rather than indefinite token use.
    pub fn authorization(
        &self,
        binding: &CredentialBinding,
        destination: &Url,
    ) -> Result<OAuthAuthorization, OAuthError> {
        if binding != &self.binding || destination != self.binding.resource() {
            return Err(OAuthError::Binding);
        }
        if self.expires.is_none_or(|expires| Instant::now() >= expires) {
            return Err(OAuthError::Expired);
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {}", self.access))
            .map_err(|_| OAuthError::Token)?;
        value.set_sensitive(true);
        Ok(OAuthAuthorization(value))
    }
}
impl fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthTokens([redacted])")
    }
}

/// Sensitive header wrapper: Debug never prints the credential.
pub struct OAuthAuthorization(HeaderValue);
impl OAuthAuthorization {
    /// Applies the credential as an Authorization header. The transport must have
    /// disabled redirects and bound the request URL before calling its provider.
    pub fn apply(self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header(reqwest::header::AUTHORIZATION, self.0)
    }
}
impl fmt::Debug for OAuthAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthAuthorization([redacted])")
    }
}

/// Async credential-provider result for native transport integration.
pub type OAuthCredentialFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<OAuthAuthorization>, OAuthError>> + Send + 'a>>;
/// Host-managed credential lookup/refresh boundary. Native HTTP must invoke this
/// on **every** POST (including responses/notifications), GET/resumption, and
/// DELETE. Implementations must verify exact server/resource/principal binding.
/// Acquisition does not authorize an MCP operation replay. The host owns storage,
/// navigation, refresh serialization, and its addressed authorization interaction.
pub trait McpCredentialProvider: Send + Sync {
    /// Returns current credentials, optionally refreshing before dispatch. `None`
    /// permits an unauthenticated first request to obtain a bearer challenge.
    fn authorization<'a>(
        &'a self,
        binding: &'a CredentialBinding,
        method: &'a Method,
        destination: &'a Url,
    ) -> OAuthCredentialFuture<'a>;
}

/// A validated token set is a read-only provider. Expiry fails explicitly; hosts
/// needing serialized refresh or persistence implement the same trait around
/// `OAuthClient` and their protected store.
impl McpCredentialProvider for OAuthTokens {
    fn authorization<'a>(
        &'a self,
        binding: &'a CredentialBinding,
        _method: &'a Method,
        destination: &'a Url,
    ) -> OAuthCredentialFuture<'a> {
        Box::pin(async move { OAuthTokens::authorization(self, binding, destination).map(Some) })
    }
}

/// OAuth mechanism with explicit host trust, pre-registration and network bounds.
/// No internal retry loop or secret cache is hidden behind this type.
pub struct OAuthClient {
    binding: CredentialBinding,
    registration: OAuthRegistration,
    policy: Arc<dyn OAuthEndpointPolicy>,
    limits: OAuthLimits,
    http: Client,
}
impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthClient([redacted])")
    }
}
impl OAuthClient {
    /// Creates a redirect-disabled, proxy-independent protocol client. Ambient
    /// proxy settings are not used for token/metadata requests. No I/O occurs.
    pub fn new(
        binding: CredentialBinding,
        registration: OAuthRegistration,
        policy: Arc<dyn OAuthEndpointPolicy>,
        limits: OAuthLimits,
    ) -> Result<Self, OAuthError> {
        limits.validate()?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .build()
            .map_err(|_| OAuthError::Transport)?;
        let client = Self {
            binding,
            registration,
            policy,
            limits,
            http,
        };
        client.secure(client.binding.resource())?;
        client.secure(&client.registration.issuer)?;
        Ok(client)
    }
    /// Rehydrates trusted host-persisted secrets for this exact binding and
    /// registration. The host must key its store by server, principal, resource,
    /// issuer and client ID, and compute remaining lifetime from a trusted clock.
    /// `None` lifetime permits refresh but never access-token dispatch. This API
    /// does not validate a token's signature/audience; those are the resource
    /// server's responsibility. Never import arbitrary peer-provided credentials.
    pub fn restore_tokens(
        &self,
        access: String,
        refresh: Option<String>,
        scopes: Option<Vec<String>>,
        remaining_lifetime: Option<Duration>,
    ) -> Result<OAuthTokens, OAuthError> {
        if !valid_access(&access)
            || access.len() > self.limits.bytes
            || refresh.as_ref().is_some_and(|s| {
                s.is_empty() || s.len() > self.limits.bytes || s.chars().any(char::is_control)
            })
        {
            return Err(OAuthError::Token);
        }
        let scopes = scopes.map(validate_scopes).transpose()?;
        let expires = remaining_lifetime
            .map(|d| Instant::now().checked_add(d).ok_or(OAuthError::Token))
            .transpose()?;
        Ok(OAuthTokens {
            binding: self.binding.clone(),
            issuer: self.registration.issuer.clone(),
            client_id: self.registration.client_id.clone(),
            access,
            refresh,
            scopes,
            expires,
            expires_in: remaining_lifetime,
        })
    }
    /// Discovers protected-resource metadata, then the pre-registered issuer's
    /// OAuth/OIDC metadata in specification priority order. Header-provided metadata
    /// is authoritative; it is not silently replaced by well-known data on failure.
    /// Only 404/405 fall through; redirects, bad metadata and trust failures abort.
    pub async fn discover(&self, challenge: &OAuthChallenge) -> Result<OAuthDiscovery, OAuthError> {
        let deadline = Instant::now() + self.limits.timeout;
        let mut remaining = self.limits.discovery_requests;
        let resources = match &challenge.metadata {
            Some(url) => vec![url.clone()],
            None => resource_locations(self.binding.resource()),
        };
        let resource = self
            .first_document(
                resources,
                OAuthEndpointKind::ResourceMetadata,
                deadline,
                &mut remaining,
            )
            .await?;
        let map = object(resource.value(), OAuthError::Metadata)?;
        let audience = Url::parse(text(map, "resource", OAuthError::Metadata)?)
            .map_err(|_| OAuthError::Metadata)?;
        if audience != self.binding.resource {
            return Err(OAuthError::Metadata);
        }
        let issuers = strings(map, "authorization_servers", OAuthError::Metadata)?
            .ok_or(OAuthError::Metadata)?;
        if issuers.is_empty() || issuers.len() > 16 {
            return Err(OAuthError::Metadata);
        }
        let issuer_identifier = issuers
            .iter()
            .find(|s| Url::parse(s).is_ok_and(|u| u == self.registration.issuer))
            .cloned()
            .ok_or(OAuthError::Metadata)?;
        let supported_scopes = strings(map, "scopes_supported", OAuthError::Metadata)?
            .map(validate_scopes)
            .transpose()?;
        let scopes = challenge.scopes.clone().or(supported_scopes);
        let metadata = self
            .first_document(
                issuer_locations(&self.registration.issuer),
                OAuthEndpointKind::AuthorizationServerMetadata,
                deadline,
                &mut remaining,
            )
            .await?;
        let map = object(metadata.value(), OAuthError::Metadata)?;
        // Exact issuer string comparison is required by RFC 8414 (not URL-normalized).
        if text(map, "issuer", OAuthError::Metadata)? != issuer_identifier {
            return Err(OAuthError::Metadata);
        }
        let methods = strings(
            map,
            "code_challenge_methods_supported",
            OAuthError::Metadata,
        )?
        .ok_or(OAuthError::Metadata)?;
        let responses = strings(map, "response_types_supported", OAuthError::Metadata)?
            .ok_or(OAuthError::Metadata)?;
        if !methods.iter().any(|s| s == "S256") || !responses.iter().any(|s| s == "code") {
            return Err(OAuthError::Metadata);
        }
        let grants = strings(map, "grant_types_supported", OAuthError::Metadata)?;
        let issuer_in_callback = match map.get("authorization_response_iss_parameter_supported") {
            None => false,
            Some(Value::Bool(value)) => *value,
            _ => return Err(OAuthError::Metadata),
        };
        if grants
            .as_ref()
            .is_some_and(|g| !g.iter().any(|s| s == "authorization_code"))
        {
            return Err(OAuthError::Metadata);
        }
        let auth = strings(
            map,
            "token_endpoint_auth_methods_supported",
            OAuthError::Metadata,
        )?
        .unwrap_or_else(|| vec!["client_secret_basic".into()]);
        if !auth.iter().any(|s| s == self.registration.auth_method()) {
            return Err(OAuthError::Metadata);
        }
        let authorization = Url::parse(text(map, "authorization_endpoint", OAuthError::Metadata)?)
            .map_err(|_| OAuthError::Metadata)?;
        let token = Url::parse(text(map, "token_endpoint", OAuthError::Metadata)?)
            .map_err(|_| OAuthError::Metadata)?;
        self.endpoint(OAuthEndpointKind::Authorization, &authorization)?;
        self.endpoint(OAuthEndpointKind::Token, &token)?;
        // Reserved query parameters would create ambiguous authorization requests.
        if authorization
            .query_pairs()
            .any(|(k, _)| reserved_authorization(&k))
        {
            return Err(OAuthError::Metadata);
        }
        Ok(OAuthDiscovery {
            binding: self.binding.clone(),
            issuer: self.registration.issuer.clone(),
            issuer_identifier,
            authorization,
            token,
            scopes,
            refresh: grants.is_none_or(|g| g.iter().any(|s| s == "refresh_token")),
            issuer_in_callback,
            client_id: self.registration.client_id.clone(),
        })
    }
    /// Builds an S256 authorization URL with exact redirect, resource and fresh
    /// state. The returned transaction must be kept private until its callback.
    pub fn begin_authorization(
        &self,
        discovery: OAuthDiscovery,
        entropy: OAuthEntropy,
    ) -> Result<OAuthPendingAuthorization, OAuthError> {
        self.check_discovery(&discovery)?;
        self.endpoint(OAuthEndpointKind::Authorization, &discovery.authorization)?;
        let state = URL_SAFE_NO_PAD.encode(entropy.state);
        let verifier = URL_SAFE_NO_PAD.encode(entropy.verifier);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = discovery.authorization.clone();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.registration.client_id)
                .append_pair("redirect_uri", self.registration.redirect.as_str())
                .append_pair("state", &state)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("resource", self.binding.resource.as_str());
            if let Some(scopes) = &discovery.scopes
                && !scopes.is_empty()
            {
                query.append_pair("scope", &scopes.join(" "));
            }
        }
        if url.as_str().len() > self.limits.bytes {
            return Err(OAuthError::Limit);
        }
        self.endpoint(OAuthEndpointKind::Authorization, &url)?;
        Ok(OAuthPendingAuthorization {
            discovery,
            url,
            redirect: self.registration.redirect.clone(),
            state,
            verifier,
            expires: Instant::now() + self.limits.authorization_lifetime,
        })
    }
    /// Validates a host-received full callback URL, consumes the transaction, and
    /// exchanges its code exactly once. Bad/expired callbacks produce zero token
    /// requests. A lost exchange response requires explicit reconciliation/new
    /// authorization, never an automatic second code exchange.
    pub async fn finish_authorization(
        &self,
        pending: OAuthPendingAuthorization,
        callback: &Url,
    ) -> Result<OAuthTokens, OAuthError> {
        self.check_discovery(&pending.discovery)?;
        if pending.redirect != self.registration.redirect || Instant::now() >= pending.expires {
            return Err(OAuthError::Callback);
        }
        let code = self.callback(&pending, callback)?;
        let fields = vec![
            ("grant_type", "authorization_code".into()),
            ("code", code),
            ("redirect_uri", pending.redirect.to_string()),
            ("code_verifier", pending.verifier),
        ];
        self.token_request(&pending.discovery, fields, None).await
    }
    /// Refreshes once, consuming old tokens to discourage reuse after ambiguous
    /// delivery or rotation. A host that persisted the old refresh token must mark
    /// it unusable/uncertain before dispatch and atomically replace it afterward.
    /// Omitted refresh_token inherits the prior value; present values rotate it.
    pub async fn refresh(
        &self,
        discovery: &OAuthDiscovery,
        previous: OAuthTokens,
    ) -> Result<OAuthTokens, OAuthError> {
        self.check_discovery(discovery)?;
        if previous.binding != self.binding
            || previous.issuer != self.registration.issuer
            || previous.client_id != self.registration.client_id
        {
            return Err(OAuthError::Binding);
        }
        if !discovery.refresh {
            return Err(OAuthError::TokenRejected);
        }
        let refresh = previous.refresh.ok_or(OAuthError::RefreshRejected)?;
        let fields = vec![
            ("grant_type", "refresh_token".into()),
            ("refresh_token", refresh.clone()),
        ];
        self.token_request(discovery, fields, Some((refresh, previous.scopes)))
            .await
    }
    fn secure(&self, url: &Url) -> Result<(), OAuthError> {
        if !http_url(url)
            || url.as_str().len() > self.limits.bytes
            || !(url.scheme() == "https" || (self.limits.allow_http_loopback && loopback(url)))
        {
            return Err(OAuthError::UntrustedEndpoint);
        }
        Ok(())
    }
    fn endpoint(&self, kind: OAuthEndpointKind, url: &Url) -> Result<(), OAuthError> {
        self.secure(url)?;
        if !self.policy.allows(&self.binding, kind, url) {
            return Err(OAuthError::UntrustedEndpoint);
        }
        Ok(())
    }
    fn check_discovery(&self, d: &OAuthDiscovery) -> Result<(), OAuthError> {
        if d.binding != self.binding
            || d.issuer != self.registration.issuer
            || d.client_id != self.registration.client_id
        {
            return Err(OAuthError::Binding);
        }
        Ok(())
    }
    async fn first_document(
        &self,
        locations: Vec<Url>,
        kind: OAuthEndpointKind,
        deadline: Instant,
        remaining: &mut usize,
    ) -> Result<JsonDocument, OAuthError> {
        for url in locations {
            self.endpoint(kind, &url)?;
            if *remaining == 0 {
                return Err(OAuthError::Limit);
            }
            *remaining -= 1;
            let timeout = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or(OAuthError::Limit)?;
            let response = self
                .http
                .get(url)
                .header("accept", "application/json")
                .timeout(timeout)
                .send()
                .await
                .map_err(network_error)?;
            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ) {
                continue;
            }
            if response.status().is_redirection() {
                return Err(OAuthError::Redirect);
            }
            if response.status() != StatusCode::OK {
                return Err(OAuthError::Metadata);
            }
            return self.document(response).await;
        }
        Err(OAuthError::DiscoveryUnavailable)
    }
    async fn document(&self, mut response: reqwest::Response) -> Result<JsonDocument, OAuthError> {
        if response
            .content_length()
            .is_some_and(|n| n > self.limits.bytes as u64)
        {
            return Err(OAuthError::Limit);
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim);
        if !mime.is_some_and(|s| s.eq_ignore_ascii_case("application/json")) {
            return Err(OAuthError::Metadata);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if chunk.len() > self.limits.bytes.saturating_sub(bytes.len()) {
                return Err(OAuthError::Limit);
            }
            bytes.extend_from_slice(&chunk);
        }
        JsonDocument::new(&bytes, &self.limits.json()).map_err(|_| OAuthError::Metadata)
    }
    fn callback(
        &self,
        pending: &OAuthPendingAuthorization,
        callback: &Url,
    ) -> Result<String, OAuthError> {
        if callback.as_str().len() > self.limits.bytes || callback.fragment().is_some() {
            return Err(OAuthError::Callback);
        }
        let mut base = callback.clone();
        base.set_query(None);
        if base != pending.redirect {
            return Err(OAuthError::Callback);
        }
        let mut params = BTreeMap::new();
        for (key, value) in callback.query_pairs() {
            if params
                .insert(key.into_owned(), value.into_owned())
                .is_some()
            {
                return Err(OAuthError::Callback);
            }
        }
        if params.get("state") != Some(&pending.state) {
            return Err(OAuthError::Callback);
        }
        if params
            .get("iss")
            .is_some_and(|s| s != &pending.discovery.issuer_identifier)
            || params
                .get("resource")
                .is_some_and(|s| s != self.binding.resource.as_str())
        {
            return Err(OAuthError::Callback);
        }
        if pending.discovery.issuer_in_callback && !params.contains_key("iss") {
            return Err(OAuthError::Callback);
        }
        if params.contains_key("error") {
            if params.contains_key("code") {
                return Err(OAuthError::Callback);
            }
            return Err(OAuthError::Denied);
        }
        params
            .remove("code")
            .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
            .ok_or(OAuthError::Callback)
    }
    async fn token_request(
        &self,
        discovery: &OAuthDiscovery,
        mut fields: Vec<(&str, String)>,
        prior: Option<(String, Option<Vec<String>>)>,
    ) -> Result<OAuthTokens, OAuthError> {
        self.endpoint(OAuthEndpointKind::Token, &discovery.token)?;
        fields.push(("resource", self.binding.resource.to_string()));
        let mut request = self
            .http
            .post(discovery.token.clone())
            .timeout(self.limits.timeout)
            .header("accept", "application/json");
        match &self.registration.authentication {
            OAuthClientAuthentication::None => {
                fields.push(("client_id", self.registration.client_id.clone()))
            }
            OAuthClientAuthentication::ClientSecretPost(secret) => {
                fields.push(("client_id", self.registration.client_id.clone()));
                fields.push(("client_secret", secret.clone()));
            }
            OAuthClientAuthentication::ClientSecretBasic(secret) => {
                let raw = format!(
                    "{}:{}",
                    form_component(&self.registration.client_id),
                    form_component(secret)
                );
                let mut header = HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(raw)))
                    .map_err(|_| OAuthError::Configuration)?;
                header.set_sensitive(true);
                request = request.header("authorization", header);
            }
        }
        // No reqwest form/json feature or serde dependency is needed.
        let mut body_url =
            Url::parse("https://form.invalid/").map_err(|_| OAuthError::Configuration)?;
        {
            let mut query = body_url.query_pairs_mut();
            for (k, v) in fields {
                query.append_pair(k, &v);
            }
        }
        let body = body_url.query().unwrap_or_default().to_owned();
        if body.len() > self.limits.bytes {
            return Err(OAuthError::Limit);
        }
        let dispatched = Instant::now();
        let response = request
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(OAuthError::Redirect);
        }
        let document = self.document(response).await?;
        let map = object(document.value(), OAuthError::Token)?;
        if status != StatusCode::OK {
            return Err(
                if prior.is_some()
                    && matches!(map.get("error"), Some(Value::Text(s)) if s == "invalid_grant")
                {
                    OAuthError::RefreshRejected
                } else {
                    OAuthError::TokenRejected
                },
            );
        }
        if map.get("error").is_some()
            || !text(map, "token_type", OAuthError::Token)?.eq_ignore_ascii_case("Bearer")
        {
            return Err(OAuthError::Token);
        }
        let access = text(map, "access_token", OAuthError::Token)?.to_owned();
        if !valid_access(&access) {
            return Err(OAuthError::Token);
        }
        let refresh = optional_text(map, "refresh_token", OAuthError::Token)?.map(str::to_owned);
        if refresh
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.chars().any(char::is_control))
        {
            return Err(OAuthError::Token);
        }
        let inherited = prior
            .as_ref()
            .map_or(&discovery.scopes, |(_, scopes)| scopes);
        let scopes = optional_text(map, "scope", OAuthError::Token)?
            .map(|s| parse_scopes(s, OAuthError::Token))
            .transpose()?
            .or_else(|| inherited.clone());
        if let (Some(granted), Some(requested)) = (&scopes, inherited)
            && granted.iter().any(|s| !requested.contains(s))
        {
            return Err(OAuthError::Token);
        }
        let expires_in = match map.get("expires_in") {
            None => None,
            Some(Value::Integer(n)) if *n >= 0 => Some(Duration::from_secs(*n as u64)),
            _ => return Err(OAuthError::Token),
        };
        let expires = expires_in
            .map(|d| dispatched.checked_add(d).ok_or(OAuthError::Token))
            .transpose()?;
        Ok(OAuthTokens {
            binding: self.binding.clone(),
            issuer: self.registration.issuer.clone(),
            client_id: self.registration.client_id.clone(),
            access,
            refresh: refresh.or_else(|| prior.map(|(refresh, _)| refresh)),
            scopes,
            expires,
            expires_in,
        })
    }
}

fn network_error(error: reqwest::Error) -> OAuthError {
    if error.is_timeout() {
        OAuthError::Limit
    } else {
        OAuthError::Transport
    }
}
fn valid_access(access: &str) -> bool {
    let (content, padding) = access
        .find('=')
        .map_or((access, ""), |index| access.split_at(index));
    !content.is_empty()
        && content
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
        && padding.bytes().all(|b| b == b'=')
}
fn http_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}
fn loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    })
}
fn resource_locations(resource: &Url) -> Vec<Url> {
    inserted_locations(resource, "oauth-protected-resource", false)
}
fn issuer_locations(issuer: &Url) -> Vec<Url> {
    let mut locations = inserted_locations(issuer, "oauth-authorization-server", false);
    locations.truncate(1);
    locations.extend(inserted_locations(issuer, "openid-configuration", true));
    locations
}
fn inserted_locations(base: &Url, name: &str, append: bool) -> Vec<Url> {
    let path = base.path().trim_end_matches('/');
    let root = format!("/.well-known/{name}");
    let mut first = base.clone();
    first.set_query(None);
    first.set_path(&format!("{root}{path}"));
    let mut locations = vec![first];
    if !path.is_empty() {
        let mut second = base.clone();
        second.set_query(None);
        second.set_path(&if append {
            format!("{path}{root}")
        } else {
            root
        });
        if !append || name == "openid-configuration" {
            locations.push(second);
        }
    }
    locations
}
fn reserved_authorization(key: &str) -> bool {
    matches!(
        key,
        "response_type"
            | "client_id"
            | "redirect_uri"
            | "state"
            | "code_challenge"
            | "code_challenge_method"
            | "resource"
            | "scope"
    )
}
fn object(value: &Value, error: OAuthError) -> Result<&Map, OAuthError> {
    if let Value::Map(map) = value {
        Ok(map)
    } else {
        Err(error)
    }
}
fn text<'a>(map: &'a Map, key: &str, error: OAuthError) -> Result<&'a str, OAuthError> {
    optional_text(map, key, error)?.ok_or(error)
}
fn optional_text<'a>(
    map: &'a Map,
    key: &str,
    error: OAuthError,
) -> Result<Option<&'a str>, OAuthError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Text(s)) => Ok(Some(s)),
        _ => Err(error),
    }
}
fn strings(map: &Map, key: &str, error: OAuthError) -> Result<Option<Vec<String>>, OAuthError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| match v {
                Value::Text(s) => Ok(s.clone()),
                _ => Err(error),
            })
            .collect::<Result<_, _>>()
            .map(Some),
        _ => Err(error),
    }
}
fn parse_scopes(value: &str, error: OAuthError) -> Result<Vec<String>, OAuthError> {
    if value.is_empty() {
        return Err(error);
    }
    validate_scopes(value.split(' ').map(str::to_owned).collect()).map_err(|_| error)
}
fn validate_scopes(scopes: Vec<String>) -> Result<Vec<String>, OAuthError> {
    if scopes.iter().any(|s| {
        s.is_empty()
            || !s
                .bytes()
                .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
    }) {
        return Err(OAuthError::Metadata);
    }
    let mut seen = BTreeSet::new();
    Ok(scopes
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect())
}
fn form_component(value: &str) -> String {
    let mut url = Url::parse("https://form.invalid/").expect("constant URL");
    url.query_pairs_mut().append_pair("v", value);
    url.query().expect("query was set")[2..].to_owned()
}
fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}
fn parse_challenges(
    header: &str,
    bearer: &mut Option<BTreeMap<String, String>>,
) -> Result<(), OAuthError> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escape = false;
    for (i, byte) in header.bytes().enumerate() {
        if !(0x20..=0x7e).contains(&byte) && byte != b'\t' {
            return Err(OAuthError::Challenge);
        }
        if escape {
            escape = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escape = true,
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                segments.push(&header[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if quoted || escape {
        return Err(OAuthError::Challenge);
    }
    segments.push(&header[start..]);
    let mut active_bearer = false;
    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(OAuthError::Challenge);
        }
        let end = segment.bytes().take_while(|b| token(*b)).count();
        if end == 0 {
            return Err(OAuthError::Challenge);
        }
        let rest = segment[end..].trim_start();
        let parameter = if rest.starts_with('=') {
            if !active_bearer {
                continue;
            }
            segment
        } else {
            active_bearer = segment[..end].eq_ignore_ascii_case("Bearer");
            if !active_bearer {
                continue;
            }
            if bearer.is_some() {
                return Err(OAuthError::Challenge);
            }
            *bearer = Some(BTreeMap::new());
            if rest.is_empty() {
                continue;
            }
            if end == segment.len() || !segment.as_bytes()[end].is_ascii_whitespace() {
                return Err(OAuthError::Challenge);
            }
            rest
        };
        let (name, value) = parameter.split_once('=').ok_or(OAuthError::Challenge)?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || !name.bytes().all(token) {
            return Err(OAuthError::Challenge);
        }
        let decoded = if value.starts_with('"') {
            if value.len() < 2 || !value.ends_with('"') {
                return Err(OAuthError::Challenge);
            }
            let mut out = String::new();
            let mut escaped = false;
            for byte in value.as_bytes()[1..value.len() - 1].iter().copied() {
                if escaped {
                    out.push(char::from(byte));
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    return Err(OAuthError::Challenge);
                } else {
                    out.push(char::from(byte));
                }
            }
            if escaped {
                return Err(OAuthError::Challenge);
            }
            out
        } else {
            if value.is_empty() || !value.bytes().all(token) {
                return Err(OAuthError::Challenge);
            }
            value.to_owned()
        };
        if bearer
            .as_mut()
            .ok_or(OAuthError::Challenge)?
            .insert(name.to_ascii_lowercase(), decoded)
            .is_some()
        {
            return Err(OAuthError::Challenge);
        }
    }
    Ok(())
}
