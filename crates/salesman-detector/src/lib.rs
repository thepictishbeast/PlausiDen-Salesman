//! salesman-detector — heuristic ensemble that scores a draft for
//! "this looks AI-generated" risk.
//!
//! Output is a `RiskScore` in [0.0, 1.0]:
//!   0.0 = looks human / no AI tells found
//!   1.0 = many strong AI tells
//!
//! The score is the WEIGHTED MAX of individual signal scores (we
//! don't average — a single very-strong tell ("hope this finds you
//! well") should be enough to fail the gate; surrounding cleanness
//! shouldn't dilute it).
//!
//! Each signal returns `SignalHit { name, weight, evidence }`.
//! `RiskScore::reasons()` exposes the hits so the operator can see
//! WHY a draft failed.
//!
//! BUG ASSUMPTION: this is a heuristic, not a classifier. It will
//! produce false positives on perfectly fine human writing that
//! happens to contain a banned phrase. The `--force-override` path
//! in the CLI is the escape hatch.
//!
//! BUG ASSUMPTION: the dictionary is intentionally small + curated.
//! Bigger lists tend to overfit to specific LLM versions and become
//! easier to evade. Keep this list to high-precision tells.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Score in [0.0, 1.0]. 0 = no tells. 1 = strong AI tells.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskScore {
    pub score: f32,
    pub hits: Vec<SignalHit>,
}

impl RiskScore {
    pub fn passes(&self, threshold: f32) -> bool {
        self.score < threshold
    }

    pub fn reasons(&self) -> Vec<String> {
        self.hits
            .iter()
            .map(|h| format!("[{}/{:.2}] {}", h.name, h.weight, h.evidence))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalHit {
    pub name: String,
    pub weight: f32,
    pub evidence: String,
}

/// Run the ensemble against a draft body. Subject is checked too if
/// non-empty.
pub fn score(body: &str, subject: Option<&str>) -> RiskScore {
    let mut hits = Vec::new();
    if let Some(s) = subject {
        check_subject(s, &mut hits);
    }
    check_cliche_openers(body, &mut hits);
    check_banned_phrases(body, &mut hits);
    check_em_dash_density(body, &mut hits);
    check_overused_signal_words(body, &mut hits);
    check_not_just_x_its_y(body, &mut hits);

    let max_weight = hits.iter().map(|h| h.weight).fold(0.0f32, f32::max);
    RiskScore {
        score: max_weight.clamp(0.0, 1.0),
        hits,
    }
}

// ---------------------------------------------------------------------------
// signals
// ---------------------------------------------------------------------------

fn check_subject(subject: &str, out: &mut Vec<SignalHit>) {
    let s = subject.to_ascii_lowercase();
    let bad = [
        ("quick question", 0.65),
        ("quick chat", 0.6),
        ("idea for {company}", 0.7),
        ("are you the right person", 0.8),
        ("worth a 15-minute chat", 0.7),
        ("a few minutes of your time", 0.65),
    ];
    for (needle, weight) in bad {
        if s.contains(needle) {
            out.push(SignalHit {
                name: "subject_cliche".into(),
                weight,
                evidence: format!("subject contains `{needle}`"),
            });
        }
    }
}

fn check_cliche_openers(body: &str, out: &mut Vec<SignalHit>) {
    // Scan only the first ~200 chars for opener clichés.
    let head = body
        .chars()
        .take(220)
        .collect::<String>()
        .to_ascii_lowercase();
    let openers = [
        ("i hope this email finds you well", 0.95),
        ("i hope this finds you well", 0.95),
        ("i hope this message finds you well", 0.95),
        ("i trust this email finds you", 0.9),
        ("just wanted to reach out", 0.85),
        ("i wanted to reach out", 0.7),
        ("i noticed", 0.5),
        ("i came across", 0.6),
        ("hope you're doing well", 0.85),
        ("happy {weekday}", 0.55),
    ];
    for (needle, weight) in openers {
        if head.contains(needle) {
            out.push(SignalHit {
                name: "cliche_opener".into(),
                weight,
                evidence: format!("opener contains `{needle}`"),
            });
        }
    }
}

fn check_banned_phrases(body: &str, out: &mut Vec<SignalHit>) {
    let s = body.to_ascii_lowercase();
    let phrases = [
        ("in today's fast-paced", 0.9),
        ("in this day and age", 0.9),
        ("at the end of the day", 0.55),
        ("leveraging cutting-edge", 0.95),
        ("cutting-edge solution", 0.85),
        ("revolutionize the way", 0.9),
        ("game-changer", 0.7),
        ("synergize", 0.85),
        ("seamlessly integrate", 0.7),
        ("unlock the full potential", 0.85),
        ("take it to the next level", 0.7),
        ("low-hanging fruit", 0.5),
        ("circle back", 0.45),
        ("touch base", 0.4),
    ];
    for (needle, weight) in phrases {
        if s.contains(needle) {
            out.push(SignalHit {
                name: "banned_phrase".into(),
                weight,
                evidence: format!("body contains `{needle}`"),
            });
        }
    }
}

/// Many em-dashes per 100 chars is a common LLM tell.
fn check_em_dash_density(body: &str, out: &mut Vec<SignalHit>) {
    let n_em = body.chars().filter(|&c| c == '—').count();
    let len = body.chars().count();
    if len < 80 {
        return;
    }
    let per_100 = (n_em as f32) / (len as f32 / 100.0);
    if per_100 >= 1.2 {
        out.push(SignalHit {
            name: "em_dash_density".into(),
            weight: (per_100 / 3.0).min(0.8),
            evidence: format!("{n_em} em-dashes in {len} chars ({per_100:.2}/100)"),
        });
    }
}

fn check_overused_signal_words(body: &str, out: &mut Vec<SignalHit>) {
    // Words LLMs reach for. One occurrence is fine; multiple is a tell.
    let s = body.to_ascii_lowercase();
    let watch: &[(&str, usize, f32)] = &[
        ("delve", 1, 0.7),
        ("delves", 1, 0.7),
        ("delving", 1, 0.7),
        ("ultimately", 2, 0.55),
        ("moreover", 1, 0.65),
        ("furthermore", 1, 0.65),
        ("nonetheless", 1, 0.5),
        ("notwithstanding", 1, 0.7),
        ("comprehensive", 2, 0.55),
        ("multifaceted", 1, 0.7),
        ("paradigm", 1, 0.7),
        ("tapestry", 1, 0.85),
        ("symphony of", 1, 0.85),
        ("realm of", 1, 0.6),
    ];
    for &(needle, threshold, weight) in watch {
        let n = s.matches(needle).count();
        if n >= threshold {
            out.push(SignalHit {
                name: "overused_word".into(),
                weight,
                evidence: format!("`{needle}` appears {n}× (threshold {threshold})"),
            });
        }
    }
}

/// "It's not just X, it's Y." / "Not only X but Y" patterns.
fn check_not_just_x_its_y(body: &str, out: &mut Vec<SignalHit>) {
    let s = body.to_ascii_lowercase();
    let patterns = [
        ("it's not just", 0.7),
        ("it's not about", 0.55),
        ("not only", 0.4),
        (", but rather", 0.5),
    ];
    for (needle, weight) in patterns {
        if s.contains(needle) {
            out.push(SignalHit {
                name: "not_just_x".into(),
                weight,
                evidence: format!("contains `{needle}`"),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sample_passes_low() {
        let body = "Saw your post about pre-image attacks on the rotation \
                    KDF and your patch is what convinced our auditor to \
                    re-open ours. We hit the same off-by-one in 2024 — \
                    happy to share the regression case if useful. \
                    Reply STOP and I won't follow up.";
        let r = score(body, Some("KDF rotation: regression case from 2024"));
        // One em-dash in 250 chars is fine, "saw" is fine.
        assert!(r.score < 0.5, "got {} reasons={:?}", r.score, r.reasons());
    }

    #[test]
    fn cliche_opener_fails() {
        let body = "I hope this email finds you well. We're a security \
                    company and would love to help.";
        let r = score(body, None);
        assert!(r.score >= 0.85, "got {} reasons={:?}", r.score, r.reasons());
        assert!(r.hits.iter().any(|h| h.name == "cliche_opener"));
    }

    #[test]
    fn banned_phrase_fails() {
        let body = "In today's fast-paced security landscape, \
                    organizations need to leverage cutting-edge solutions.";
        let r = score(body, None);
        assert!(r.score >= 0.85, "got {} reasons={:?}", r.score, r.reasons());
    }

    #[test]
    fn em_dash_density_fails() {
        let body = "We help — in real ways — companies — that struggle — \
                    with — the things — that — matter — most — to them — \
                    and — for — them.";
        let r = score(body, None);
        assert!(r.score >= 0.4, "got {} reasons={:?}", r.score, r.reasons());
        assert!(r.hits.iter().any(|h| h.name == "em_dash_density"));
    }

    #[test]
    fn delve_fails() {
        let body = "I wanted to delve into the specifics of how our \
                    multifaceted approach can help.";
        let r = score(body, None);
        assert!(r.score >= 0.65, "got {} reasons={:?}", r.score, r.reasons());
    }

    #[test]
    fn passes_method_works_correctly() {
        let r = RiskScore {
            score: 0.5,
            hits: vec![],
        };
        assert!(r.passes(0.6));
        assert!(!r.passes(0.4));
        assert!(!r.passes(0.5)); // strict less-than
    }

    #[test]
    fn reasons_method_returns_human_readable() {
        let r = score("I hope this finds you well.", None);
        let reasons = r.reasons();
        assert!(!reasons.is_empty());
        assert!(reasons.iter().any(|s| s.contains("cliche_opener")));
    }
}
