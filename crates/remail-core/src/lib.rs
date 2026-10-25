//! Remail mail engine.
//!
//! IMAP sync into a local SQLite cache, SMTP sending with undo and send
//! later, local snooze, signatures, templates and one-click unsubscribe.
//! Mail and calendars go straight to the providers (IMAP/SMTP, Graph,
//! CalDAV); the Remail server ([`cloud`]) only keeps the user's setup.

pub mod ai;
pub mod assistant;
pub mod caldav;
pub mod calendar;
pub mod classify;
pub mod cloud;
pub mod compose;
pub mod engine;
pub mod graph;
pub mod imap;
pub mod layout;
pub mod mcp;
pub mod mime;
pub mod model;
pub mod oauth;
pub mod presets;
pub mod render;
pub mod search;
pub mod secrets;
pub mod smtp;
pub mod store;
pub mod sync;

pub use engine::{Engine, EventSink};
pub use model::*;
/// The API and sync types shared with the Remail server.
pub use remail_proto as proto;
