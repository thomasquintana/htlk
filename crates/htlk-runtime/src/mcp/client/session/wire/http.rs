//! Streamable HTTP: each sent message has its own POST; SSE resumes only by GET.
use super::*;
use reqwest::{Client, Response, StatusCode};

pub(super) struct Http {
    client: Client,
    url: reqwest::Url,
    headers: BTreeMap<String, String>,
    binding: auth::CredentialBinding,
    credentials: Option<Arc<dyn auth::McpCredentialProvider>>,
    session: Mutex<Option<String>>,
    streams: AtomicU64,
}
struct Acquisition<'a> {
    http: &'a Http,
    wire: &'a Wire,
    method: reqwest::Method,
    request_id: Option<&'a McpRequestId>,
    complete: bool,
}
impl Drop for Acquisition<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.http.concern(
                self.wire,
                self.method.clone(),
                self.request_id,
                auth::McpAuthenticationReason::AcquisitionInterrupted,
            );
        }
    }
}
impl Http {
    pub fn new(
        url: String,
        headers: BTreeMap<String, String>,
        owner: &McpOwner,
        server: &ServerIdentity,
        limits: &Limits,
        credentials: Option<Arc<dyn auth::McpCredentialProvider>>,
    ) -> Result<Self, RuntimeFailure> {
        let parsed = reqwest::Url::parse(&url).map_err(|_| fail(FailureCode::McpIdentity))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || (credentials.is_some()
                && (parsed.query().is_some()
                    || headers
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case("authorization"))))
            || headers.keys().any(|name| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "mcp-session-id"
                        | "mcp-protocol-version"
                        | "content-type"
                        | "accept"
                        | "last-event-id"
                )
            })
        {
            return Err(fail(FailureCode::McpIdentity));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .build()
            .map_err(|_| fail(FailureCode::McpTransport))?;
        // Legacy static-header endpoints may retain a deployment query. OAuth
        // provider destinations are exact query-free resource indicators.
        let mut resource = parsed.clone();
        resource.set_query(None);
        let binding = auth::CredentialBinding::for_server(
            server,
            owner.host().principal().into(),
            resource,
            limits,
        )
        .map_err(|_| fail(FailureCode::McpIdentity))?;
        Ok(Self {
            client,
            url: parsed,
            headers,
            binding,
            credentials,
            session: Mutex::new(None),
            streams: AtomicU64::new(0),
        })
    }
    fn concern(
        &self,
        wire: &Wire,
        method: reqwest::Method,
        request_id: Option<&McpRequestId>,
        reason: auth::McpAuthenticationReason,
    ) {
        wire.config
            .authentication
            .record(auth::McpAuthenticationConcern {
                sequence: 0,
                binding: self.binding.clone(),
                epoch: wire.epoch,
                method,
                request_id: request_id.cloned(),
                reason,
            });
    }
    async fn request(
        &self,
        method: reqwest::Method,
        wire: &Wire,
        request_id: Option<&McpRequestId>,
    ) -> Result<reqwest::RequestBuilder, RuntimeFailure> {
        let mut request = self
            .client
            .request(method.clone(), self.url.clone())
            .header("mcp-protocol-version", MCP_VERSION);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(session) = &*self
            .session
            .lock()
            .map_err(|_| fail(FailureCode::Invariant))?
        {
            request = request.header("mcp-session-id", session);
        }
        if let Some(provider) = &self.credentials {
            let mut acquisition = Acquisition {
                http: self,
                wire,
                method: method.clone(),
                request_id,
                complete: false,
            };
            let result = tokio::time::timeout(
                Duration::from_millis(wire.config.limits.timeout_ms),
                provider.authorization(&self.binding, &method, &self.url),
            )
            .await
            .unwrap_or(Err(auth::OAuthError::Limit));
            acquisition.complete = true;
            match result {
                Ok(Some(authorization)) => request = authorization.apply(request),
                Ok(None) => {}
                Err(error) => {
                    self.concern(
                        wire,
                        method,
                        request_id,
                        auth::McpAuthenticationReason::Credential(error),
                    );
                    return Err(fail(FailureCode::Policy));
                }
            }
            if method != reqwest::Method::DELETE {
                wire.check_owner().await?;
                if wire.stop.is_cancelled() {
                    return Err(fail(FailureCode::Cancelled));
                }
            }
        }
        Ok(request)
    }
    fn check(
        &self,
        response: &Response,
        wire: &Wire,
        initialize: bool,
        method: reqwest::Method,
        request_id: Option<&McpRequestId>,
    ) -> Result<(), RuntimeFailure> {
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            let limits = auth::OAuthLimits {
                bytes: wire.limits.max_document_bytes.min(64 * 1024),
                ..auth::OAuthLimits::default()
            };
            let reason = match auth::OAuthChallenge::from_headers(
                response.status(),
                response.headers(),
                &limits,
            ) {
                Ok(challenge) => auth::McpAuthenticationReason::Challenge {
                    status: response.status().as_u16(),
                    challenge,
                },
                Err(error) => auth::McpAuthenticationReason::InvalidChallenge {
                    status: response.status().as_u16(),
                    error,
                },
            };
            self.concern(wire, method, request_id, reason);
            // An auth response is not a JSON-RPC reply or proof of no effects.
            // Policy failure also prevents cached callback/control transport retry.
            return Err(fail(FailureCode::Policy));
        }
        if response.status() == StatusCode::NOT_FOUND
            && self
                .session
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?
                .is_some()
        {
            wire.end(McpSessionEnd::SessionExpired);
            return Err(fail(FailureCode::McpTransport));
        }
        if !response.status().is_success() {
            return Err(fail(FailureCode::McpTransport));
        }
        if let Some(session) = response.headers().get("mcp-session-id") {
            let session = session
                .to_str()
                .map_err(|_| fail(FailureCode::McpProtocol))?;
            if session.is_empty() || !session.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err(fail(FailureCode::McpProtocol));
            }
            let mut previous = self
                .session
                .lock()
                .map_err(|_| fail(FailureCode::Invariant))?;
            if previous.as_ref().is_some_and(|s| s != session)
                || (previous.is_none() && !initialize)
            {
                return Err(fail(FailureCode::McpProtocol));
            }
            *previous = Some(session.into());
        }
        Ok(())
    }
    pub async fn control(&self, bytes: &[u8], wire: &Wire) -> Result<(), RuntimeFailure> {
        let mut response = self
            .request(reqwest::Method::POST, wire, None)
            .await?
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| fail(FailureCode::McpTransport))?;
        self.check(&response, wire, false, reqwest::Method::POST, None)?;
        if response.status() != StatusCode::ACCEPTED
            || response
                .chunk()
                .await
                .map_err(|_| fail(FailureCode::McpTransport))?
                .is_some()
        {
            return Err(fail(FailureCode::McpProtocol));
        }
        Ok(())
    }
    pub async fn exchange(
        &self,
        bytes: &[u8],
        id: &McpRequestId,
        wire: &Arc<Wire>,
        idempotency_key: Option<&str>,
        sent: &AtomicBool,
    ) -> Result<(), RuntimeFailure> {
        let mut request = self
            .request(reqwest::Method::POST, wire, Some(id))
            .await?
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(bytes.to_vec());
        if let Some(key) = idempotency_key {
            let value = reqwest::header::HeaderValue::from_str(key)
                .map_err(|_| fail(FailureCode::Input))?;
            request = request.header("idempotency-key", value);
        }
        let request = request
            .build()
            .map_err(|_| fail(FailureCode::McpTransport))?;
        if wire.stop.is_cancelled() {
            return Err(fail(FailureCode::Cancelled));
        }
        sent.store(true, Ordering::Release);
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|_| fail(FailureCode::McpTransport))?;
        self.check(
            &response,
            wire,
            wire.initialization.get().is_none(),
            reqwest::Method::POST,
            Some(id),
        )?;
        let mut cursor = None;
        let mut retry = 1000;
        let received = self
            .receive(response, wire, Some(id), &mut cursor, &mut retry)
            .await;
        resumable(received, &cursor)?;
        while wire
            .pending
            .lock()
            .map_err(|_| fail(FailureCode::Invariant))?
            .contains_key(id)
        {
            let Some(last) = cursor.as_deref().filter(|s: &&str| !s.is_empty()) else {
                return Err(fail(FailureCode::McpProtocol));
            };
            tokio::time::sleep(Duration::from_millis(retry)).await;
            let response = self.get(Some(last), wire, Some(id)).await?;
            self.check(&response, wire, false, reqwest::Method::GET, Some(id))?;
            let received = self
                .receive(response, wire, Some(id), &mut cursor, &mut retry)
                .await;
            resumable(received, &cursor)?;
        }
        Ok(())
    }
    async fn get(
        &self,
        cursor: Option<&str>,
        wire: &Wire,
        request_id: Option<&McpRequestId>,
    ) -> Result<Response, RuntimeFailure> {
        if self.streams.fetch_add(1, Ordering::Relaxed)
            >= wire.config.limits.requests_per_session as u64
        {
            return Err(fail(FailureCode::McpTransport));
        }
        let mut request = self
            .request(reqwest::Method::GET, wire, request_id)
            .await?
            .header("accept", "text/event-stream");
        if let Some(cursor) = cursor {
            request = request.header("last-event-id", cursor);
        }
        request
            .send()
            .await
            .map_err(|_| fail(FailureCode::McpTransport))
    }
    pub async fn events(&self, wire: &Arc<Wire>) -> Result<(), RuntimeFailure> {
        let mut cursor: Option<String> = None;
        let mut retry = 1000;
        loop {
            let response = self
                .get(cursor.as_deref().filter(|s| !s.is_empty()), wire, None)
                .await?;
            if response.status() == StatusCode::METHOD_NOT_ALLOWED {
                wire.server_events.send_replace(Some(false));
                return Ok(());
            }
            self.check(&response, wire, false, reqwest::Method::GET, None)?;
            wire.server_events.send_replace(Some(true));
            let received = self
                .receive(response, wire, None, &mut cursor, &mut retry)
                .await;
            resumable(received, &cursor)?;
            if cursor.as_deref().is_none_or(str::is_empty) {
                return Err(fail(FailureCode::McpTransport));
            }
            tokio::time::sleep(Duration::from_millis(retry)).await;
        }
    }
    async fn receive(
        &self,
        mut response: Response,
        wire: &Arc<Wire>,
        id: Option<&McpRequestId>,
        cursor: &mut Option<String>,
        retry: &mut u64,
    ) -> Result<(), RuntimeFailure> {
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .ok_or_else(|| fail(FailureCode::McpProtocol))?;
        if mime.eq_ignore_ascii_case("application/json") && id.is_some() {
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| fail(FailureCode::McpTransport))?
            {
                if bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|n| n > wire.limits.max_document_bytes)
                {
                    return Err(fail(FailureCode::McpProtocol));
                }
                bytes.extend_from_slice(&chunk);
            }
            return wire.route(&bytes, id, true).await;
        }
        if !mime.eq_ignore_ascii_case("text/event-stream") {
            return Err(fail(FailureCode::McpProtocol));
        }
        let mut decoder = sse::Decoder::new(wire.limits.max_document_bytes);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| fail(FailureCode::McpTransport))?
        {
            for byte in chunk {
                if let Some(event) = decoder.byte(byte)? {
                    if let Some(delay) = event.retry {
                        if delay > i64::MAX as u64
                            || std::time::Instant::now()
                                .checked_add(Duration::from_millis(delay))
                                .is_none()
                        {
                            return Err(fail(FailureCode::McpProtocol));
                        }
                        *retry = delay;
                    }
                    if !event.data.is_empty() {
                        wire.route(&event.data, id, id.is_some()).await?;
                    }
                    // Advance only after successful admission of this event.
                    if let Some(event_id) = event.id {
                        *cursor = Some(event_id);
                    }
                    if let Some(id) = id
                        && !wire
                            .pending
                            .lock()
                            .map_err(|_| fail(FailureCode::Invariant))?
                            .contains_key(id)
                    {
                        return Ok(());
                    }
                }
            }
        }
        decoder.finish()
    }
    pub async fn close(&self, wire: &Wire) {
        if self.session.lock().is_ok_and(|session| session.is_some())
            && let Ok(request) = self.request(reqwest::Method::DELETE, wire, None).await
            && let Ok(response) = request.send().await
        {
            let _ = self.check(&response, wire, false, reqwest::Method::DELETE, None);
        }
    }
}

fn resumable(
    result: Result<(), RuntimeFailure>,
    cursor: &Option<String>,
) -> Result<(), RuntimeFailure> {
    match result {
        Err(error)
            if error.code() == FailureCode::McpTransport
                && cursor.as_ref().is_some_and(|s| !s.is_empty()) =>
        {
            Ok(())
        }
        result => result,
    }
}
