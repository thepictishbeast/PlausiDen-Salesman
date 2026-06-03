//! salesman-core — shared types, error model, identifiers.
//!
//! Every other Salesman crate depends on this. Keep it stable and
//! free of heavy dependencies.
#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]
#![deny(missing_docs)]

pub mod email_match;
pub mod error;
pub mod header;
pub mod ids;
pub mod model;
pub mod redact;
pub mod tool;

pub use email_match::{email_match_candidates, mask_email, normalize_email_for_match};
pub use error::{Error, Result};
pub use header::sanitize_header_value;
pub use redact::Redacted;

#[cfg(test)]
mod transition_props;
#[cfg(test)]
mod reply_kind_props;
pub use ids::{CampaignId, CompanyId, ContactId, ProspectId, ReceiptId, TouchId};
pub use model::{
    Campaign, CampaignStatus, Company, Contact, ContactKind, FunnelState, Prospect, Reply,
    ReplyKind, Touch, TouchChannel, TouchOutcome,
};
pub use tool::{ToolArgs, ToolCall, ToolResult};
