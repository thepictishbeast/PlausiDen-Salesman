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
#![deny(missing_docs)]

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

/// Literal PII terms — beyond the emails/phones that [`salesman_core::redact`]
/// already strips — to redact from a prospect dossier before it is sent to a
/// SaaS LLM: the prospect's company `display_name` and `homepage`, which the
/// system already knows.
///
/// `redact()` matches terms as raw, case-sensitive substrings (no word
/// boundary), so a short or common company name would clobber unrelated
/// substrings and corrupt the prompt. We therefore include `display_name` only
/// when it is at least 4 characters (the `homepage` URL is distinctive and
/// always safe). Free-text fields such as `description` are deliberately left to
/// the email/phone scanner rather than term-redacted, since the model needs
/// them to personalize; residual free-text names are an accepted limitation.
pub(crate) fn prospect_pii_terms(prospect: &serde_json::Value) -> Vec<String> {
    let mut terms = Vec::new();
    if let Some(hp) = prospect.get("homepage").and_then(|v| v.as_str()) {
        let hp = hp.trim();
        if hp.len() >= 4 {
            terms.push(hp.to_string());
        }
    }
    if let Some(name) = prospect.get("display_name").and_then(|v| v.as_str()) {
        let name = name.trim();
        if name.chars().count() >= 4 {
            terms.push(name.to_string());
        }
    }
    terms
}
