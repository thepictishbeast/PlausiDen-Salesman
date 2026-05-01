//! salesman-reply — IMAP poller that ingests inbound replies into
//! the system as `replies` rows with kind=Unclassified.
//!
//! The classifier (Phase 1.5) and reply→FunnelState transitions
//! (Phase 1.6) are downstream consumers — this crate's job is only:
//!   1. Connect to the configured IMAP server (TLS).
//!   2. SEARCH UNSEEN.
//!   3. FETCH each new message.
//!   4. Parse headers + plain-text body via mail-parser.
//!   5. Hand the parsed record to a callback (the CLI / orchestrator
//!      decides what to do — usually persist to db).
//!   6. Mark message \Seen on success.
//!
//! BUG ASSUMPTION: we use SEARCH UNSEEN instead of IDLE for the first
//! cut. IDLE is more efficient but more complex (heartbeats, reconnect
//! on disconnect). A periodic poll on a 60-second interval is fine for
//! Phase 1.5.
//!
//! BUG ASSUMPTION: we treat the *first* text/plain part of the message
//! as the body. Quoted reply chains live in there too — the
//! classifier downstream is responsible for stripping them if it
//! cares. We do NOT execute HTML.
//!
//! SECURITY: IMAP password held in `Zeroizing<String>`. Connection is
//! TLS-only via tokio-rustls. We refuse to connect to non-TLS ports
//! to remove the foot-gun.
#![forbid(unsafe_code)]

pub mod auth_results;
pub mod config;
pub mod dsn;
pub mod parsed;
pub mod poller;

pub use auth_results::{AuthResult, AuthResults, MethodResult};
pub use config::ImapConfig;
pub use dsn::DsnInfo;
pub use parsed::ParsedReply;
pub use poller::ImapPoller;
