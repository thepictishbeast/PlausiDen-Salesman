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

/// Every ReplyKind variant — keep in sync with the enum.
const ALL_KINDS: &[ReplyKind] = &[
    ReplyKind::Engaged,
    ReplyKind::Question,
    ReplyKind::Objection,
    ReplyKind::Optout,
    ReplyKind::OutOfOffice,
    ReplyKind::Bounce,
    ReplyKind::Spam,
    ReplyKind::Unclassified,
    ReplyKind::LegalThreat,
];

#[test]
fn funnel_state_label_pins_reply_to_funnel_policy() {
    // The compliance-critical mapping from a classified reply to the
    // prospect's funnel state. Pinned so a refactor cannot silently
    // re-route — e.g. stop suppressing opt-outs, or start advancing a
    // bounce. (Used by salesman-state::apply_reply_to_prospect.)
    assert_eq!(ReplyKind::Engaged.funnel_state_label(), Some("engaged"));
    assert_eq!(ReplyKind::Question.funnel_state_label(), Some("engaged"));
    assert_eq!(ReplyKind::Optout.funnel_state_label(), Some("suppressed"));
    assert_eq!(
        ReplyKind::LegalThreat.funnel_state_label(),
        Some("suppressed")
    );
    assert_eq!(ReplyKind::Bounce.funnel_state_label(), Some("lost"));
    // Ambiguous kinds leave the funnel untouched for operator judgement.
    assert_eq!(ReplyKind::Objection.funnel_state_label(), None);
    assert_eq!(ReplyKind::OutOfOffice.funnel_state_label(), None);
    assert_eq!(ReplyKind::Spam.funnel_state_label(), None);
    assert_eq!(ReplyKind::Unclassified.funnel_state_label(), None);
}

#[test]
fn suppression_trigger_is_exactly_optout_and_legal_threat() {
    for &k in ALL_KINDS {
        let expected = matches!(k, ReplyKind::Optout | ReplyKind::LegalThreat);
        assert_eq!(
            k.is_suppression_trigger(),
            expected,
            "is_suppression_trigger() wrong for {k:?} — this is the consent/legal gate"
        );
    }
}

#[test]
fn every_suppression_trigger_routes_prospect_to_suppressed() {
    // The two gates must agree: anything that suppresses the sender must
    // also move the prospect's funnel state to "suppressed".
    for &k in ALL_KINDS {
        if k.is_suppression_trigger() {
            assert_eq!(
                k.funnel_state_label(),
                Some("suppressed"),
                "{k:?} suppresses the sender but does not route the prospect to suppressed"
            );
        }
    }
}

#[test]
fn funnel_state_labels_are_known_states() {
    // Guard against a typo'd label that would write a bogus
    // prospects.state value the transition graph doesn't recognise.
    for &k in ALL_KINDS {
        if let Some(label) = k.funnel_state_label() {
            assert!(
                matches!(label, "engaged" | "lost" | "suppressed"),
                "unexpected funnel label {label:?} for {k:?}"
            );
        }
    }
}
