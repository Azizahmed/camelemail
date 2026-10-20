//! Exposes Remail's own tools to external agents over Streamable HTTP on
//! `127.0.0.1`. Requests are answered with plain JSON; no server-initiated
//! stream is offered.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::mcp::protocol::{self, Kind};
use crate::mcp::{ToolInfo, ToolOutput};

/// Supplies the tools served to external agents.
pub trait ToolHandler: Send + Sync + 'static {
    fn instructions(&self) -> Option<String>;
    /// May change at runtime (permission toggles).
    fn tools(&self) -> Vec<ToolInfo>;
    fn call(&self, name: String, arguments: Value) -> BoxFuture<'static, ToolOutput>;
}

/// A running server. Dropping the handle also shuts the server down.
pub struct McpServerHandle {
    port: u16,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl McpServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stops accepting connections and finishes in-flight requests.
    pub fn stop(self) {
        let _ = self.shutdown.send(());
    }

    /// Like [`stop`](Self::stop), but waits (up to 5 s) until the port is released,
    /// so a server can be restarted on the same port.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.task).await;
    }
}

struct AppState {
    token: String,
    /// The bound port, for Host header checks.
    port: u16,
    name: String,
    version: String,
    handler: Arc<dyn ToolHandler>,
}

/// Bind 127.0.0.1:port (port 0 = ephemeral). Every request must carry `Authorization: Bearer <token>` (constant-time compare) → 401 otherwise.
/// Reject requests whose Origin header is present and not http(s)://localhost|127.0.0.1[:port], or whose Host
/// isn't 127.0.0.1:<port> / localhost:<port> (DNS-rebinding protection) → 403.
/// `GET /health?nonce=N` answers [`health_proof`]`(token, N)` so a client can tell this server from whatever else took the port.
pub async fn serve(
    port: u16,
    token: String,
    server_name: &str,
    server_version: &str,
    handler: Arc<dyn ToolHandler>,
) -> anyhow::Result<McpServerHandle> {
    if token.is_empty() {
        anyhow::bail!("MCP server token must not be empty");
    }
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
    let port = listener.local_addr()?.port();
    let state = Arc::new(AppState { token, port, name: server_name.into(), version: server_version.into(), handler });
    let app = Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/health", get(health))
        .with_state(state);
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = rx.await;
        });
        if let Err(e) = server.await {
            tracing::error!("MCP server stopped: {e}");
        }
    });
    tracing::info!("MCP server listening on 127.0.0.1:{port}");
    Ok(McpServerHandle { port, shutdown: tx, task })
}

/// Proof that the server holds `token`: hex HMAC-SHA256 of the nonce, keyed by the token.
pub fn health_proof(token: &str, nonce: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, token.as_bytes());
    let tag = ring::hmac::sign(&key, format!("remail-mcp-health\0{nonce}").as_bytes());
    tag.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// `ok`, or with `?nonce=N` the [`health_proof`] for N. Needs no token: the proof reveals nothing.
async fn health(State(state): State<Arc<AppState>>, uri: Uri) -> String {
    let nonce = uri.query().unwrap_or_default().split('&').find_map(|kv| kv.strip_prefix("nonce="));
    match nonce {
        Some(n) if !n.is_empty() && n.len() <= 128 => health_proof(&state.token, n),
        _ => "ok".into(),
    }
}

/// Why a request was refused.
enum Denied {
    Origin,
    Host,
    Token,
}

impl IntoResponse for Denied {
    fn into_response(self) -> Response {
        match self {
            Denied::Origin => (StatusCode::FORBIDDEN, "forbidden origin").into_response(),
            Denied::Host => (StatusCode::FORBIDDEN, "forbidden host").into_response(),
            Denied::Token => {
                (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "unauthorized").into_response()
            }
        }
    }
}

fn check_access(state: &AppState, headers: &HeaderMap) -> Result<(), Denied> {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !is_local_host(host, state.port) {
        return Err(Denied::Host);
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if !origin.to_str().is_ok_and(is_local_origin) {
            return Err(Denied::Origin);
        }
    }
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim);
    match presented {
        Some(t) if constant_time_eq(t.as_bytes(), state.token.as_bytes()) => Ok(()),
        _ => Err(Denied::Token),
    }
}

/// `127.0.0.1:<port>` or `localhost:<port>`: a rebound DNS name shows up as its own Host.
fn is_local_host(host: &str, port: u16) -> bool {
    let Some((name, p)) = host.rsplit_once(':') else { return false };
    p.parse::<u16>().is_ok_and(|p| p == port) && (name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost"))
}

/// `http(s)://localhost|127.0.0.1|[::1]` with an optional port.
fn is_local_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else {
        return false;
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let host = match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => rest,
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn get_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(denied) = check_access(&state, &headers) {
        return denied.into_response();
    }
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST, DELETE")], "").into_response()
}

async fn delete_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match check_access(&state, &headers) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(denied) => denied.into_response(),
    }
}

async fn post_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(denied) = check_access(&state, &headers) {
        return denied.into_response();
    }
    if let Some(version) = headers.get(protocol::VERSION_HEADER) {
        let ok = version.to_str().is_ok_and(|v| protocol::SUPPORTED_VERSIONS.contains(&v));
        if !ok {
            let msg = protocol::error(Value::Null, protocol::INVALID_REQUEST, "unsupported MCP-Protocol-Version");
            return json_response(StatusCode::BAD_REQUEST, &msg, None);
        }
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = protocol::error(Value::Null, protocol::PARSE_ERROR, format!("parse error: {e}"));
            return json_response(StatusCode::BAD_REQUEST, &msg, None);
        }
    };
    let batch = parsed.is_array();
    let messages = protocol::messages(parsed);
    if messages.is_empty() {
        let msg = protocol::error(Value::Null, protocol::INVALID_REQUEST, "empty batch");
        return json_response(StatusCode::BAD_REQUEST, &msg, None);
    }
    let initializes = messages.iter().any(|m| m.get("method").and_then(Value::as_str) == Some("initialize"));
    let replies: Vec<Value> = futures::future::join_all(messages.into_iter().map(|m| handle_message(&state, m)))
        .await
        .into_iter()
        .flatten()
        .collect();
    if replies.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    let session = initializes.then(|| uuid::Uuid::new_v4().to_string());
    let body = if batch { Value::Array(replies) } else { replies.into_iter().next().unwrap_or_default() };
    json_response(StatusCode::OK, &body, session)
}

fn json_response(status: StatusCode, body: &Value, session: Option<String>) -> Response {
    let mut resp = Response::new(Body::from(body.to_string()));
    *resp.status_mut() = status;
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(value) = session.and_then(|s| HeaderValue::from_str(&s).ok()) {
        resp.headers_mut().insert(protocol::SESSION_HEADER, value);
    }
    resp
}

/// Handles one message; returns a reply only for requests and invalid messages.
async fn handle_message(state: &AppState, msg: Value) -> Option<Value> {
    match protocol::kind(&msg) {
        Kind::Notification | Kind::Response => None,
        Kind::Invalid => {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            Some(protocol::error(id, protocol::INVALID_REQUEST, "invalid JSON-RPC message"))
        }
        Kind::Request => {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            Some(match dispatch(state, method, params).await {
                Ok(result) => protocol::result(id, result),
                Err((code, message)) => protocol::error(id, code, message),
            })
        }
    }
}

async fn dispatch(state: &AppState, method: &str, params: Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = protocol::SUPPORTED_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .copied()
                .unwrap_or(protocol::LATEST_VERSION);
            let mut result = json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": state.name, "version": state.version },
            });
            if let Some(instructions) = state.handler.instructions() {
                result["instructions"] = Value::String(instructions);
            }
            Ok(result)
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools =
                serde_json::to_value(state.handler.tools()).map_err(|e| (protocol::INTERNAL_ERROR, e.to_string()))?;
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let invalid = |m: &str| (protocol::INVALID_PARAMS, m.to_string());
            let name = params.get("name").and_then(Value::as_str).ok_or_else(|| invalid("missing tool name"))?;
            let arguments = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(args @ Value::Object(_)) => args.clone(),
                Some(_) => return Err(invalid("arguments must be an object")),
            };
            if !state.handler.tools().iter().any(|t| t.name == name) {
                return Ok(ToolOutput::error(format!("Unknown tool: {name}")).to_call_result());
            }
            Ok(state.handler.call(name.to_string(), arguments).await.to_call_result())
        }
        other => Err((protocol::METHOD_NOT_FOUND, format!("method not found: {other}"))),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mcp::client::{McpClient, McpTransport};
    use std::collections::BTreeMap;
    use std::time::Duration;

    pub(crate) const TOKEN: &str = "secret-token";

    /// Echoes its `text` argument; `fail` returns a tool error.
    pub(crate) struct FakeTools;

    impl ToolHandler for FakeTools {
        fn instructions(&self) -> Option<String> {
            Some("Use echo to echo.".into())
        }

        fn tools(&self) -> Vec<ToolInfo> {
            let schema = json!({ "type": "object", "properties": { "text": { "type": "string" } } });
            vec![
                ToolInfo { name: "echo".into(), description: "Echo text".into(), input_schema: schema.clone() },
                ToolInfo { name: "fail".into(), description: "Always fails".into(), input_schema: schema },
            ]
        }

        fn call(&self, name: String, arguments: Value) -> BoxFuture<'static, ToolOutput> {
            Box::pin(async move {
                let text = arguments.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                match name.as_str() {
                    "echo" => ToolOutput::ok(text),
                    _ => ToolOutput::error(format!("failed: {text}")),
                }
            })
        }
    }

    pub(crate) async fn start() -> McpServerHandle {
        serve(0, TOKEN.into(), "remail", "1.2.3", Arc::new(FakeTools)).await.expect("serve")
    }

    fn transport(port: u16, token: &str) -> McpTransport {
        let headers = BTreeMap::from([("Authorization".to_string(), format!("Bearer {token}"))]);
        McpTransport::Http { url: format!("http://127.0.0.1:{port}/mcp"), headers }
    }

    async fn raw_post(port: u16, body: &str, extra: &[(&str, &str)]) -> reqwest::Response {
        let mut req = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {TOKEN}"))
            .header("Content-Type", "application/json")
            .body(body.to_string());
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        req.send().await.expect("send")
    }

    #[tokio::test]
    async fn client_round_trip() {
        let server = start().await;
        let client = McpClient::connect(&transport(server.port(), TOKEN), "test", "0.1").await.expect("connect");
        assert_eq!(client.server_info().name, "remail");
        assert_eq!(client.server_info().version, "1.2.3");
        assert_eq!(client.server_info().instructions.as_deref(), Some("Use echo to echo."));
        let tools = client.list_tools().await.expect("list");
        assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["echo", "fail"]);
        let out = client.call_tool("echo", json!({ "text": "hi" }), Duration::from_secs(5)).await.expect("call");
        assert_eq!(out, ToolOutput::ok("hi"));
        let out = client.call_tool("fail", json!({ "text": "x" }), Duration::from_secs(5)).await.expect("call");
        assert_eq!(out, ToolOutput::error("failed: x"));
        let out = client.call_tool("nope", Value::Null, Duration::from_secs(5)).await.expect("call");
        assert!(out.is_error && out.text.contains("Unknown tool"));
        let err = client.call_tool("echo", json!(["bad"]), Duration::from_secs(5)).await.expect_err("invalid params");
        assert!(format!("{err:#}").contains("-32602"), "{err:#}");
        assert!(client.is_alive());
        client.close().await;
        assert!(!client.is_alive());
        let port = server.port();
        server.shutdown().await;
        let again = serve(port, TOKEN.into(), "remail", "1.2.3", Arc::new(FakeTools)).await.expect("rebind same port");
        again.stop();
    }

    #[tokio::test]
    async fn rejects_bad_token_and_origin() {
        let server = start().await;
        let err = McpClient::connect(&transport(server.port(), "wrong"), "test", "0.1").await.err().expect("rejected");
        assert!(format!("{err:#}").contains("401"), "{err:#}");

        let url = format!("http://127.0.0.1:{}/mcp", server.port());
        let http = reqwest::Client::new();
        let resp = http.post(&url).body("{}").send().await.expect("send");
        assert_eq!(resp.status(), 401);
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = raw_post(server.port(), ping, &[("Origin", "http://evil.example")]).await;
        assert_eq!(resp.status(), 403);
        let resp = raw_post(server.port(), ping, &[("Origin", "http://localhost:5173")]).await;
        assert_eq!(resp.status(), 200);
        let resp = raw_post(server.port(), ping, &[("Origin", "http://localhost.evil.example")]).await;
        assert_eq!(resp.status(), 403);
        let health = http.get(format!("http://127.0.0.1:{}/health", server.port())).send().await.expect("health");
        assert_eq!(health.status(), 200);
        assert_eq!(health.text().await.expect("body"), "ok");
        let get = http.get(&url).bearer_auth(TOKEN).send().await.expect("get");
        assert_eq!(get.status(), 405);
        let delete = http.delete(&url).bearer_auth(TOKEN).send().await.expect("delete");
        assert_eq!(delete.status(), 200);
        server.stop();
    }

    #[tokio::test]
    async fn batches_notifications_and_errors() {
        let server = start().await;
        let port = server.port();
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
        let resp = raw_post(port, init, &[]).await;
        assert!(resp.headers().contains_key("mcp-session-id"));
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["capabilities"]["tools"]["listChanged"], false);

        let unknown = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#;
        let body: Value = raw_post(port, unknown, &[]).await.json().await.expect("json");
        assert_eq!(body["result"]["protocolVersion"], protocol::LATEST_VERSION);

        let resp = raw_post(port, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &[]).await;
        assert_eq!(resp.status(), 202);
        assert!(resp.text().await.expect("body").is_empty());

        let batch = r#"[
            {"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"echo","arguments":{"text":"one"}}},
            {"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}},
            {"jsonrpc":"2.0","id":"b","method":"ping"},
            {"jsonrpc":"2.0","id":"c","method":"nope"}
        ]"#;
        let body: Value =
            raw_post(port, batch, &[("MCP-Protocol-Version", "2025-06-18")]).await.json().await.expect("json");
        let replies = body.as_array().expect("array");
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0]["id"], "a");
        assert_eq!(replies[0]["result"]["content"][0]["text"], "one");
        assert_eq!(replies[0]["result"]["isError"], false);
        assert_eq!(replies[1]["result"], json!({}));
        assert_eq!(replies[2]["error"]["code"], protocol::METHOD_NOT_FOUND);

        let resp = raw_post(port, "{not json", &[]).await;
        assert_eq!(resp.status(), 400);
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["error"]["code"], protocol::PARSE_ERROR);

        let resp =
            raw_post(port, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#, &[("MCP-Protocol-Version", "1.0")]).await;
        assert_eq!(resp.status(), 400);
        server.stop();
    }

    /// Every way in without the right credentials or from a non-local page is refused.
    #[tokio::test]
    async fn check_access_refusals() {
        let server = start().await;
        let port = server.port();
        let url = format!("http://127.0.0.1:{port}/mcp");
        let http = reqwest::Client::new();
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let post = |auth: &str| {
            http.post(&url).header("Authorization", auth).header("Content-Type", "application/json").body(ping)
        };
        assert_eq!(post("Bearer wrong-token").send().await.expect("send").status(), 401);
        assert_eq!(post(&format!("Bearer {TOKEN}x")).send().await.expect("send").status(), 401);
        assert_eq!(post("Basic dXNlcjpwYXNz").send().await.expect("send").status(), 401);
        assert_eq!(post(&format!("Basic {TOKEN}")).send().await.expect("send").status(), 401);
        assert_eq!(http.get(&url).send().await.expect("get").status(), 401);
        assert_eq!(http.delete(&url).send().await.expect("delete").status(), 401);
        let resp = raw_post(port, ping, &[("Origin", "null")]).await;
        assert_eq!(resp.status(), 403);
        for host in ["evil.example", "evil.example:80", &format!("rebind.evil.example:{port}"), "127.0.0.1:1", "localhost"] {
            let resp = raw_post(port, ping, &[("Host", host)]).await;
            assert_eq!(resp.status(), 403, "{host}");
        }
        let resp = raw_post(port, ping, &[("Host", &format!("localhost:{port}"))]).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(raw_post(port, ping, &[]).await.status(), 200);

        let proof = http.get(format!("http://127.0.0.1:{port}/health?nonce=abc")).send().await.expect("health");
        assert_eq!(proof.text().await.expect("body"), health_proof(TOKEN, "abc"));
        assert_ne!(health_proof(TOKEN, "abc"), health_proof("other", "abc"));
        server.stop();
    }

    #[test]
    fn host_checks() {
        assert!(is_local_host("127.0.0.1:4000", 4000));
        assert!(is_local_host("LocalHost:4000", 4000));
        for bad in ["127.0.0.1:4001", "127.0.0.1", "", "example.com:4000", "[::1]:4000", "127.0.0.1.nip.io:4000"] {
            assert!(!is_local_host(bad, 4000), "{bad}");
        }
    }

    #[test]
    fn origin_checks() {
        for ok in ["http://localhost", "https://127.0.0.1:8443", "http://[::1]:3000", "http://localhost/"] {
            assert!(is_local_origin(ok), "{ok}");
        }
        for bad in ["null", "http://example.com", "file://", "http://127.0.0.1.nip.io", "http://localhost:abc"] {
            assert!(!is_local_origin(bad), "{bad}");
        }
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
