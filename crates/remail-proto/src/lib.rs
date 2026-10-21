//! Types shared by the Remail server and its clients.
//!
//! No IO lives here: request and response shapes ([`api`]), the organisation
//! configuration clients receive ([`org`]), the per-section sync documents and
//! their merge ([`sync`], [`sections`]) and the signed licence format
//! ([`licence`]).

pub mod api;
pub mod error;
pub mod licence;
pub mod org;
pub mod sections;
pub mod sync;

pub use error::{ApiError, ErrorCode};

/// `prefix_<uuid v4, 32 hex>`, the ID shape used everywhere in Remail.
pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
