//! The assistant's tool loop against mock OpenAI- and Anthropic-compatible
//! servers, and Remail's MCP server driven by an MCP client.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use remail_core::ai::AiSettings;
use remail_core::assistant::{ChatEvent, Permissions};
use remail_core::engine::McpSettings;
use remail_core::mcp::client::{McpClient, McpTransport};
use remail_core::secrets::MemoryStore;
use remail_core::store::Store;
use remail_core::*;
use serde_json::{json, Value};

struct Quiet;

impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

fn engine() -> Engine {
    Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(MemoryStore::default()), Arc::new(Quiet)).unwrap()
}

#[derive(Default)]
struct Seen {
    headers: Vec<(String, String)>,
    requests: Vec<Value>,
}

type Shared = Arc<Mutex<Seen>>;

/// Which tool the mock model calls on its first turn.
#[derive(Clone)]
struct Mock {
    seen: Shared,
    tool: &'static str,
    args: Value,
}

fn sse(events: Vec<Value>, done: bool) -> Response {
    let mut body = String::new();
    for e in events {
        body.push_str(&format!("data: {e}\n\n"));
    }
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    ([("content-type", "text/event-stream")], body).into_response()
}

fn record(m: &Mock, headers: &HeaderMap, body: &Value) {
    let mut seen = m.seen.lock().unwrap();
    for (k, v) in headers {
        seen.headers.push((k.to_string(), v.to_str().unwrap_or("").to_string()));
    }
    seen.requests.push(body.clone());
}

async fn openai(State(m): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    record(&m, &headers, &body);
    let tool_result = body["messages"].as_array().unwrap().iter().find(|x| x["role"] == "tool").cloned();
    match tool_result {
        None => sse(
            vec![
                json!({"choices":[{"delta":{"role":"assistant","content":"Let me check. "}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":m.tool,"arguments":""}}]}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":m.args.to_string()}}]}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ],
            true,
        ),
        Some(r) => {
            let content = r["content"].as_str().unwrap_or("").to_string();
            sse(
                vec![
                    json!({"choices":[{"delta":{"content":"Tool said: "}}]}),
                    json!({"choices":[{"delta":{"content":content},"finish_reason":"stop"}]}),
                ],
                true,
            )
        }
    }
}

async fn anthropic(State(m): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    record(&m, &headers, &body);
    let result = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|x| x["content"].as_array().cloned().unwrap_or_default())
        .find(|b| b["type"] == "tool_result");
    match result {
        None => sse(
            vec![
                json!({"type":"message_start","message":{"id":"msg_1","role":"assistant","content":[]}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me check. "}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":m.tool,"input":{}}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":m.args.to_string()}}),
                json!({"type":"content_block_stop","index":1}),
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
                json!({"type":"message_stop"}),
            ],
            false,
        ),
        Some(r) => sse(
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Tool said: "}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":r["content"].as_str().unwrap_or("")}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            ],
            false,
        ),
    }
}

async fn mock(tool: &'static str, args: Value) -> (String, Shared) {
    let seen: Shared = Arc::default();
    let state = Mock { seen: seen.clone(), tool, args };
    let app = Router::new().route("/v1/chat/completions", post(openai)).route("/v1/messages", post(anthropic)).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

async fn chat(e: &Engine, text: &str) -> Vec<ChatEvent> {
    let events: Arc<Mutex<Vec<ChatEvent>>> = Arc::default();
    let sink = events.clone();
    e.ai_chat("c1", "r1", text.into(), None, Arc::new(move |ev| sink.lock().unwrap().push(ev))).await.unwrap();
    let out = events.lock().unwrap().clone();
    out
}

fn text_of(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn openai_compatible_tool_loop() {
    let (url, seen) = mock("list_accounts", json!({})).await;
    let e = engine();
    let s = AiSettings { enabled: true, provider: "openai".into(), base_url: format!("{url}/v1"), model: "mock-1".into(), ..Default::default() };
    e.set_ai_settings(&s, Some("sk-test")).unwrap();

    let events = chat(&e, "Which accounts do I have?").await;
    assert!(events.iter().any(|ev| matches!(ev, ChatEvent::ToolStart { name, .. } if name == "list_accounts")));
    assert!(events.iter().any(|ev| matches!(ev, ChatEvent::ToolEnd { ok: true, .. })));
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
    let text = text_of(&events);
    assert!(text.starts_with("Let me check. Tool said: "), "{text}");
    assert!(text.contains("\"accounts\":[]"), "{text}");

    let seen = seen.lock().unwrap();
    assert!(seen.headers.iter().any(|(k, v)| k == "authorization" && v == "Bearer sk-test"));
    let first = &seen.requests[0];
    assert_eq!(first["model"], "mock-1");
    assert_eq!(first["stream"], true);
    assert!(first["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "search_emails"));
    // The second request replays the tool call and its result.
    let second = &seen.requests[1]["messages"];
    assert!(second.as_array().unwrap().iter().any(|m| m["tool_calls"][0]["id"] == "call_1"));
}

#[tokio::test]
async fn anthropic_compatible_tool_loop_with_declined_approval() {
    let (url, seen) = mock("modify_emails", json!({"threads":[{"account_id":"a","thread_id":"t"}],"action":"archive"})).await;
    let e = engine();
    let s = AiSettings { enabled: true, provider: "anthropic".into(), base_url: url.clone(), model: "claude-mock".into(), ..Default::default() };
    e.set_ai_settings(&s, Some("sk-ant-test")).unwrap();

    let events: Arc<Mutex<Vec<ChatEvent>>> = Arc::default();
    let sink = events.clone();
    let approver = e.clone();
    e.ai_chat(
        "c2",
        "r2",
        "Archive it".into(),
        None,
        Arc::new(move |ev| {
            if let ChatEvent::Approval { id, .. } = &ev {
                approver.ai_approve(id, false);
            }
            sink.lock().unwrap().push(ev);
        }),
    )
    .await
    .unwrap();
    let events = events.lock().unwrap().clone();
    assert!(events.iter().any(|ev| matches!(ev, ChatEvent::Approval { name, .. } if name == "modify_emails")));
    assert!(events.iter().any(|ev| matches!(ev, ChatEvent::ToolEnd { ok: false, .. })));
    assert!(text_of(&events).contains("The user declined this action."));

    let seen = seen.lock().unwrap();
    assert!(seen.headers.iter().any(|(k, v)| k == "x-api-key" && v == "sk-ant-test"));
    assert!(seen.headers.iter().any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
    let first = &seen.requests[0];
    assert!(first["system"].as_str().unwrap().contains("Remail"));
    assert!(first["max_tokens"].as_u64().unwrap() > 0);
    assert!(first["tools"].as_array().unwrap().iter().any(|t| t["name"] == "read_thread" && t["input_schema"]["type"] == "object"));
}

#[tokio::test]
async fn ai_requires_setup() {
    let e = engine();
    let err = e.ai_chat("c", "r", "hi".into(), None, Arc::new(|_| {})).await.unwrap_err();
    assert!(err.to_string().contains("Turn on AI assistance"), "{err}");
}

#[tokio::test]
async fn mcp_server_serves_mailbox_tools() {
    let e = engine();
    let mut s = McpSettings { server_enabled: true, port: 0, ..Default::default() };
    let status = e.set_mcp_settings(s.clone()).await.unwrap();
    assert!(status.running, "{:?}", status.error);
    let url = status.url.clone().unwrap();

    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {}", status.token));
    let transport = McpTransport::Http { url: url.clone(), headers };
    let client = McpClient::connect(&transport, "test", "1").await.unwrap();
    assert_eq!(client.server_info().name, "remail");
    let names: Vec<String> = client.list_tools().await.unwrap().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"search_emails".to_string()) && names.contains(&"create_draft".to_string()));
    assert!(!names.contains(&"send_email".to_string()));

    let out = client.call_tool("list_accounts", json!({}), std::time::Duration::from_secs(10)).await.unwrap();
    assert!(!out.is_error && out.text.contains("\"accounts\":[]"), "{}", out.text);
    let denied = client.call_tool("send_email", json!({"to":["x@y.z"],"body":"hi"}), std::time::Duration::from_secs(10)).await.unwrap();
    assert!(denied.is_error, "{}", denied.text);

    // Permissions apply without reconnecting.
    s.permissions = Permissions { drafts: true, send: true, organize: true };
    s.port = status.url.as_deref().and_then(|u| u.rsplit(':').next()).and_then(|p| p.trim_end_matches("/mcp").parse().ok()).unwrap();
    e.set_mcp_settings(s).await.unwrap();
    let names: Vec<String> = client.list_tools().await.unwrap().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"send_email".to_string()) && names.contains(&"snooze_emails".to_string()));

    // A wrong token is refused.
    let mut bad = BTreeMap::new();
    bad.insert("Authorization".to_string(), "Bearer nope".to_string());
    assert!(McpClient::connect(&McpTransport::Http { url, headers: bad }, "test", "1").await.is_err());
    e.shutdown_mcp();
}

/// An OpenAI-compatible model that calls the next queued tool for each user message.
#[derive(Clone)]
struct Scripted {
    seen: Shared,
    calls: Arc<Mutex<std::collections::VecDeque<(&'static str, Value)>>>,
}

async fn scripted_openai(State(m): State<Scripted>, Json(body): Json<Value>) -> Response {
    m.seen.lock().unwrap().requests.push(body.clone());
    let last_is_tool = body["messages"].as_array().unwrap().last().is_some_and(|x| x["role"] == "tool");
    let next = if last_is_tool { None } else { m.calls.lock().unwrap().pop_front() };
    match next {
        Some((tool, args)) => sse(
            vec![
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("call_{tool}_{}", m.seen.lock().unwrap().requests.len()),"type":"function","function":{"name":tool,"arguments":args.to_string()}}]}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ],
            true,
        ),
        None => sse(vec![json!({"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]})], true),
    }
}

async fn chat_in(e: &Engine, conversation: &str, text: &str, allow: bool) -> Vec<ChatEvent> {
    let events: Arc<Mutex<Vec<ChatEvent>>> = Arc::default();
    let sink = events.clone();
    let approver = e.clone();
    let emit = Arc::new(move |ev: ChatEvent| {
        if let ChatEvent::Approval { id, .. } = &ev {
            approver.ai_approve(id, allow);
        }
        sink.lock().unwrap().push(ev);
    });
    e.ai_chat(conversation, "r", text.into(), None, emit).await.unwrap();
    let out = events.lock().unwrap().clone();
    out
}

/// Outside servers' env/header values live in the secret store, not the database, and
/// v0.1.4 settings with plaintext values are migrated when first read.
#[tokio::test]
async fn mcp_server_credentials_stay_out_of_the_database() {
    use remail_core::mcp::client::{secret_id, McpServerConfig, SECRET_PLACEHOLDER};
    use remail_core::secrets::SecretStore;
    let secrets = Arc::new(MemoryStore::default());
    let e = Engine::with_store(Store::open_in_memory().unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();

    // Saved by an older version: plaintext in the settings row.
    let old = json!({ "servers": [
        { "id": "gh", "name": "GitHub", "enabled": false, "transport": { "type": "stdio", "command": "npx", "env": { "GITHUB_TOKEN": "ghp_old" } } },
        { "id": "web", "name": "Web", "enabled": false, "transport": { "type": "http", "url": "https://mcp.example/s/k/mcp", "headers": { "Authorization": "Bearer web-token" } } }
    ] });
    e.store().set_setting("mcp", &old).unwrap();
    let s = e.mcp_settings();
    let raw: Value = e.store().setting("mcp").unwrap();
    assert!(!raw.to_string().contains("ghp_old") && !raw.to_string().contains("web-token"), "{raw}");
    let McpTransport::Stdio { env, .. } = &s.servers[0].transport else { panic!("stdio") };
    assert_eq!(env["GITHUB_TOKEN"], SECRET_PLACEHOLDER);
    assert_eq!(secrets.get(&secret_id("gh", "env", "GITHUB_TOKEN")).unwrap().as_deref(), Some("ghp_old"));
    assert_eq!(secrets.get(&secret_id("web", "header", "Authorization")).unwrap().as_deref(), Some("Bearer web-token"));

    // Saving placeholders keeps the stored value; a new value replaces it; removed keys and servers lose theirs.
    let mut next = s.clone();
    if let McpTransport::Stdio { env, .. } = &mut next.servers[0].transport {
        env.insert("EXTRA".into(), "x1".into());
    }
    e.set_mcp_settings(next.clone()).await.unwrap();
    assert_eq!(secrets.get(&secret_id("gh", "env", "GITHUB_TOKEN")).unwrap().as_deref(), Some("ghp_old"));
    assert_eq!(secrets.get(&secret_id("gh", "env", "EXTRA")).unwrap().as_deref(), Some("x1"));
    if let McpTransport::Stdio { env, .. } = &mut next.servers[0].transport {
        env.insert("GITHUB_TOKEN".into(), "ghp_new".into());
        env.remove("EXTRA");
    }
    next.servers.truncate(1);
    e.set_mcp_settings(next).await.unwrap();
    assert_eq!(secrets.get(&secret_id("gh", "env", "GITHUB_TOKEN")).unwrap().as_deref(), Some("ghp_new"));
    assert_eq!(secrets.get(&secret_id("gh", "env", "EXTRA")).unwrap(), None);
    assert_eq!(secrets.get(&secret_id("web", "header", "Authorization")).unwrap(), None);
    assert!(!e.store().setting::<Value>("mcp").unwrap().to_string().contains("ghp_new"));

    // A placeholder with nothing behind it can't be saved.
    let mut copy = e.mcp_settings();
    let mut dup: McpServerConfig = copy.servers[0].clone();
    dup.id = "copy".into();
    copy.servers.push(dup);
    let err = e.set_mcp_settings(copy).await.unwrap_err();
    assert!(err.to_string().contains("GITHUB_TOKEN"), "{err}");
    // "Test" only fills hidden values in for the saved server's own URL or command.
    let elsewhere = McpTransport::Http {
        url: "https://elsewhere.example/mcp".into(),
        headers: BTreeMap::from([("Authorization".to_string(), SECRET_PLACEHOLDER.to_string())]),
    };
    let err = e.mcp_test(&elsewhere).await.unwrap_err();
    assert!(err.to_string().contains("again"), "{err}");
}

/// After email content enters a chat, an auto-approved outside tool asks first (prompt
/// injection could otherwise send mail out through it). Unreachable servers are named to
/// the model without their URL.
#[tokio::test]
async fn auto_approve_stops_once_mail_is_in_the_chat() {
    let seen: Shared = Arc::default();
    let calls = Arc::new(Mutex::new(std::collections::VecDeque::from([
        ("mcp__self__list_accounts", json!({})),
        ("list_emails", json!({})),
        ("mcp__self__list_accounts", json!({})),
    ])));
    let app = Router::new().route("/v1/chat/completions", post(scripted_openai)).with_state(Scripted { seen: seen.clone(), calls });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let e = engine();
    let s = AiSettings { enabled: true, provider: "openai".into(), base_url: format!("{url}/v1"), model: "m".into(), ..Default::default() };
    e.set_ai_settings(&s, Some("sk")).unwrap();
    // Remail's own MCP server stands in for an outside one; its token travels as a stored header.
    let status = e.set_mcp_settings(McpSettings { server_enabled: true, port: 0, ..Default::default() }).await.unwrap();
    let mcp_url = status.url.clone().unwrap();
    let port: u16 = mcp_url.trim_end_matches("/mcp").rsplit(':').next().unwrap().parse().unwrap();
    let headers = BTreeMap::from([("Authorization".to_string(), format!("Bearer {}", status.token))]);
    let me = remail_core::mcp::client::McpServerConfig {
        id: "self".into(),
        name: "self".into(),
        enabled: true,
        transport: McpTransport::Http { url: mcp_url, headers },
        auto_approve: true,
    };
    let dead = remail_core::mcp::client::McpServerConfig {
        id: "dead".into(),
        name: "Dead Server".into(),
        enabled: true,
        transport: McpTransport::Http { url: "http://127.0.0.1:1/s/SECRET-PATH/mcp".into(), headers: BTreeMap::new() },
        auto_approve: false,
    };
    e.set_mcp_settings(McpSettings { server_enabled: true, port, servers: vec![me, dead], ..Default::default() }).await.unwrap();

    let approvals = |events: &[ChatEvent]| -> Vec<Option<String>> {
        events.iter().filter_map(|ev| match ev { ChatEvent::Approval { reason, .. } => Some(reason.clone()), _ => None }).collect()
    };
    let first = chat_in(&e, "c", "accounts via the server", true).await;
    assert!(approvals(&first).is_empty(), "{first:?}");
    assert!(first.iter().any(|ev| matches!(ev, ChatEvent::ToolEnd { ok: true, .. })), "{first:?}");
    let second = chat_in(&e, "c", "what's in my inbox", true).await;
    assert!(approvals(&second).is_empty());
    let third = chat_in(&e, "c", "accounts via the server again", false).await;
    let asked = approvals(&third);
    assert_eq!(asked.len(), 1, "{third:?}");
    assert!(asked[0].as_deref().is_some_and(|r| r.contains("email content")));
    assert!(text_of(&third).contains("done"));
    assert!(third.iter().any(|ev| matches!(ev, ChatEvent::ToolEnd { ok: false, .. })));

    let seen = seen.lock().unwrap();
    let system = seen.requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Dead Server"), "{system}");
    assert!(!system.contains("SECRET-PATH") && !system.contains("127.0.0.1:1"), "{system}");
    drop(seen);
    e.shutdown_mcp();
}
