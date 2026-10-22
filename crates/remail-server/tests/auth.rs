//! Sign-up → verify → login → 2FA → refresh rotation → reuse detection →
//! device sign-out → reset; lockout; the required-2FA flow.

mod common;

use std::time::Duration;

use common::*;
use remail_server::Mode;
use serde_json::{json, Value};

fn totp_now(secret_b32: &str) -> String {
    let secret = totp_rs::Secret::Encoded(secret_b32.to_string()).to_bytes().unwrap();
    let now = chrono::Utc::now().timestamp() as u64;
    remail_server::auth::totp::code_at(&secret, now)
}

/// A code for the next 30 s step (the current one may already be used).
fn totp_next(secret_b32: &str) -> String {
    let secret = totp_rs::Secret::Encoded(secret_b32.to_string()).to_bytes().unwrap();
    let now = chrono::Utc::now().timestamp() as u64;
    remail_server::auth::totp::code_at(&secret, now + 30)
}

async fn audit_actions(s: &Server) -> Vec<String> {
    sqlx::query_scalar("select action from audit_log order by id").fetch_all(&s.db).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_flow() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "sara@example.com";

    // Meta before anything.
    let meta = s.get("/meta", None).await;
    assert_eq!(meta.body["mode"], "hosted");
    assert_eq!(meta.body["signup"], "open");
    assert_eq!(meta.body["verifyEmail"], true);
    assert_eq!(meta.body["licence"], "none");

    // Weak passwords are refused with a stable code.
    let r = s.post("/auth/signup", json!({"name": "Sara", "email": email, "password": "password1"}), None).await;
    assert_eq!(r.status, 400);
    assert_eq!(r.code(), "weak_password");
    let r = s.post("/auth/signup", json!({"name": "Sara", "email": email, "password": "sara-Lighthouse-Quiet-91"}), None).await;
    assert_eq!(r.code(), "weak_password", "contains the e-mail");

    let r = s.post("/auth/signup", json!({"name": "Sara", "email": email, "password": PASSWORD, "locale": "ar"}), None).await;
    assert_eq!(r.status, 202);
    assert_eq!(r.body["verify"], "email");
    // Not verified yet.
    let r = s.login(email, PASSWORD, "Laptop").await;
    assert_eq!(r.code(), "email_not_verified");
    // Signing up again doesn't reveal the account (an e-mail goes out instead).
    let r = s.post("/auth/signup", json!({"name": "Sara", "email": email, "password": PASSWORD}), None).await;
    assert_eq!(r.status, 202);

    let t = s.mailed_token(email, "verify").await;
    assert_eq!(s.post("/auth/verify", json!({"token": t}), None).await.status, 204);
    assert_eq!(s.post("/auth/verify", json!({"token": t}), None).await.code(), "invalid_token", "single use");

    // Wrong password and unknown e-mail look the same.
    let wrong = s.login(email, "not the password at all", "Laptop").await;
    let unknown = s.login("nobody@example.com", PASSWORD, "Laptop").await;
    assert_eq!(wrong.status, 401);
    assert_eq!(wrong.body, unknown.body);

    let tokens = s.login(email, PASSWORD, "Laptop").await;
    assert_eq!(tokens.status, 200, "{:?}", tokens.body);
    let access = token(&tokens.body);
    assert_eq!(tokens.body["user"]["email"], email);
    assert_eq!(tokens.body["user"]["totp"], false);
    assert!(tokens.body["accessExpiresAt"].as_i64().unwrap() > remail_proto::now_ms());

    // Me + profile edit.
    let me = s.get("/me", Some(&access)).await;
    assert_eq!(me.body["name"], "Sara");
    assert_eq!(me.body["policies"]["sessionDays"], 30);
    let r = s.send(s.http.patch(s.api("/me")).bearer_auth(&access).json(&json!({"name": "Sara A."}))).await;
    assert_eq!(r.body["name"], "Sara A.");
    assert_eq!(s.get("/me", None).await.status, 401);
    assert_eq!(s.get("/me", Some("rma_bogus")).await.code(), "unauthorized");

    // Turn on 2FA.
    let setup = s.post("/me/2fa/setup", json!({}), Some(&access)).await;
    assert_eq!(setup.status, 200, "{:?}", setup.body);
    let secret = setup.body["secret"].as_str().unwrap().to_string();
    assert!(setup.body["otpauthUrl"].as_str().unwrap().starts_with("otpauth://totp/Camel%20Mail:sara@example.com?"));
    let r = s.post("/me/2fa/enable", json!({"code": "000000"}), Some(&access)).await;
    assert_eq!(r.code(), "invalid_code");
    let r = s.post("/me/2fa/enable", json!({"code": totp_now(&secret)}), Some(&access)).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    let backup: Vec<String> = serde_json::from_value(r.body["backupCodes"].clone()).unwrap();
    assert_eq!(backup.len(), 10);
    assert!(r.body.get("tokens").is_none());

    // Login now asks for a code.
    let r = s.login(email, PASSWORD, "Phone").await;
    let challenge = r.body["challenge"].as_str().expect("challenge").to_string();
    assert_eq!(r.body["methods"], json!(["totp", "backup"]));
    assert_eq!(r.body["setupRequired"], false);
    let r = s.post("/auth/login/2fa", json!({"challenge": challenge, "code": "123456"}), None).await;
    assert_eq!(r.code(), "invalid_code");
    // The code used to enable can't be replayed; the next step's code works.
    let r = s.post("/auth/login/2fa", json!({"challenge": challenge, "code": totp_next(&secret)}), None).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["user"]["totp"], true);
    let phone = r.body.clone();
    let r = s.post("/auth/login/2fa", json!({"challenge": challenge, "code": totp_next(&secret)}), None).await;
    assert_eq!(r.code(), "invalid_token", "challenge is single use");

    // Backup code: works once.
    let r = s.login(email, PASSWORD, "Tablet").await;
    let ch = r.body["challenge"].as_str().unwrap().to_string();
    let r = s.post("/auth/login/2fa", json!({"challenge": ch, "code": backup[0].to_uppercase()}), None).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    let tablet = r.body.clone();
    let r = s.login(email, PASSWORD, "Tablet 2").await;
    let ch = r.body["challenge"].as_str().unwrap().to_string();
    let r = s.post("/auth/login/2fa", json!({"challenge": ch, "code": backup[0]}), None).await;
    assert_eq!(r.code(), "invalid_code", "backup codes are single use");

    // Refresh rotation.
    let r1 = refresh_token(&phone);
    let rotated = s.post("/auth/refresh", json!({"refreshToken": r1}), None).await;
    assert_eq!(rotated.status, 200, "{:?}", rotated.body);
    let r2 = refresh_token(&rotated.body);
    assert_ne!(r1, r2);
    assert_eq!(rotated.body["deviceId"], phone["deviceId"]);
    let phone_access = token(&rotated.body);
    assert_eq!(s.get("/me", Some(&phone_access)).await.status, 200);
    // Reusing the rotated token revokes the whole family (and the device).
    let reuse = s.post("/auth/refresh", json!({"refreshToken": r1}), None).await;
    assert_eq!(reuse.status, 401);
    assert_eq!(s.post("/auth/refresh", json!({"refreshToken": r2}), None).await.status, 401);
    assert_eq!(s.get("/me", Some(&phone_access)).await.status, 401);

    // Devices: list and revoke the tablet.
    let devices = s.get("/me/devices", Some(&access)).await;
    let list = devices.body.as_array().unwrap();
    assert_eq!(list.len(), 2, "laptop + tablet (phone was revoked): {list:?}");
    assert_eq!(list.iter().filter(|d| d["current"] == true).count(), 1);
    let tablet_id = tablet["deviceId"].as_str().unwrap();
    let r = s.send(s.http.delete(s.api(&format!("/me/devices/{tablet_id}"))).bearer_auth(&access)).await;
    assert_eq!(r.status, 204);
    assert_eq!(s.get("/me", Some(&token(&tablet))).await.status, 401);
    assert_eq!(s.post("/auth/refresh", json!({"refreshToken": refresh_token(&tablet)}), None).await.status, 401);
    let r = s.send(s.http.delete(s.api("/me/devices/dev_nope")).bearer_auth(&access)).await;
    assert_eq!(r.code(), "not_found");

    // Push registration stores an encrypted token.
    let r = s.send(s.http.put(s.api("/me/devices/current/push")).bearer_auth(&access).json(&json!({"kind": "fcm", "token": "push-token-xyz"}))).await;
    assert_eq!(r.status, 204);
    let enc: Vec<u8> = sqlx::query_scalar("select push_token_enc from devices where push_kind = 'fcm'").fetch_one(&s.db).await.unwrap();
    assert!(!String::from_utf8_lossy(&enc).contains("push-token-xyz"));

    // Reset: always 202; the link signs out every device; data is untouched.
    let doc = json!({"records": {"_": {"fields": {"theme": {"value": "dark", "at": {"ms": 1, "device": "A"}}}}}});
    assert_eq!(s.put_section(&access, "prefs", 0, &doc).await.status, 200);
    assert_eq!(s.post("/auth/reset/request", json!({"email": "nobody@example.com"}), None).await.status, 202);
    assert_eq!(s.post("/auth/reset/request", json!({"email": email}), None).await.status, 202);
    let rt = s.mailed_token(email, "reset").await;
    let r = s.post("/auth/reset", json!({"token": rt, "password": "short"}), None).await;
    assert_eq!(r.code(), "weak_password");
    let new_password = "another long passphrase here";
    let r = s.post("/auth/reset", json!({"token": rt, "password": new_password}), None).await;
    assert_eq!(r.status, 204, "{:?}", r.body);
    assert_eq!(s.get("/me", Some(&access)).await.status, 401, "all sessions revoked");
    assert_eq!(s.login(email, PASSWORD, "Laptop").await.status, 401);
    let r = s.login(email, new_password, "Laptop").await;
    let ch = r.body["challenge"].as_str().unwrap().to_string();
    let r = s.post("/auth/login/2fa", json!({"challenge": ch, "code": backup[1]}), None).await;
    assert_eq!(r.status, 200);
    let access = token(&r.body);
    let prefs = s.get("/sections/prefs", Some(&access)).await;
    assert_eq!(prefs.body["doc"]["records"]["_"]["fields"]["theme"]["value"], "dark", "settings survive a reset");

    // Password change revokes the other devices, keeps this one.
    let other = s.login(email, new_password, "Other").await;
    let ch = other.body["challenge"].as_str().unwrap().to_string();
    let other = s.post("/auth/login/2fa", json!({"challenge": ch, "code": backup[2]}), None).await.body;
    let r = s.post("/me/password", json!({"current": "wrong", "new": PASSWORD}), Some(&access)).await;
    assert_eq!(r.code(), "invalid_credentials");
    let r = s.post("/me/password", json!({"current": new_password, "new": PASSWORD}), Some(&access)).await;
    assert_eq!(r.status, 204);
    assert_eq!(s.get("/me", Some(&access)).await.status, 200);
    assert_eq!(s.get("/me", Some(&token(&other))).await.status, 401);

    // 2FA disable needs password and a code.
    let r = s.post("/me/2fa/disable", json!({"password": PASSWORD, "code": "000000"}), Some(&access)).await;
    assert_eq!(r.code(), "invalid_code");
    let r = s.post("/me/2fa/disable", json!({"password": PASSWORD, "code": backup[3]}), Some(&access)).await;
    assert_eq!(r.status, 204);
    assert_eq!(s.login(email, PASSWORD, "x").await.status, 200);

    // Logout signs this device out.
    assert_eq!(s.post("/auth/logout", json!({}), Some(&access)).await.status, 204);
    assert_eq!(s.get("/me", Some(&access)).await.status, 401);

    let actions = audit_actions(&s).await;
    for a in ["auth.signup", "auth.verify", "auth.login", "auth.login.fail", "auth.2fa.enable", "auth.2fa.backup_used",
              "auth.refresh.reuse", "auth.device.revoke", "auth.reset", "auth.password.change", "auth.2fa.disable", "auth.logout"] {
        assert!(actions.iter().any(|x| x == a), "missing audit {a}: {actions:?}");
    }
    // No secrets in the audit log.
    let meta: Vec<Value> = sqlx::query_scalar("select meta from audit_log").fetch_all(&s.db).await.unwrap();
    let all = serde_json::to_string(&meta).unwrap();
    assert!(!all.contains(PASSWORD) && !all.contains(&backup[0]) && !all.contains("rmr_"));
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lockout() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "lock@example.com";
    s.new_user(email, PASSWORD).await;
    for _ in 0..5 {
        assert_eq!(s.login(email, "wrong password!!", "L").await.code(), "invalid_credentials");
    }
    // Locked: even the right password is refused, with Retry-After.
    let r = s.login(email, PASSWORD, "L").await;
    assert_eq!(r.status, 429);
    assert_eq!(r.code(), "account_locked");
    assert!(r.headers.get("retry-after").is_some());
    // Unknown e-mails lock the same way.
    for _ in 0..5 {
        assert_eq!(s.login("ghost@example.com", "wrong password!!", "L").await.code(), "invalid_credentials");
    }
    assert_eq!(s.login("ghost@example.com", "wrong password!!", "L").await.code(), "account_locked");

    // Base is 1 s in tests: after it passes, sign-in works and the count resets.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(s.login(email, PASSWORD, "L").await.status, 200);
    let fails: i32 = sqlx::query_scalar("select failed_logins from users where email = $1").bind(email).fetch_one(&s.db).await.unwrap();
    assert_eq!(fails, 0);

    // Doubling: the 6th straight failure locks for 2 s.
    for _ in 0..5 {
        s.login(email, "wrong password!!", "L").await;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    s.login(email, "wrong password!!", "L").await;
    let r = s.login(email, PASSWORD, "L").await;
    assert_eq!(r.code(), "account_locked");
    let secs: u64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=2).contains(&secs), "retry-after {secs}");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_per_ip() {
    let Some(s) = start_with(Mode::Hosted, |c| {
        c.rate.ip_burst = 3;
        c.rate.ip_per_sec = 0.01;
    })
    .await
    else {
        return;
    };
    for _ in 0..3 {
        assert_eq!(s.login("a@example.com", "whatever password", "L").await.status, 401);
    }
    let r = s.login("a@example.com", "whatever password", "L").await;
    assert_eq!(r.status, 429);
    assert_eq!(r.code(), "rate_limited");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_2fa() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "req@example.com";
    s.new_user(email, PASSWORD).await;
    // The org now requires 2FA.
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let mut cfg = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().1;
    cfg.policies.require_2fa = true;
    remail_server::orgs::set_config(&s.db, &s.state.env, &org.id, &cfg, "test").await.unwrap();

    let r = s.login(email, PASSWORD, "Laptop").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["setupRequired"], true, "{:?}", r.body);
    assert_eq!(r.body["methods"], json!(["totp"]));
    let ch = r.body["challenge"].as_str().unwrap().to_string();
    // The setup challenge can't be used as a 2FA login code challenge.
    assert_eq!(s.post("/auth/login/2fa", json!({"challenge": ch, "code": "123456"}), None).await.code(), "invalid_token");
    // No bearer and no challenge: refused.
    assert_eq!(s.post("/me/2fa/setup", json!({}), None).await.status, 401);

    let setup = s.post("/me/2fa/setup", json!({"challenge": ch}), None).await;
    assert_eq!(setup.status, 200, "{:?}", setup.body);
    let secret = setup.body["secret"].as_str().unwrap().to_string();
    let r = s.post("/me/2fa/enable", json!({"code": totp_now(&secret), "challenge": ch}), None).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["backupCodes"].as_array().unwrap().len(), 10);
    let tokens = r.body["tokens"].clone();
    let access = token(&tokens);
    assert_eq!(tokens["user"]["totp"], true);
    let me = s.get("/me", Some(&access)).await;
    assert_eq!(me.body["policies"]["require2fa"], true);
    // Can't switch it off while the org requires it.
    let code = r.body["backupCodes"][0].as_str().unwrap();
    let r = s.post("/me/2fa/disable", json!({"password": PASSWORD, "code": code}), Some(&access)).await;
    assert_eq!(r.code(), "forbidden");
    // The challenge finished the sign-in; it can't be used again.
    let r = s.post("/me/2fa/setup", json!({"challenge": ch}), None).await;
    assert_eq!(r.code(), "invalid_token");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_policy_and_disabled_users() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "pol@example.com";
    s.new_user(email, PASSWORD).await;
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let mut cfg = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().1;
    cfg.policies.platforms = vec!["macos".into()];
    remail_server::orgs::set_config(&s.db, &s.state.env, &org.id, &cfg, "test").await.unwrap();
    assert_eq!(s.login(email, PASSWORD, "L").await.code(), "platform_not_allowed");
    let r = s.post("/auth/login", json!({"email": email, "password": PASSWORD, "device": {"name": "M", "platform": "macos"}}), None).await;
    assert_eq!(r.status, 200);
    let access = token(&r.body);
    let r = s.post("/auth/login", json!({"email": email, "password": PASSWORD, "device": {"name": "M", "platform": "toaster"}}), None).await;
    assert_eq!(r.code(), "bad_request");

    sqlx::query("update users set disabled_at = now() where email = $1").bind(email).execute(&s.db).await.unwrap();
    let r = s.post("/auth/login", json!({"email": email, "password": PASSWORD, "device": {"name": "M", "platform": "macos"}}), None).await;
    assert_eq!(r.code(), "account_disabled");
    assert_eq!(s.get("/me", Some(&access)).await.status, 401, "disabled users lose access at once");

    // Errors are always the JSON body, even for unknown routes and bad JSON.
    let r = s.get("/nope", None).await;
    assert_eq!(r.status, 404);
    assert_eq!(r.code(), "not_found");
    let r = s.send(s.http.post(s.api("/auth/login")).header("content-type", "application/json").body("{not json")).await;
    assert_eq!(r.code(), "bad_request");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn onprem_invites() {
    let Some(s) = start(Mode::Onprem).await else { return };
    let meta = s.get("/meta", None).await;
    assert_eq!(meta.body["mode"], "onprem");
    assert_eq!(meta.body["signup"], "invite");
    assert_eq!(meta.body["orgName"], "Acme");
    assert_eq!(meta.body["verifyEmail"], false);

    // No invite: refused.
    let r = s.post("/auth/signup", json!({"name": "A", "email": "a@acme.com", "password": PASSWORD}), None).await;
    assert_eq!(r.code(), "invite_required");

    // The first-start admin invite.
    let url = s.first_invite.clone().expect("first invite");
    let invite = url.split("t=").nth(1).unwrap().to_string();
    let r = s.post("/auth/signup", json!({"name": "Admin", "email": "admin@acme.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.status, 202, "{:?}", r.body);
    assert_eq!(r.body["verify"], "none");
    let admin = s.login("admin@acme.com", PASSWORD, "Laptop").await;
    assert_eq!(admin.body["user"]["role"], "admin");
    // Used up.
    let r = s.post("/auth/signup", json!({"name": "B", "email": "b@acme.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.code(), "invalid_token");

    // Invites pinned to an address.
    let org = s.org_id().await;
    let inv = s.invite(&org, Some("Bob@Acme.com"), "user").await;
    let r = s.post("/auth/signup", json!({"name": "B", "email": "eve@acme.com", "password": PASSWORD, "invite": inv}), None).await;
    assert_eq!(r.code(), "invalid_token");
    let r = s.post("/auth/signup", json!({"name": "Bob", "email": "bob@acme.com", "password": PASSWORD, "invite": inv}), None).await;
    assert_eq!(r.status, 202);
    assert_eq!(s.login("bob@acme.com", PASSWORD, "L").await.body["user"]["role"], "user");

    // Allowed domains.
    sqlx::query("update orgs set allowed_domains = '{acme.com}'").execute(&s.db).await.unwrap();
    let inv = s.invite(&org, None, "user").await;
    let r = s.post("/auth/signup", json!({"name": "C", "email": "c@other.com", "password": PASSWORD, "invite": inv}), None).await;
    assert_eq!(r.code(), "domain_not_allowed");
    let r = s.post("/auth/signup", json!({"name": "C", "email": "c@acme.com", "password": PASSWORD, "invite": inv}), None).await;
    assert_eq!(r.status, 202);
    s.finish().await;
}

/// A hosted service without mail delivery still gets its first account: each
/// start offers a one-time owner invite until an owner exists.
#[tokio::test]
async fn hosted_owner_invite_until_an_owner_exists() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let url = s.first_invite.clone().expect("owner invite on first start");
    let invite = url.split("t=").nth(1).unwrap().to_string();
    let r = s
        .post(
            "/auth/signup",
            json!({"name": "Owner", "email": "owner@example.com", "password": PASSWORD, "invite": invite}),
            None,
        )
        .await;
    assert_eq!(r.status, 202, "{:?}", r.body);
    assert_eq!(r.body["verify"], "none", "an invite needs no e-mail check");
    let r = s.login("owner@example.com", PASSWORD, "Laptop").await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["user"]["role"], "owner");
    // Once there's an owner, starts don't offer another.
    let again = remail_server::orgs::startup(&s.state.cfg, &s.db, &s.state.env, true).await.unwrap();
    assert!(again.is_none());
    s.finish().await;
}

/// Without mail delivery, open sign-up would strand people on "check your
/// inbox": the server takes invitations only, and says so in /v1/meta.
#[tokio::test]
async fn no_mail_means_invitations_only() {
    let Some(s) = start_with(Mode::Hosted, |c| c.dev_mail = false).await else { return };
    let meta = s.get("/meta", None).await;
    assert_eq!(meta.body["signup"], "invite");
    let r = s.post("/auth/signup", json!({"name": "A", "email": "a@example.com", "password": PASSWORD}), None).await;
    assert_eq!(r.code(), "invite_required", "{:?}", r.body);
    s.finish().await;
}

/// Someone who signed up but never confirmed (no mail arrived) can still use
/// an invitation for the same address: it takes the unconfirmed account over.
#[tokio::test]
async fn invite_claims_an_unconfirmed_address() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "stuck@example.com";
    let r = s.post("/auth/signup", json!({"name": "Old", "email": email, "password": PASSWORD}), None).await;
    assert_eq!(r.status, 202);
    let old_verify = s.mailed_token(email, "verify").await;
    assert_eq!(s.login(email, PASSWORD, "Laptop").await.code(), "email_not_verified");

    let invite = s.first_invite.clone().unwrap().split("t=").nth(1).unwrap().to_string();
    let new_password = "maple harbour quiet seventeen";
    let r = s
        .post("/auth/signup", json!({"name": "Owner", "email": email, "password": new_password, "invite": invite}), None)
        .await;
    assert_eq!(r.status, 202, "{:?}", r.body);
    assert_eq!(r.body["verify"], "none");
    let r = s.login(email, new_password, "Laptop").await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["user"]["role"], "owner");
    assert_eq!(r.body["user"]["name"], "Owner");
    assert_eq!(s.login(email, PASSWORD, "Laptop").await.code(), "invalid_credentials");
    // The old confirmation link is dead, and the invitation is used up.
    assert_eq!(s.post("/auth/verify", json!({"token": old_verify}), None).await.code(), "invalid_token");
    let r = s.post("/auth/signup", json!({"name": "X", "email": "x@example.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.code(), "invalid_token");

    // A confirmed account is never taken over.
    let other = remail_server::auth::tokens::create_invite(&s.db, &s.org_id().await, None, "user").await.unwrap();
    let r = s
        .post("/auth/signup", json!({"name": "Thief", "email": email, "password": new_password, "invite": other.expose()}), None)
        .await;
    assert_eq!(r.code(), "email_taken");
    s.finish().await;
}
