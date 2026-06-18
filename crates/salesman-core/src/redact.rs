//! PII redaction / rehydration for the SaaS-LLM boundary.
//!
//! PlausiDen's doctrine (CLAUDE.md): never put prospect PII in a SaaS
//! LLM's data path. The adopted resolution for using Claude is a
//! *redaction boundary* — the model reasons over text in which PII has
//! been replaced by stable placeholders, and the originals are
//! rehydrated locally before anything is sent or stored. This module is
//! that boundary: pure, no I/O, no model call. It UPHOLDS the doctrine
//! (PII never leaves the box) rather than waiving it.
//!
//! v1 redacts email addresses via a hand-rolled linear scan (no regex,
//! so no catastrophic-backtracking risk) plus caller-supplied literal
//! terms — e.g. the prospect's name / company, which the system already
//! knows — and phone numbers (conservatively; see [`find_phone_spans`]).
//! Other structured detectors can layer on later.
//!
//! Reusable beyond the LLM path: log-line scrubbing, redacting the
//! owner-notification body, etc.

use std::collections::BTreeMap;

/// Placeholder prefix. The trailing `]]` (see [`redact`]) delimits the
/// numeric id so `[[REDACTED_1]]` is never a prefix of `[[REDACTED_10]]`.
const PH_PREFIX: &str = "[[REDACTED_";

/// A redacted string plus the map needed to rehydrate it.
#[derive(Debug, Clone)]
pub struct Redacted {
    text: String,
    /// placeholder -> original PII value.
    map: BTreeMap<String, String>,
}

impl Redacted {
    /// The redacted text (PII replaced by placeholders); safe to hand to
    /// a SaaS model.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of distinct PII values that were redacted.
    pub fn redacted_count(&self) -> usize {
        self.map.len()
    }

    /// Replace every known placeholder in `s` with its original value —
    /// applied to the model's output before it is shown, sent, or stored,
    /// so the final copy contains the real names/addresses again. Text
    /// that isn't a known placeholder is left untouched.
    pub fn rehydrate(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (ph, orig) in &self.map {
            if out.contains(ph.as_str()) {
                out = out.replace(ph.as_str(), orig);
            }
        }
        out
    }
}

/// Redact email addresses and any `extra_terms` (literal, case-sensitive)
/// from `text`, returning the redacted text plus a rehydration map.
///
/// The same original value always maps to the same placeholder, so the
/// model sees consistent tokens (and the map stays small). Overlapping
/// matches are resolved earliest-first, longest-on-tie.
pub fn redact(text: &str, extra_terms: &[&str]) -> Redacted {
    // 1) Collect candidate spans (byte ranges into `text`).
    let mut spans: Vec<(usize, usize)> = find_email_spans(text);
    spans.extend(find_phone_spans(text));
    for term in extra_terms {
        if term.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(rel) = text[from..].find(term) {
            let start = from + rel;
            let end = start + term.len();
            spans.push((start, end));
            from = end;
        }
    }

    // 2) Sort by start (longest first on a tie) and drop overlaps.
    spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut chosen: Vec<(usize, usize)> = Vec::new();
    let mut last_end = 0usize;
    for (s, e) in spans {
        if s >= last_end {
            chosen.push((s, e));
            last_end = e;
        }
    }

    // 3) Rebuild the text, replacing each span with a stable placeholder.
    let mut out = String::with_capacity(text.len());
    let mut orig_to_ph: BTreeMap<String, String> = BTreeMap::new();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut cursor = 0usize;
    let mut next_id = 1usize;
    for (s, e) in chosen {
        out.push_str(&text[cursor..s]);
        let orig = &text[s..e];
        let ph = match orig_to_ph.get(orig) {
            Some(p) => p.clone(),
            None => {
                let p = format!("{PH_PREFIX}{next_id}]]");
                next_id += 1;
                orig_to_ph.insert(orig.to_string(), p.clone());
                map.insert(p.clone(), orig.to_string());
                p
            }
        };
        out.push_str(&ph);
        cursor = e;
    }
    out.push_str(&text[cursor..]);

    Redacted { text: out, map }
}

/// Is `b` allowed in an email local-part (before the `@`)?
fn is_local_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

/// Is `b` allowed in an email domain (after the `@`)?
fn is_domain_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// Find byte ranges of email addresses. All matched bytes are ASCII, so
/// the returned ranges are valid UTF-8 boundaries.
fn find_email_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Expand left over the local part.
        let mut start = i;
        while start > 0 && is_local_byte(bytes[start - 1]) {
            start -= 1;
        }
        // Expand right over the domain.
        let mut end = i + 1;
        while end < bytes.len() && is_domain_byte(bytes[end]) {
            end += 1;
        }
        // Trim trailing '.'/'-' (a domain ends in alphanumerics).
        while end > i + 1 && matches!(bytes[end - 1], b'.' | b'-') {
            end -= 1;
        }
        // Validate: non-empty local part; domain has a dot with an
        // alphanumeric label on each side of it.
        let local_ok = start < i;
        let domain = &text[i + 1..end];
        let domain_ok = domain.len() >= 3
            && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphanumeric())
            && domain.contains('.')
            && domain.rsplit('.').next().is_some_and(|tld| {
                tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphanumeric())
            });
        if local_ok && domain_ok {
            out.push((start, end));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Find byte ranges of phone numbers, conservatively. A run of
/// `[0-9 () .-]` (optionally led by `+`) qualifies only as a
/// HIGH-PRECISION phone — a leading `+` with at least 8 digits, or at
/// least 10 digits with at least one separator. This deliberately skips
/// bare digit runs (order/account numbers), short ranges like
/// `2020-2024`, and comma-grouped prices (commas break the run), to
/// avoid redacting numbers the model legitimately needs. All matched
/// bytes are ASCII, so the ranges are valid UTF-8 boundaries.
fn find_phone_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // A run starts at '+' or a digit, not glued to the right of an
        // alphanumeric (avoids matching inside identifiers).
        let starts = bytes[i] == b'+' || bytes[i].is_ascii_digit();
        if !starts || (i > 0 && bytes[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let start = i;
        let lead_plus = bytes[i] == b'+';
        let mut end = i;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_digit() || matches!(b, b' ' | b'-' | b'.' | b'(' | b')') {
                end += 1;
            } else if b == b'+' && end == start {
                end += 1; // a leading '+' only
            } else {
                break;
            }
        }
        // Trim trailing non-digits so the span ends on the number, THEN
        // count digits + separators over the trimmed span — otherwise a
        // trailing space/paren would wrongly count as an internal
        // separator (making a bare digit run qualify).
        while end > start && !bytes[end - 1].is_ascii_digit() {
            end -= 1;
        }
        if end <= start {
            i += 1;
            continue;
        }
        let span = &bytes[start..end];
        let digits = span.iter().filter(|b| b.is_ascii_digit()).count();
        let has_sep = span
            .iter()
            .any(|b| matches!(b, b' ' | b'-' | b'.' | b'(' | b')'));
        let qualifies = (lead_plus && digits >= 8) || (digits >= 10 && has_sep);
        let bounded = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if qualifies && bounded {
            out.push((start, end));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email_and_rehydrates() {
        let r = redact("please contact jane.doe@acme.com today", &[]);
        assert!(!r.text().contains("jane.doe@acme.com"));
        assert!(r.text().contains("[[REDACTED_1]]"));
        assert_eq!(r.redacted_count(), 1);
        // round-trip on the redacted text
        assert_eq!(
            r.rehydrate(r.text()),
            "please contact jane.doe@acme.com today"
        );
    }

    #[test]
    fn redacts_phone_numbers() {
        for p in [
            "+1-415-555-0123",
            "(415) 555-0123",
            "415.555.0123",
            "+44 20 1234 5678",
        ] {
            let text = format!("call me at {p} anytime");
            let r = redact(&text, &[]);
            assert!(!r.text().contains(p), "phone `{p}` must be redacted");
            assert_eq!(r.rehydrate(r.text()), text, "round-trip for `{p}`");
        }
    }

    #[test]
    fn does_not_redact_non_phone_numbers() {
        // Bare digit runs, short ranges, and comma-grouped prices must
        // survive — the model needs these and they aren't PII.
        for s in [
            "fiscal years 2020-2024",
            "order 12345 shipped",
            "call 1234567890 now", // 10 digits but NO separator
            "total was 1,234,567.89",
            "see RFC 8058 section 2",
        ] {
            let r = redact(s, &[]);
            assert_eq!(r.redacted_count(), 0, "`{s}` should have no redactions");
            assert_eq!(r.text(), s);
        }
    }

    #[test]
    fn same_value_reuses_one_placeholder() {
        let r = redact("a@x.com and again a@x.com", &[]);
        assert_eq!(r.redacted_count(), 1);
        assert_eq!(r.rehydrate(r.text()), "a@x.com and again a@x.com");
    }

    #[test]
    fn distinct_emails_get_distinct_placeholders() {
        let r = redact("a@x.com, b@y.org", &[]);
        assert_eq!(r.redacted_count(), 2);
        assert!(!r.text().contains("a@x.com"));
        assert!(!r.text().contains("b@y.org"));
    }

    #[test]
    fn redacts_literal_terms() {
        let r = redact("Hi Jane at Acme Inc", &["Jane", "Acme Inc"]);
        assert!(!r.text().contains("Jane"));
        assert!(!r.text().contains("Acme Inc"));
        assert_eq!(r.rehydrate(r.text()), "Hi Jane at Acme Inc");
    }

    #[test]
    fn rehydrates_model_output_with_placeholders() {
        let r = redact("write to jane@acme.com", &[]);
        // Simulate the model echoing the placeholder back in its reply.
        let model = "Sure — I'll reach out to [[REDACTED_1]] this week.";
        assert_eq!(
            r.rehydrate(model),
            "Sure — I'll reach out to jane@acme.com this week."
        );
    }

    #[test]
    fn no_pii_leaves_text_unchanged() {
        let r = redact("just a plain sentence, nothing here", &[]);
        assert_eq!(r.text(), "just a plain sentence, nothing here");
        assert_eq!(r.redacted_count(), 0);
    }

    #[test]
    fn does_not_grab_trailing_punctuation() {
        // The sentence-ending period must not be part of the address.
        let r = redact("mail me at bob@host.io.", &[]);
        assert_eq!(r.rehydrate(r.text()), "mail me at bob@host.io.");
        assert!(r.text().ends_with("]]."));
    }

    #[test]
    fn bare_at_sign_is_not_an_email() {
        let r = redact("meet @ 5pm @ the office", &[]);
        assert_eq!(r.redacted_count(), 0);
    }

    #[test]
    fn unknown_placeholders_are_left_untouched() {
        let r = redact("no pii here", &[]);
        // rehydrate must not invent replacements for placeholders it
        // didn't create.
        assert_eq!(
            r.rehydrate("model said [[REDACTED_9]]"),
            "model said [[REDACTED_9]]"
        );
    }

    use proptest::prelude::*;

    proptest! {
        /// Round-trip: rehydrating the redacted text restores the input
        /// exactly, for any text that doesn't already contain our
        /// sentinel. (Real prospect context never contains it.)
        #[test]
        fn rehydrate_is_left_inverse_of_redact(s in "[^\\[]{0,300}") {
            let r = redact(&s, &[]);
            prop_assert_eq!(r.rehydrate(r.text()), s);
        }

        /// The redacted text never contains a redacted original value.
        #[test]
        fn redacted_text_hides_the_email(local in "[a-z]{1,8}", dom in "[a-z]{1,8}", tld in "[a-z]{2,4}") {
            let email = format!("{local}@{dom}.{tld}");
            let text = format!("reach {email} now");
            let r = redact(&text, &[]);
            prop_assert!(!r.text().contains(&email));
            prop_assert_eq!(r.rehydrate(r.text()), text);
        }

        /// redact never panics and rehydrate of the redacted text always
        /// round-trips, even with literal terms in the mix.
        #[test]
        fn never_panics_with_terms(
            s in "[^\\[]{0,200}",
            terms in proptest::collection::vec("[a-zA-Z ]{1,10}", 0..4),
        ) {
            let refs: Vec<&str> = terms.iter().map(|t| t.as_str()).collect();
            let r = redact(&s, &refs);
            prop_assert_eq!(r.rehydrate(r.text()), s);
        }
    }
}
