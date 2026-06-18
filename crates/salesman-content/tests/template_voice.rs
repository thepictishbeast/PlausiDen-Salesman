//! Cross-template voice + compliance invariants for cold-email templates.
//!
//! Per-template `forbidden_phrases` capture each author's intent; this test
//! enforces the *floor* that must hold for EVERY shipped cold template, so a
//! new template that drifts from Salesman's voice — or quietly drops the
//! opt-out — fails here rather than reaching a prospect. The prose guide these
//! invariants encode is `docs/BRAND_VOICE.md`.

use salesman_content::draft_email::ColdTemplate;
use std::path::{Path, PathBuf};

/// Repo root, derived from this crate's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// Every directory of cold templates shipped in the repo.
fn template_dirs() -> Vec<PathBuf> {
    let root = repo_root();
    [
        root.join("templates/cold"),
        root.join("vertical/realestate/templates/cold"),
    ]
    .into_iter()
    .filter(|p| p.is_dir())
    .collect()
}

/// Load every `*.toml` template across all template dirs via the public
/// loader, returned as `(key, template)`.
fn all_templates() -> Vec<(String, ColdTemplate)> {
    let mut out = Vec::new();
    for dir in template_dirs() {
        for entry in std::fs::read_dir(&dir).expect("read_dir templates") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let key = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("utf-8 file stem")
                .to_string();
            let t = ColdTemplate::load(&dir, &key)
                .unwrap_or_else(|e| panic!("load template `{key}`: {e}"))
                .unwrap_or_else(|| panic!("template `{key}` resolved to None"));
            out.push((key, t));
        }
    }
    assert!(
        !out.is_empty(),
        "no cold templates found under {:?}",
        template_dirs()
    );
    out
}

/// Dark patterns CLAUDE.md forbids: fake urgency / social proof / countdowns.
const DARK_PATTERNS: &[&str] = &[
    "act now",
    "limited time",
    "last chance",
    "don't miss",
    "dont miss",
    "100% guarant",
    "risk-free",
    "you've been selected",
    "youve been selected",
    "exclusive offer",
    "expires today",
    "expires soon",
    "hurry",
    "buy now",
    "click here now",
    "while supplies last",
    "once-in-a-lifetime",
];

/// Opt-out / cessation signals; at least one must be present per template.
const OPT_OUT_SIGNALS: &[&str] = &[
    "stop",
    "won't follow up",
    "wont follow up",
    "won't hear from me",
    "wont hear from me",
    "unsubscribe",
    "opt out",
    "opt-out",
    "won't contact you again",
    "remove you",
];

/// The template's `key` field must match its filename stem, so callers that
/// load by key get the file they expect.
#[test]
fn keys_match_filenames() {
    for (key, t) in all_templates() {
        assert_eq!(
            t.key, key,
            "file `{key}.toml` declares key=`{}` — they must match",
            t.key
        );
    }
}

/// Seeds are present and stay within cold-outreach length norms.
#[test]
fn seeds_present_and_concise() {
    for (key, t) in all_templates() {
        assert!(
            !t.subject_seed.trim().is_empty(),
            "`{key}`: empty subject_seed"
        );
        assert!(!t.body_seed.trim().is_empty(), "`{key}`: empty body_seed");
        let subj = t.subject_seed.chars().count();
        let body = t.body_seed.chars().count();
        assert!(subj <= 140, "`{key}`: subject_seed too long ({subj} chars)");
        assert!(body <= 2200, "`{key}`: body_seed too long ({body} chars)");
    }
}

/// No dark patterns in the visible copy (subject + body). The phrases may
/// legitimately appear in `forbidden_phrases` (as things to avoid), so only
/// the visible seeds are scanned.
#[test]
fn no_dark_patterns_in_visible_copy() {
    for (key, t) in all_templates() {
        let hay = format!("{}\n{}", t.subject_seed, t.body_seed).to_lowercase();
        for pat in DARK_PATTERNS {
            assert!(
                !hay.contains(pat),
                "`{key}`: visible copy contains dark pattern `{pat}` (see docs/BRAND_VOICE.md)"
            );
        }
    }
}

/// Every cold message guarantees a working opt-out — in its body or its
/// mandatory phrases (CLAUDE.md + CAN-SPAM).
#[test]
fn every_template_guarantees_an_opt_out() {
    for (key, t) in all_templates() {
        let hay = format!("{}\n{}", t.body_seed, t.mandatory_phrases.join("\n")).to_lowercase();
        assert!(
            OPT_OUT_SIGNALS.iter().any(|s| hay.contains(s)),
            "`{key}`: no opt-out/cessation signal in body_seed or mandatory_phrases — \
             every cold message needs a working opt-out"
        );
    }
}

/// A template must not use, in its visible copy, a phrase it lists in its own
/// `forbidden_phrases` — that would be self-contradiction.
#[test]
fn templates_obey_their_own_forbidden_phrases() {
    for (key, t) in all_templates() {
        let hay = format!("{}\n{}", t.subject_seed, t.body_seed).to_lowercase();
        for p in &t.forbidden_phrases {
            let needle = p.to_lowercase();
            assert!(
                !hay.contains(&needle),
                "`{key}`: visible copy contains its own forbidden phrase `{p}`"
            );
        }
    }
}
