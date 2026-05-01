//! Wire-format invariants for ReplyKind.
//!
//! The classifier prompts the LLM to emit one of these snake_case
//! strings. If a future refactor flips strum's serialize_all or
//! changes serde rename_all, the LLM's output would silently fail
//! to parse and `FromStr::from_str` would error in
//! `salesman-content::classify_reply::invoke`. These tests pin the
//! wire form so any break shows up in CI before it bites in
//! production.

use crate::model::ReplyKind;
use std::str::FromStr;

/// Every (variant, wire-string) pair the rest of the system relies
/// on. Add to this when a new ReplyKind variant lands.
const PAIRS: &[(ReplyKind, &str)] = &[
    (ReplyKind::Engaged, "engaged"),
    (ReplyKind::Question, "question"),
    (ReplyKind::Objection, "objection"),
    (ReplyKind::Optout, "optout"),
    (ReplyKind::OutOfOffice, "out_of_office"),
    (ReplyKind::Bounce, "bounce"),
    (ReplyKind::Spam, "spam"),
    (ReplyKind::Unclassified, "unclassified"),
    (ReplyKind::LegalThreat, "legal_threat"),
];

#[test]
fn display_emits_snake_case() {
    for (variant, wire) in PAIRS {
        assert_eq!(
            variant.to_string(),
            *wire,
            "ReplyKind::{:?} must Display as `{wire}` — wire format \
             is what the LLM is prompted to emit, breaking it would \
             silently miss-classify replies",
            variant
        );
    }
}

#[test]
fn from_str_parses_snake_case() {
    for (variant, wire) in PAIRS {
        let parsed = ReplyKind::from_str(wire).unwrap_or_else(|e| {
            panic!("ReplyKind::from_str(`{wire}`) failed: {e:?}")
        });
        assert_eq!(parsed, *variant);
    }
}

#[test]
fn serde_round_trips_snake_case() {
    for (variant, wire) in PAIRS {
        let s = serde_json::to_string(variant).unwrap();
        // serde_json wraps the string in quotes.
        assert_eq!(s, format!("\"{wire}\""),
            "ReplyKind::{:?} serializes to `{}`, expected `\"{}\"`",
            variant, s, wire);
        let back: ReplyKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, *variant);
    }
}

#[test]
fn legal_threat_is_distinct_from_optout() {
    // Defense-in-depth check: legal_threat must NEVER be the same
    // wire-form as optout — the downstream handler dispatches
    // suppression source on this string, and conflating the two
    // would lose audit-trail provenance for legally-charged
    // inbounds.
    assert_ne!(
        ReplyKind::LegalThreat.to_string(),
        ReplyKind::Optout.to_string()
    );
    assert_ne!(ReplyKind::LegalThreat, ReplyKind::Optout);
}
