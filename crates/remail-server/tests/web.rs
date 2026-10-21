//! The pages e-mail links open: opening one changes nothing, the form does.

mod common;

use common::*;
use remail_server::Mode;
use serde_json::json;

async fn get_page(s: &Server, path: &str, lang: Option<&str>) -> (u16, reqwest::header::HeaderMap, String) {
    let mut r = s.http.get(format!("{}{path}", s.url));
    if let Some(l) = lang {
        r = r.header("accept-language", l);
    }
    let res = r.send().await.unwrap();
    (res.status().as_u16(), res.headers().clone(), res.text().await.unwrap())
}

async fn post_form(s: &Server, path: &str, form: &[(&str, &str)]) -> String {
    s.http.post(format!("{}{path}", s.url)).form(form).send().await.unwrap().text().await.unwrap()
}

#[tokio::test]
async fn verify_and_reset_pages() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "web@example.com";
    let r = s.post("/auth/signup", json!({"name": "Web", "email": email, "password": PASSWORD}), None).await;
    assert_eq!(r.status, 202);
    let t = s.mailed_token(email, "verify").await;

    // Opening the link (or a mail scanner opening it) doesn't verify.
    let (status, headers, html) = get_page(&s, &format!("/verify?t={t}"), None).await;
    assert_eq!(status, 200);
    assert!(html.contains(r#"<form method="post" action="/verify">"#));
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert!(headers["content-security-policy"].to_str().unwrap().contains("default-src 'none'"));
    assert_eq!(s.login(email, PASSWORD, "Laptop").await.code(), "email_not_verified");

    // Arabic from the browser's language.
    let (_, _, html) = get_page(&s, &format!("/verify?t={t}"), Some("ar-SA,ar;q=0.9")).await;
    assert!(html.contains(r#"dir="rtl""#) && html.contains("تأكيد"));

    let html = post_form(&s, "/verify", &[("t", &t), ("lang", "en")]).await;
    assert!(html.contains("E-mail confirmed"), "{html}");
    assert_eq!(s.login(email, PASSWORD, "Laptop").await.status, 200);
    // A link works once.
    let html = post_form(&s, "/verify", &[("t", &t), ("lang", "en")]).await;
    assert!(html.contains("not valid or has expired"));

    // Reset: the page is a form; weak or mismatched passwords keep the form.
    let r = s.post("/auth/reset/request", json!({"email": email}), None).await;
    assert_eq!(r.status, 202);
    let t = s.mailed_token(email, "reset").await;
    let (_, _, html) = get_page(&s, &format!("/reset?t={t}&lang=ar"), None).await;
    assert!(html.contains(r#"type="password""#) && html.contains(r#"dir="rtl""#));
    let html = post_form(&s, "/reset", &[("t", &t), ("password", "a-new-pass-phrase-1"), ("confirm", "other")]).await;
    assert!(html.contains("don&#39;t match") && html.contains(r#"name="password""#));
    let html = post_form(&s, "/reset", &[("t", &t), ("password", "password12"), ("confirm", "password12")]).await;
    assert!(html.contains("stronger password") && html.contains(r#"name="password""#), "{html}");
    let new = "violet canyon lamp ninety";
    let html = post_form(&s, "/reset", &[("t", &t), ("password", new), ("confirm", new)]).await;
    assert!(html.contains("Password changed"), "{html}");
    assert_eq!(s.login(email, PASSWORD, "Laptop").await.code(), "invalid_credentials");
    assert_eq!(s.login(email, new, "Laptop").await.status, 200);

    // Tokens and markup are escaped.
    let (_, _, html) = get_page(&s, "/invite?t=%3Cscript%3E", None).await;
    assert!(html.contains("&lt;script&gt;") && !html.contains("<script>"));
    s.finish().await;
}
