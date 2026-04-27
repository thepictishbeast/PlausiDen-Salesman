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
    check_marketing_superlative_density(body, &mut hits);
    check_unanchored_numeric_claims(body, &mut hits);
    check_empty_hedge_phrases(body, &mut hits);
    check_recap_connectives(body, &mut hits);

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

/// Catch numeric claims that read like fabricated marketing
/// metrics — "cut X by 60%", "saved $40K", "improved by 3x" —
/// that aren't tied to anything specific in the message.
///
/// We can't fully verify "trace to facts" without the input facts
/// in scope; this is a soft-fail at 0.40 that flags any draft
/// containing 2+ numeric claims with percent / dollar / multiplier
/// shapes. The operator decides — they're often FINE if the
/// numbers are real, but should re-read carefully.
fn check_unanchored_numeric_claims(body: &str, out: &mut Vec<SignalHit>) {
    let bytes = body.as_bytes();
    let mut hits = 0u32;
    let mut samples: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // Look for: number-followed-by-{%, x, K/M, dollar-prefixed}.
        if c.is_ascii_digit() {
            // Read the run of digits + optional ',' or '.'.
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b',' || bytes[i] == b'.')
            {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let after = bytes[i];
            // Skip spaces between number and unit.
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let suffix = bytes[j];
            // Must be % | x | K | M | "th" | preceded by $.
            let next_after_i_ok = i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_alphanumeric();
            let next_after_j_ok = j + 1 >= bytes.len() || !bytes[j + 1].is_ascii_alphanumeric();
            let matched = (after == b'%' || suffix == b'%')
                || ((after == b'x' || after == b'X') && next_after_i_ok)
                || ((suffix == b'x' || suffix == b'X') && next_after_j_ok)
                || (start > 0 && bytes[start - 1] == b'$')
                || ((suffix == b'K' || suffix == b'M' || suffix == b'B') && next_after_j_ok);
            if matched {
                hits += 1;
                let lo = start.saturating_sub(20);
                let hi = (j + 4).min(bytes.len());
                let mut a = lo;
                while !body.is_char_boundary(a) && a > 0 {
                    a -= 1;
                }
                let mut b = hi;
                while !body.is_char_boundary(b) && b < body.len() {
                    b += 1;
                }
                if samples.len() < 3 {
                    samples.push(body[a..b].chars().take(50).collect::<String>());
                }
            }
            continue;
        }
        i += 1;
    }
    if hits >= 2 {
        out.push(SignalHit {
            name: "unanchored_numeric_claim".into(),
            // Soft signal — operator review, not auto-fail.
            weight: 0.40,
            evidence: format!("{hits} numeric claim(s); samples: {samples:?}"),
        });
    }
}

/// Density of marketing-superlative words. One is fine; three+
/// concentrated in one message is a strong LLM tell — no human
/// writer reaches for "industry-leading" + "world-class" +
/// "unparalleled" in the same email by accident.
fn check_marketing_superlative_density(body: &str, out: &mut Vec<SignalHit>) {
    let s = body.to_ascii_lowercase();
    const TERMS: &[&str] = &[
        "best-in-class",
        "world-class",
        "unparalleled",
        "revolutionary",
        "industry-leading",
        "transformative",
        "game-changing",
        "empower",
        "empowers",
        "empowering",
        "leverage",
        "leverages",
        "leveraging",
        "holistic",
        "unprecedented",
        "unleash",
        "elevate",
        "transcend",
        "pioneer",
        "harness",
        "robust governance",
        "seamlessly",
        "thrive",
        "trusted partner",
        "drive operational excellence",
    ];
    let mut hit_terms: Vec<&str> = TERMS.iter().filter(|t| s.contains(*t)).copied().collect();
    hit_terms.dedup();
    let n = hit_terms.len();
    if n >= 3 {
        // Scale: 3 → 0.7, 4 → 0.8, 5+ → 0.9.
        let weight = (0.55 + 0.075 * n as f32).min(0.92);
        let preview: Vec<&&str> = hit_terms.iter().take(5).collect();
        out.push(SignalHit {
            name: "marketing_superlative_density".into(),
            weight,
            evidence: format!("{n} marketing terms (e.g. {preview:?})"),
        });
    }
}

/// Empty-hedge phrasing. LLMs reach for "I completely understand" /
/// "no pressure at all" / "happy to" patterns to soften copy. One is
/// fine; two or more in one message is a tell.
fn check_empty_hedge_phrases(body: &str, out: &mut Vec<SignalHit>) {
    let s = body.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "completely understand",
        "totally understand",
        "no pressure at all",
        "no pressure whatsoever",
        "looking forward to connecting",
        "looking forward to hearing your thoughts",
        "i was wondering if you might",
        "i'd love to",
        "i would love to",
        "happy to schedule",
        "happy to chat",
        "thrilled to",
        "excited to announce",
        "thought i'd check in",
        "really appreciate you taking",
        "really appreciate your time",
        "gently follow up",
        "whenever you have a moment",
        "whenever you have a chance",
    ];
    let hits: Vec<&str> = PATTERNS
        .iter()
        .filter(|p| s.contains(*p))
        .copied()
        .collect();
    if hits.len() >= 2 {
        let weight = if hits.len() >= 3 { 0.75 } else { 0.6 };
        out.push(SignalHit {
            name: "empty_hedge".into(),
            weight,
            evidence: format!("{} hedge phrases: {hits:?}", hits.len()),
        });
    }
}

/// "To recap" / "In summary" / "To summarize" — recap connectives at
/// the start of a paragraph are a strong LLM tell (humans rarely
/// recap their own one-paragraph emails). When multiple co-occur
/// the signal escalates.
fn check_recap_connectives(body: &str, out: &mut Vec<SignalHit>) {
    let s = body.to_ascii_lowercase();
    const PATTERNS: &[(&str, f32)] = &[
        ("to recap", 0.6),
        ("in summary", 0.6),
        ("to summarize", 0.6),
        ("as we delve", 0.7),
        ("rich tapestry", 0.85),
        ("ever-evolving landscape", 0.7),
        ("rapidly evolving landscape", 0.6),
        ("complex interplay", 0.6),
        ("ever-changing", 0.5),
    ];
    let hits: Vec<&(&str, f32)> = PATTERNS.iter().filter(|(p, _)| s.contains(p)).collect();
    for (needle, weight) in &hits {
        out.push(SignalHit {
            name: "recap_connective".into(),
            weight: *weight,
            evidence: format!("contains `{needle}`"),
        });
    }
    if hits.len() >= 2 {
        // Multiple recap markers in one message is a strong tell —
        // boost the ensemble's max-weight floor.
        let needles: Vec<&str> = hits.iter().map(|(n, _)| *n).collect();
        out.push(SignalHit {
            name: "recap_stack".into(),
            weight: 0.78,
            evidence: format!("multiple recap connectives: {needles:?}"),
        });
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
    fn unanchored_numeric_claims_flag_softly() {
        let body = "We cut their CI time by 60% and saved them $40K \
                    on infra in 3 months. Want to chat?";
        let r = score(body, None);
        // Soft signal at 0.40 — the operator reviews + can override.
        assert!(
            r.hits.iter().any(|h| h.name == "unanchored_numeric_claim"),
            "expected unanchored_numeric_claim hit; got reasons={:?}",
            r.reasons()
        );
    }

    #[test]
    fn single_numeric_claim_does_not_flag() {
        // A draft with ONE specific number is fine — that's how
        // honest specifics look. Only a stack triggers.
        let body = "Tested it on a 50TB/day fixture; latency was \
                    flat at p99. Want me to send the bench?";
        let r = score(body, None);
        assert!(
            !r.hits.iter().any(|h| h.name == "unanchored_numeric_claim"),
            "single-number draft should not flag; got reasons={:?}",
            r.reasons()
        );
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
