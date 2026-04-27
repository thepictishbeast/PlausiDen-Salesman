//! salesman-content — LLM-backed draft + brand-content generators.
//!
//! Phase 1.2 ships `DraftColdEmailTool`. Future phases bring
//! `ComparisonPageTool`, `CaseStudyDraftTool`, `LinkedInPostTool`,
//! and the brand voice guideline loader.
//!
//! BUG ASSUMPTION: drafts produced here ALWAYS land in the
//! AwaitingApproval queue — they are never sent without an explicit
//! operator approve. Anything that bypasses owner-review is a bug.
#![forbid(unsafe_code)]

pub mod draft_email;
pub use draft_email::{ColdEmailDraft, DraftColdEmailTool};
