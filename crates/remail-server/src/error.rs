//! Every route answers errors as `{"error":{"code","message"}}` with a stable
//! [`ErrorCode`]. Internal failures are logged and reported as `internal`
//! without detail.

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use remail_proto::error::ErrorBody;
use remail_proto::{ApiError, ErrorCode};

#[derive(Debug)]
pub enum AppError {
    Api(ApiError),
    /// Logged server-side; the client sees `internal`.
    Internal(anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AppError::Api(ApiError::new(code, message))
    }

    pub fn retry_after(code: ErrorCode, message: impl Into<String>, secs: u64) -> Self {
        let mut e = ApiError::new(code, message);
        e.retry_after = Some(secs.max(1));
        AppError::Api(e)
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            AppError::Api(e) => e.code,
            AppError::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(ErrorCode::Unauthorized, "Sign in again.")
    }

    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound, "Not found.")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }
}

impl From<ApiError> for AppError {
    fn from(e: ApiError) -> Self {
        AppError::Api(e)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl From<crate::crypto::CryptoError> for AppError {
    fn from(e: crate::crypto::CryptoError) -> Self {
        AppError::Internal(e.into())
    }
}

impl From<JsonRejection> for AppError {
    fn from(e: JsonRejection) -> Self {
        // The rejection text names the field that failed, never its value.
        match e {
            JsonRejection::BytesRejection(_) => Self::new(ErrorCode::TooLarge, "The request body is too large."),
            other => Self::bad_request(other.body_text()),
        }
    }
}

impl From<PathRejection> for AppError {
    fn from(e: PathRejection) -> Self {
        Self::bad_request(e.body_text())
    }
}

pub fn error_response(e: ApiError) -> Response {
    let status = StatusCode::from_u16(e.code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let retry = e.retry_after;
    let mut res = (status, Json(ErrorBody { error: e })).into_response();
    if let Some(secs) = retry {
        if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
            res.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
        }
    }
    res
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Api(e) => error_response(e),
            AppError::Internal(e) => {
                tracing::error!(error = %format!("{e:#}"), "request failed");
                error_response(ApiError::new(ErrorCode::Internal, "Something went wrong on the server."))
            }
        }
    }
}

/// `Json<T>` whose rejections use the API error body.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct ApiJson<T>(pub T);

impl<T: serde::Serialize> IntoResponse for ApiJson<T> {
    fn into_response(self) -> Response {
        Json(self.0).into_response()
    }
}

/// `Path<T>` whose rejections use the API error body.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct ApiPath<T>(pub T);
