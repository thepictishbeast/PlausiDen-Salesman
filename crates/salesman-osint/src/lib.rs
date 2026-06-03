//! salesman-osint — per-prospect intelligence sources beyond the
//! basic homepage scrape.
//!
//! Each adapter wraps a free-or-cheap public API:
//! - GDELT  — recent news articles mentioning a query (no auth)
//! - GitHub — org / repo discovery (REST API; works unauthenticated
//!   with strict rate limits, or with a PAT)
//! - HackerNews — Algolia-backed search of stories + comments
//!
//! All three are wrapped as `Tool`s so the agent loop can drive them.
//!
//! BUG ASSUMPTION: every API has rate limits. We don't enforce a
//! global throttle here — caller (orchestrator) is expected to
//! schedule calls sensibly. Per-call timeouts are 20s.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod dns_info;
pub mod gdelt;
pub mod github_org;
pub mod hn;
pub mod wayback;
pub mod wikipedia;

pub use dns_info::{DnsInfoClient, DnsInfoTool};
pub use gdelt::{GdeltClient, GdeltTool};
pub use github_org::{GithubOrgClient, GithubOrgTool};
pub use hn::{HnClient, HnTool};
pub use wayback::{WaybackClient, WaybackTool};
pub use wikipedia::{WikipediaClient, WikipediaTool};
