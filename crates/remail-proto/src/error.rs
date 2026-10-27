//! Error body every API route returns: `{"error":{"code":"…","message":"…"}}`.
//!
//! Codes are stable; clients translate them (`t("err.<code>")`) rather than
//! showing the English message.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    /// `If-Match` didn't match: the body carries the current section.
    VersionMismatch,
    TooLarge,
    RateLimited,
    /// Wrong e-mail or password (deliberately the same for both).
    InvalidCredentials,
    AccountLocked,
    AccountDisabled,
    EmailNotVerified,
    EmailTaken,
    WeakPassword,
    InvalidCode,
    InvalidToken,
    TwoFactorRequired,
    SignupClosed,
    InviteRequired,
    DomainNotAllowed,
    ProviderNotAllowed,
    PlatformNotAllowed,
    LicenceExpired,
    LicenceInvalid,
    SeatLimit,
    /// This app is too old for the account (it signs in without a password proof).
    UpdateRequired,
    Unavailable,
    Internal,
}

impl ErrorCode {
    /// The wire name, e.g. `"provider_not_allowed"`.
    pub fn name(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(s)) => s,
            _ => "internal".into(),
        }
    }

    /// HTTP status the server answers with.
    pub fn status(self) -> u16 {
        use ErrorCode::*;
        match self {
            BadRequest | WeakPassword | InvalidCode | InvalidToken | DomainNotAllowed => 400,
            Unauthorized | InvalidCredentials | TwoFactorRequired => 401,
            Forbidden | AccountDisabled | EmailNotVerified | SignupClosed | InviteRequired
            | PlatformNotAllowed | LicenceExpired | LicenceInvalid | SeatLimit => 403,
            NotFound => 404,
            Conflict | EmailTaken => 409,
            VersionMismatch => 412,
            TooLarge => 413,
            ProviderNotAllowed => 422,
            UpdateRequired => 426,
            AccountLocked | RateLimited => 429,
            Unavailable => 503,
            Internal => 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
    /// Seconds to wait (rate limits, lockout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ApiError,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError { code, message: message.into(), retry_after: None }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.code.name())
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ApiError {}
