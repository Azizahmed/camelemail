//! JSON over HTTPS to the Remail server. Errors become `CloudError`s with the
//! server's stable code; network failures are `unavailable`. Nothing here
//! logs tokens or bodies.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;

use remail_proto::api::{PutSection, Section, VersionMismatch};
use remail_proto::error::ErrorBody;

use super::CloudError;

pub(crate) struct Api {
    http: reqwest::Client,
}

/// What a section write got back.
pub(crate) enum PutOutcome {
    Saved(i64),
    /// Someone wrote first: the current section, to merge and write again.
    Conflict(Section),
}

impl Api {
    pub fn new() -> anyhow::Result<Api> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("Remail/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Api { http })
    }

    /// `method base+path`, with a bearer token and a JSON body when given.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        base: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T, CloudError> {
        let (status, bytes) = self.send(method, base, path, token, body, None).await?;
        decode(status, &bytes)
    }

    /// `PUT /v1/sections/{id}` with `If-Match: "v<version>"`.
    pub async fn put_section(&self, base: &str, token: &str, id: &str, version: i64, doc: &PutSection) -> Result<PutOutcome, CloudError> {
        let path = format!("/v1/sections/{}", encode(id));
        let (status, bytes) = self.send(Method::PUT, base, &path, Some(token), Some(doc), Some(format!("\"v{version}\""))).await?;
        if status == StatusCode::PRECONDITION_FAILED {
            let m: VersionMismatch =
                serde_json::from_slice(&bytes).map_err(|_| CloudError::new("internal", "The server sent an unreadable conflict"))?;
            return Ok(PutOutcome::Conflict(m.current));
        }
        let r: remail_proto::api::PutSectionResponse = decode(status, &bytes)?;
        Ok(PutOutcome::Saved(r.version))
    }

    async fn send(
        &self,
        method: Method,
        base: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&(impl Serialize + ?Sized)>,
        if_match: Option<String>,
    ) -> Result<(StatusCode, Vec<u8>), CloudError> {
        let mut req = self.http.request(method, format!("{}{path}", base.trim_end_matches('/')));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        if let Some(m) = if_match {
            req = req.header(reqwest::header::IF_MATCH, m);
        }
        let resp = req.send().await.map_err(|e| unavailable(&e))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| unavailable(&e))?;
        Ok((status, bytes.to_vec()))
    }
}

/// Path segment for a section id (`cred:acc_…` keeps its colon).
fn encode(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')).collect()
}

fn unavailable(e: &reqwest::Error) -> CloudError {
    let what = if e.is_timeout() { "The Remail server didn't answer in time" } else { "Couldn't reach the Remail server" };
    tracing::debug!("Remail server request failed: {}", e.to_string().split(" for url").next().unwrap_or_default());
    CloudError::new("unavailable", what)
}

fn decode<T: DeserializeOwned>(status: StatusCode, bytes: &[u8]) -> Result<T, CloudError> {
    if status.is_success() {
        let body = if bytes.iter().all(u8::is_ascii_whitespace) { b"null".as_slice() } else { bytes };
        return serde_json::from_slice(body).map_err(|_| CloudError::new("internal", "The Remail server sent an unexpected reply"));
    }
    if let Ok(e) = serde_json::from_slice::<ErrorBody>(bytes) {
        return Err(CloudError::from_api(&e.error));
    }
    let code = match status.as_u16() {
        400 => "bad_request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        413 => "too_large",
        429 => "rate_limited",
        502..=504 => "unavailable",
        _ => "internal",
    };
    Err(CloudError::new(code, format!("The Remail server answered HTTP {}", status.as_u16())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_keep_the_server_code() {
        let body = br#"{"error":{"code":"weak_password","message":"Too short"}}"#;
        let e = decode::<()>(StatusCode::BAD_REQUEST, body).unwrap_err();
        assert_eq!(e.to_string(), "cloud:weak_password:Too short");
        assert_eq!(decode::<()>(StatusCode::BAD_GATEWAY, b"<html>").unwrap_err().code, "unavailable");
        decode::<()>(StatusCode::NO_CONTENT, b"").unwrap();
        assert_eq!(encode("cred:acc_1/../x"), "cred:acc_1..x");
    }
}
