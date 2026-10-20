//! JSON-RPC 2.0 message helpers and MCP protocol constants.

use serde_json::{json, Value};

/// Protocol revisions we speak, newest first.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
pub const LATEST_VERSION: &str = SUPPORTED_VERSIONS[0];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// Implementation-defined server error, used for transport failures.
pub const SERVER_ERROR: i64 = -32000;

pub const SESSION_HEADER: &str = "mcp-session-id";
pub const VERSION_HEADER: &str = "mcp-protocol-version";

pub fn request(id: u64, method: &str, params: Option<Value>) -> Value {
    let mut msg = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    if let Some(params) = params {
        msg["params"] = params;
    }
    msg
}

pub fn notification(method: &str, params: Option<Value>) -> Value {
    let mut msg = json!({ "jsonrpc": "2.0", "method": method });
    if let Some(params) = params {
        msg["params"] = params;
    }
    msg
}

pub fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// The role a JSON-RPC message plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Request,
    Notification,
    Response,
    Invalid,
}

pub fn kind(msg: &Value) -> Kind {
    let Some(obj) = msg.as_object() else { return Kind::Invalid };
    let has_id = obj.get("id").is_some_and(|id| !id.is_null());
    match (obj.get("method").and_then(Value::as_str), has_id) {
        (Some(_), true) => Kind::Request,
        (Some(_), false) => Kind::Notification,
        (None, _) if obj.contains_key("result") || obj.contains_key("error") => Kind::Response,
        _ => Kind::Invalid,
    }
}

/// Numeric request id, tolerating servers that echo ids back as strings.
pub fn numeric_id(msg: &Value) -> Option<u64> {
    match msg.get("id")? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Converts a response into its `result`, or an error describing the JSON-RPC error.
pub fn into_result(msg: Value) -> anyhow::Result<Value> {
    if let Some(err) = msg.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
        anyhow::bail!("MCP error {code}: {message}");
    }
    match msg {
        Value::Object(mut obj) => Ok(obj.remove("result").unwrap_or(Value::Null)),
        _ => anyhow::bail!("malformed JSON-RPC response"),
    }
}

/// Answer to a server→client request: `{}` for `ping`, "method not found" otherwise.
pub fn reply_to_peer_request(msg: &Value) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    match msg.get("method").and_then(Value::as_str) {
        Some("ping") => result(id, json!({})),
        Some(method) => error(id, METHOD_NOT_FOUND, format!("method not found: {method}")),
        None => error(id, INVALID_REQUEST, "invalid request"),
    }
}

/// Splits a single message or a batch array into its messages.
pub fn messages(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        other => vec![other],
    }
}
