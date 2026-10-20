//! SMTP submission.

use std::time::Duration;

use anyhow::{Context, Result};
use lettre::address::Envelope;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::model::{Login, Security, ServerConfig};

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// Bad credentials (535 etc.). The account needs re-authentication.
    #[error("SMTP authentication failed: {0}")]
    Auth(String),
    /// The server rejected the message for good (5xx).
    #[error("message rejected: {0}")]
    Permanent(String),
    /// Network trouble or 4xx; worth retrying.
    #[error("{0}")]
    Transient(String),
}

fn transport(
    server: &ServerConfig,
    username: &str,
    login: &Login,
    allow_invalid: bool,
) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let tls_params = || {
        TlsParameters::builder(server.host.clone())
            .dangerous_accept_invalid_certs(allow_invalid)
            .dangerous_accept_invalid_hostnames(allow_invalid)
            .build_rustls()
            .context("TLS setup failed")
    };
    let tls = match server.security {
        Security::Tls => Tls::Wrapper(tls_params()?),
        Security::Starttls => Tls::Required(tls_params()?),
        Security::None => Tls::None,
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(server.host.clone())
        .port(server.port)
        .tls(tls)
        .timeout(Some(Duration::from_secs(60)));
    builder = match login {
        Login::Password(password) if !username.is_empty() => builder.credentials(Credentials::new(username.to_string(), password.clone())),
        Login::OAuth(token) => builder.credentials(Credentials::new(username.to_string(), token.clone())).authentication(vec![Mechanism::Xoauth2]),
        Login::Password(_) => builder,
    };
    Ok(builder.build())
}

fn classify(e: lettre::transport::smtp::Error) -> SendError {
    classify_parts(e.status().map(u16::from), e.is_permanent(), e.to_string())
}

/// Sort a failure by its SMTP reply code (`status`), not by words in the
/// text: "454 4.7.0 Temporary authentication failure" is worth retrying, and
/// a size limit that happens to contain "534" is not a login problem.
fn classify_parts(status: Option<u16>, permanent: bool, msg: String) -> SendError {
    let lower = msg.to_lowercase();
    if lower.contains("smtpclientauthentication is disabled") || lower.contains("5.7.139") {
        // Microsoft 365: signing in worked, but the organization turned off SMTP sending.
        return SendError::Permanent(
            "Your organization has turned off sending from mail apps (SMTP AUTH) for this mailbox. Ask your Microsoft 365 admin to enable \"Authenticated SMTP\" for it.".into(),
        );
    }
    match status {
        Some(530 | 534 | 535) => SendError::Auth(msg),
        Some(400..=499) => SendError::Transient(msg),
        // No reply code: the client found no login method both sides support.
        None if lower.contains("authentication mechanism") => SendError::Auth(msg),
        _ if permanent => SendError::Permanent(msg),
        _ => SendError::Transient(msg),
    }
}

/// Send already-formatted message bytes to the given envelope recipients.
pub async fn send(
    server: &ServerConfig,
    username: &str,
    login: &Login,
    allow_invalid: bool,
    from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<(), SendError> {
    let t = transport(server, username, login, allow_invalid).map_err(|e| SendError::Transient(e.to_string()))?;
    let from = from.parse().map_err(|_| SendError::Permanent(format!("invalid sender {from}")))?;
    let to = recipients
        .iter()
        .map(|r| r.parse().map_err(|_| SendError::Permanent(format!("invalid recipient {r}"))))
        .collect::<Result<Vec<_>, _>>()?;
    let envelope = Envelope::new(Some(from), to).map_err(|e| SendError::Permanent(e.to_string()))?;
    t.send_raw(&envelope, raw).await.map_err(classify)?;
    Ok(())
}

/// Check that we can connect and authenticate.
pub async fn verify(server: &ServerConfig, username: &str, login: &Login, allow_invalid: bool) -> Result<(), SendError> {
    let t = transport(server, username, login, allow_invalid).map_err(|e| SendError::Transient(e.to_string()))?;
    match t.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(SendError::Transient(format!("could not connect to {}:{}", server.host, server.port))),
        Err(e) => Err(classify(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_reply_code() {
        let c = |status, permanent, msg: &str| classify_parts(status, permanent, msg.to_string());
        assert!(matches!(c(Some(535), true, "535 5.7.8 Username and Password not accepted"), SendError::Auth(_)));
        assert!(matches!(c(Some(534), true, "534 5.7.9 Application-specific password required"), SendError::Auth(_)));
        assert!(matches!(c(Some(530), true, "530 5.7.0 Authentication required"), SendError::Auth(_)));
        assert!(matches!(c(Some(454), false, "454 4.7.0 Temporary authentication failure"), SendError::Transient(_)));
        assert!(matches!(c(Some(421), false, "421 try again later"), SendError::Transient(_)));
        assert!(matches!(c(Some(552), true, "552 message size 5342880 exceeds limit"), SendError::Permanent(_)));
        assert!(matches!(c(Some(550), true, "550 5.1.1 no such user (authentication)"), SendError::Permanent(_)));
        assert!(matches!(c(None, false, "network error: connection reset"), SendError::Transient(_)));
        assert!(matches!(c(Some(535), true, "535 5.7.139 Authentication unsuccessful, SmtpClientAuthentication is disabled"), SendError::Permanent(m) if m.contains("Microsoft 365")));
    }
}
