//! The admin API: who gets in, providers and AI setup, write-only secrets,
//! the org AI key for members' devices, roles and the last-admin rule, and
//! `REMAIL_ADMIN_EMAILS`.

mod common;

use common::*;
use remail_server::Mode;
use serde_json::{json, Value};

const ADMIN: &str = "boss@example.com";
const AI_KEY: &str = "sk-live-0123456789abcdef";

async fn start_hosted() -> Option<Server> {
    start_with(Mode::Hosted, |c| c.admin_emails = vec![ADMIN.into()]).await
}

async fn put(s: &Server, path: &str, body: Value, token: &str) -> Resp {
    s.send(s.http.put(s.api(path)).bearer_auth(token).json(&body)).await
}

fn ai_body(enabled: bool, key: Option<&str>) -> Value {
    json!({
        "ai": {"enabled": enabled, "provider": "openai", "baseUrl": "https://llm.example.com/v1", "model": "small-1", "maxTokens": 2048},
        "apiKey": key,
    })
}

async fn user_id(s: &Server, email: &str) -> String {
    sqlx::query_scalar("select id from users where email = $1").bind(email).fetch_one(&s.db).await.unwrap()
}

/// Every admin route with a body it would accept.
fn admin_routes(target: &str) -> Vec<(&'static str, String, Value)> {
    vec![
        ("GET", "/admin/config".into(), Value::Null),
        ("PUT", "/admin/providers".into(), json!({"providers": {"google": false}, "googleClientSecret": null})),
        ("PUT", "/admin/ai".into(), ai_body(false, None)),
        ("GET", "/admin/users".into(), Value::Null),
        ("PUT", format!("/admin/users/{target}/role"), json!({"role": "admin"})),
    ]
}

async fn call(s: &Server, method: &str, path: &str, body: &Value, token: Option<&str>) -> Resp {
    let mut r = match method {
        "GET" => s.http.get(s.api(path)),
        _ => s.http.put(s.api(path)).json(body),
    };
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    s.send(r).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_admins_get_in() {
    let Some(s) = start_hosted().await else { return };
    let member = token(&s.new_user("member@example.com", PASSWORD).await);
    let other = user_id(&s, "member@example.com").await;
    for (method, path, body) in admin_routes(&other) {
        assert_eq!(call(&s, method, &path, &body, None).await.status, 401, "{method} {path} signed out");
        let r = call(&s, method, &path, &body, Some(&member)).await;
        assert_eq!((r.status.as_u16(), r.code()), (403, "forbidden"), "{method} {path} as a member: {:?}", r.body);
    }
    // Nothing changed.
    let role: String = sqlx::query_scalar("select role from users where id = $1").bind(&other).fetch_one(&s.db).await.unwrap();
    assert_eq!(role, "user");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admins_set_up_providers() {
    let Some(s) = start_hosted().await else { return };
    let boss = token(&s.new_user(ADMIN, PASSWORD).await);
    let member = token(&s.new_user("m@example.com", PASSWORD).await);

    let c = s.get("/admin/config", Some(&boss)).await;
    assert_eq!(c.status, 200, "{:?}", c.body);
    assert_eq!(c.body["providers"]["imap"], true);
    assert_eq!(c.body["googleSecretSet"], false);
    assert_eq!(c.body["aiKeySet"], false);
    let v0 = c.body["version"].as_i64().unwrap();

    let mut providers = c.body["providers"].clone();
    providers["google"] = json!(true);
    providers["googleApp"]["clientId"] = json!("123-abc.apps.googleusercontent.com");
    providers["microsoftApp"] = json!({"clientId": "9832A9E1-5779-4E2F-B2CB-8D2BDF4E9B6E", "tenant": "organizations"});
    providers["manualServers"] = json!(false);
    providers["presets"] = json!([{
        "name": "Acme mail", "domain": "acme.com",
        "imapHost": "imap.acme.com", "imapPort": 993, "imapSecurity": "tls",
        "smtpHost": "smtp.acme.com", "smtpPort": 587, "smtpSecurity": "starttls",
        "caldavUrl": "https://dav.acme.com/"
    }]);
    let r = put(&s, "/admin/providers", json!({"providers": providers, "googleClientSecret": "GOCSPX-desktop"}), &boss).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert!(r.body["version"].as_i64().unwrap() > v0);
    assert_eq!(r.body["googleSecretSet"], true);
    assert_eq!(r.body["providers"]["googleApp"]["clientSecret"], Value::Null, "write-only: {:?}", r.body);
    assert_eq!(r.body["providers"]["microsoftApp"]["clientId"], "9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6e");

    // Members' devices see the change (the installed-app secret is theirs to use).
    let p = s.get("/providers", Some(&member)).await;
    assert_eq!(p.body[0]["enabled"], true);
    assert_eq!(p.body[0]["oauth"]["clientSecret"], "GOCSPX-desktop");
    assert_eq!(p.body[1]["oauth"]["tenant"], "organizations");
    assert_eq!(p.body[2]["manualServers"], false);
    assert_eq!(p.body[2]["presets"][0]["smtpPort"], 587);
    // Never the server's own secret: hosted Google sign-ins are device-only.
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    assert_eq!(remail_server::orgs::secrets(&s.db, &s.state.env, &org.id).await.unwrap().google_client_secret, "");

    // `null` keeps the secret, "" clears it.
    let again = put(&s, "/admin/providers", json!({"providers": r.body["providers"], "googleClientSecret": null}), &boss).await;
    assert_eq!(again.body["googleSecretSet"], true);
    assert_eq!(again.body["version"], r.body["version"], "nothing changed, nothing saved");
    let cleared = put(&s, "/admin/providers", json!({"providers": r.body["providers"], "googleClientSecret": ""}), &boss).await;
    assert_eq!(cleared.body["googleSecretSet"], false);

    // Bad input is refused and nothing is saved.
    for (field, value) in [
        ("/microsoftApp/clientId", json!("not-a-guid")),
        ("/microsoftApp/tenant", json!("everyone")),
        ("/googleApp/clientId", json!("123-abc")),
        ("/presets/0/imapHost", json!("imap acme com")),
        ("/presets/0/smtpPort", json!(0)),
        ("/presets/0/imapSecurity", json!("ssl")),
        ("/presets/0/caldavUrl", json!("dav.acme.com")),
        ("/presets/0/name", json!("  ")),
    ] {
        let mut p = cleared.body["providers"].clone();
        *p.pointer_mut(field).unwrap() = value.clone();
        let r = put(&s, "/admin/providers", json!({"providers": p}), &boss).await;
        assert_eq!(r.code(), "bad_request", "{field} = {value}: {:?}", r.body);
    }
    let r = put(&s, "/admin/providers", json!({"providers": cleared.body["providers"], "googleClientSecret": "two words"}), &boss).await;
    assert_eq!(r.code(), "bad_request");
    assert_eq!(s.get("/admin/config", Some(&boss)).await.body["version"], cleared.body["version"]);

    // Audited without the secret.
    let rows: Vec<(String, Value)> =
        sqlx::query_as("select action, meta from audit_log where action like 'admin.%' order by id").fetch_all(&s.db).await.unwrap();
    assert!(rows.iter().any(|(a, m)| a == "admin.providers.update" && m["googleSecret"] == "replaced"), "{rows:?}");
    assert!(!format!("{rows:?}").contains("GOCSPX"), "{rows:?}");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ai_key_goes_only_to_members_devices() {
    let Some(s) = start_hosted().await else { return };
    let boss = token(&s.new_user(ADMIN, PASSWORD).await);
    let member = token(&s.new_user("m@example.com", PASSWORD).await);
    let mut seen = String::new();

    // Not set up: no key for anyone.
    assert_eq!(s.get("/org/ai-key", Some(&member)).await.code(), "forbidden");
    assert_eq!(s.get("/org/ai-key", Some(&boss)).await.code(), "not_found");

    // Saved while off: the admin may test it, members can't fetch it yet.
    let r = put(&s, "/admin/ai", ai_body(false, Some(AI_KEY)), &boss).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["aiKeySet"], true);
    seen += &r.body.to_string();
    assert_eq!(s.get("/org/ai-key", Some(&member)).await.code(), "forbidden");
    assert_eq!(s.get("/org/ai-key", Some(&boss)).await.body["apiKey"], AI_KEY);

    // On: in the org config (without the key) for every device, and the key for members.
    let v_before = s.get("/org/config", Some(&member)).await.body["version"].as_i64().unwrap();
    let r = put(&s, "/admin/ai", ai_body(true, None), &boss).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["aiKeySet"], true, "null keeps the key");
    seen += &r.body.to_string();
    let cfg = s.get("/org/config", Some(&member)).await;
    assert!(cfg.body["version"].as_i64().unwrap() > v_before);
    assert_eq!(cfg.body["config"]["ai"], json!({"enabled": true, "provider": "openai", "baseUrl": "https://llm.example.com/v1", "model": "small-1", "maxTokens": 2048}));
    seen += &cfg.body.to_string();
    let k = s.get("/org/ai-key", Some(&member)).await;
    assert_eq!(k.status, 200);
    assert_eq!(k.body, json!({"apiKey": AI_KEY}));
    assert_eq!(s.get("/org/ai-key", None).await.status, 401);

    // A new key bumps the version (devices fetch again); "" clears it.
    let v = cfg.body["version"].as_i64().unwrap();
    let r = put(&s, "/admin/ai", ai_body(true, Some("sk-second-key")), &boss).await;
    assert!(r.body["version"].as_i64().unwrap() > v);
    assert_eq!(s.get("/org/ai-key", Some(&member)).await.body["apiKey"], "sk-second-key");

    // Validation.
    let mut b = ai_body(true, None);
    b["ai"]["model"] = json!("");
    assert_eq!(put(&s, "/admin/ai", b, &boss).await.code(), "bad_request");
    let mut b = ai_body(true, None);
    b["ai"]["provider"] = json!("gemini");
    assert_eq!(put(&s, "/admin/ai", b, &boss).await.code(), "bad_request");
    assert_eq!(put(&s, "/admin/ai", ai_body(true, Some("has space")), &boss).await.code(), "bad_request");

    // Nothing else ever carries the key.
    for path in ["/admin/config", "/admin/users", "/org/config", "/providers", "/me", "/meta"] {
        seen += &s.get(path, Some(&boss)).await.body.to_string();
        seen += &s.get(path, Some(&member)).await.body.to_string();
    }
    assert!(!seen.contains(AI_KEY) && !seen.contains("sk-second-key"), "{seen}");
    // Ciphertext at rest, and no key in the audit log.
    let blobs: Vec<Vec<u8>> = sqlx::query_scalar("select ciphertext from org_secrets union all select ciphertext from org_config")
        .fetch_all(&s.db)
        .await
        .unwrap();
    for b in &blobs {
        assert!(!String::from_utf8_lossy(b).contains("sk-"), "plaintext at rest");
    }
    let audit: Vec<(String, Value)> = sqlx::query_as("select action, meta from audit_log order by id").fetch_all(&s.db).await.unwrap();
    assert!(!format!("{audit:?}").contains("sk-"), "{audit:?}");
    assert!(audit.iter().filter(|(a, _)| a == "org.ai_key.read").count() >= 3, "{audit:?}");
    assert!(audit.iter().any(|(a, m)| a == "admin.ai.update" && m["key"] == "replaced"));

    let r = put(&s, "/admin/ai", ai_body(false, Some("")), &boss).await;
    assert_eq!(r.body["aiKeySet"], false);
    assert_eq!(s.get("/org/ai-key", Some(&boss)).await.code(), "not_found");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roles_and_the_last_admin() {
    let Some(s) = start_hosted().await else { return };
    let boss = token(&s.new_user(ADMIN, PASSWORD).await);
    let a = token(&s.new_user("a@example.com", PASSWORD).await);
    s.new_user("b@example.com", PASSWORD).await;
    let (boss_id, a_id, b_id) = (user_id(&s, ADMIN).await, user_id(&s, "a@example.com").await, user_id(&s, "b@example.com").await);

    // Owners first; metadata only.
    let list = s.get("/admin/users", Some(&boss)).await;
    assert_eq!(list.status, 200, "{:?}", list.body);
    let rows = list.body.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0]["email"].as_str(), rows[0]["role"].as_str()), (Some(ADMIN), Some("owner")));
    let keys: Vec<&str> = rows[0].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["createdAt", "disabled", "email", "id", "lastActiveAt", "name", "role", "totp", "verified"]);
    assert_eq!(s.get("/admin/users?query=B%40EXAMPLE", Some(&boss)).await.body.as_array().unwrap().len(), 1);
    assert_eq!(s.get("/admin/users?query=%25", Some(&boss)).await.body.as_array().unwrap().len(), 0, "% is literal");

    // The owner makes `a` an admin: it gets in at once.
    let r = put(&s, &format!("/admin/users/{a_id}/role"), json!({"role": "admin"}), &boss).await;
    assert_eq!((r.status.as_u16(), r.body["role"].as_str()), (200, Some("admin")), "{:?}", r.body);
    assert_eq!(s.get("/admin/config", Some(&a)).await.status, 200);
    assert_eq!(s.get("/me", Some(&a)).await.body["role"], "admin");
    // An admin manages admins too, but never owners, and never makes one.
    assert_eq!(put(&s, &format!("/admin/users/{b_id}/role"), json!({"role": "admin"}), &a).await.status, 200);
    assert_eq!(put(&s, &format!("/admin/users/{boss_id}/role"), json!({"role": "user"}), &a).await.code(), "forbidden");
    assert_eq!(put(&s, &format!("/admin/users/{b_id}/role"), json!({"role": "owner"}), &a).await.code(), "bad_request");
    assert_eq!(put(&s, "/admin/users/usr_nobody/role", json!({"role": "user"}), &a).await.code(), "not_found");

    // Demoting is fine while someone else administers the org…
    assert_eq!(put(&s, &format!("/admin/users/{b_id}/role"), json!({"role": "user"}), &a).await.body["role"], "user");
    // …so with the owner gone, the last admin stays.
    sqlx::query("update users set role = 'user' where id = $1").bind(&boss_id).execute(&s.db).await.unwrap();
    let r = put(&s, &format!("/admin/users/{a_id}/role"), json!({"role": "user"}), &a).await;
    assert_eq!(r.code(), "conflict", "{:?}", r.body);
    // A disabled admin doesn't count.
    sqlx::query("update users set role = 'admin', disabled_at = now() where id = $1").bind(&b_id).execute(&s.db).await.unwrap();
    assert_eq!(put(&s, &format!("/admin/users/{a_id}/role"), json!({"role": "user"}), &a).await.code(), "conflict");
    sqlx::query("update users set disabled_at = null where id = $1").bind(&b_id).execute(&s.db).await.unwrap();
    // Demoting oneself works when another admin remains.
    let r = put(&s, &format!("/admin/users/{a_id}/role"), json!({"role": "user"}), &a).await;
    assert_eq!(r.body["role"], "user");
    assert_eq!(s.get("/admin/users", Some(&a)).await.code(), "forbidden");

    // Another org's people are out of reach.
    let acme = remail_server::orgs::create(&s.db, &s.state.env, "Acme", "acme", "invite").await.unwrap();
    let invite = s.invite(&acme.id, Some("x@acme.com"), "user").await;
    let r = s.post("/auth/signup", json!({"name": "X", "email": "x@acme.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.status, 202);
    let x_id = user_id(&s, "x@acme.com").await;
    let b = token(&s.device("b@example.com", PASSWORD, "Phone").await);
    assert_eq!(put(&s, &format!("/admin/users/{x_id}/role"), json!({"role": "admin"}), &b).await.code(), "not_found");
    assert!(!s.get("/admin/users", Some(&b)).await.body.to_string().contains("x@acme.com"));

    let audit: Vec<(String, Value)> =
        sqlx::query_as("select action, meta from audit_log where action = 'admin.user.role' order by id").fetch_all(&s.db).await.unwrap();
    assert_eq!(audit.len(), 4, "{audit:?}");
    assert_eq!(audit[0].1, json!({"from": "user", "to": "admin"}));
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_emails_become_owners() {
    // Accounts that exist before the address is listed: promoted at start.
    let Some(s) = start(Mode::Hosted).await else { return };
    s.new_user("ops@example.com", PASSWORD).await;
    s.new_user("member@example.com", PASSWORD).await;
    let ops = user_id(&s, "ops@example.com").await;
    let role = |id: String| {
        let db = s.db.clone();
        async move { sqlx::query_scalar::<_, String>("select role from users where id = $1").bind(id).fetch_one(&db).await.unwrap() }
    };
    assert_eq!(role(ops.clone()).await, "user");
    let mut cfg = s.state.cfg.clone();
    cfg.admin_emails = remail_server::config::parse_emails("OPS@example.com, boss@example.com");
    remail_server::orgs::startup(&cfg, &s.db, &s.state.env, true).await.unwrap();
    assert_eq!(role(ops.clone()).await, "owner");
    assert_eq!(role(user_id(&s, "member@example.com").await).await, "user");
    let audit: Vec<Value> = sqlx::query_scalar("select meta from audit_log where action = 'admin.user.role'").fetch_all(&s.db).await.unwrap();
    assert_eq!(audit, [json!({"from": "user", "to": "owner", "by": "REMAIL_ADMIN_EMAILS"})]);
    // Again: nothing to do.
    remail_server::orgs::startup(&cfg, &s.db, &s.state.env, true).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from audit_log where action = 'admin.user.role'").fetch_one(&s.db).await.unwrap(), 1);
    s.finish().await;

    // Signing up (or accepting an invitation) with a listed address.
    let Some(s) = start_hosted().await else { return };
    let t = s.new_user("boss@example.com", PASSWORD).await;
    assert_eq!(t["user"]["role"], "owner");
    let org = s.org_id().await;
    let invite = s.invite(&org, None, "user").await;
    let r = s.post("/auth/signup", json!({"name": "Plain", "email": "plain@example.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.status, 202);
    assert_eq!(role_of(&s, "plain@example.com").await, "user");
    s.finish().await;
}

async fn role_of(s: &Server, email: &str) -> String {
    sqlx::query_scalar("select role from users where email = $1").bind(email).fetch_one(&s.db).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invited_admin_emails_become_owners() {
    let Some(s) = start_with(Mode::Hosted, |c| c.admin_emails = vec!["boss@example.com".into()]).await else { return };
    let org = s.org_id().await;
    let invite = s.invite(&org, Some("boss@example.com"), "user").await;
    let r = s.post("/auth/signup", json!({"name": "Boss", "email": "Boss@Example.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.status, 202, "{:?}", r.body);
    assert_eq!(role_of(&s, "boss@example.com").await, "owner");
    let t = token(&s.login("boss@example.com", PASSWORD, "Laptop").await.body);
    assert_eq!(s.get("/admin/config", Some(&t)).await.status, 200);
    s.finish().await;
}
