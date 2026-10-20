//! Remail mail engine.
//!
//! IMAP sync into a local SQLite cache, SMTP sending with undo and send
//! later, local snooze, signatures, templates and one-click unsubscribe.
//! Nothing here talks to a vendor cloud; the engine speaks IMAP/SMTP directly.

pub mod ai;
pub mod assistant;
pub mod caldav;
pub mod calendar;
pub mod classify;
pub mod compose;
pub mod engine;
pub mod graph;
pub mod imap;
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
