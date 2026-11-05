//! Streamable HTTP: each sent message has its own POST; SSE resumes only by GET.
use super::*;
use reqwest::{Client, Response, StatusCode};

pub(super) struct Http {
    client: Client,
    url: String,
    headers: BTreeMap<String, String>,
    session: Mutex<Option<String>>,
    streams: AtomicU64,
}
impl Http {
    pub fn new(url: String, headers: BTreeMap<String, String>) -> Result<Self, RuntimeFailure> {
        let parsed = reqwest::Url::parse(&url).map_err(|_| fail(FailureCode::McpIdentity))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
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
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .build()
            .map_err(|_| fail(FailureCode::McpTransport))?;
        Ok(Self {
            client,
            url,
            headers,
            session: Mutex::new(None),
            streams: AtomicU64::new(0),
        })
    }
    fn request(&self, method: reqwest::Method) -> Result<reqwest::RequestBuilder, RuntimeFailure> {
        let mut request = self
            .client
            .request(method, &self.url)
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
        Ok(request)
    }
    fn check(
        &self,
        response: &Response,
        wire: &Wire,
        initialize: bool,
    ) -> Result<(), RuntimeFailure> {
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
            .request(reqwest::Method::POST)?
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| fail(FailureCode::McpTransport))?;
        self.check(&response, wire, false)?;
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
            .request(reqwest::Method::POST)?
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
        self.check(&response, wire, wire.initialization.get().is_none())?;
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
            let response = self.get(Some(last), wire).await?;
            self.check(&response, wire, false)?;
            let received = self
                .receive(response, wire, Some(id), &mut cursor, &mut retry)
                .await;
            resumable(received, &cursor)?;
        }
        Ok(())
    }
    async fn get(&self, cursor: Option<&str>, wire: &Wire) -> Result<Response, RuntimeFailure> {
        if self.streams.fetch_add(1, Ordering::Relaxed)
            >= wire.config.limits.requests_per_session as u64
        {
            return Err(fail(FailureCode::McpTransport));
        }
        let mut request = self
            .request(reqwest::Method::GET)?
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
                .get(cursor.as_deref().filter(|s| !s.is_empty()), wire)
                .await?;
            if response.status() == StatusCode::METHOD_NOT_ALLOWED {
                wire.server_events.send_replace(Some(false));
                return Ok(());
            }
            self.check(&response, wire, false)?;
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
    pub async fn close(&self) {
        if self.session.lock().is_ok_and(|session| session.is_some())
            && let Ok(request) = self.request(reqwest::Method::DELETE)
        {
            let _ = request.send().await;
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
