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

pub mod angle_picker;
pub mod case_study;
pub mod classify_reply;
pub mod comparison_page;
pub mod competitor_detect;
pub mod draft_email;
pub mod draft_reply;
pub mod extract_interests;
pub mod geo;
pub mod seo_meta;
pub mod site;
pub use angle_picker::{AnglePick, AnglePickerTool, ProductEntry, load_catalog_toml};
pub use case_study::CaseStudyDraftTool;
pub use classify_reply::{ClassifyReply, ReplyClassifyTool};
pub use comparison_page::ComparisonPageTool;
pub use competitor_detect::{CompetitorCatalog, CompetitorEntry, load_competitors_toml};
pub use draft_email::{ColdEmailDraft, DraftColdEmailTool};
pub use draft_reply::{DraftReplyTool, ReplyDraft};
pub use extract_interests::{ExtractedInterests, InterestExtractTool, shape_tags};
pub use geo::{GeoReport, GeoTool};
pub use seo_meta::{SeoMeta, SeoMetaTool};
pub use site::{RenderedPage, SiteConfig, render_site};
