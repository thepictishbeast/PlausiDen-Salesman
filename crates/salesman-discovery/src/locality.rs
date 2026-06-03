//! Local-first prioritization.
//!
//! Scores how well a company's free-text `region` matches an
//! operator-configured target locality, so already-discovered prospects
//! can be ranked local-first ("worry about local prospects first").
//!
//! SCOPE: this is pure ranking/filtering over data the system ALREADY
//! has. It does NOT change what is scraped or how much is discovered
//! (discovery-yield changes are gated) — it only orders/filters existing
//! companies by region. Regions are free-text (e.g. "San Francisco, CA",
//! "Edinburgh, Scotland, UK", "Remote"), so matching is token-based and
//! deliberately fuzzy rather than relying on structured geo data.

use std::collections::BTreeSet;

/// Normalize a free-text region for comparison: lowercased, with every
/// run of non-alphanumeric characters treated as a separator, and
/// surrounding whitespace trimmed.
pub fn normalize_region(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_sep = false;
        } else if !prev_sep {
            out.push(' ');
            prev_sep = true;
        }
    }
    out.trim().to_string()
}

/// Split a normalized region into its set of word tokens.
fn tokens(region: &str) -> BTreeSet<String> {
    normalize_region(region)
        .split_whitespace()
        .map(|t| t.to_string())
        .collect()
}

/// Score how "local" `region` is relative to `target_terms`, in
/// `0.0..=1.0`.
///
/// Each target term (e.g. `"Edinburgh"`, `"Scotland, UK"`) is tokenized;
/// a term's score is the fraction of its tokens that appear in the
/// company's region. The overall score is the best-matching target term
/// (max), so a company matching ANY configured locality ranks high.
///
/// Returns `0.0` when `region` is `None`/empty, when `target_terms` is
/// empty, or when nothing overlaps. A whole-term match yields `1.0`.
pub fn locality_score(region: Option<&str>, target_terms: &[&str]) -> f32 {
    let region_tokens = match region {
        Some(r) => tokens(r),
        None => return 0.0,
    };
    if region_tokens.is_empty() {
        return 0.0;
    }
    let mut best = 0.0_f32;
    for term in target_terms {
        let term_tokens = tokens(term);
        if term_tokens.is_empty() {
            continue;
        }
        let matched = term_tokens
            .iter()
            .filter(|t| region_tokens.contains(*t))
            .count();
        let score = matched as f32 / term_tokens.len() as f32;
        if score > best {
            best = score;
        }
    }
    best
}

/// True if `region` matches any target locality strongly enough to be
/// treated as "local" — i.e. [`locality_score`] is at least `threshold`.
/// A `0.5` threshold treats a partial (e.g. city-only) match as local.
pub fn is_local(region: Option<&str>, target_terms: &[&str], threshold: f32) -> bool {
    locality_score(region, target_terms) >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_punctuation_and_case() {
        assert_eq!(normalize_region("  San Francisco, CA "), "san francisco ca");
        assert_eq!(normalize_region("EDINBURGH."), "edinburgh");
        assert_eq!(normalize_region("New-York / NY"), "new york ny");
        assert_eq!(normalize_region(""), "");
    }

    #[test]
    fn whole_term_match_scores_one() {
        assert_eq!(
            locality_score(Some("San Francisco, CA"), &["san francisco"]),
            1.0
        );
        assert_eq!(
            locality_score(Some("Edinburgh, Scotland, UK"), &["scotland"]),
            1.0
        );
    }

    #[test]
    fn partial_token_overlap_scores_fraction() {
        // "edinburgh, scotland" is the target; region has only "edinburgh"
        // → 1 of 2 tokens.
        assert!((locality_score(Some("Edinburgh"), &["edinburgh, scotland"]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn best_target_wins() {
        // No match on first term, full match on second.
        assert_eq!(
            locality_score(Some("London, UK"), &["san francisco", "london"]),
            1.0
        );
    }

    #[test]
    fn no_match_or_missing_region_scores_zero() {
        assert_eq!(locality_score(Some("Tokyo, JP"), &["san francisco"]), 0.0);
        assert_eq!(locality_score(None, &["london"]), 0.0);
        assert_eq!(locality_score(Some("   "), &["london"]), 0.0);
        assert_eq!(locality_score(Some("London"), &[]), 0.0);
    }

    #[test]
    fn is_local_respects_threshold() {
        // City-only match = 0.5; local at threshold 0.5, not at 0.75.
        assert!(is_local(Some("Edinburgh"), &["edinburgh, scotland"], 0.5));
        assert!(!is_local(Some("Edinburgh"), &["edinburgh, scotland"], 0.75));
        assert!(is_local(Some("Scotland"), &["scotland"], 1.0));
    }
}
