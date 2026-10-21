//! Remail server: accounts and sign-in, encrypted settings sync, org config
//! and change notices.
//!
//! The server stores setup, not mail. Every user section is AES-GCM ciphertext
//! under that user's data key, which is wrapped by a master key held outside
//! the database (see [`crypto`]). Only the sync path in [`sections`] can turn a
//! user section back into plaintext.

pub mod admin;
pub mod app;
pub mod audit;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod keyops;
pub mod licence;
pub mod mailer;
pub mod notices;
pub mod org_api;
pub mod orgs;
pub mod ratelimit;
pub mod redact;
pub mod sections;
pub mod web;
#[cfg(feature = "testing")]
pub mod testing;

pub use app::{router, AppState};
pub use config::{Config, Mode};
pub use error::{AppError, AppResult};
