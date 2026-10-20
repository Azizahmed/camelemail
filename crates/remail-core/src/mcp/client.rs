//! Client side: connects to external MCP servers over stdio or Streamable HTTP.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mcp::http::HttpConn;
use crate::mcp::stdio::StdioConn;
use crate::mcp::{lock, protocol, url_origin, ServerInfo, ToolInfo, ToolOutput};

/// Handshake timeout; generous because `npx` may download the server first.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const LIST_TIMEOUT: Duration = Duration::from_secs(60);
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PAGES: usize = 100;
/// After a failed connect, the hub reports that error instead of retrying for this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(60);
/// [`McpHub::list_all`] reports servers that take longer as unavailable (they keep connecting in the background).
pub const LIST_ALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Stands in for an env var or header value that is kept in the secret store.
pub const SECRET_PLACEHOLDER: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// Secret-store id of env var / header `key` of server `server_id` (`kind` from [`McpTransport::secret_values`]).
pub fn secret_id(server_id: &str, kind: &str, key: &str) -> String {
    format!("mcp:{server_id}:{kind}:{key}")
}

/// A user-configured external MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
    /// Run tools without asking the user.
    #[serde(default)]
    pub auto_approve: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl McpTransport {
    /// The values that may hold credentials (env vars or headers) and their kind for [`secret_id`].
    pub fn secret_values(&self) -> (&'static str, &BTreeMap<String, String>) {
        match self {
            McpTransport::Stdio { env, .. } => ("env", env),
            McpTransport::Http { headers, .. } => ("header", headers),
        }
    }

    pub fn secret_values_mut(&mut self) -> (&'static str, &mut BTreeMap<String, String>) {
        match self {
            McpTransport::Stdio { env, .. } => ("env", env),
            McpTransport::Http { headers, .. } => ("header", headers),
        }
    }

    /// Same process or URL, whatever the env vars or headers.
    pub fn same_endpoint(&self, other: &McpTransport) -> bool {
        match (self, other) {
            (
                McpTransport::Stdio { command, args, cwd, .. },
                McpTransport::Stdio { command: c2, args: a2, cwd: d2, .. },
            ) => command == c2 && args == a2 && cwd == d2,
            (McpTransport::Http { url, .. }, McpTransport::Http { url: u2, .. }) => url == u2,
            _ => false,
        }
    }
}

/// Env var and header values (tokens) and the URL path (often a secret) stay out of logs.
impl std::fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys = |m: &BTreeMap<String, String>| m.keys().cloned().collect::<Vec<_>>();
        match self {
            McpTransport::Stdio { command, args, env, cwd } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field("env_keys", &keys(env))
                .field("cwd", cwd)
                .finish(),
            McpTransport::Http { url, headers } => {
                f.debug_struct("Http").field("url", &url_origin(url)).field("header_keys", &keys(headers)).finish()
            }
        }
    }
}

/// Connection state shared between a client and its transport tasks.
pub(crate) struct PeerState {
    pub(crate) alive: AtomicBool,
    pub(crate) tools_changed: AtomicBool,
    next_id: AtomicU64,
}

impl PeerState {
    pub(crate) fn new() -> Self {
        Self { alive: AtomicBool::new(true), tools_changed: AtomicBool::new(false), next_id: AtomicU64::new(1) }
    }

    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Handles a server→client request or notification; returns the reply to send, if any.
pub(crate) fn handle_peer_message(state: &PeerState, msg: &Value) -> Option<Value> {
    match protocol::kind(msg) {
        protocol::Kind::Request => Some(protocol::reply_to_peer_request(msg)),
        protocol::Kind::Notification => {
            if msg.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed") {
                state.tools_changed.store(true, Ordering::SeqCst);
            }
            None
        }
        _ => None,
    }
}

enum Conn {
    Stdio(Box<StdioConn>),
    Http(Arc<HttpConn>),
}

impl Conn {
    async fn request(&self, method: &str, params: Option<Value>, timeout: Duration) -> anyhow::Result<Value> {
        match self {
            Conn::Stdio(c) => c.request(method, params, timeout).await,
            Conn::Http(c) => c.request(method, params, timeout).await,
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        match self {
            Conn::Stdio(c) => c.notify(method, params),
            Conn::Http(c) => c.notify(method, params).await,
        }
    }

    async fn close(&self) {
        match self {
            Conn::Stdio(c) => c.close().await,
            Conn::Http(c) => c.close().await,
        }
    }
}

/// A connected, initialised MCP session.
pub struct McpClient {
    conn: Conn,
    info: ServerInfo,
    state: Arc<PeerState>,
}

impl McpClient {
    /// Spawn/connect and run the initialize handshake (+ notifications/initialized).
    pub async fn connect(
        transport: &McpTransport,
        client_name: &str,
        client_version: &str,
    ) -> anyhow::Result<McpClient> {
        let state = Arc::new(PeerState::new());
        let conn = match transport {
            McpTransport::Stdio { command, args, env, cwd } => {
                Conn::Stdio(Box::new(StdioConn::spawn(command, args, env, cwd.as_deref(), state.clone()).await?))
            }
            McpTransport::Http { url, headers } => Conn::Http(Arc::new(HttpConn::new(url, headers, state.clone())?)),
        };
        match handshake(&conn, client_name, client_version).await {
            Ok(info) => Ok(McpClient { conn, info, state }),
            Err(e) => {
                let detail = match &conn {
                    Conn::Stdio(c) => c.failure_detail().await,
                    Conn::Http(_) => None,
                };
                conn.close().await;
                Err(match detail {
                    Some(detail) if !format!("{e:#}").contains(&detail) => anyhow!("{e:#}\n{detail}"),
                    _ => e,
                })
            }
        }
    }

    pub fn server_info(&self) -> &ServerInfo {
        &self.info
    }

    /// Lists all tools, following `nextCursor` pagination.
    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolInfo>> {
        self.state.tools_changed.store(false, Ordering::SeqCst);
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = cursor.as_ref().map(|c| json!({ "cursor": c }));
            let page = self.conn.request("tools/list", params, LIST_TIMEOUT).await?;
            for tool in page.get("tools").and_then(Value::as_array).into_iter().flatten() {
                match parse_tool(tool) {
                    Some(t) => tools.push(t),
                    None => tracing::warn!("mcp[{}]: skipping malformed tool {tool}", self.info.name),
                }
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() && cursor.as_deref() != Some(next) => cursor = Some(next.to_string()),
                _ => return Ok(tools),
            }
        }
        bail!("tools/list returned more than {MAX_PAGES} pages")
    }

    pub async fn call_tool(&self, name: &str, arguments: Value, timeout: Duration) -> anyhow::Result<ToolOutput> {
        let arguments = if arguments.is_null() { json!({}) } else { arguments };
        let result =
            self.conn.request("tools/call", Some(json!({ "name": name, "arguments": arguments })), timeout).await?;
        Ok(ToolOutput::from_call_result(&result))
    }

    pub fn is_alive(&self) -> bool {
        self.state.alive.load(Ordering::SeqCst)
    }

    /// Whether the server announced `notifications/tools/list_changed` since the last listing.
    pub fn tools_changed(&self) -> bool {
        self.state.tools_changed.load(Ordering::SeqCst)
    }

    pub async fn close(&self) {
        self.conn.close().await;
    }
}

async fn handshake(conn: &Conn, client_name: &str, client_version: &str) -> anyhow::Result<ServerInfo> {
    let params = json!({
        "protocolVersion": protocol::LATEST_VERSION,
        "capabilities": {},
        "clientInfo": { "name": client_name, "version": client_version },
    });
    let result = conn.request("initialize", Some(params), INIT_TIMEOUT).await?;
    let version = result.get("protocolVersion").and_then(Value::as_str).unwrap_or(protocol::LATEST_VERSION);
    if !protocol::SUPPORTED_VERSIONS.contains(&version) {
        tracing::warn!("mcp: server negotiated unsupported protocol version {version}; continuing");
    }
    if let Conn::Http(c) = conn {
        c.set_protocol_version(version);
    }
    let server = result.get("serverInfo");
    let field = |k: &str| server.and_then(|s| s.get(k)).and_then(Value::as_str).unwrap_or_default().to_string();
    let info = ServerInfo {
        name: field("name"),
        version: field("version"),
        instructions: result.get("instructions").and_then(Value::as_str).map(str::to_string),
    };
    conn.notify("notifications/initialized", None).await?;
    Ok(info)
}

fn parse_tool(tool: &Value) -> Option<ToolInfo> {
    Some(ToolInfo {
        name: tool.get("name")?.as_str()?.to_string(),
        description: tool.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
        input_schema: tool.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
    })
}

#[derive(Default)]
struct Slot {
    transport: Option<McpTransport>,
    client: Option<Arc<McpClient>>,
    tools: Option<Vec<ToolInfo>>,
    /// The last connect failed: report its error instead of retrying until then.
    failed: Option<(Instant, String)>,
}

type SharedSlot = Arc<tokio::sync::Mutex<Slot>>;

/// Returns a live client for `server` from `slot`, connecting if needed. With
/// `fresh`, any existing client is replaced (and a recent failure retried).
async fn slot_client(
    slot: &tokio::sync::Mutex<Slot>,
    server: &McpServerConfig,
    fresh: bool,
    (client_name, client_version): (&str, &str),
) -> anyhow::Result<Arc<McpClient>> {
    let mut slot = slot.lock().await;
    let same_transport = slot.transport.as_ref() == Some(&server.transport);
    if let Some(client) = &slot.client {
        if !fresh && client.is_alive() && same_transport {
            return Ok(client.clone());
        }
    }
    if let Some((until, error)) = &slot.failed {
        if !fresh && same_transport && Instant::now() < *until {
            bail!("{error}");
        }
    }
    if let Some(old) = slot.client.take() {
        old.close().await;
    }
    slot.tools = None;
    slot.transport = Some(server.transport.clone());
    match McpClient::connect(&server.transport, client_name, client_version).await {
        Ok(client) => {
            let client = Arc::new(client);
            slot.client = Some(client.clone());
            slot.failed = None;
            Ok(client)
        }
        Err(e) => {
            let error = format!("{}: {e:#}", server.name);
            slot.failed = Some((Instant::now() + RETRY_AFTER, error.clone()));
            Err(anyhow!(error))
        }
    }
}

/// The server's tools, listed once per connection (again after `tools/list_changed`).
async fn slot_tools(
    slot: SharedSlot,
    server: McpServerConfig,
    who: (String, String),
) -> anyhow::Result<Vec<ToolInfo>> {
    let who = (who.0.as_str(), who.1.as_str());
    let client = slot_client(&slot, &server, false, who).await?;
    if let Some(tools) = &slot.lock().await.tools {
        if !client.tools_changed() {
            return Ok(tools.clone());
        }
    }
    let tools = match client.list_tools().await {
        Err(_) if !client.is_alive() => slot_client(&slot, &server, true, who).await?.list_tools().await,
        other => other,
    }
    .map_err(|e| anyhow!("{}: {e:#}", server.name))?;
    slot.lock().await.tools = Some(tools.clone());
    Ok(tools)
}

/// Pool of lazily-connected clients keyed by server id; reconnects a dead client once on use.
pub struct McpHub {
    client_name: String,
    client_version: String,
    slots: Mutex<HashMap<String, SharedSlot>>,
    /// Listings still running in the background after [`McpHub::list_all`] gave up waiting.
    listing: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl McpHub {
    pub fn new(client_name: &str, client_version: &str) -> McpHub {
        McpHub {
            client_name: client_name.into(),
            client_version: client_version.into(),
            slots: Mutex::default(),
            listing: Mutex::default(),
        }
    }

    fn slot(&self, id: &str) -> SharedSlot {
        lock(&self.slots).entry(id.to_string()).or_default().clone()
    }

    async fn client(&self, server: &McpServerConfig, fresh: bool) -> anyhow::Result<Arc<McpClient>> {
        slot_client(&self.slot(&server.id), server, fresh, (&self.client_name, &self.client_version)).await
    }

    /// Tools of every enabled server; a failing server yields Err in its slot instead of failing the whole call.
    /// Servers slower than [`LIST_ALL_TIMEOUT`] are reported unavailable and keep connecting in the background.
    pub async fn list_all(&self, servers: &[McpServerConfig]) -> Vec<(McpServerConfig, anyhow::Result<Vec<ToolInfo>>)> {
        self.list_all_within(servers, LIST_ALL_TIMEOUT).await
    }

    async fn list_all_within(
        &self,
        servers: &[McpServerConfig],
        timeout: Duration,
    ) -> Vec<(McpServerConfig, anyhow::Result<Vec<ToolInfo>>)> {
        let enabled: Vec<&McpServerConfig> = servers.iter().filter(|s| s.enabled).collect();
        let listing = enabled.iter().map(|s| {
            let who = (self.client_name.clone(), self.client_version.clone());
            let task = tokio::spawn(slot_tools(self.slot(&s.id), (*s).clone(), who));
            {
                let mut listing = lock(&self.listing);
                listing.retain(|t| !t.is_finished());
                listing.push(task.abort_handle());
            }
            async move {
                match tokio::time::timeout(timeout, task).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(e)) => Err(anyhow!("{}: {e}", s.name)),
                    Err(_) => Err(anyhow!("{}: not responding", s.name)),
                }
            }
        });
        let results = futures::future::join_all(listing).await;
        enabled.into_iter().cloned().zip(results).collect()
    }

    /// Calls `tool` on `server` with a 120 s timeout.
    pub async fn call(&self, server: &McpServerConfig, tool: &str, arguments: Value) -> anyhow::Result<ToolOutput> {
        if !server.enabled {
            bail!("MCP server `{}` is disabled", server.name);
        }
        let client = self.client(server, false).await?;
        client.call_tool(tool, arguments, TOOL_TIMEOUT).await.map_err(|e| anyhow!("{}: {e:#}", server.name))
    }

    /// Connect fresh (not pooled), list tools, close. For the settings "Test" button.
    pub async fn test(transport: &McpTransport) -> anyhow::Result<(ServerInfo, Vec<ToolInfo>)> {
        let client = McpClient::connect(transport, "Remail", env!("CARGO_PKG_VERSION")).await?;
        let tools = client.list_tools().await;
        client.close().await;
        Ok((client.info.clone(), tools?))
    }

    /// Drop clients whose config changed or was removed.
    pub async fn retain(&self, servers: &[McpServerConfig]) {
        let stale: Vec<SharedSlot> = {
            let mut slots = lock(&self.slots);
            let ids: Vec<String> = slots.keys().cloned().collect();
            ids.into_iter()
                .filter(|id| !servers.iter().any(|s| &s.id == id && s.enabled))
                .filter_map(|id| slots.remove(&id))
                .collect()
        };
        let kept: Vec<(SharedSlot, McpTransport)> = {
            let slots = lock(&self.slots);
            servers.iter().filter_map(|s| Some((slots.get(&s.id)?.clone(), s.transport.clone()))).collect()
        };
        for slot in stale {
            if let Some(client) = slot.lock().await.client.take() {
                client.close().await;
            }
        }
        for (slot, transport) in kept {
            let mut slot = slot.lock().await;
            if slot.transport.as_ref().is_some_and(|t| t != &transport) {
                if let Some(client) = slot.client.take() {
                    client.close().await;
                }
                *slot = Slot::default();
            }
        }
    }

    pub async fn shutdown(&self) {
        // Dropping a half-done connect kills the server it started.
        for task in lock(&self.listing).drain(..) {
            task.abort();
        }
        let slots: Vec<_> = lock(&self.slots).drain().map(|(_, slot)| slot).collect();
        let closing = slots.into_iter().map(|slot| async move {
            if let Some(client) = slot.lock().await.client.take() {
                client.close().await;
            }
        });
        futures::future::join_all(closing).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::server::tests::{start, TOKEN};

    fn python(script: &str) -> McpTransport {
        McpTransport::Stdio {
            command: "python3".into(),
            args: vec!["-u".into(), format!("{}/src/mcp/testdata/{script}", env!("CARGO_MANIFEST_DIR"))],
            env: BTreeMap::from([("PYTHONDONTWRITEBYTECODE".into(), "1".into())]),
            cwd: None,
        }
    }

    fn config(id: &str, transport: McpTransport) -> McpServerConfig {
        McpServerConfig { id: id.into(), name: id.into(), enabled: true, transport, auto_approve: false }
    }

    #[tokio::test]
    async fn stdio_round_trip() {
        let client = McpClient::connect(&python("fake_server.py"), "test", "0.1").await.expect("connect");
        assert_eq!(client.server_info().name, "fake");
        assert_eq!(client.server_info().instructions.as_deref(), Some("fake instructions"));
        let tools = client.list_tools().await.expect("list");
        assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["echo", "fail", "ask_ping"]);
        assert_eq!(tools[2].description, "");

        let secs = Duration::from_secs(10);
        let out = client.call_tool("echo", json!({ "text": "hello" }), secs).await.expect("echo");
        assert_eq!(out, ToolOutput::ok("hello\n[image: image/png]"));
        let out = client.call_tool("fail", Value::Null, secs).await.expect("fail");
        assert_eq!(out, ToolOutput::error("boom"));
        let out = client.call_tool("ask_ping", json!({}), secs).await.expect("ping");
        assert_eq!(out.text, "pong ok");
        let err = client.call_tool("missing", json!({}), secs).await.expect_err("rpc error");
        assert!(format!("{err:#}").contains("-32602"));

        let err = client.call_tool("crash", json!({}), secs).await.expect_err("crash");
        assert!(format!("{err:#}").contains("exited"), "{err:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!client.is_alive());
        client.close().await;
    }

    #[tokio::test]
    async fn stdio_startup_failure_includes_stderr() {
        let err = McpClient::connect(&python("broken_server.py"), "test", "0.1").await.err().expect("fails");
        assert!(format!("{err:#}").contains("GITHUB_TOKEN environment variable is required"), "{err:#}");

        let missing =
            McpTransport::Stdio { command: "no-such-mcp-binary".into(), args: vec![], env: BTreeMap::new(), cwd: None };
        let err = McpClient::connect(&missing, "test", "0.1").await.err().expect("fails");
        assert!(format!("{err:#}").contains("command not found"), "{err:#}");
    }

    /// Answers `initialize` but never `notifications/initialized`.
    async fn stalling_server() -> u16 {
        async fn handle(body: String) -> axum::response::Response {
            use axum::response::IntoResponse;
            let msg: Value = serde_json::from_str(&body).unwrap_or_default();
            if msg["method"] == "initialize" {
                let result = json!({ "jsonrpc": "2.0", "id": msg["id"], "result": {
                    "protocolVersion": "2025-03-26", "capabilities": {}, "serverInfo": { "name": "stall", "version": "1" } } });
                return ([("content-type", "application/json")], result.to_string()).into_response();
            }
            futures::future::pending::<()>().await;
            unreachable!()
        }
        let app = axum::Router::new().route("/s/SECRET/mcp", axum::routing::post(handle));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move { axum::serve(listener, app).await });
        port
    }

    #[tokio::test]
    async fn hub_gives_up_on_stalled_servers_and_backs_off() {
        let port = stalling_server().await;
        let transport = McpTransport::Http { url: format!("http://127.0.0.1:{port}/s/SECRET/mcp"), headers: BTreeMap::new() };
        let started = Instant::now();
        let err = McpClient::connect(&transport, "test", "0.1").await.err().expect("stalled handshake fails");
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");

        let hub = McpHub::new("test", "0.1");
        let servers = vec![config("stall", transport)];
        let started = Instant::now();
        let listed = hub.list_all(&servers).await;
        assert!(started.elapsed() <= LIST_ALL_TIMEOUT + Duration::from_secs(1), "{:?}", started.elapsed());
        let err = listed[0].1.as_ref().expect_err("unavailable");
        assert!(!format!("{err:#}").contains("SECRET"), "{err:#}");
        // The failure is remembered instead of reconnecting on every chat message.
        let started = Instant::now();
        let again = hub.list_all(&servers).await;
        assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
        assert_eq!(format!("{:#}", again[0].1.as_ref().expect_err("cached")), format!("{err:#}"));
        hub.shutdown().await;
    }

    /// A server slower than the overall budget is reported unavailable without waiting for it.
    #[tokio::test]
    async fn list_all_reports_slow_servers() {
        let hub = McpHub::new("test", "0.1");
        let slow = McpTransport::Stdio { command: "sleep".into(), args: vec!["30".into()], env: BTreeMap::new(), cwd: None };
        let started = Instant::now();
        let listed = hub.list_all_within(&[config("slow", slow)], Duration::from_millis(300)).await;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(format!("{:#}", listed[0].1.as_ref().expect_err("slow")).contains("not responding"));
        hub.shutdown().await;
    }

    #[test]
    fn transports_hide_credentials_in_debug() {
        let http = McpTransport::Http {
            url: "https://mcp.example.com/s/SECRET/mcp".into(),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer TOKEN".to_string())]),
        };
        let stdio = McpTransport::Stdio {
            command: "npx".into(),
            args: vec![],
            env: BTreeMap::from([("GITHUB_TOKEN".to_string(), "ghp_TOKEN".to_string())]),
            cwd: None,
        };
        for debug in [format!("{http:?}"), format!("{:?}", config("x", stdio))] {
            assert!(!debug.contains("TOKEN") || debug.contains("GITHUB_TOKEN") && !debug.contains("ghp_"), "{debug}");
            assert!(!debug.contains("SECRET") && !debug.contains("Bearer"), "{debug}");
        }
    }

    #[tokio::test]
    async fn hub_pools_and_reconnects() {
        let hub = McpHub::new("test", "0.1");
        let fake = config("fake", python("fake_server.py"));
        let broken = config("broken", python("broken_server.py"));
        let mut disabled = config("off", python("fake_server.py"));
        disabled.enabled = false;
        let servers = vec![fake.clone(), broken.clone(), disabled.clone()];

        let listed = hub.list_all(&servers).await;
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].1.as_ref().expect("fake tools").len(), 3);
        assert!(listed[1].1.as_ref().is_err());
        assert!(hub.call(&disabled, "echo", json!({})).await.is_err());

        let out = hub.call(&fake, "echo", json!({ "text": "pooled" })).await.expect("call");
        assert_eq!(out.text, "pooled\n[image: image/png]");
        assert!(hub.call(&fake, "crash", json!({})).await.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let out = hub.call(&fake, "echo", json!({ "text": "again" })).await.expect("reconnected");
        assert_eq!(out.text, "again\n[image: image/png]");

        let server = start().await;
        let headers = BTreeMap::from([("Authorization".to_string(), format!("Bearer {TOKEN}"))]);
        let http = McpTransport::Http { url: format!("http://127.0.0.1:{}/mcp", server.port()), headers };
        let (info, tools) = McpHub::test(&http).await.expect("test");
        assert_eq!(info.name, "remail");
        assert_eq!(tools.len(), 2);

        let moved = config("fake", http);
        hub.retain(std::slice::from_ref(&moved)).await;
        assert_eq!(lock(&hub.slots).len(), 1);
        let out = hub.call(&moved, "echo", json!({ "text": "over http" })).await.expect("http call");
        assert_eq!(out, ToolOutput::ok("over http"));
        hub.shutdown().await;
        assert!(lock(&hub.slots).is_empty());
        server.stop();
    }
}
