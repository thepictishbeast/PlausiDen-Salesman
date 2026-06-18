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
#![deny(missing_docs)]

use serde::{Deserialize, Serialize};

/// Score in [0.0, 1.0]. 0 = no tells. 1 = strong AI tells.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskScore {
    /// Overall risk in `0.0..=1.0` (weighted max of the signal hits).
    pub score: f32,
    /// The individual signals that fired, for explainability.
    pub hits: Vec<SignalHit>,
}

impl RiskScore {
    /// True if the AI-tell score is strictly below `threshold` — i.e. the
    /// draft is human-enough to pass the gate at that threshold.
    pub fn passes(&self, threshold: f32) -> bool {
        self.score < threshold
    }

    /// Human-readable explanation lines, one per signal hit, formatted as
    /// `[name/weight] evidence` — surfaced to the operator at review.
    pub fn reasons(&self) -> Vec<String> {
        self.hits
            .iter()
            .map(|h| format!("[{}/{:.2}] {}", h.name, h.weight, h.evidence))
            .collect()
    }
}

/// A single AI-tell signal that fired against a draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalHit {
    /// Signal identifier (e.g. `fabricated_numeric_claim`).
    pub name: String,
    /// This signal's weight / contribution to the overall score.
    pub weight: f32,
    /// The matched text or reason, surfaced to the operator.
    pub evidence: String,
}

/// Run the ensemble against a draft body. Subject is checked too if
/// non-empty.
pub fn score(body: &str, subject: Option<&str>) -> RiskScore {
    score_with_facts(body, subject, None)
}

/// Same as `score`, plus a hard fact-trace gate when `facts` is
/// provided. Every numeric claim in the body whose digit run does
/// NOT appear in the facts JSONB is flagged as `fabricated_numeric_claim`
/// at weight 0.85 — strong enough to fail the standard 0.50 detector
/// threshold on its own. The soft `unanchored_numeric_claim` heuristic
/// is suppressed when facts are present (the harder check supersedes it).
///
/// Why this matters: stops the drafter from inventing stats. If the
/// LLM writes "saved 60% on incident response" but no input fact
/// mentions 60, the claim was hallucinated and the message MUST NOT
/// ship. Closes the single biggest "AI invents numbers" failure mode.
pub fn score_with_facts(
    body: &str,
    subject: Option<&str>,
    facts: Option<&serde_json::Value>,
) -> RiskScore {
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
    if let Some(f) = facts {
        check_fabricated_numeric_claims(body, f, &mut hits);
        check_personalization_missing(body, f, &mut hits);
    } else {
        check_unanchored_numeric_claims(body, &mut hits);
    }
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

/// Hard fact-trace gate: every numeric claim in `body` must trace
/// back to a digit run in `facts` (the JSONB the drafter received).
///
/// The check is digit-run based, not semantic: extract each numeric
/// claim from body (same scanner the soft heuristic uses), then look
/// up its DIGIT STRING in a serialized walk of the facts JSON. If
/// the digits don't appear with non-digit boundaries on either side,
/// the claim is fabricated and we emit a high-weight hit.
///
/// BUG ASSUMPTION: digit-run match is necessary but not sufficient
/// for "the claim is true". A draft that says "60% improvement in
/// breach rate" passes the gate if any input fact mentions 60 (e.g.
/// "founded in 1960"). That's an acceptable false-NEGATIVE rate at
/// this layer; the higher-weight strong-tell signals + operator
/// review still gate the send. The point is to catch hallucinations,
/// not enforce semantic faithfulness.
fn check_fabricated_numeric_claims(
    body: &str,
    facts: &serde_json::Value,
    out: &mut Vec<SignalHit>,
) {
    let haystack = fact_haystack(facts);
    let bytes = body.as_bytes();
    let mut samples: Vec<String> = Vec::new();
    let mut hits = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
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
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let suffix = bytes[j];
            let next_after_i_ok = i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_alphanumeric();
            let next_after_j_ok = j + 1 >= bytes.len() || !bytes[j + 1].is_ascii_alphanumeric();
            let is_claim_shape = (after == b'%' || suffix == b'%')
                || ((after == b'x' || after == b'X') && next_after_i_ok)
                || ((suffix == b'x' || suffix == b'X') && next_after_j_ok)
                || (start > 0 && bytes[start - 1] == b'$')
                || ((suffix == b'K' || suffix == b'M' || suffix == b'B') && next_after_j_ok);
            if is_claim_shape {
                let digits: String = body[start..i]
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    // The claim "$40K" can be quoted in facts as "40K"
                    // OR as "$40,000". Build candidate digit forms so
                    // either spelling anchors the claim.
                    let mut candidates: Vec<String> = vec![digits.clone()];
                    let magnitude = if suffix == b'K' {
                        Some(1_000u64)
                    } else if suffix == b'M' {
                        Some(1_000_000u64)
                    } else if suffix == b'B' {
                        Some(1_000_000_000u64)
                    } else {
                        None
                    };
                    if let Some(mag) = magnitude
                        && let Ok(n) = digits.parse::<u64>()
                        && let Some(scaled) = n.checked_mul(mag)
                    {
                        candidates.push(scaled.to_string());
                    }
                    let any_match = candidates
                        .iter()
                        .any(|c| haystack_contains_number(&haystack, c));
                    if !any_match {
                        hits += 1;
                        if samples.len() < 3 {
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
                            samples.push(body[a..b].chars().take(80).collect::<String>());
                        }
                    }
                }
            }
            continue;
        }
        i += 1;
    }
    if hits > 0 {
        out.push(SignalHit {
            name: "fabricated_numeric_claim".into(),
            weight: 0.85,
            evidence: format!(
                "{hits} numeric claim(s) NOT traceable to input facts; samples: {samples:?}"
            ),
        });
    }
}

/// Recursive walk of a JSON value into a single text haystack.
/// Joins all leaf strings + numbers with a space delimiter so a
/// digit-run search is just `str::contains` against the result.
pub fn fact_haystack(v: &serde_json::Value) -> String {
    let mut out = String::new();
    fact_haystack_walk(v, &mut out);
    out
}

fn fact_haystack_walk(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::String(s) => {
            out.push(' ');
            out.push_str(s);
        }
        serde_json::Value::Number(n) => {
            out.push(' ');
            out.push_str(&n.to_string());
        }
        serde_json::Value::Bool(b) => {
            out.push(' ');
            out.push_str(if *b { "true" } else { "false" });
        }
        serde_json::Value::Array(arr) => {
            for x in arr {
                fact_haystack_walk(x, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, x) in map {
                out.push(' ');
                out.push_str(k);
                fact_haystack_walk(x, out);
            }
        }
        serde_json::Value::Null => {}
    }
}

/// Substring-search `digits` in `haystack` requiring non-digit
/// boundaries on both sides — so "40" matches "$40K" but not "204"
/// or "1407". Strips commas from haystack to handle "$40,000" → "40000".
fn haystack_contains_number(haystack: &str, digits: &str) -> bool {
    let normalized: String = haystack.chars().filter(|&c| c != ',').collect();
    let bytes = normalized.as_bytes();
    let needle = digits.as_bytes();
    if needle.is_empty() || needle.len() > bytes.len() {
        return false;
    }
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let left_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
            let right_ok =
                i + needle.len() == bytes.len() || !bytes[i + needle.len()].is_ascii_digit();
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Personalization-quality gate: a draft must reference at least
/// ONE substantive fact about the prospect — company name, industry,
/// a tech_signals token, or a meaningful word from description.
/// If the body matches none of those, the draft is generic. Generic
/// cold emails close zero deals; flag at weight 0.70 so the operator
/// either rejects or strengthens before sending.
///
/// BUG ASSUMPTION: substring matching is naïve. A draft that says
/// "we've been thinking about your team's posture" might match
/// "posture" in a tech_signals fact about "security posture
/// monitoring" — fine. False POSITIVES (saying generic when the
/// draft is fine) are uncomfortable but recoverable via override.
/// False NEGATIVES (saying personalized when the draft is generic)
/// are the real risk; we keep the bar low (one match passes).
fn check_personalization_missing(body: &str, facts: &serde_json::Value, out: &mut Vec<SignalHit>) {
    let body_lc = body.to_ascii_lowercase();
    let mut anchors: Vec<String> = Vec::new();
    if let Some(name) = facts.get("company").and_then(|v| v.as_str()) {
        for tok in meaningful_tokens(name) {
            anchors.push(tok);
        }
    }
    if let Some(industry) = facts.get("industry").and_then(|v| v.as_str()) {
        for tok in meaningful_tokens(industry) {
            anchors.push(tok);
        }
    }
    if let Some(arr) = facts.get("tech_signals").and_then(|v| v.as_array()) {
        for s in arr.iter().filter_map(|v| v.as_str()) {
            for tok in meaningful_tokens(s) {
                anchors.push(tok);
            }
        }
    } else if let Some(s) = facts.get("tech_signals").and_then(|v| v.as_str()) {
        for tok in meaningful_tokens(s) {
            anchors.push(tok);
        }
    }
    if let Some(desc) = facts.get("description").and_then(|v| v.as_str()) {
        for tok in meaningful_tokens(desc) {
            anchors.push(tok);
        }
    }
    if anchors.is_empty() {
        // No facts to anchor on → can't fairly grade personalization.
        return;
    }
    let any_match = anchors.iter().any(|a| body_lc.contains(a));
    if !any_match {
        let preview: Vec<&str> = anchors.iter().take(5).map(String::as_str).collect();
        out.push(SignalHit {
            name: "personalization_missing".into(),
            weight: 0.70,
            evidence: format!(
                "draft references none of the prospect's facts; expected at \
                 least one of (sample): {preview:?}"
            ),
        });
    }
}

/// Lower-case + strip non-alpha boundary; keep tokens of length ≥ 4
/// that aren't on the stop-word list. Used by personalization-missing
/// to extract anchorable facts from string fields.
fn meaningful_tokens(s: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the",
        "and",
        "for",
        "with",
        "from",
        "their",
        "your",
        "this",
        "that",
        "have",
        "company",
        "platform",
        "service",
        "services",
        "solutions",
        "solution",
        "technology",
        "technologies",
        "team",
        "teams",
        "based",
        "industry",
        "business",
        "global",
        "leading",
        "founded",
        "headquartered",
        "provides",
        "offers",
        "delivers",
        "enables",
        "helps",
        "into",
        "about",
        "across",
        "around",
    ];
    let lc = s.to_ascii_lowercase();
    lc.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 4 && !STOP.contains(t))
        .map(|t| t.to_string())
        .collect()
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

    // -----------------------------------------------------------------
    // U44: fact-trace gate
    // -----------------------------------------------------------------

    #[test]
    fn anchored_numeric_claim_passes_fact_trace() {
        // Body cites 60% — the input facts say the company posted a
        // 60% YoY growth blog. Anchored, no hit.
        let body = "Saw your 60% YoY growth post — the breakdown on \
                    incident-response cost was the part I'd love to compare. \
                    Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "Acme",
            "recent_signals": ["posted 60% YoY growth blog"],
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            !r.hits.iter().any(|h| h.name == "fabricated_numeric_claim"),
            "got reasons={:?}",
            r.reasons()
        );
    }

    #[test]
    fn fabricated_numeric_claim_fails_fact_trace() {
        // Body cites 80% — facts mention NO 80. Hallucinated.
        let body = "We helped a peer cut their breach-response cost by 80% \
                    — would love to walk you through the same play. \
                    Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "Acme",
            "recent_signals": ["posted Q4 growth blog", "hired CISO"],
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            r.hits.iter().any(|h| h.name == "fabricated_numeric_claim"),
            "expected fabricated_numeric_claim hit; got {:?}",
            r.reasons()
        );
        assert!(r.score >= 0.80, "expected hard fail; got {}", r.score);
    }

    #[test]
    fn fact_trace_handles_dollar_suffix() {
        // "$40K" in body, fact says "ARR: $40,000". Comma-stripping
        // should let the digit run "40" anchor on "40000".
        let body = "We saw your $40K ARR milestone — congrats. The play \
                    we ran for a peer at $40K was to instrument retries \
                    first. Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "Acme",
            "metrics": { "arr_dollars": "$40,000" },
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            !r.hits.iter().any(|h| h.name == "fabricated_numeric_claim"),
            "got reasons={:?}",
            r.reasons()
        );
    }

    #[test]
    fn fact_trace_rejects_substring_of_unrelated_number() {
        // Body cites 40 — facts only mention 1407. Digit boundary
        // check should reject the substring match.
        let body = "Cut response time by 40% in our pilot, which mirrors \
                    the breakdown your team posted. Cut by 40% again. \
                    Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "Acme",
            "incident_id": "INC-1407",
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            r.hits.iter().any(|h| h.name == "fabricated_numeric_claim"),
            "expected fabricated_numeric_claim hit; got {:?}",
            r.reasons()
        );
    }

    #[test]
    fn fact_trace_no_facts_falls_back_to_soft_heuristic() {
        // No facts → run the existing soft heuristic; multiple
        // numeric claims surface as the soft signal, not the hard one.
        let body = "Cut by 60%, saved $40K, improved 3x. \
                    Reply STOP and I won't follow up.";
        let r = score(body, None);
        assert!(
            r.hits.iter().any(|h| h.name == "unanchored_numeric_claim"),
            "expected soft heuristic; got {:?}",
            r.reasons()
        );
        assert!(
            !r.hits.iter().any(|h| h.name == "fabricated_numeric_claim"),
            "did not expect hard heuristic without facts",
        );
    }

    #[test]
    fn fact_haystack_walks_nested_objects() {
        let v = serde_json::json!({
            "outer": {
                "inner": ["a", 42, true, { "leaf": "deep" }]
            },
            "n": 7
        });
        let h = fact_haystack(&v);
        assert!(h.contains("a"), "missing array string: {h}");
        assert!(h.contains("42"), "missing array number: {h}");
        assert!(h.contains("deep"), "missing nested leaf: {h}");
        assert!(h.contains("7"), "missing top-level number: {h}");
    }

    #[test]
    fn haystack_contains_number_respects_boundaries() {
        // "40" in "1407" must NOT match.
        assert!(!haystack_contains_number("INC-1407 launched", "40"));
        // "40" in "$40K" should match.
        assert!(haystack_contains_number("$40K ARR", "40"));
        // "40" in "40,000" with comma stripped should match.
        assert!(haystack_contains_number("ARR $40,000", "40000"));
        // Empty needle never matches.
        assert!(!haystack_contains_number("anything", ""));
    }

    // -----------------------------------------------------------------
    // U46: personalization-quality gate
    // -----------------------------------------------------------------

    #[test]
    fn personalized_draft_passes() {
        // Body name-checks "Acme" (company) and "Postgres" (tech).
        let body = "Saw Acme is on Postgres — the upgrade-ladder play \
                    we ran for a peer in healthcare-tech turned a 3-day \
                    cutover into a 30-minute one. Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "Acme",
            "industry": "Healthcare technology",
            "tech_signals": ["Postgres", "AWS"],
            "description": "Acme builds clinical workflow tools.",
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            !r.hits.iter().any(|h| h.name == "personalization_missing"),
            "got reasons={:?}",
            r.reasons()
        );
    }

    #[test]
    fn generic_draft_fails_personalization() {
        // Body says nothing about the prospect.
        let body = "We help teams improve their security posture. Worth a chat? \
                    Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "Acme",
            "industry": "Healthcare technology",
            "tech_signals": ["Postgres", "AWS"],
            "description": "Acme builds clinical workflow tools.",
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            r.hits.iter().any(|h| h.name == "personalization_missing"),
            "expected personalization_missing hit; got {:?}",
            r.reasons()
        );
        assert!(
            r.score >= 0.65,
            "expected fail-grade score; got {}",
            r.score
        );
    }

    #[test]
    fn personalization_skipped_when_facts_have_no_anchors() {
        // Empty facts → no anchors → skip the gate (no false-positive).
        let body = "We help teams improve their security posture. \
                    Reply STOP and I won't follow up.";
        let facts = serde_json::json!({
            "company": "",
            "industry": null,
            "tech_signals": [],
            "description": "",
        });
        let r = score_with_facts(body, None, Some(&facts));
        assert!(
            !r.hits.iter().any(|h| h.name == "personalization_missing"),
            "should not fire on empty facts; got {:?}",
            r.reasons()
        );
    }

    #[test]
    fn meaningful_tokens_strips_stopwords_and_short() {
        let toks = meaningful_tokens("Healthcare technology platform team");
        // "technology", "platform", "team" are stopwords; "Healthcare" stays.
        assert!(toks.contains(&"healthcare".to_string()));
        assert!(!toks.contains(&"technology".to_string()));
        assert!(!toks.contains(&"team".to_string()));
        // Length-3 tokens dropped.
        let toks2 = meaningful_tokens("AI ML K8s Postgres");
        assert!(toks2.contains(&"postgres".to_string()));
        assert!(!toks2.contains(&"ai".to_string()));
        assert!(!toks2.contains(&"ml".to_string()));
    }
}
