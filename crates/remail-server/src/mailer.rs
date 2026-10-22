//! Server e-mail: verification, password reset and invites.
//!
//! With `REMAIL_SMTP_URL` mail goes out through lettre; else with
//! `REMAIL_SNDR_API_KEY` through SNDR's HTTPS API (CranL's e-mail service).
//! Without either, debug builds started with `REMAIL_DEV_MAIL=1` keep mail in
//! memory for `GET /v1/dev/mail` (tests read links from there); otherwise the
//! server logs that mail isn't configured, never the link itself.

use std::sync::Mutex;

use anyhow::{Context, Result};
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::Serialize;

use crate::config::Config;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

enum Transport {
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    /// SNDR-style JSON API: `POST {url}` with a bearer key.
    Sndr { http: reqwest::Client, url: String, key: String },
    Memory,
    Off,
}

pub struct Mailer {
    transport: Transport,
    from: Mailbox,
    outbox: Mutex<Vec<Mail>>,
}

impl Mailer {
    pub fn new(cfg: &Config) -> Result<Self> {
        let from: Mailbox = cfg.mail_from.parse().context("REMAIL_MAIL_FROM is not an address")?;
        let transport = match &cfg.smtp_url {
            Some(url) => Transport::Smtp(
                AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                    .context("REMAIL_SMTP_URL is not a valid SMTP URL")?
                    .build(),
            ),
            None => match &cfg.sndr_api_key {
                Some(key) => Transport::Sndr {
                    http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build()?,
                    url: cfg.sndr_url.clone(),
                    key: key.clone(),
                },
                None if cfg!(debug_assertions) && cfg.dev_mail => Transport::Memory,
                None => Transport::Off,
            },
        };
        Ok(Mailer { transport, from, outbox: Mutex::new(Vec::new()) })
    }

    /// Can users get mail from this server at all?
    pub fn configured(&self) -> bool {
        !matches!(self.transport, Transport::Off)
    }

    /// Sends (SMTP in the background); failures are logged without the content.
    pub async fn send(&self, mail: Mail) {
        self.deliver(mail, false).await
    }

    /// Like [`Mailer::send`] but waits for SMTP (the CLI exits right after).
    pub async fn send_and_wait(&self, mail: Mail) {
        self.deliver(mail, true).await
    }

    async fn deliver(&self, mail: Mail, wait: bool) {
        match &self.transport {
            Transport::Off => {
                tracing::warn!(kind = %mail.subject, "mail not configured (REMAIL_SMTP_URL or REMAIL_SNDR_API_KEY); message not sent");
            }
            Transport::Memory => {
                self.outbox.lock().unwrap_or_else(|p| p.into_inner()).push(mail);
            }
            Transport::Sndr { http, url, key } => {
                let body = serde_json::json!({
                    "from": self.from.to_string(),
                    "to": [mail.to],
                    "subject": mail.subject,
                    "text": mail.body,
                });
                let req = http.post(url.as_str()).bearer_auth(key).json(&body);
                // In the background, like SMTP. Failures log the status and
                // the service's error code only, never the message.
                let job = tokio::spawn(async move {
                    match req.send().await {
                        Ok(r) if r.status().is_success() => {}
                        Ok(r) => {
                            let status = r.status().as_u16();
                            let code = r
                                .json::<serde_json::Value>()
                                .await
                                .ok()
                                .and_then(|v| v["error"]["code"].as_str().map(String::from))
                                .unwrap_or_default();
                            tracing::warn!(status, code = %code, "mail not sent (SNDR)");
                        }
                        Err(e) => tracing::warn!(error = %e.without_url(), "mail not sent (SNDR)"),
                    }
                });
                if wait {
                    let _ = job.await;
                }
            }
            Transport::Smtp(t) => {
                let to = match mail.to.parse::<Mailbox>() {
                    Ok(to) => to,
                    Err(_) => {
                        tracing::warn!("mail not sent: bad recipient address");
                        return;
                    }
                };
                let msg = Message::builder()
                    .from(self.from.clone())
                    .to(to)
                    .subject(mail.subject.clone())
                    .header(lettre::message::header::ContentType::TEXT_PLAIN)
                    .body(mail.body);
                match msg {
                    // In the background: the answer to the request must not
                    // wait on (or reveal through timing) the SMTP round trip.
                    Ok(msg) => {
                        let t = t.clone();
                        let job = tokio::spawn(async move {
                            if let Err(e) = t.send(msg).await {
                                tracing::warn!(error = %e, "mail not sent");
                            }
                        });
                        if wait {
                            let _ = job.await;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "mail not built"),
                }
            }
        }
    }

    /// Debug builds: mail kept in memory, newest last.
    pub fn dev_outbox(&self) -> Vec<Mail> {
        self.outbox.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

fn ar(locale: &str) -> bool {
    locale.starts_with("ar")
}

/// The links in e-mails: an app deep link plus the https fallback.
fn links(public_url: &str, path: &str, token: &str) -> String {
    format!("remail://{path}?t={token}\n{public_url}/{path}?t={token}")
}

pub fn verify_mail(locale: &str, public_url: &str, to: &str, token: &str) -> Mail {
    let l = links(public_url, "verify", token);
    let (subject, body) = if ar(locale) {
        ("تأكيد بريدك في Camel Mail", format!("افتح هذا الرابط لتأكيد بريدك الإلكتروني:\n\n{l}\n\nينتهي الرابط خلال 24 ساعة."))
    } else {
        ("Confirm your Camel Mail e-mail", format!("Open this link to confirm your e-mail address:\n\n{l}\n\nThe link expires in 24 hours."))
    };
    Mail { to: to.into(), subject: subject.into(), body }
}

pub fn reset_mail(locale: &str, public_url: &str, to: &str, token: &str) -> Mail {
    let l = links(public_url, "reset", token);
    let (subject, body) = if ar(locale) {
        ("إعادة تعيين كلمة مرور Camel Mail", format!("افتح هذا الرابط لاختيار كلمة مرور جديدة:\n\n{l}\n\nينتهي الرابط خلال ساعة. إن لم تطلب ذلك فتجاهل هذه الرسالة."))
    } else {
        ("Reset your Camel Mail password", format!("Open this link to choose a new password:\n\n{l}\n\nThe link expires in an hour. If you didn't ask for this, ignore this e-mail."))
    };
    Mail { to: to.into(), subject: subject.into(), body }
}

pub fn invite_mail(public_url: &str, org_name: &str, to: &str, token: &str) -> Mail {
    let l = links(public_url, "invite", token);
    Mail {
        to: to.into(),
        subject: format!("You're invited to {org_name} on Camel Mail"),
        body: format!("Open this link in Camel Mail to create your account:\n\n{l}\n\nThe invite expires in 7 days."),
    }
}

pub fn already_registered_mail(locale: &str, public_url: &str, to: &str) -> Mail {
    let (subject, body) = if ar(locale) {
        ("لديك حساب في Camel Mail", format!("طلب أحدهم إنشاء حساب بهذا البريد، ولديك حساب بالفعل. إن نسيت كلمة المرور فأعد تعيينها من {public_url}."))
    } else {
        ("You already have a Camel Mail account", format!("Someone tried to sign up with this e-mail, which already has an account. If you forgot your password, reset it from {public_url}."))
    };
    Mail { to: to.into(), subject: subject.into(), body }
}
