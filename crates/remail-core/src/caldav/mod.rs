//! CalDAV (RFC 4791) client and iCalendar (RFC 5545) data layer.
//!
//! [`ical`] is a lossless iCalendar DOM, [`event`] expands and edits events,
//! [`client`] talks to CalDAV servers, [`feed`] reads iCal subscriptions.

pub mod client;
pub mod event;
pub mod feed;
pub mod ical;
pub mod recur;
pub mod tz;

pub use chrono_tz::Tz;

#[cfg(test)]
mod tests;
