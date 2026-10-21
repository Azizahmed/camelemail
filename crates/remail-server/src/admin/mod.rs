//! Admin API (`/v1/admin/*`, milestone M3). Empty for now.
//!
//! Isolation rule: admin code works with account metadata only. It must never
//! read `user_sections` or ask for a user data key. That is enforced by
//! construction: user data keys are only handed out against
//! [`crate::sections::SyncPath`], which only the `sections` module can build,
//! and `tests/admin_isolation.rs` scans this directory for any reference to
//! sections, user keys or the sync decrypt path.

use axum::Router;

use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}
