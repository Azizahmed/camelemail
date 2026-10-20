//! `remail --mcp`: a stdio⇄HTTP proxy for MCP clients that only speak stdio
//! (Claude Desktop etc.), forwarding to the running app's HTTP endpoint.
//! Stdout carries only protocol messages; diagnostics go through `tracing`,
//! which the binary must route to stderr.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::mcp::http::{error_chain, http_client, ACCEPT_BOTH};
use crate::mcp::protocol::{self, Kind};
use crate::mcp::{lock, sse};

const ENDPOINT_FILE: &str = "mcp-endpoint.json";
const NOT_RUNNING: &str =
    "Remail is not running or its MCP server is disabled. Start Remail and enable the MCP server in Settings.";
const BAD_TOKEN: &str =
    "Remail rejected the MCP access token. Copy the current connection settings from Remail's Settings into this MCP client.";

/// Where a running Remail app advertises its MCP endpoint.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointFile {
    pub url: String,
    pub token: String,
    pub pid: u32,
}

impl std::fmt::Debug for EndpointFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointFile").field("url", &self.url).field("pid", &self.pid).finish_non_exhaustive()
    }
}

/// Writes `<data_dir>/mcp-endpoint.json` atomically, readable only by the user on unix.
pub fn write_endpoint_file(data_dir: &Path, ep: &EndpointFile) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!("{ENDPOINT_FILE}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(ep)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, data_dir.join(ENDPOINT_FILE))?;
    Ok(())
}

pub fn read_endpoint_file(data_dir: &Path) -> Option<EndpointFile> {
    let text = std::fs::read_to_string(data_dir.join(ENDPOINT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn remove_endpoint_file(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(ENDPOINT_FILE));
}

/// Default profile dir used by the desktop app: $REMAIL_DATA_DIR if set, else dirs::data_dir()/app.remail.desktop
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("REMAIL_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("app.remail.desktop")
}

/// Reads newline-delimited JSON-RPC from stdin, POSTs each to the app's endpoint with its bearer token
/// (and session header once known), and writes every JSON-RPC response to stdout as one line.
/// The endpoint comes from `<data_dir>/mcp-endpoint.json`, which is re-read whenever the app is
/// unreachable (it may have restarted on a new port); see [`discover_endpoint`].
pub async fn run_stdio_bridge_discovering(data_dir: &Path) -> anyhow::Result<()> {
    let http = http_client()?;
    let endpoint = discover_endpoint(&http, data_dir).await;
    let bridge = Bridge::new(None, Some(data_dir.to_path_buf()))?;
    *lock(&bridge.endpoint) = endpoint.map(Endpoint::from);
    bridge.run(tokio::io::stdin(), tokio::io::stdout()).await
}

/// The endpoint file, if the app that wrote it is still running and still owns that port: its
/// pid must be alive and `/health` must prove it knows the token (so a stale file never sends
/// the token to whatever process now listens there).
pub async fn discover_endpoint(http: &reqwest::Client, data_dir: &Path) -> Option<EndpointFile> {
    let dir = data_dir.to_path_buf();
    let ep = tokio::task::spawn_blocking(move || read_endpoint_file(&dir)).await.ok()??;
    verify_endpoint(http, &ep).await.then_some(ep)
}

/// See [`discover_endpoint`].
pub async fn verify_endpoint(http: &reqwest::Client, ep: &EndpointFile) -> bool {
    if !pid_alive(ep.pid) {
        tracing::debug!("mcp bridge: the app that wrote the endpoint file (pid {}) is gone", ep.pid);
        return false;
    }
    let Some(base) = ep.url.strip_suffix("/mcp") else { return false };
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let check = http.get(format!("{base}/health?nonce={nonce}")).timeout(std::time::Duration::from_secs(3)).send();
    let body = match check.await {
        Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
        _ => return false,
    };
    let expected = crate::mcp::server::health_proof(&ep.token, &nonce);
    let ok = crate::mcp::server::constant_time_eq(body.trim().as_bytes(), expected.as_bytes());
    if !ok {
        tracing::warn!("mcp bridge: the process on {} is not this Remail; not sending it the token", ep.url);
    }
    ok
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists and is ours to signal.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// A verified endpoint; `pid` (None in tests) is re-checked before each request.
#[derive(Clone, PartialEq)]
struct Endpoint {
    url: String,
    token: String,
    pid: Option<u32>,
}

impl From<EndpointFile> for Endpoint {
    fn from(ep: EndpointFile) -> Self {
        Endpoint { url: ep.url, token: ep.token, pid: Some(ep.pid) }
    }
}

/// Forwarding failures, carrying the message shown to the MCP client.
enum ForwardError {
    /// The endpoint could not be reached or refused us; worth re-discovering.
    Unreachable(String),
    Failed(String),
}

/// The per-message forwarding logic of the stdio bridge.
pub struct Bridge {
    http: reqwest::Client,
    endpoint: Mutex<Option<Endpoint>>,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    data_dir: Option<PathBuf>,
}

impl Bridge {
    /// `endpoint` is `(url, token)`; with `data_dir`, the endpoint file is re-read on connection failures.
    pub fn new(endpoint: Option<(String, String)>, data_dir: Option<PathBuf>) -> anyhow::Result<std::sync::Arc<Self>> {
        Ok(std::sync::Arc::new(Self {
            http: http_client()?,
            endpoint: Mutex::new(endpoint.map(|(url, token)| Endpoint { url, token, pid: None })),
            session: Mutex::new(None),
            version: Mutex::new(None),
            data_dir,
        }))
    }

    /// Pumps `input` lines through [`Bridge::handle_line`] into `output`.
    /// Requests are forwarded concurrently, except `initialize`, which completes
    /// before the next line is read so that the session id is known.
    pub async fn run(
        self: std::sync::Arc<Self>,
        input: impl AsyncRead + Unpin,
        output: impl AsyncWrite + Unpin + Send + 'static,
    ) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            let mut output = output;
            while let Some(line) = rx.recv().await {
                output.write_all(line.as_bytes()).await?;
                output.write_all(b"\n").await?;
                output.flush().await?;
            }
            anyhow::Ok(())
        });
        let mut reader = BufReader::new(input);
        let mut tasks = JoinSet::new();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf).await? == 0 {
                break;
            }
            let line = String::from_utf8_lossy(&buf).into_owned();
            if is_initialize(&line) {
                for out in self.handle_line(&line).await {
                    let _ = tx.send(out);
                }
            } else {
                let (this, tx) = (self.clone(), tx.clone());
                tasks.spawn(async move {
                    for out in this.handle_line(&line).await {
                        let _ = tx.send(out);
                    }
                });
            }
            while tasks.try_join_next().is_some() {}
        }
        while tasks.join_next().await.is_some() {}
        drop(tx);
        self.end_session().await;
        writer.await?
    }

    /// Forwards one stdin line; returns the lines to write to stdout.
    pub async fn handle_line(&self, line: &str) -> Vec<String> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let err = protocol::error(Value::Null, protocol::PARSE_ERROR, format!("parse error: {e}"));
                return vec![err.to_string()];
            }
        };
        let batch = value.is_array();
        let ids: Vec<Value> = protocol::messages(value.clone())
            .iter()
            .filter(|m| protocol::kind(m) == Kind::Request)
            .filter_map(|m| m.get("id").cloned())
            .collect();
        let replies = match self.forward(&value, &ids).await {
            Err(ForwardError::Unreachable(_)) if self.rediscover().await => self.forward(&value, &ids).await,
            other => other,
        };
        let replies = match replies {
            Ok(replies) => replies,
            Err(ForwardError::Unreachable(message) | ForwardError::Failed(message)) => errors_for(&ids, &message),
        };
        if batch && !replies.is_empty() {
            vec![Value::Array(replies).to_string()]
        } else {
            replies.iter().map(Value::to_string).collect()
        }
    }

    async fn forward(&self, msg: &Value, ids: &[Value]) -> Result<Vec<Value>, ForwardError> {
        let Some(Endpoint { url, token, pid }) = lock(&self.endpoint).clone() else {
            return Err(ForwardError::Unreachable(NOT_RUNNING.into()));
        };
        if pid.is_some_and(|pid| !pid_alive(pid)) {
            // Remail quit; another process may own the port now.
            lock(&self.endpoint).take();
            return Err(ForwardError::Unreachable(NOT_RUNNING.into()));
        }
        let mut req = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(ACCEPT, ACCEPT_BOTH)
            .header(CONTENT_TYPE, "application/json")
            .body(msg.to_string());
        if let Some(session) = lock(&self.session).clone() {
            req = req.header(protocol::SESSION_HEADER, session);
        }
        if let Some(version) = lock(&self.version).clone() {
            req = req.header(protocol::VERSION_HEADER, version);
        }
        let resp = req.send().await.map_err(|e| {
            let connect = e.is_connect();
            let detail = format!("cannot reach {url}: {}", error_chain(&e.without_url()));
            tracing::warn!("mcp bridge: {detail}");
            if connect {
                ForwardError::Unreachable(NOT_RUNNING.into())
            } else {
                ForwardError::Failed(detail)
            }
        })?;
        if let Some(session) = resp.headers().get(protocol::SESSION_HEADER).and_then(|v| v.to_str().ok()) {
            *lock(&self.session) = Some(session.to_string());
        }
        match resp.status() {
            StatusCode::UNAUTHORIZED => return Err(ForwardError::Unreachable(BAD_TOKEN.into())),
            StatusCode::ACCEPTED => return Ok(Vec::new()),
            s if !s.is_success() => {
                let body = resp.text().await.unwrap_or_default();
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    if protocol::kind(&v) == Kind::Response && !ids.is_empty() {
                        return Ok(vec![v]);
                    }
                }
                return Err(ForwardError::Failed(format!("Remail MCP endpoint returned HTTP {s}")));
            }
            _ => {}
        }
        let mut pending: Vec<Value> = ids.to_vec();
        let mut out = Vec::new();
        sse::read_messages(resp, |m| {
            if protocol::kind(&m) == Kind::Response {
                pending.retain(|id| Some(id) != m.get("id"));
                self.note_version(&m);
            }
            out.push(m);
            !ids.is_empty() && pending.is_empty()
        })
        .await
        .map_err(|e| ForwardError::Failed(format!("{e:#}")))?;
        Ok(out)
    }

    /// Remembers the negotiated protocol version from an initialize result.
    fn note_version(&self, msg: &Value) {
        let result = msg.get("result");
        let is_init = result.is_some_and(|r| r.get("serverInfo").is_some() && r.get("capabilities").is_some());
        if let Some(v) = result.and_then(|r| r.get("protocolVersion")).and_then(Value::as_str).filter(|_| is_init) {
            *lock(&self.version) = Some(v.to_string());
        }
    }

    /// Re-reads (and verifies) the endpoint file; true if it now points somewhere new.
    async fn rediscover(&self) -> bool {
        let Some(dir) = self.data_dir.clone() else { return false };
        let Some(ep) = discover_endpoint(&self.http, &dir).await else { return false };
        let fresh = Some(Endpoint::from(ep));
        let mut endpoint = lock(&self.endpoint);
        if *endpoint == fresh {
            return false;
        }
        *endpoint = fresh;
        true
    }

    async fn end_session(&self) {
        let Some(session) = lock(&self.session).take() else { return };
        let Some(Endpoint { url, token, .. }) = lock(&self.endpoint).clone() else { return };
        let req = self
            .http
            .delete(url)
            .bearer_auth(token)
            .header(protocol::SESSION_HEADER, session)
            .timeout(std::time::Duration::from_secs(2));
        let _ = req.send().await;
    }
}

fn is_initialize(line: &str) -> bool {
    serde_json::from_str::<Value>(line.trim())
        .is_ok_and(|v| v.get("method").and_then(Value::as_str) == Some("initialize"))
}

fn errors_for(ids: &[Value], message: &str) -> Vec<Value> {
    ids.iter().map(|id| protocol::error(id.clone(), protocol::SERVER_ERROR, message)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::server::tests::{start, TOKEN};
    use axum::Router;
    use serde_json::json;

    fn parse(lines: &[String]) -> Vec<Value> {
        lines.iter().map(|l| serde_json::from_str(l).expect("json line")).collect()
    }

    #[tokio::test]
    async fn forwards_lines_to_http_server() {
        let server = start().await;
        let url = format!("http://127.0.0.1:{}/mcp", server.port());
        let bridge = Bridge::new(Some((url, TOKEN.into())), None).expect("bridge");

        let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
        let out = parse(&bridge.handle_line(init).await);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
        assert!(lock(&bridge.session).is_some());
        assert_eq!(lock(&bridge.version).as_deref(), Some("2025-03-26"));

        assert!(bridge.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await.is_empty());
        assert!(bridge.handle_line("   ").await.is_empty());

        let out = parse(&bridge.handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await);
        assert_eq!(out[0]["result"]["tools"][0]["name"], "echo");

        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"via bridge"}}}"#;
        let out = parse(&bridge.handle_line(call).await);
        assert_eq!(out[0]["result"]["content"][0]["text"], "via bridge");

        let out = parse(&bridge.handle_line("{oops").await);
        assert_eq!(out[0]["error"]["code"], protocol::PARSE_ERROR);

        let batch = r#"[{"jsonrpc":"2.0","id":3,"method":"ping"},{"jsonrpc":"2.0","id":4,"method":"ping"}]"#;
        let out = parse(&bridge.handle_line(batch).await);
        assert_eq!(out[0].as_array().map(Vec::len), Some(2));
        server.stop();
    }

    #[tokio::test]
    async fn unreachable_endpoint_explains_how_to_fix() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let bridge = Bridge::new(Some((format!("http://127.0.0.1:{port}/mcp"), "t".into())), None).expect("bridge");
        let out = parse(&bridge.handle_line(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).await);
        assert_eq!(out[0]["id"], 7);
        assert!(out[0]["error"]["message"].as_str().is_some_and(|m| m.contains("Start Remail")));
        assert!(bridge.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await.is_empty());

        let none = Bridge::new(None, None).expect("bridge");
        let out = parse(&none.handle_line(r#"{"jsonrpc":"2.0","id":"x","method":"tools/list"}"#).await);
        assert_eq!(out[0]["id"], "x");
        assert_eq!(out[0]["error"]["code"], protocol::SERVER_ERROR);
    }

    #[tokio::test]
    async fn rediscovers_endpoint_and_pumps_io() {
        let server = start().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let bridge = Bridge::new(None, Some(dir.path().to_path_buf())).expect("bridge");
        let ep = EndpointFile {
            url: format!("http://127.0.0.1:{}/mcp", server.port()),
            token: TOKEN.into(),
            pid: std::process::id(),
        };
        write_endpoint_file(dir.path(), &ep).expect("write");
        assert_eq!(read_endpoint_file(dir.path()), Some(ep));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(ENDPOINT_FILE)).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"pumped"}}}"#,
        ]
        .join("\n");
        let (mut out_rx, out_tx) = (Vec::new(), tokio::io::duplex(64 * 1024));
        let (read_half, write_half) = out_tx;
        bridge.clone().run(input.as_bytes(), write_half).await.expect("run");
        let mut read_half = read_half;
        tokio::io::AsyncReadExt::read_to_end(&mut read_half, &mut out_rx).await.expect("read");
        let text = String::from_utf8(out_rx).expect("utf8");
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).expect("json")).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["result"]["content"], json!([{ "type": "text", "text": "pumped" }]));

        remove_endpoint_file(dir.path());
        assert_eq!(read_endpoint_file(dir.path()), None);
        server.stop();
    }

    /// A stale endpoint file must not hand the token to whatever listens on that port now.
    #[tokio::test]
    async fn stale_endpoint_files_are_not_trusted() {
        let server = start().await;
        let http = reqwest::Client::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let url = format!("http://127.0.0.1:{}/mcp", server.port());
        let good = EndpointFile { url: url.clone(), token: TOKEN.into(), pid: std::process::id() };
        assert!(verify_endpoint(&http, &good).await);
        write_endpoint_file(dir.path(), &good).expect("write");
        assert_eq!(discover_endpoint(&http, dir.path()).await, Some(good.clone()));

        // A process that answers /health with 200 but doesn't know the token.
        let impostor = Router::new().route("/health", axum::routing::get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move { axum::serve(listener, impostor).await });
        let other = EndpointFile { url: format!("http://127.0.0.1:{port}/mcp"), ..good.clone() };
        assert!(!verify_endpoint(&http, &other).await);
        // The right server, but the app that wrote the file is gone.
        #[cfg(unix)]
        {
            let mut child = std::process::Command::new("true").spawn().expect("spawn");
            let dead = child.id();
            child.wait().expect("wait");
            assert!(!verify_endpoint(&http, &EndpointFile { pid: dead, ..good.clone() }).await);
        }
        assert!(!verify_endpoint(&http, &EndpointFile { token: "other".into(), ..good }).await);
        assert!(!format!("{other:?}").contains(TOKEN));
        server.stop();
    }
}
