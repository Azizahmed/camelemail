//! Model Context Protocol support.
//!
//! Remail speaks MCP in both directions: [`client`] connects to external MCP
//! servers (stdio or Streamable HTTP) so the assistant can use their tools,
//! [`server`] exposes Remail's own tools to external agents over Streamable
//! HTTP, and [`bridge`] proxies stdio-only agents to that HTTP endpoint.
//! [`import`] reads server definitions other apps already have configured.
//!
//! Only the tools capability is implemented; JSON-RPC 2.0 framing lives in
//! [`protocol`].

pub mod bridge;
pub mod client;
mod http;
pub mod import;
pub mod protocol;
pub mod server;
pub(crate) mod sse;
mod stdio;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool advertised by an MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: Value,
}

/// Flattened result of a `tools/call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// All text content blocks joined with `\n`; other blocks are summarised.
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }

    /// Builds the output from a `CallToolResult` JSON object.
    pub fn from_call_result(result: &Value) -> Self {
        let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let mut parts = Vec::new();
        if let Some(blocks) = result.get("content").and_then(Value::as_array) {
            parts.extend(blocks.iter().map(summarise_block));
        }
        if parts.is_empty() {
            if let Some(structured) = result.get("structuredContent") {
                parts.push(structured.to_string());
            }
        }
        Self { text: parts.join("\n"), is_error }
    }

    /// Serialises as a `CallToolResult` with a single text block.
    pub fn to_call_result(&self) -> Value {
        serde_json::json!({
            "content": [{ "type": "text", "text": self.text }],
            "isError": self.is_error,
        })
    }
}

fn summarise_block(block: &Value) -> String {
    let field = |k: &str| block.get(k).and_then(Value::as_str).unwrap_or("");
    match field("type") {
        "text" => field("text").to_string(),
        "image" | "audio" => format!("[{}: {}]", field("type"), field("mimeType")),
        "resource" => {
            let res = block.get("resource").cloned().unwrap_or_default();
            match res.get("text").and_then(Value::as_str) {
                Some(text) => text.to_string(),
                None => format!("[resource: {}]", res.get("uri").and_then(Value::as_str).unwrap_or("")),
            }
        }
        "resource_link" => format!("[resource link: {}]", field("uri")),
        other => format!("[{other} content]"),
    }
}

/// Locks a std mutex, recovering the data if a holder panicked.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `scheme://host[:port]` of a URL: hosted MCP URLs often carry a secret in the path or query.
pub(crate) fn url_origin(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) if u.host_str().is_some() => match u.port() {
            Some(port) => format!("{}://{}:{port}", u.scheme(), u.host_str().unwrap_or_default()),
            None => format!("{}://{}", u.scheme(), u.host_str().unwrap_or_default()),
        },
        _ => "(invalid URL)".into(),
    }
}

/// Shortens every http(s) URL in `text` to its origin, for logs.
pub fn redact_urls(text: &str) -> String {
    static URL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"(?i)\bhttps?://[^\s'"`<>]*[^\s'"`<>.,;:!?)]"#).expect("url regex"));
    URL.replace_all(text, |c: &regex::Captures| url_origin(&c[0])).into_owned()
}

/// Identity a server reports during `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub instructions: Option<String>,
}

const MAX_TOOL_NAME: usize = 64;

/// Builds the `mcp__<server>__<tool>` name exposed to LLM APIs, restricted to
/// `[a-zA-Z0-9_-]` and at most 64 characters (truncated with a hash suffix).
pub fn sanitize_tool_name(server_name: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
    };
    let full = format!("mcp__{}__{}", clean(server_name), clean(tool));
    if full.len() <= MAX_TOOL_NAME {
        return full;
    }
    let hash = fnv1a(format!("{server_name}\0{tool}").as_bytes());
    let suffix = format!("_{hash:08x}");
    let mut out = full[..MAX_TOOL_NAME - suffix.len()].to_string();
    out.push_str(&suffix);
    out
}

/// FNV-1a (32-bit): a hash that is stable across Rust releases.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_names_are_sanitized_and_bounded() {
        assert_eq!(sanitize_tool_name("git hub", "list.repos"), "mcp__git_hub__list_repos");
        let long = sanitize_tool_name("a-very-long-server-name-indeed", &"tool_".repeat(12));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with("mcp__a-very-long-server-name-indeed__tool_"));
        assert_eq!(long, sanitize_tool_name("a-very-long-server-name-indeed", &"tool_".repeat(12)));
        assert_ne!(long, sanitize_tool_name("a-very-long-server-name-indeed", &"tool_".repeat(13)));
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn call_results_are_flattened() {
        let out = ToolOutput::from_call_result(&json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "image", "data": "...", "mimeType": "image/png" },
                { "type": "resource", "resource": { "uri": "file:///a", "text": "body" } },
                { "type": "resource_link", "uri": "file:///b", "name": "b" },
            ],
            "isError": true,
        }));
        assert_eq!(out.text, "hello\n[image: image/png]\nbody\n[resource link: file:///b]");
        assert!(out.is_error);
        let structured = ToolOutput::from_call_result(&json!({ "content": [], "structuredContent": { "n": 1 } }));
        assert_eq!(structured, ToolOutput::ok("{\"n\":1}"));
    }

    #[test]
    fn urls_are_redacted_to_their_origin() {
        assert_eq!(url_origin("https://mcp.example.com/s/SECRET/mcp?key=x"), "https://mcp.example.com");
        assert_eq!(url_origin("http://127.0.0.1:8080/mcp"), "http://127.0.0.1:8080");
        assert_eq!(
            redact_urls("cannot reach https://h.example/s/SECRET/mcp: timed out (`http://a.b:1/x?t=SECRET`)"),
            "cannot reach https://h.example: timed out (`http://a.b:1`)"
        );
    }
}
