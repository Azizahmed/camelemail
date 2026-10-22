//! Language-model access for the assistant: OpenAI-compatible Chat
//! Completions and Anthropic-compatible Messages, both streamed, with tool
//! calling. Also the prompts for the one-shot writing tasks (summarize,
//! reply, compose, rewrite) and Smart Inbox sorting.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::classify::Category;
use crate::compose::escape_html;
use crate::mcp::sse::SseParser;
use crate::model::{EmailAddress, MessageFull, ThreadDetail};

/// Secret-store id of the API key the user entered (before AI was set up by
/// the organisation; deleted when the setup moves to a Camel Mail account).
pub const KEY_SECRET_ID: &str = "ai-api-key";
/// Secret-store id of the organisation's AI key on this device.
pub const ORG_KEY_SECRET_ID: &str = "org-ai-key";

/// The provider refused the API key (HTTP 401/403).
#[derive(Debug)]
pub struct KeyRejected(pub String);

impl std::fmt::Display for KeyRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyRejected {}

/// Fetches the key again (the organisation's key changed or this device
/// has none yet). `Ok(None)`: there's no key.
pub type KeySource = Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<Option<String>>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiSettings {
    pub enabled: bool,
    /// `openai` (Chat Completions) or `anthropic` (Messages).
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Extra guidance for everything the assistant writes (tone, name, language).
    pub instructions: String,
    /// Let the model refine Smart Inbox sorting of new mail.
    pub smart_sort: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: String::new(),
            max_tokens: 4096,
            instructions: String::new(),
            smart_sort: false,
        }
    }
}

impl AiSettings {
    fn anthropic(&self) -> bool {
        self.provider == "anthropic"
    }

    fn base(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }

    fn endpoint(&self) -> String {
        let base = self.base();
        if self.anthropic() {
            if base.ends_with("/messages") {
                base
            } else if base.ends_with("/v1") {
                format!("{base}/messages")
            } else {
                format!("{base}/v1/messages")
            }
        } else if base.ends_with("/chat/completions") {
            base
        } else {
            format!("{base}/chat/completions")
        }
    }

    fn models_endpoint(&self) -> String {
        let base = self.base();
        let base = base.trim_end_matches("/chat/completions").trim_end_matches("/messages");
        if self.anthropic() && !base.ends_with("/v1") {
            format!("{base}/v1/models")
        } else {
            format!("{base}/models")
        }
    }

    /// Local servers (Ollama, LM Studio, …) work without a key.
    pub fn is_local(&self) -> bool {
        let b = self.base_url.to_ascii_lowercase();
        b.contains("://localhost") || b.contains("://127.") || b.contains("://[::1]") || b.contains("://0.0.0.0")
    }
}

// ----------------------------------------------------------------------------
// Conversation model (provider-neutral)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Block {
    Text { text: String },
    #[serde(rename_all = "camelCase")]
    ToolUse { id: String, name: String, input: Value },
    #[serde(rename_all = "camelCase")]
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Message {
        Message { role: Role::User, content: vec![Block::Text { text: text.into() }] }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// One assistant turn.
#[derive(Debug, Clone)]
pub struct Turn {
    pub content: Vec<Block>,
    /// `end_turn`, `tool_use` or `max_tokens`.
    pub stop_reason: String,
    /// Tool calls whose arguments weren't valid JSON (e.g. cut off); they must not run.
    pub bad_args: Vec<String>,
}

impl Turn {
    pub fn text(&self) -> String {
        Message { role: Role::Assistant, content: self.content.clone() }.text()
    }

    pub fn tool_uses(&self) -> Vec<(String, String, Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }

    /// Whether the arguments of tool call `id` failed to parse.
    pub fn has_bad_args(&self, id: &str) -> bool {
        self.bad_args.iter().any(|b| b == id)
    }
}

// ----------------------------------------------------------------------------
// Client
// ----------------------------------------------------------------------------

pub struct Client {
    http: reqwest::Client,
    settings: AiSettings,
    key: Mutex<Option<String>>,
    /// Where a refused (or missing) key is fetched again, once per request.
    refetch: Option<KeySource>,
}

/// HTTP client for AI providers: long read timeout for slow models, no total timeout
/// (answers stream). Build it once and share it so connections are reused.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(180))
        .user_agent("Remail")
        .build()
        .unwrap_or_default()
}

impl Client {
    pub fn new(http: reqwest::Client, settings: AiSettings, key: Option<String>) -> Result<Client> {
        Client::with_key_source(http, settings, key, None)
    }

    /// With `refetch`, a key the provider refuses (or a missing one) is
    /// fetched again before giving up: the organisation's key lives on the
    /// Camel Mail server.
    pub fn with_key_source(http: reqwest::Client, settings: AiSettings, key: Option<String>, refetch: Option<KeySource>) -> Result<Client> {
        if settings.base_url.trim().is_empty() {
            bail!("Set the AI provider's API URL in Settings → AI Assistant");
        }
        if settings.model.trim().is_empty() {
            bail!("Choose a model in Settings → AI Assistant");
        }
        let key = key.filter(|k| !k.trim().is_empty());
        if key.is_none() && !settings.is_local() && refetch.is_none() {
            bail!("Add your API key in Settings → AI Assistant");
        }
        Ok(Client { http, settings, key: Mutex::new(key), refetch })
    }

    pub fn settings(&self) -> &AiSettings {
        &self.settings
    }

    fn key(&self) -> Option<String> {
        self.key.lock().unwrap().clone()
    }

    fn request(&self, url: &str, key: Option<&str>) -> reqwest::RequestBuilder {
        self.auth(self.http.post(url).header("content-type", "application/json"), key)
    }

    fn auth(&self, mut r: reqwest::RequestBuilder, key: Option<&str>) -> reqwest::RequestBuilder {
        if let Some(k) = key {
            if self.settings.anthropic() {
                r = r.header("x-api-key", k);
                // Most Anthropic-compatible gateways take a bearer token instead.
                if !self.settings.base_url.contains("api.anthropic.com") {
                    r = r.bearer_auth(k);
                }
            } else {
                r = r.bearer_auth(k);
            }
        }
        if self.settings.anthropic() {
            r = r.header("anthropic-version", "2023-06-01");
        }
        r
    }

    /// The key to use, fetched first when there's none and it can be.
    async fn current_key(&self) -> Result<Option<String>> {
        if let Some(k) = self.key() {
            return Ok(Some(k));
        }
        match &self.refetch {
            Some(f) if !self.settings.is_local() => {
                let k = f().await?.filter(|k| !k.trim().is_empty());
                *self.key.lock().unwrap() = k.clone();
                if k.is_none() {
                    bail!("Your organisation hasn't added an API key for AI assistance yet");
                }
                Ok(k)
            }
            _ => Ok(None),
        }
    }

    /// The provider refused `used`: fetch the key again. A different key
    /// means try once more.
    async fn refetched(&self, used: Option<&str>) -> Option<String> {
        let f = self.refetch.as_ref()?;
        match f().await {
            Ok(Some(k)) if !k.trim().is_empty() && Some(k.as_str()) != used => {
                *self.key.lock().unwrap() = Some(k.clone());
                Some(k)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::info!("couldn't fetch the AI key again: {e:#}");
                None
            }
        }
    }

    /// Send a request built by `build` with the key; when the provider
    /// refuses the key, fetch it again and retry once.
    async fn send(&self, build: impl Fn(Option<&str>) -> reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let key = self.current_key().await?;
        let resp = build(key.as_deref()).send().await.context("contacting the AI provider")?;
        if matches!(resp.status().as_u16(), 401 | 403) {
            if let Some(new) = self.refetched(key.as_deref()).await {
                return build(Some(&new)).send().await.context("contacting the AI provider");
            }
        }
        Ok(resp)
    }

    /// Model ids the provider offers.
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let r = self.send(|key| self.auth(self.http.get(self.settings.models_endpoint()), key)).await?;
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(api_failure(status, &body));
        }
        let v: Value = serde_json::from_str(&body).context("unexpected reply from the provider")?;
        let list = v.get("data").or_else(|| v.get("models")).and_then(|d| d.as_array()).cloned().unwrap_or_default();
        let mut ids: Vec<String> = list
            .iter()
            .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(|s| s.as_str()).map(String::from))
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Run one turn, streaming text through `on_text`.
    pub async fn stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: Option<u32>,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<Turn> {
        let max_tokens = max_tokens.unwrap_or(self.settings.max_tokens).max(256);
        let body = if self.settings.anthropic() {
            anthropic_body(&self.settings.model, system, messages, tools, max_tokens)
        } else {
            openai_body(&self.settings, system, messages, tools, max_tokens)
        };
        let endpoint = self.settings.endpoint();
        let resp = self.send(|key| self.request(&endpoint, key).json(&body)).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(api_failure(status, &text));
        }
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("event-stream"));
        if !is_sse {
            // Some compatible servers ignore `stream: true`.
            let v: Value = resp.json().await.context("unexpected reply from the provider")?;
            let turn = if self.settings.anthropic() { anthropic_full(&v)? } else { openai_full(&v)? };
            let text = turn.text();
            if !text.is_empty() {
                on_text(&text);
            }
            return Ok(turn);
        }
        let mut acc = if self.settings.anthropic() { Acc::Anthropic(Default::default()) } else { Acc::OpenAi(Default::default()) };
        let mut sse = SseParser::default();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("the AI provider closed the connection")?;
            for data in sse.feed(&chunk)? {
                acc.feed_event(&data, on_text)?;
            }
        }
        if let Some(data) = sse.finish() {
            acc.feed_event(&data, on_text)?;
        }
        acc.finish()
    }

    /// Plain request/response without tools.
    pub async fn complete(&self, system: &str, user: &str, max_tokens: Option<u32>) -> Result<String> {
        let turn = self.stream(system, &[Message::user(user)], &[], max_tokens, &mut |_| {}).await?;
        Ok(turn.text())
    }
}

/// An unsuccessful answer as an error; a refused key is a [`KeyRejected`].
fn api_failure(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let msg = api_error(status, body);
    if matches!(status.as_u16(), 401 | 403) {
        anyhow::Error::new(KeyRejected(msg))
    } else {
        anyhow!(msg)
    }
}

fn api_error(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("message"))
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status.as_u16() {
        401 | 403 => format!("The AI provider rejected the API key ({detail})"),
        404 => format!("The AI provider doesn't know this model or URL ({detail})"),
        429 => format!("The AI provider is rate limiting requests ({detail})"),
        _ => format!("AI provider error {}: {detail}", status.as_u16()),
    }
}

// ----------------------------------------------------------------------------
// Request bodies
// ----------------------------------------------------------------------------

fn openai_body(s: &AiSettings, system: &str, messages: &[Message], tools: &[ToolDef], max_tokens: u32) -> Value {
    let mut out = vec![json!({ "role": "system", "content": system })];
    for m in messages {
        match m.role {
            Role::User => {
                for b in &m.content {
                    if let Block::ToolResult { tool_use_id, content, .. } = b {
                        out.push(json!({ "role": "tool", "tool_call_id": tool_use_id, "content": content }));
                    }
                }
                let text = m.text();
                if !text.is_empty() {
                    out.push(json!({ "role": "user", "content": text }));
                }
            }
            Role::Assistant => {
                let calls: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::ToolUse { id, name, input } => Some(json!({
                            "id": id, "type": "function",
                            "function": { "name": name, "arguments": input.to_string() }
                        })),
                        _ => None,
                    })
                    .collect();
                let text = m.text();
                let mut msg = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) } });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
        }
    }
    let mut body = json!({ "model": s.model, "messages": out, "stream": true });
    // OpenAI's newer models only accept `max_completion_tokens`; compatible servers expect `max_tokens`.
    if s.base_url.contains("api.openai.com") {
        body["max_completion_tokens"] = json!(max_tokens);
    } else {
        body["max_tokens"] = json!(max_tokens);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.input_schema } }))
                .collect(),
        );
    }
    body
}

fn anthropic_body(model: &str, system: &str, messages: &[Message], tools: &[ToolDef], max_tokens: u32) -> Value {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|m| {
            let content: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text } if text.is_empty() => None,
                    Block::Text { text } => Some(json!({ "type": "text", "text": text })),
                    Block::ToolUse { id, name, input } => Some(json!({ "type": "tool_use", "id": id, "name": name, "input": input })),
                    Block::ToolResult { tool_use_id, content, is_error } => Some(json!({
                        "type": "tool_result", "tool_use_id": tool_use_id,
                        "content": if content.is_empty() { "(empty)" } else { content.as_str() }, "is_error": is_error
                    })),
                })
                .collect();
            json!({ "role": if m.role == Role::User { "user" } else { "assistant" }, "content": content })
        })
        .collect();
    let mut body = json!({ "model": model, "max_tokens": max_tokens, "system": system, "messages": msgs, "stream": true });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })).collect(),
        );
    }
    body
}

/// Tool arguments; Err (with the raw text kept as `_raw`) when they aren't a JSON object.
fn parse_args(raw: &str) -> std::result::Result<Value, Value> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Err(json!({ "_raw": raw })),
    }
}

/// Adds a tool call to `content`, noting its id in `bad` if the arguments didn't parse.
fn push_tool_use(content: &mut Vec<Block>, bad: &mut Vec<String>, id: String, name: String, args: &str) {
    let input = parse_args(args).unwrap_or_else(|raw| {
        bad.push(id.clone());
        raw
    });
    content.push(Block::ToolUse { id, name, input });
}

/// Anthropic requires tool_use ids; some compatible gateways send none.
fn anthropic_tool_id(id: Option<&str>) -> String {
    match id.filter(|i| !i.is_empty()) {
        Some(i) => i.to_string(),
        None => format!("toolu_{}", uuid::Uuid::new_v4().simple()),
    }
}

fn openai_full(v: &Value) -> Result<Turn> {
    if let Some(e) = v.pointer("/error/message").and_then(|m| m.as_str()) {
        bail!("AI provider error: {e}");
    }
    let msg = v.pointer("/choices/0/message").context("unexpected reply from the provider")?;
    let mut content = Vec::new();
    let mut bad_args = Vec::new();
    if let Some(t) = msg.get("content").and_then(|c| c.as_str()).filter(|t| !t.is_empty()) {
        content.push(Block::Text { text: t.to_string() });
    }
    for c in msg.get("tool_calls").and_then(|c| c.as_array()).into_iter().flatten() {
        let id = c
            .get("id")
            .and_then(|i| i.as_str())
            .filter(|i| !i.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
        let name = c.pointer("/function/name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
        let args = c.pointer("/function/arguments").and_then(|a| a.as_str()).unwrap_or("");
        push_tool_use(&mut content, &mut bad_args, id, name, args);
    }
    let finish = v.pointer("/choices/0/finish_reason").and_then(|f| f.as_str()).unwrap_or("stop");
    Ok(Turn { stop_reason: stop_from_openai(finish, &content), content, bad_args })
}

fn anthropic_full(v: &Value) -> Result<Turn> {
    if let Some(e) = v.pointer("/error/message").and_then(|m| m.as_str()) {
        bail!("AI provider error: {e}");
    }
    let mut content = Vec::new();
    for b in v.get("content").and_then(|c| c.as_array()).context("unexpected reply from the provider")? {
        match b.get("type").and_then(|t| t.as_str()) {
            Some("text") => content.push(Block::Text { text: b.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string() }),
            Some("tool_use") => content.push(Block::ToolUse {
                id: anthropic_tool_id(b.get("id").and_then(|i| i.as_str())),
                name: b.get("name").and_then(|i| i.as_str()).unwrap_or_default().to_string(),
                input: b.get("input").cloned().unwrap_or(json!({})),
            }),
            _ => {}
        }
    }
    let stop = v.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("end_turn").to_string();
    Ok(Turn { content, stop_reason: stop, bad_args: Vec::new() })
}

fn stop_from_openai(finish: &str, content: &[Block]) -> String {
    if content.iter().any(|b| matches!(b, Block::ToolUse { .. })) {
        "tool_use".into()
    } else if finish == "length" {
        "max_tokens".into()
    } else {
        "end_turn".into()
    }
}

// ----------------------------------------------------------------------------
// Streaming
// ----------------------------------------------------------------------------

#[derive(Default)]
struct OpenAiAcc {
    text: String,
    /// index → (id, name, arguments)
    calls: Vec<(String, String, String)>,
    finish: String,
    /// `finish_reason` or `[DONE]` arrived.
    done: bool,
}

enum Partial {
    Text(String),
    Tool { id: String, name: String, json: String },
    Other,
}

#[derive(Default)]
struct AnthropicAcc {
    blocks: Vec<Partial>,
    stop: String,
    /// `message_stop` or a final `stop_reason` arrived.
    done: bool,
}

enum Acc {
    OpenAi(OpenAiAcc),
    Anthropic(AnthropicAcc),
}

impl Acc {
    /// One SSE `data` payload.
    fn feed_event(&mut self, data: &str, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
        if data.trim() == "[DONE]" {
            if let Acc::OpenAi(a) = self {
                a.done = true;
            }
            return Ok(());
        }
        match serde_json::from_str::<Value>(data) {
            Ok(v) => self.feed(&v, on_text),
            Err(_) => Ok(()),
        }
    }

    fn feed(&mut self, v: &Value, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
        if let Some(e) = v.get("error") {
            let msg = e.get("message").and_then(|m| m.as_str()).map(String::from).unwrap_or_else(|| e.to_string());
            bail!("AI provider error: {msg}");
        }
        match self {
            Acc::OpenAi(a) => {
                let Some(choice) = v.pointer("/choices/0") else { return Ok(()) };
                if let Some(delta) = choice.get("delta") {
                    if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
                        if !t.is_empty() {
                            a.text.push_str(t);
                            on_text(t);
                        }
                    }
                    for c in delta.get("tool_calls").and_then(|c| c.as_array()).into_iter().flatten() {
                        let id = c.get("id").and_then(|i| i.as_str()).filter(|i| !i.is_empty());
                        let name = c.pointer("/function/name").and_then(|n| n.as_str()).filter(|n| !n.is_empty());
                        let idx = match c.get("index").and_then(|i| i.as_u64()) {
                            Some(i) => i as usize,
                            // Without an index, a fragment continues the last call unless it starts another one.
                            None => match a.calls.last() {
                                Some(last) => {
                                    let new_id = id.is_some_and(|id| !last.0.is_empty() && last.0 != id);
                                    let new_name = name.is_some_and(|n| !last.1.is_empty() && !n.starts_with(last.1.as_str()));
                                    if new_id || new_name { a.calls.len() } else { a.calls.len() - 1 }
                                }
                                None => 0,
                            },
                        };
                        while a.calls.len() <= idx {
                            a.calls.push(Default::default());
                        }
                        let slot = &mut a.calls[idx];
                        if let Some(id) = id {
                            slot.0 = id.to_string();
                        }
                        if let Some(n) = name {
                            // Some servers repeat the whole name on every delta; others stream it in pieces.
                            if n.starts_with(slot.1.as_str()) {
                                slot.1 = n.to_string();
                            } else if !slot.1.ends_with(n) {
                                slot.1.push_str(n);
                            }
                        }
                        if let Some(args) = c.pointer("/function/arguments").and_then(|n| n.as_str()) {
                            slot.2.push_str(args);
                        }
                    }
                }
                if let Some(f) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    a.finish = f.to_string();
                    a.done = true;
                }
            }
            Acc::Anthropic(a) => match v.get("type").and_then(|t| t.as_str()) {
                Some("content_block_start") => {
                    let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(a.blocks.len() as u64) as usize;
                    let b = v.get("content_block").cloned().unwrap_or_default();
                    let p = match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            let t = b.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string();
                            if !t.is_empty() {
                                on_text(&t);
                            }
                            Partial::Text(t)
                        }
                        Some("tool_use") => Partial::Tool {
                            id: anthropic_tool_id(b.get("id").and_then(|i| i.as_str())),
                            name: b.get("name").and_then(|i| i.as_str()).unwrap_or_default().to_string(),
                            json: String::new(),
                        },
                        _ => Partial::Other,
                    };
                    while a.blocks.len() < idx {
                        a.blocks.push(Partial::Other);
                    }
                    if a.blocks.len() == idx {
                        a.blocks.push(p);
                    } else {
                        a.blocks[idx] = p;
                    }
                }
                Some("content_block_delta") => {
                    let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    let d = v.get("delta").cloned().unwrap_or_default();
                    match (a.blocks.get_mut(idx), d.get("type").and_then(|t| t.as_str())) {
                        (Some(Partial::Text(t)), Some("text_delta")) => {
                            let s = d.get("text").and_then(|t| t.as_str()).unwrap_or_default();
                            t.push_str(s);
                            on_text(s);
                        }
                        (Some(Partial::Tool { json, .. }), Some("input_json_delta")) => {
                            json.push_str(d.get("partial_json").and_then(|t| t.as_str()).unwrap_or_default());
                        }
                        _ => {}
                    }
                }
                Some("message_delta") => {
                    if let Some(s) = v.pointer("/delta/stop_reason").and_then(|s| s.as_str()) {
                        a.stop = s.to_string();
                        a.done = true;
                    }
                }
                Some("message_stop") => a.done = true,
                _ => {}
            },
        }
        Ok(())
    }

    /// The finished turn; Err if the stream ended before the provider said it was done.
    fn finish(self) -> Result<Turn> {
        let done = match &self {
            Acc::OpenAi(a) => a.done,
            Acc::Anthropic(a) => a.done,
        };
        if !done {
            bail!("the AI provider closed the connection before the answer was complete");
        }
        let mut bad_args = Vec::new();
        Ok(match self {
            Acc::OpenAi(a) => {
                let mut content = Vec::new();
                if !a.text.is_empty() {
                    content.push(Block::Text { text: a.text });
                }
                for (id, name, args) in a.calls {
                    if name.is_empty() {
                        continue;
                    }
                    // Unique even across turns: the chat panel matches events by id.
                    let id = if id.is_empty() { format!("call_{}", uuid::Uuid::new_v4().simple()) } else { id };
                    push_tool_use(&mut content, &mut bad_args, id, name, &args);
                }
                Turn { stop_reason: stop_from_openai(&a.finish, &content), content, bad_args }
            }
            Acc::Anthropic(a) => {
                let mut content = Vec::new();
                for p in a.blocks {
                    match p {
                        Partial::Text(t) if !t.is_empty() => content.push(Block::Text { text: t }),
                        Partial::Tool { id, name, json } => push_tool_use(&mut content, &mut bad_args, id, name, &json),
                        _ => {}
                    }
                }
                let stop = if a.stop.is_empty() { "end_turn".into() } else { a.stop };
                Turn { content, stop_reason: stop, bad_args }
            }
        })
    }
}

// ----------------------------------------------------------------------------
// Prompts
// ----------------------------------------------------------------------------

const MAX_MESSAGE_CHARS: usize = 6000;
const MAX_THREAD_CHARS: usize = 40_000;

/// Readable text of a message body, without the quoted history.
pub fn message_text(m: &MessageFull, max_chars: usize) -> String {
    let raw = match (&m.text, &m.html) {
        (Some(t), _) if !t.trim().is_empty() => t.clone(),
        (_, Some(h)) => crate::mime::html_to_text(h),
        _ => m.meta.snippet.clone(),
    };
    let mut out = String::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with('>') {
            continue;
        }
        if (t.starts_with("On ") && t.ends_with("wrote:")) || t.starts_with("---------- Forwarded message") {
            break;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    let collapsed = collapse_blank_lines(&out);
    crate::mime::truncate_chars(collapsed.trim(), max_chars)
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn addr_list(v: &[EmailAddress]) -> String {
    v.iter().map(|a| a.display_full()).collect::<Vec<_>>().join(", ")
}

pub fn format_date(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_millis_opt(ms).single().map(|d| d.format("%a %d %b %Y %H:%M").to_string()).unwrap_or_default()
}

/// The conversation as plain text for a prompt (newest messages kept when long).
pub fn thread_text(t: &ThreadDetail) -> String {
    let mut parts: Vec<String> = t
        .messages
        .iter()
        .filter(|m| !m.meta.is_draft)
        .map(|m| {
            let mut s = format!("From: {}\nDate: {}\n", m.meta.from.display_full(), format_date(m.meta.date));
            if !m.meta.to.is_empty() {
                s.push_str(&format!("To: {}\n", addr_list(&m.meta.to)));
            }
            if !m.meta.cc.is_empty() {
                s.push_str(&format!("Cc: {}\n", addr_list(&m.meta.cc)));
            }
            if !m.attachments.is_empty() {
                let names: Vec<&str> = m.attachments.iter().map(|a| a.filename.as_str()).collect();
                s.push_str(&format!("Attachments: {}\n", names.join(", ")));
            }
            s.push('\n');
            s.push_str(&message_text(m, MAX_MESSAGE_CHARS));
            s
        })
        .collect();
    let mut total: usize = parts.iter().map(|p| p.len()).sum();
    while total > MAX_THREAD_CHARS && parts.len() > 1 {
        total -= parts.remove(0).len();
    }
    format!("Subject: {}\n\n{}", t.subject, parts.join("\n\n-----\n\n"))
}

/// Shared system prompt preamble.
pub fn base_system(user: &EmailAddress, extra: &str) -> String {
    let now = chrono::Local::now().format("%A %d %B %Y, %H:%M");
    let mut s = format!(
        "You are the AI assistant built into Remail, a desktop email client. The user is {} <{}>. Current local time: {now}.",
        if user.name.is_empty() { "the account owner" } else { user.name.as_str() },
        user.email
    );
    if !extra.trim().is_empty() {
        s.push_str("\n\nThe user's standing instructions (follow them):\n");
        s.push_str(extra.trim());
    }
    s
}

pub fn summarize_prompt() -> &'static str {
    "Summarize the email conversation for the user. Start with a one-sentence TL;DR in bold. Then give up to five short \
     bullet points with the key facts, decisions, dates, amounts and open questions. Finish with a line naming the \
     action items, followed by bullets for what the user needs to do, or a line saying no action is needed. Write all of \
     it, that line too, in the language of the emails (for Arabic emails: 'المطلوب:' and 'لا يلزم أي إجراء.'; in English: \
     'Action items:' and 'No action needed.'). Use Markdown. No preamble."
}

pub fn reply_prompt() -> &'static str {
    "Write the body of an email reply on the user's behalf. Output only the reply body as plain text paragraphs \
     (simple '- ' bullets are fine). No subject line, no quoted text, no signature or sign-off name (Remail adds the \
     user's signature), and no placeholders like [Name] unless information is genuinely missing. Greet the recipient \
     by first name when natural. Match the language and formality of the conversation and keep it concise."
}

pub fn compose_prompt() -> &'static str {
    "Write a new email on the user's behalf. Output format: the first line is 'Subject: <subject>', then an empty line, \
     then the body as plain text paragraphs (simple '- ' bullets are fine). No signature or sign-off name (Remail adds \
     the user's signature) and no placeholders unless information is genuinely missing. If the user writes in another \
     language, write the email in that language."
}

pub fn rewrite_instruction(mode: &str, custom: &str) -> String {
    let m = match mode {
        "improve" => "Improve the writing: clearer, more natural and well structured.".to_string(),
        "fix" => "Fix spelling, grammar and punctuation only. Change nothing else.".to_string(),
        "shorter" => "Make it shorter and more direct while keeping every important point.".to_string(),
        "longer" => "Expand it with a little more detail and warmth, without inventing facts.".to_string(),
        "formal" => "Make the tone more formal and professional.".to_string(),
        "friendly" => "Make the tone warmer and friendlier.".to_string(),
        m if m.starts_with("translate:") => format!("Translate it to {}.", m.trim_start_matches("translate:")),
        _ => custom.trim().to_string(),
    };
    format!(
        "Rewrite the email text the user gives you. {m} Keep names, facts, numbers and links. Output only the rewritten \
         text as plain text paragraphs, with no commentary."
    )
}

/// `Subject: …` first line → (subject, body).
pub fn split_subject(text: &str) -> (Option<String>, String) {
    let t = strip_fences(text);
    let mut lines = t.lines();
    if let Some(first) = lines.clone().next() {
        if let Some(rest) = first.trim().strip_prefix("Subject:").or_else(|| first.trim().strip_prefix("subject:")) {
            lines.next();
            let body: Vec<&str> = lines.collect();
            return (Some(rest.trim().to_string()), body.join("\n").trim().to_string());
        }
    }
    (None, t.trim().to_string())
}

/// Drop a surrounding ``` fence some models add.
pub fn strip_fences(text: &str) -> String {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix("```") {
        let inner = inner.split_once('\n').map(|(_, b)| b).unwrap_or(inner);
        return inner.trim_end().trim_end_matches("```").trim().to_string();
    }
    t.to_string()
}

/// Model output (plain text with light Markdown) → composer HTML.
pub fn text_to_email_html(text: &str) -> String {
    let text = strip_fences(text);
    let mut out = String::new();
    let paragraphs: Vec<&str> = text.split("\n\n").map(|p| p.trim_matches('\n')).filter(|p| !p.trim().is_empty()).collect();
    for (i, para) in paragraphs.iter().enumerate() {
        let lines: Vec<&str> = para.lines().collect();
        let bullet = |l: &str| {
            let t = l.trim_start();
            t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")).map(str::to_string)
        };
        let numbered = |l: &str| {
            let t = l.trim_start();
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            (digits > 0 && t[digits..].starts_with(". ")).then(|| t[digits + 2..].to_string())
        };
        // A list follows its first item; an item going the other way keeps its bullet inside.
        let item_html = |item: &str| {
            let d = dir_attr(item);
            let inside = if d != dir_attr(para) { " style=\"list-style-position:inside\"" } else { "" };
            format!("<li{d}{inside}>{}</li>", inline_md(item))
        };
        if lines.iter().all(|l| bullet(l).is_some()) {
            out.push_str(&format!("<ul{}>", dir_attr(para)));
            for l in &lines {
                out.push_str(&item_html(&bullet(l).unwrap_or_default()));
            }
            out.push_str("</ul>");
        } else if lines.iter().all(|l| numbered(l).is_some()) {
            out.push_str(&format!("<ol{}>", dir_attr(para)));
            for l in &lines {
                out.push_str(&item_html(&numbered(l).unwrap_or_default()));
            }
            out.push_str("</ol>");
        } else {
            for l in &lines {
                let l = l.trim_end();
                if l.is_empty() {
                    out.push_str("<div><br></div>");
                } else {
                    out.push_str(&format!("<div{}>{}</div>", dir_attr(l), inline_md(l)));
                }
            }
        }
        if i + 1 < paragraphs.len() {
            out.push_str("<div><br></div>");
        }
    }
    out
}

/// ` dir="rtl"` for text whose first letter is right-to-left (Arabic, Hebrew…),
/// so every mail app shows that line from the right.
fn dir_attr(text: &str) -> &'static str {
    let rtl = |c: char| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF);
    match text.chars().find(|c| c.is_alphabetic()) {
        Some(c) if rtl(c) => " dir=\"rtl\"",
        _ => "",
    }
}

static BOLD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\*\*([^*]+)\*\*").expect("bold regex"));
static ITALIC: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(^|[^*\w])\*([^*\s][^*]*)\*").expect("italic regex"));
static LINK: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(https?://[^\s<>"]+[^\s<>".,;:!?)])"#).expect("link regex"));

/// Escape, then `**bold**`, `*italic*` and bare links.
fn inline_md(s: &str) -> String {
    let escaped = escape_html(s);
    let bold = BOLD.replace_all(&escaped, "<b>$1</b>");
    let italic = ITALIC.replace_all(&bold, "$1<i>$2</i>");
    LINK.replace_all(&italic, r#"<a href="$1">$1</a>"#).into_owned()
}

// ----------------------------------------------------------------------------
// Smart Inbox sorting
// ----------------------------------------------------------------------------

pub struct SortItem {
    pub id: String,
    pub from: EmailAddress,
    pub subject: String,
    pub snippet: String,
    pub bulk: bool,
}

const SORT_PROMPT: &str = "You sort incoming email for a Smart Inbox. Categories:\n\
- primary: written by a real person to the user, or anything that needs the user's attention or action (questions, \
meetings, documents to review, personal security problems, bills that are due).\n\
- updates: automated notifications, receipts, orders, shipping, account notices, reports, digests of activity.\n\
- promotions: marketing, newsletters, deals, offers, product announcements, event promotions.\n\
- social: social networks, dating and community apps.\n\
- forums: mailing lists, group discussions, community Q&A.\n\
Reply with only a JSON object mapping every id to its category, for example {\"1\":\"primary\",\"2\":\"promotions\"}.";

/// Ask the model to sort a batch; returns `(message id, category)`.
pub async fn classify_batch(client: &Client, items: &[SortItem]) -> Result<Vec<(String, Category)>> {
    if items.is_empty() {
        return Ok(vec![]);
    }
    let mut user = String::new();
    for (i, it) in items.iter().enumerate() {
        user.push_str(&format!(
            "id: {}\nFrom: {}\nSubject: {}\nPreview: {}\nSent by a bulk mailer: {}\n\n",
            i + 1,
            it.from.display_full(),
            it.subject,
            crate::mime::truncate_chars(&it.snippet, 200),
            if it.bulk { "yes" } else { "no" }
        ));
    }
    let reply = client.complete(SORT_PROMPT, &user, None).await?;
    parse_sort_reply(&reply, items)
}

/// The model's `{"1":"primary",…}` answer → `(message id, category)` for the ids it knows.
pub fn parse_sort_reply(reply: &str, items: &[SortItem]) -> Result<Vec<(String, Category)>> {
    let start = reply.find('{').ok_or_else(|| anyhow!("the model didn't return JSON"))?;
    let end = reply.rfind('}').filter(|end| *end > start).ok_or_else(|| anyhow!("the model didn't return JSON"))?;
    let map: Map<String, Value> = serde_json::from_str(&reply[start..=end]).context("the model returned invalid JSON")?;
    let mut out = Vec::new();
    for (k, v) in map {
        let Ok(n) = k.trim().parse::<usize>() else { continue };
        let (Some(item), Some(cat)) = (items.get(n.wrapping_sub(1)), v.as_str().and_then(Category::parse)) else { continue };
        out.push((item.id.clone(), cat));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        let mut s = AiSettings { base_url: "https://api.openai.com/v1/".into(), ..Default::default() };
        assert_eq!(s.endpoint(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(s.models_endpoint(), "https://api.openai.com/v1/models");
        s.provider = "anthropic".into();
        s.base_url = "https://api.anthropic.com".into();
        assert_eq!(s.endpoint(), "https://api.anthropic.com/v1/messages");
        assert_eq!(s.models_endpoint(), "https://api.anthropic.com/v1/models");
        s.base_url = "https://gateway.example/anthropic/v1".into();
        assert_eq!(s.endpoint(), "https://gateway.example/anthropic/v1/messages");
    }

    #[test]
    fn sse_parser_handles_split_chunks() {
        let mut p = SseParser::default();
        assert!(p.feed(b"event: x\ndata: {\"a\"").unwrap().is_empty());
        assert_eq!(p.feed(b":1}\n\ndata: [DONE]\n\n").unwrap(), vec!["{\"a\":1}".to_string(), "[DONE]".to_string()]);
    }

    fn openai_turn(events: &[Value]) -> Result<Turn> {
        let mut acc = Acc::OpenAi(Default::default());
        for v in events {
            acc.feed(v, &mut |_| {})?;
        }
        acc.finish()
    }

    #[test]
    fn openai_repeated_tool_names_are_not_doubled() {
        let t = openai_turn(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"search_emails","arguments":"{\"query\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"search_emails","arguments":"\"x\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ])
        .unwrap();
        assert_eq!(t.tool_uses(), vec![("c1".into(), "search_emails".into(), json!({"query":"x"}))]);
        // Names streamed in pieces still join up.
        let t = openai_turn(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"thread","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ])
        .unwrap();
        assert_eq!(t.tool_uses()[0].1, "read_thread");
    }

    #[test]
    fn openai_tool_calls_without_index() {
        let t = openai_turn(&[
            json!({"choices":[{"delta":{"tool_calls":[{"id":"c1","function":{"name":"search_emails","arguments":"{\"qu"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"ery\":\"x\"}"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"id":"c2","function":{"name":"list_folders","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ])
        .unwrap();
        assert_eq!(
            t.tool_uses(),
            vec![
                ("c1".into(), "search_emails".into(), json!({"query":"x"})),
                ("c2".into(), "list_folders".into(), json!({})),
            ]
        );
        assert!(t.bad_args.is_empty());
    }

    #[test]
    fn stream_without_stop_is_an_error() {
        let err = openai_turn(&[json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"send_email","arguments":"{\"to\":[\"a"}}]}}]})])
            .unwrap_err();
        assert!(err.to_string().contains("closed the connection"), "{err}");
        let mut acc = Acc::Anthropic(Default::default());
        acc.feed(&json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Hi"}}), &mut |_| {}).unwrap();
        assert!(acc.finish().is_err());
        // [DONE] alone counts as the end of an OpenAI stream.
        let mut acc = Acc::OpenAi(Default::default());
        acc.feed_event("{\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}", &mut |_| {}).unwrap();
        acc.feed_event("[DONE]", &mut |_| {}).unwrap();
        assert_eq!(acc.finish().unwrap().text(), "ok");
    }

    #[test]
    fn truncated_tool_arguments_are_flagged() {
        let t = openai_turn(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"send_email","arguments":"{\"to\":[\"a"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"length"}]}),
        ])
        .unwrap();
        assert!(t.has_bad_args("c1"));
        assert_eq!(t.tool_uses()[0].2, json!({"_raw":"{\"to\":[\"a"}));
        let full = openai_full(&json!({"choices":[{"message":{"tool_calls":[{"id":"c2","function":{"name":"x","arguments":"[1"}}]}}]})).unwrap();
        assert!(full.has_bad_args("c2"));
    }

    #[test]
    fn anthropic_empty_tool_ids_are_generated() {
        let mut acc = Acc::Anthropic(Default::default());
        for v in [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"","name":"list_accounts","input":{}}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"list_folders","input":{}}}),
            json!({"type":"message_stop"}),
        ] {
            acc.feed(&v, &mut |_| {}).unwrap();
        }
        let ids: Vec<String> = acc.finish().unwrap().tool_uses().into_iter().map(|u| u.0).collect();
        assert!(ids.iter().all(|id| id.starts_with("toolu_") && id.len() > 6), "{ids:?}");
        assert_ne!(ids[0], ids[1]);
        let full = anthropic_full(&json!({"content":[{"type":"tool_use","id":"","name":"x","input":{}}]})).unwrap();
        assert!(full.tool_uses()[0].0.starts_with("toolu_"));
    }

    #[test]
    fn sort_reply_parsing() {
        let items = vec![SortItem { id: "m1".into(), from: EmailAddress::default(), subject: String::new(), snippet: String::new(), bulk: false }];
        assert!(parse_sort_reply("} oops {", &items).is_err());
        assert!(parse_sort_reply("no json", &items).is_err());
        let got = parse_sort_reply("Sure: {\"1\":\"promotions\",\"9\":\"primary\"}", &items).unwrap();
        assert_eq!(got, vec![("m1".to_string(), Category::Promotions)]);
    }

    #[test]
    fn openai_stream_with_tool_calls() {
        let mut acc = Acc::OpenAi(Default::default());
        let mut seen = String::new();
        let mut cb = |t: &str| seen.push_str(t);
        for v in [
            json!({"choices":[{"delta":{"content":"Hi"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"search_emails","arguments":"{\"qu"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ery\":\"x\"}"}}]},"finish_reason":"tool_calls"}]}),
        ] {
            acc.feed(&v, &mut cb).unwrap();
        }
        let t = acc.finish().unwrap();
        assert_eq!(seen, "Hi");
        assert_eq!(t.stop_reason, "tool_use");
        assert_eq!(t.tool_uses(), vec![("c1".into(), "search_emails".into(), json!({"query":"x"}))]);
    }

    #[test]
    fn anthropic_stream_with_tool_use() {
        let mut acc = Acc::Anthropic(Default::default());
        let mut seen = String::new();
        let mut cb = |t: &str| seen.push_str(t);
        for v in [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me look."}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"read_thread","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"thread_id\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"t:1\"}"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        ] {
            acc.feed(&v, &mut cb).unwrap();
        }
        let t = acc.finish().unwrap();
        assert_eq!(seen, "Let me look.");
        assert_eq!(t.stop_reason, "tool_use");
        assert_eq!(t.tool_uses(), vec![("t1".into(), "read_thread".into(), json!({"thread_id":"t:1"}))]);
    }

    #[test]
    fn provider_error_in_stream() {
        let mut acc = Acc::Anthropic(Default::default());
        let err = acc.feed(&json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}), &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("Overloaded"));
    }

    #[test]
    fn bodies_convert_roles() {
        let msgs = vec![
            Message::user("find invoices"),
            Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse { id: "c1".into(), name: "search_emails".into(), input: json!({"query":"invoice"}) }],
            },
            Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "c1".into(), content: "[]".into(), is_error: false }] },
        ];
        let s = AiSettings { model: "m".into(), ..Default::default() };
        let o = openai_body(&s, "sys", &msgs, &[], 100);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["messages"][2]["tool_calls"][0]["function"]["arguments"], "{\"query\":\"invoice\"}");
        assert_eq!(o["messages"][3]["role"], "tool");
        assert_eq!(o["max_completion_tokens"], 100);
        let a = anthropic_body("m", "sys", &msgs, &[], 100);
        assert_eq!(a["system"], "sys");
        assert_eq!(a["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(a["messages"][2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn email_html_from_text() {
        let html = text_to_email_html("Hi Sara,\n\nThanks for **the notes**.\n- one\n- two\n\nSee https://x.test/a.");
        assert_eq!(
            html,
            "<div>Hi Sara,</div><div><br></div><div>Thanks for <b>the notes</b>.</div><div>- one</div><div>- two</div>\
             <div><br></div><div>See <a href=\"https://x.test/a\">https://x.test/a</a>.</div>"
        );
        assert_eq!(text_to_email_html("- a\n- b"), "<ul><li>a</li><li>b</li></ul>");
        assert_eq!(text_to_email_html("<script>"), "<div>&lt;script&gt;</div>");
        // Arabic lines are marked right-to-left; English ones in the same email aren't.
        assert_eq!(
            text_to_email_html("مرحباً سارة،\nThanks!\n\n- ١٢ ملف\n- ثانياً"),
            "<div dir=\"rtl\">مرحباً سارة،</div><div>Thanks!</div><div><br></div><ul dir=\"rtl\"><li dir=\"rtl\">١٢ ملف</li><li dir=\"rtl\">ثانياً</li></ul>"
        );
        assert_eq!(
            text_to_email_html("- Meeting\n- اجتماع"),
            "<ul><li>Meeting</li><li dir=\"rtl\" style=\"list-style-position:inside\">اجتماع</li></ul>"
        );
    }

    #[test]
    fn subject_split() {
        assert_eq!(split_subject("Subject: Hello\n\nBody here"), (Some("Hello".into()), "Body here".into()));
        assert_eq!(split_subject("```\nJust body\n```"), (None, "Just body".into()));
    }
}
