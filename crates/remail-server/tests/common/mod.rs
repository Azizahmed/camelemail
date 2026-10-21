//! Test harness: a fresh Postgres database per test and the server running
//! in-process on 127.0.0.1:0.
//!
//! Needs `REMAIL_TEST_DATABASE_URL` (see `scripts/test-postgres.sh`); without
//! it every test prints a skip notice and passes.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use remail_server::config::{Argon2Cost, RateConfig};
use remail_server::crypto::MemoryKeyProvider;
use remail_server::{AppState, Config, Mode};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::PgPool;

pub struct Server {
    pub url: String,
    pub http: reqwest::Client,
    pub db: PgPool,
    pub state: AppState,
    pub keys: Arc<MemoryKeyProvider>,
    /// On-prem: the one-time admin invite printed on first start.
    pub first_invite: Option<String>,
    admin: PgPool,
    db_name: String,
    task: tokio::task::JoinHandle<()>,
}

pub fn test_db_url() -> Option<String> {
    let url = std::env::var("REMAIL_TEST_DATABASE_URL").ok().filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("skipped: REMAIL_TEST_DATABASE_URL is not set (run scripts/test-postgres.sh)");
    }
    url
}

fn with_db(url: &str, db: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    let cut = base.rfind('/').unwrap_or(base.len());
    format!("{}/{db}", &base[..cut])
}

pub async fn start(mode: Mode) -> Option<Server> {
    start_with(mode, |_| {}).await
}

pub async fn start_with(mode: Mode, tweak: impl FnOnce(&mut Config)) -> Option<Server> {
    let admin_url = test_db_url()?;
    let admin = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&admin_url).await.expect("test postgres");
    let db_name = format!("test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {db_name}"))).execute(&admin).await.expect("create db");
    let url = with_db(&admin_url, &db_name);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut cfg = Config::new(url.clone(), mode);
    cfg.bind = addr;
    cfg.public_url = format!("http://{addr}");
    cfg.dev_mail = true;
    cfg.argon2 = Argon2Cost { m_kib: 1024, t: 1, p: 1 };
    cfg.rate = RateConfig { ip_burst: 100_000, ip_per_sec: 10_000.0, account_burst: 100_000, account_per_sec: 10_000.0 };
    cfg.lockout_base = Duration::from_secs(1);
    cfg.licence_keys = Vec::new();
    cfg.first_org_name = "Acme".into();
    tweak(&mut cfg);

    let db = remail_server::db::connect(&url, 10).await.expect("connect test db");
    remail_server::db::migrate(&db).await.expect("migrate");
    let keys = Arc::new(MemoryKeyProvider::new(remail_proto::new_id("dep")));
    let state = AppState::new(cfg, db.clone(), keys.clone()).await.expect("state");
    let first_invite = remail_server::orgs::startup(&state.cfg, &db, &state.env, true).await.expect("startup");
    let serve_state = state.clone();
    let task = tokio::spawn(async move {
        let _ = remail_server::app::serve(serve_state, listener, std::future::pending()).await;
    });
    Some(Server {
        url: format!("http://{addr}"),
        http: reqwest::Client::new(),
        db,
        state,
        keys,
        first_invite,
        admin,
        db_name,
        task,
    })
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub body: Value,
}

impl Resp {
    pub fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
}

impl Server {
    pub async fn finish(self) {
        self.task.abort();
        self.db.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {} with (force)", self.db_name)))
            .execute(&self.admin)
            .await;
        self.admin.close().await;
    }

    pub fn api(&self, path: &str) -> String {
        format!("{}/v1{path}", self.url)
    }

    pub async fn send(&self, req: reqwest::RequestBuilder) -> Resp {
        let res = req.send().await.expect("request");
        let status = res.status();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap_or_default();
        let body = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
        Resp { status, headers, body }
    }

    pub async fn post(&self, path: &str, body: Value, token: Option<&str>) -> Resp {
        let mut r = self.http.post(self.api(path)).json(&body);
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        self.send(r).await
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> Resp {
        let mut r = self.http.get(self.api(path));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        self.send(r).await
    }

    pub async fn put_section(&self, token: &str, id: &str, if_match: i64, doc: &Value) -> Resp {
        let r = self
            .http
            .put(self.api(&format!("/sections/{id}")))
            .bearer_auth(token)
            .header("If-Match", format!("\"v{if_match}\""))
            .json(&json!({ "doc": doc }));
        self.send(r).await
    }

    /// Mail kept by the dev mailer, newest last.
    pub async fn mail(&self) -> Vec<Value> {
        let r = self.get("/dev/mail", None).await;
        assert_eq!(r.status, 200, "dev mail: {:?}", r.body);
        r.body.as_array().cloned().unwrap_or_default()
    }

    /// The token of the newest `remail://<kind>?t=` link sent to `to`.
    pub async fn mailed_token(&self, to: &str, kind: &str) -> String {
        let mails = self.mail().await;
        let m = mails.iter().rev().find(|m| m["to"] == to && m["body"].as_str().unwrap_or("").contains(&format!("remail://{kind}?t=")));
        let body = m.unwrap_or_else(|| panic!("no {kind} mail to {to}: {mails:?}"))["body"].as_str().unwrap().to_string();
        let start = body.find(&format!("remail://{kind}?t=")).unwrap() + format!("remail://{kind}?t=").len();
        body[start..].split_whitespace().next().unwrap().to_string()
    }

    pub async fn login(&self, email: &str, password: &str, device: &str) -> Resp {
        self.post(
            "/auth/login",
            json!({"email": email, "password": password, "device": {"name": device, "platform": "linux", "appVersion": "0.3.0"}}),
            None,
        )
        .await
    }

    /// Hosted: sign up, confirm the e-mail and sign in. Returns the tokens.
    pub async fn new_user(&self, email: &str, password: &str) -> Value {
        let r = self.post("/auth/signup", json!({"name": "Test User", "email": email, "password": password}), None).await;
        assert_eq!(r.status, 202, "signup: {:?}", r.body);
        let t = self.mailed_token(email, "verify").await;
        let r = self.post("/auth/verify", json!({"token": t}), None).await;
        assert_eq!(r.status, 204, "verify: {:?}", r.body);
        let r = self.login(email, password, "Laptop").await;
        assert_eq!(r.status, 200, "login: {:?}", r.body);
        r.body
    }

    /// Signs in a second device of an existing user.
    pub async fn device(&self, email: &str, password: &str, name: &str) -> Value {
        let r = self.login(email, password, name).await;
        assert_eq!(r.status, 200, "login: {:?}", r.body);
        r.body
    }

    /// An invite token for `org_id` (what `user invite` prints).
    pub async fn invite(&self, org_id: &str, email: Option<&str>, role: &str) -> String {
        let t = remail_server::auth::tokens::create_invite(&self.db, org_id, email, role).await.unwrap();
        t.expose().clone()
    }

    pub async fn org_id(&self) -> String {
        sqlx::query_scalar("select id from orgs order by created_at limit 1").fetch_one(&self.db).await.unwrap()
    }
}

pub fn token(tokens: &Value) -> String {
    tokens["accessToken"].as_str().expect("accessToken").to_string()
}

pub fn refresh_token(tokens: &Value) -> String {
    tokens["refreshToken"].as_str().expect("refreshToken").to_string()
}

pub const PASSWORD: &str = "correct horse battery staple";
