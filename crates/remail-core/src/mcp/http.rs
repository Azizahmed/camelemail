//! Streamable HTTP client transport.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::Value;

use crate::mcp::client::{handle_peer_message, PeerState};
use crate::mcp::{lock, protocol, sse, stdio, url_origin};

pub(crate) const ACCEPT_BOTH: &str = "application/json, text/event-stream";
/// How long a notification POST may take; servers answer them with 202 right away.
pub(crate) const NOTIFY_TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(10) };

pub(crate) struct HttpConn {
    http: reqwest::Client,
    url: String,
    headers: HeaderMap,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    state: Arc<PeerState>,
}

impl HttpConn {
    pub(crate) fn new(url: &str, headers: &BTreeMap<String, String>, state: Arc<PeerState>) -> anyhow::Result<Self> {
        let parsed = reqwest::Url::parse(url).map_err(|e| anyhow!("invalid MCP server URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("MCP server URL must be http(s), not `{}:`", parsed.scheme());
        }
        Ok(Self {
            http: http_client()?,
            url: url.to_string(),
            headers: header_map(headers)?,
            session: Mutex::new(None),
            version: Mutex::new(None),
            state,
        })
    }

    pub(crate) fn set_protocol_version(&self, version: &str) {
        *lock(&self.version) = Some(version.to_string());
    }

    async fn post(&self, msg: &Value) -> anyhow::Result<reqwest::Response> {
        let mut req = self
            .http
            .post(&self.url)
            .headers(self.headers.clone())
            .header(ACCEPT, ACCEPT_BOTH)
            .header(CONTENT_TYPE, "application/json")
            .body(msg.to_string());
        if let Some(session) = lock(&self.session).clone() {
            req = req.header(protocol::SESSION_HEADER, session);
        }
        if let Some(version) = lock(&self.version).clone() {
            req = req.header(protocol::VERSION_HEADER, version);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("cannot reach {}: {}", url_origin(&self.url), error_chain(&e.without_url())))?;
        if let Some(session) = resp.headers().get(protocol::SESSION_HEADER).and_then(|v| v.to_str().ok()) {
            *lock(&self.session) = Some(session.to_string());
        }
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status == StatusCode::NOT_FOUND && lock(&self.session).is_some() {
            self.state.alive.store(false, Ordering::SeqCst);
            bail!("MCP session expired (HTTP 404)");
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            bail!("HTTP {status}: the server requires authorization (OAuth is not supported; add an Authorization header)");
        }
        let body = resp.text().await.unwrap_or_default();
        bail!("HTTP {status}: {}", body.chars().take(500).collect::<String>().trim())
    }

    pub(crate) async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.state.next_id();
        let msg = protocol::request(id, method, params);
        let exchange = async {
            let resp = self.post(&msg).await?;
            if resp.status() == StatusCode::ACCEPTED {
                bail!("server accepted `{method}` without responding");
            }
            let mut found = None;
            sse::read_messages(resp, |m| {
                if protocol::kind(&m) == protocol::Kind::Response {
                    if protocol::numeric_id(&m) == Some(id) {
                        found = Some(m);
                        return true;
                    }
                } else if let Some(reply) = handle_peer_message(&self.state, &m) {
                    let this = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = this.post(&reply).await {
                            tracing::debug!("mcp: failed to answer server request: {e:#}");
                        }
                    });
                }
                false
            })
            .await?;
            found.ok_or_else(|| anyhow!("server closed the stream without answering `{method}`"))
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(msg) => protocol::into_result(msg?),
            Err(_) => {
                let this = self.clone();
                tokio::spawn(async move {
                    let _ = this.notify("notifications/cancelled", Some(stdio::cancel_params(id, "timeout"))).await;
                });
                bail!("`{method}` timed out after {}s", timeout.as_secs())
            }
        }
    }

    pub(crate) async fn notify(&self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        match tokio::time::timeout(NOTIFY_TIMEOUT, self.post(&protocol::notification(method, params))).await {
            Ok(resp) => resp.map(drop),
            Err(_) => bail!("`{method}` timed out after {}s", NOTIFY_TIMEOUT.as_secs()),
        }
    }

    /// Ends the session with a best-effort `DELETE`.
    pub(crate) async fn close(&self) {
        self.state.alive.store(false, Ordering::SeqCst);
        let Some(session) = lock(&self.session).take() else { return };
        let req = self
            .http
            .delete(&self.url)
            .headers(self.headers.clone())
            .header(protocol::SESSION_HEADER, session)
            .timeout(Duration::from_secs(5));
        let _ = req.send().await;
    }
}

pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?)
}

fn header_map(headers: &BTreeMap<String, String>) -> anyhow::Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::try_from(name.as_str()).map_err(|_| anyhow!("invalid header name `{name}`"))?;
        let value = HeaderValue::try_from(value.as_str()).map_err(|_| anyhow!("invalid value for header `{name}`"))?;
        map.insert(name, value);
    }
    Ok(map)
}

/// A reqwest error with its source chain, which carries the useful detail.
pub(crate) fn error_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use serde_json::{json, Value};

    use crate::mcp::client::{McpClient, McpTransport};
    use crate::mcp::lock;

    /// A server that answers `tools/list` over SSE, preceded by a ping to the client.
    async fn sse_server(
        State(seen): State<Arc<std::sync::Mutex<Vec<Value>>>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let msg: Value = serde_json::from_slice(&body).unwrap_or_default();
        lock(&seen).push(json!({ "msg": msg, "session": headers.get("mcp-session-id").and_then(|v| v.to_str().ok()),
            "version": headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok()) }));
        let id = msg["id"].clone();
        match msg["method"].as_str() {
            Some("initialize") => {
                let result = json!({ "jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": "2025-03-26", "capabilities": {}, "serverInfo": { "name": "sse", "version": "1" } } });
                ([("mcp-session-id", "s-1"), ("content-type", "application/json")], result.to_string()).into_response()
            }
            Some("tools/list") => {
                let ping = json!({ "jsonrpc": "2.0", "id": "p1", "method": "ping" });
                let tools = json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [
                    { "name": "t", "inputSchema": { "type": "object" } }] } });
                let body = format!(": hello\n\nevent: message\ndata: {ping}\n\nevent: message\ndata: {tools}\n\n");
                ([("content-type", "text/event-stream")], body).into_response()
            }
            _ => StatusCode::ACCEPTED.into_response(),
        }
    }

    #[tokio::test]
    async fn streamable_http_with_sse_and_sessions() {
        let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::default();
        let app = axum::Router::new().route("/mcp", axum::routing::post(sse_server)).with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let transport = McpTransport::Http { url: format!("http://127.0.0.1:{port}/mcp"), headers: Default::default() };
        let client = McpClient::connect(&transport, "test", "0.1").await.expect("connect");
        assert_eq!(client.server_info().name, "sse");
        let tools = client.list_tools().await.expect("list");
        assert_eq!(tools[0].name, "t");

        let mut pong = None;
        for _ in 0..50 {
            pong = lock(&seen).iter().find(|s| s["msg"]["id"] == "p1").cloned();
            if pong.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pong = pong.expect("client answered the server ping");
        assert_eq!(pong["msg"]["result"], json!({}));
        let seen = lock(&seen).clone();
        assert_eq!(seen[0]["session"], Value::Null);
        assert_eq!(seen[1]["msg"]["method"], "notifications/initialized");
        assert_eq!(seen[1]["session"], "s-1");
        assert_eq!(seen[1]["version"], "2025-03-26");
        assert_eq!(seen[2]["session"], "s-1");
    }
}
