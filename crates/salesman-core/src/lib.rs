//! salesman-core — shared types, error model, identifiers.
//!
//! Every other Salesman crate depends on this. Keep it stable and
//! free of heavy dependencies.
#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

pub mod error;
pub mod ids;
pub mod model;
pub mod tool;

pub use error::{Error, Result};
pub use ids::{CampaignId, CompanyId, ContactId, ProspectId, ReceiptId, TouchId};
pub use model::{
    Campaign, CampaignStatus, Company, Contact, ContactKind, FunnelState, Prospect, Reply,
    ReplyKind, Touch, TouchChannel, TouchOutcome,
};
pub use tool::{ToolArgs, ToolCall, ToolResult};
