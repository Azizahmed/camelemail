//! The small web pages links in the server's e-mails open: confirm an
//! address, choose a new password, see an invitation. English and Arabic,
//! no scripts, nothing loaded from elsewhere.
//!
//! Opening a link never acts by itself (mail scanners open links too): the page
//! asks for a click, and the form posts back here.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Form, Router};
use remail_proto::ErrorCode;
use serde::Deserialize;

use crate::app::{AppState, ClientIp};
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/verify", get(verify_page).post(verify_submit))
        .route("/reset", get(reset_page).post(reset_submit))
        .route("/invite", get(invite_page))
}

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    En,
    Ar,
}

impl Lang {
    fn pick(explicit: Option<&str>, headers: &HeaderMap) -> Lang {
        let wanted = explicit.map(str::to_string).or_else(|| {
            headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).map(str::to_string)
        });
        match wanted {
            Some(l) if l.trim_start().to_ascii_lowercase().starts_with("ar") => Lang::Ar,
            _ => Lang::En,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ar => "ar",
        }
    }

    fn tr(self, en: &'static str, ar: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Ar => ar,
        }
    }
}

#[derive(Deserialize)]
struct LinkQuery {
    #[serde(default)]
    t: String,
    #[serde(default)]
    lang: Option<String>,
}

#[derive(Deserialize)]
struct VerifyForm {
    #[serde(default)]
    t: String,
    #[serde(default)]
    lang: Option<String>,
}

#[derive(Deserialize)]
struct ResetForm {
    #[serde(default)]
    t: String,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm: String,
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn page(lang: Lang, title: &str, body: &str) -> Response {
    let dir = if lang == Lang::Ar { "rtl" } else { "ltr" };
    let html = format!(
        r#"<!doctype html>
<html lang="{code}" dir="{dir}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>{title} · Camel Mail</title>
<style>
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: #f7f8fa; color: #1c1d1f;
         font: 15px/1.5 system-ui, -apple-system, "Segoe UI", "Noto Sans", "Noto Sans Arabic", sans-serif; }}
  main {{ width: min(400px, calc(100vw - 32px)); background: #fff; border: 1px solid #e2e4e8; border-radius: 12px;
         padding: 28px; box-shadow: 0 8px 24px rgb(0 0 0 / 6%); }}
  h1 {{ font-size: 20px; margin: 0 0 8px; }}
  p {{ margin: 0 0 16px; color: #55595f; }}
  label {{ display: block; font-weight: 600; margin: 12px 0 4px; }}
  input[type=password] {{ box-sizing: border-box; width: 100%; padding: 9px 11px; border: 1px solid #cfd3d9;
         border-radius: 8px; font: inherit; }}
  button {{ width: 100%; margin-top: 16px; padding: 10px; border: 0; border-radius: 8px; background: #e2620c;
         color: #fff; font: inherit; font-weight: 600; cursor: pointer; }}
  .err {{ color: #b42318; }}
  code {{ display: block; padding: 10px; background: #f1f2f4; border-radius: 8px; word-break: break-all;
         direction: ltr; text-align: start; user-select: all; }}
  .lang {{ margin-top: 20px; font-size: 13px; text-align: center; }}
  .lang a {{ color: #e2620c; }}
</style>
</head>
<body><main>
<h1>{title}</h1>
{body}
</main></body>
</html>"#,
        code = lang.code(),
    );
    let mut res = axum::response::Html(html).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'"),
    );
    // The token is in the URL: never pass it on, never keep it.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    res
}

/// Link to the same page in the other language.
fn switch(lang: Lang, path: &str, token: &str) -> String {
    let (other, label) = match lang {
        Lang::En => ("ar", "العربية"),
        Lang::Ar => ("en", "English"),
    };
    format!(r#"<p class="lang"><a href="{path}?t={t}&amp;lang={other}">{label}</a></p>"#, t = esc(token))
}

fn hidden(token: &str, lang: Lang) -> String {
    format!(
        r#"<input type="hidden" name="t" value="{}"><input type="hidden" name="lang" value="{}">"#,
        esc(token),
        lang.code()
    )
}

fn error_text(lang: Lang, e: &AppError) -> &'static str {
    match e.code() {
        ErrorCode::InvalidToken => lang.tr(
            "This link is not valid or has expired. Ask for a new one in the Camel Mail app.",
            "هذا الرابط غير صالح أو انتهت صلاحيته. اطلب رابطًا جديدًا من تطبيق Camel Mail.",
        ),
        ErrorCode::WeakPassword => lang.tr(
            "Choose a stronger password: at least 10 characters, not easy to guess, and without your name or e-mail.",
            "اختر كلمة مرور أقوى: 10 أحرف على الأقل، يصعب تخمينها، ولا تتضمن اسمك أو بريدك الإلكتروني.",
        ),
        ErrorCode::RateLimited | ErrorCode::AccountLocked => {
            lang.tr("Too many attempts. Wait a few minutes and try again.", "محاولات كثيرة. انتظر بضع دقائق ثم حاول مجددًا.")
        }
        _ => lang.tr("Something went wrong. Try again in a moment.", "حدث خطأ ما. حاول مجددًا بعد قليل."),
    }
}

async fn verify_page(headers: HeaderMap, Query(q): Query<LinkQuery>) -> Response {
    let lang = Lang::pick(q.lang.as_deref(), &headers);
    let body = format!(
        r#"<p>{}</p><form method="post" action="/verify">{}<button type="submit">{}</button></form>{}"#,
        lang.tr(
            "Confirm this e-mail address for your Camel Mail account.",
            "أكّد عنوان البريد هذا لحسابك في Camel Mail."
        ),
        hidden(&q.t, lang),
        lang.tr("Confirm e-mail", "تأكيد البريد"),
        switch(lang, "/verify", &q.t),
    );
    page(lang, lang.tr("Confirm your e-mail", "تأكيد بريدك الإلكتروني"), &body)
}

async fn verify_submit(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Form(f): Form<VerifyForm>,
) -> Response {
    let lang = Lang::pick(f.lang.as_deref(), &headers);
    match crate::auth::verify_email_token(&s, ip, f.t).await {
        Ok(()) => page(
            lang,
            lang.tr("E-mail confirmed", "تم تأكيد البريد"),
            &format!(
                "<p>{}</p>",
                lang.tr("You can now sign in to Camel Mail on any device.", "يمكنك الآن تسجيل الدخول إلى Camel Mail من أي جهاز.")
            ),
        ),
        Err(e) => page(
            lang,
            lang.tr("Couldn't confirm", "تعذّر التأكيد"),
            &format!(r#"<p class="err">{}</p>"#, error_text(lang, &e)),
        ),
    }
}

fn reset_form(lang: Lang, token: &str, error: Option<&str>) -> Response {
    let err = error.map(|e| format!(r#"<p class="err">{}</p>"#, esc(e))).unwrap_or_default();
    let body = format!(
        r#"<p>{intro}</p>{err}<form method="post" action="/reset">{hidden}
<label for="pw">{new}</label><input id="pw" type="password" name="password" autocomplete="new-password" minlength="10" required autofocus>
<label for="pw2">{again}</label><input id="pw2" type="password" name="confirm" autocomplete="new-password" minlength="10" required>
<button type="submit">{save}</button></form>{switch}"#,
        intro = lang.tr(
            "Choose a new password. You'll be signed out on all your devices; your mailboxes and settings stay as they are.",
            "اختر كلمة مرور جديدة. سيُسجَّل خروجك من جميع أجهزتك، وتبقى صناديق بريدك وإعداداتك كما هي."
        ),
        hidden = hidden(token, lang),
        new = lang.tr("New password", "كلمة المرور الجديدة"),
        again = lang.tr("Repeat it", "أعد كتابتها"),
        save = lang.tr("Set password", "تعيين كلمة المرور"),
        switch = switch(lang, "/reset", token),
    );
    page(lang, lang.tr("Reset your password", "إعادة تعيين كلمة المرور"), &body)
}

async fn reset_page(headers: HeaderMap, Query(q): Query<LinkQuery>) -> Response {
    let lang = Lang::pick(q.lang.as_deref(), &headers);
    reset_form(lang, &q.t, None)
}

async fn reset_submit(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Form(f): Form<ResetForm>,
) -> Response {
    let lang = Lang::pick(f.lang.as_deref(), &headers);
    if f.password != f.confirm {
        return reset_form(lang, &f.t, Some(lang.tr("The passwords don't match.", "كلمتا المرور غير متطابقتين.")));
    }
    let token = f.t.clone();
    match crate::auth::reset_with_token(&s, ip, f.t, f.password).await {
        Ok(()) => page(
            lang,
            lang.tr("Password changed", "تم تغيير كلمة المرور"),
            &format!(
                "<p>{}</p>",
                lang.tr("Sign in to Camel Mail with your new password.", "سجّل الدخول إلى Camel Mail بكلمة المرور الجديدة.")
            ),
        ),
        // A weak password can be fixed on the same page; anything else can't.
        Err(e) if e.code() == ErrorCode::WeakPassword => reset_form(lang, &token, Some(error_text(lang, &e))),
        Err(e) => page(
            lang,
            lang.tr("Couldn't change the password", "تعذّر تغيير كلمة المرور"),
            &format!(r#"<p class="err">{}</p>"#, error_text(lang, &e)),
        ),
    }
}

async fn invite_page(State(s): State<AppState>, headers: HeaderMap, Query(q): Query<LinkQuery>) -> Response {
    let lang = Lang::pick(q.lang.as_deref(), &headers);
    let body = format!(
        r#"<p>{steps}</p><label>{server_l}</label><code>{server}</code><label>{code_l}</label><code>{token}</code>{switch}"#,
        steps = lang.tr(
            "Open the Camel Mail app, choose \u{201c}Using Camel Mail at work?\u{201d} and enter this server, then Create account and paste the invitation code.",
            "افتح تطبيق Camel Mail، واختر «تستخدم Camel Mail في العمل؟» وأدخل هذا الخادم، ثم اختر «إنشاء حساب» والصق رمز الدعوة."
        ),
        server_l = lang.tr("Server", "الخادم"),
        server = esc(&s.cfg.public_url),
        code_l = lang.tr("Invitation code", "رمز الدعوة"),
        token = esc(&q.t),
        switch = switch(lang, "/invite", &q.t),
    );
    page(lang, lang.tr("You're invited to Camel Mail", "دعوة للانضمام إلى Camel Mail"), &body)
}
