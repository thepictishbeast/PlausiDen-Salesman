//! SMTP failure classification.
//!
//! Lettre's error messages embed the SMTP response (e.g. "permanent
//! error: 550 5.1.1 user unknown"). We parse out the basic code +
//! enhanced status code and decide whether the failure is a HARD
//! bounce that should auto-suppress the recipient, a SOFT bounce that
//! should retry, or something else (config / network).
//!
//! BUG ASSUMPTION: SMTP servers are wildly inconsistent in how they
//! format DSNs. We err on the SIDE OF NOT auto-suppressing — false
//! positives (we keep mailing a dead address) hurt deliverability,
//! but false-negatives (we suppress a live address on a transient
//! glitch) hurt the BUSINESS. The owner can always manually add a
//! suppression. The reverse — manually un-suppressing because we
//! over-eager-blacklisted a real prospect — costs more.
//!
//! References:
//! - RFC 3463 enhanced status codes (X.Y.Z)
//! - RFC 5321 basic SMTP reply codes (5xx permanent, 4xx transient)

use std::fmt;

/// A parsed classification of an SMTP send failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpFailure {
    /// 5xx + a subcode that is unambiguously about the RECIPIENT
    /// being undeliverable. Caller should mark the recipient
    /// suppressed with source="bounce".
    HardBounce {
        /// Basic SMTP reply code (5xx).
        basic: u16,
        /// RFC 3463 enhanced status code (X.Y.Z), if present.
        enhanced: Option<String>,
        /// The raw failure message.
        message: String,
    },
    /// 5xx that is NOT about the recipient (rate limit on sender,
    /// content rejection, policy block). Do NOT auto-suppress; the
    /// owner needs to investigate.
    PermanentOther {
        /// Basic SMTP reply code (5xx).
        basic: u16,
        /// RFC 3463 enhanced status code (X.Y.Z), if present.
        enhanced: Option<String>,
        /// The raw failure message.
        message: String,
    },
    /// 4xx — try later. Caller should leave the touch in
    /// `awaiting_send` for a retry.
    Transient {
        /// Basic SMTP reply code (4xx).
        basic: u16,
        /// The raw failure message.
        message: String,
    },
    /// Couldn't extract a code at all. Treat like a network/transport
    /// failure — log + retry, but don't suppress.
    Unstructured {
        /// The raw failure message.
        message: String,
    },
}

impl fmt::Display for SmtpFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HardBounce {
                basic,
                enhanced,
                message,
            } => write!(
                f,
                "hard-bounce {basic}{} {message}",
                enhanced
                    .as_deref()
                    .map(|e| format!(" {e}"))
                    .unwrap_or_default()
            ),
            Self::PermanentOther {
                basic,
                enhanced,
                message,
            } => write!(
                f,
                "permanent {basic}{} {message}",
                enhanced
                    .as_deref()
                    .map(|e| format!(" {e}"))
                    .unwrap_or_default()
            ),
            Self::Transient { basic, message } => write!(f, "transient {basic} {message}"),
            Self::Unstructured { message } => write!(f, "unstructured {message}"),
        }
    }
}

impl SmtpFailure {
    /// True if the failure justifies adding the recipient to the
    /// global suppression list.
    pub fn should_auto_suppress(&self) -> bool {
        matches!(self, Self::HardBounce { .. })
    }

    /// Suggest a `source` tag for the suppression row.
    pub fn suppression_source(&self) -> &'static str {
        "bounce"
    }
}

/// Recipient-fatal enhanced status codes — these and only these
/// trigger auto-suppression. Conservative on purpose.
const HARD_BOUNCE_ENHANCED: &[&str] = &[
    "5.1.1",  // bad destination mailbox
    "5.1.2",  // bad destination system
    "5.1.3",  // bad destination mailbox syntax
    "5.1.6",  // mailbox has moved (no forwarding)
    "5.1.10", // recipient address has null MX
    "5.4.1",  // no answer from host
    "5.4.4",  // unable to route
    "5.7.27", // sender does not match SPF — recipient-side rejection of OUR mail; counted as hard
];

/// Subcodes that are 5xx but explicitly NOT about the recipient.
/// Matches against a prefix so 5.7.x policy rejections also fall here.
const PERMANENT_NON_RECIPIENT_PREFIXES: &[&str] = &[
    "5.0.0",  // generic
    "5.2.",   // mailbox-status (full, unavailable) — could go either way; treat as non-fatal
    "5.3.",   // mail-system status
    "5.5.",   // protocol
    "5.6.",   // media / content
    "5.7.0",  // delivery not authorized
    "5.7.1",  // delivery not authorized (often spam policy)
    "5.7.7",  // message integrity
    "5.7.26", // multiple auth checks failed (Gmail)
];

/// Classify an SMTP error / DSN string into an [`SmtpFailure`] from its
/// enhanced status code (e.g. `5.1.1` hard bounce vs a `4.x` transient
/// failure) plus message text, so the caller can decide suppress-vs-retry.
pub fn classify(error_text: &str) -> SmtpFailure {
    let basic = find_basic_code(error_text);
    let enhanced = find_enhanced_code(error_text);

    match basic {
        Some(b) if (500..600).contains(&b) => match &enhanced {
            Some(en) if HARD_BOUNCE_ENHANCED.iter().any(|h| en == h) => SmtpFailure::HardBounce {
                basic: b,
                enhanced: enhanced.clone(),
                message: error_text.to_string(),
            },
            Some(en)
                if PERMANENT_NON_RECIPIENT_PREFIXES
                    .iter()
                    .any(|p| en.starts_with(p)) =>
            {
                SmtpFailure::PermanentOther {
                    basic: b,
                    enhanced: enhanced.clone(),
                    message: error_text.to_string(),
                }
            }
            _ => {
                // 5xx with no recognized subcode — be conservative.
                // 550 alone is sometimes "user unknown", sometimes
                // "policy reject". Don't auto-suppress on ambiguity.
                SmtpFailure::PermanentOther {
                    basic: b,
                    enhanced: enhanced.clone(),
                    message: error_text.to_string(),
                }
            }
        },
        Some(b) if (400..500).contains(&b) => SmtpFailure::Transient {
            basic: b,
            message: error_text.to_string(),
        },
        _ => SmtpFailure::Unstructured {
            message: error_text.to_string(),
        },
    }
}

fn find_basic_code(s: &str) -> Option<u16> {
    // Look for the first standalone 3-digit number in 4xx/5xx range
    // followed by a space or '-' (SMTP continuation marker).
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        let c0 = bytes[i];
        let c1 = bytes[i + 1];
        let c2 = bytes[i + 2];
        let next = bytes[i + 3];
        if (c0 == b'4' || c0 == b'5')
            && (c1.is_ascii_digit() && c2.is_ascii_digit())
            && (next == b' ' || next == b'-')
            && (i == 0 || !bytes[i - 1].is_ascii_digit())
        {
            let code = (c0 - b'0') as u16 * 100 + (c1 - b'0') as u16 * 10 + (c2 - b'0') as u16;
            return Some(code);
        }
        i += 1;
    }
    None
}

fn find_enhanced_code(s: &str) -> Option<String> {
    // Looks for X.Y.Z where X ∈ {4,5}, Y ∈ 0..=9 (one or two digits),
    // Z ∈ 0..=99. We accept multi-digit minor + minor2.
    let bytes = s.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if (c == b'4' || c == b'5')
            && i + 4 < bytes.len()
            && bytes[i + 1] == b'.'
            && bytes[i + 2].is_ascii_digit()
            && (i == 0 || !bytes[i - 1].is_ascii_digit())
        {
            // Walk forward grabbing digits + dots.
            let mut end = i + 1;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
                end += 1;
            }
            let candidate = &s[i..end];
            // Sanity: must have exactly two dots.
            if candidate.matches('.').count() == 2 {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_user_unknown_as_hard_bounce() {
        let f = classify("permanent error: 550 5.1.1 <foo@bar.com> user unknown");
        match f {
            SmtpFailure::HardBounce {
                basic, enhanced, ..
            } => {
                assert_eq!(basic, 550);
                assert_eq!(enhanced.as_deref(), Some("5.1.1"));
            }
            other => panic!("expected HardBounce, got {other:?}"),
        }
    }

    #[test]
    fn classifies_no_such_domain_as_hard_bounce() {
        let f = classify("550 5.1.2 host nobody.example does not exist");
        assert!(matches!(f, SmtpFailure::HardBounce { .. }));
        assert!(f.should_auto_suppress());
    }

    #[test]
    fn classifies_routing_failure_as_hard_bounce() {
        let f = classify("550 5.4.4 [foo.bar.com] No route to host");
        assert!(matches!(f, SmtpFailure::HardBounce { .. }));
    }

    #[test]
    fn classifies_spam_policy_as_permanent_other() {
        // 5.7.1 is policy — recipient mailbox exists, sender rejected.
        // Should NOT auto-suppress (we'd lose the prospect entirely
        // when really we just got flagged).
        let f = classify("550 5.7.1 message blocked by policy");
        match f {
            SmtpFailure::PermanentOther { basic, .. } => assert_eq!(basic, 550),
            other => panic!("expected PermanentOther, got {other:?}"),
        }
        assert!(!f.should_auto_suppress());
    }

    #[test]
    fn classifies_gmail_spf_as_permanent_other() {
        // 5.7.26 ≠ recipient-fatal; sender-side issue.
        let f = classify(
            "550-5.7.26 The MAIL FROM domain failed multiple authentication \
             checks 550 5.7.26 [...]",
        );
        assert!(!f.should_auto_suppress());
        assert!(matches!(f, SmtpFailure::PermanentOther { .. }));
    }

    #[test]
    fn classifies_4xx_as_transient() {
        let f = classify("transient error: 421 4.7.0 try again later");
        match f {
            SmtpFailure::Transient { basic, .. } => assert_eq!(basic, 421),
            other => panic!("expected Transient, got {other:?}"),
        }
        assert!(!f.should_auto_suppress());
    }

    #[test]
    fn classifies_no_code_as_unstructured() {
        let f = classify("connection reset by peer");
        assert!(matches!(f, SmtpFailure::Unstructured { .. }));
        assert!(!f.should_auto_suppress());
    }

    #[test]
    fn ambiguous_5xx_without_enhanced_code_is_permanent_other() {
        // Belt-and-suspenders: if no enhanced code is present, we
        // do NOT auto-suppress on a bare 550.
        let f = classify("550 service unavailable");
        assert!(!f.should_auto_suppress());
        assert!(matches!(f, SmtpFailure::PermanentOther { .. }));
    }

    #[test]
    fn handles_continuation_dash() {
        // Multi-line SMTP responses use "550-foo" / "550 foo" — the
        // basic-code parser needs to accept both space and dash.
        let f = classify("550-5.1.1 user does not exist 550 5.1.1 here");
        assert!(matches!(f, SmtpFailure::HardBounce { .. }));
    }

    #[test]
    fn ignores_digits_inside_words() {
        // Don't false-match on something like "host10500" or
        // "550000th customer". The basic code must be word-boundary-
        // adjacent on the leading side.
        let f = classify("connection lost after host420in.example");
        assert!(matches!(f, SmtpFailure::Unstructured { .. }));
    }

    #[test]
    fn suppression_source_is_bounce() {
        let f = classify("550 5.1.1 user unknown");
        assert_eq!(f.suppression_source(), "bounce");
    }

    // ------------------------------------------------------------------
    // proptest — fuzz classify() against arbitrary input.
    // Properties:
    //   1. classify() NEVER panics.
    //   2. The Display impl NEVER panics.
    //   3. should_auto_suppress() iff the variant is HardBounce.
    //   4. The classifier is order-independent for trivial perturbations
    //      that don't change the SMTP-code substring.
    // ------------------------------------------------------------------
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2048))]

        #[test]
        fn never_panics_on_arbitrary_text(s in ".{0,512}") {
            let f = classify(&s);
            // Display must also be safe.
            let _ = format!("{f}");
            // suppression_source is currently constant, but exercise it.
            let _ = f.suppression_source();
        }

        #[test]
        fn auto_suppress_iff_hard_bounce(s in ".{0,256}") {
            let f = classify(&s);
            // matches! with `{ .. }` confuses proptest's macro
            // format-string parser, so destructure into a bool first.
            let is_hard = is_hard_bounce(&f);
            prop_assert_eq!(f.should_auto_suppress(), is_hard);
        }

        #[test]
        fn whitespace_padding_preserves_classification(
            pad_left in "[ \\t\\r\\n]{0,8}",
            pad_right in "[ \\t\\r\\n]{0,8}",
            tmpl in prop::sample::select(vec![
                "550 5.1.1 user unknown",
                "550 5.7.1 policy reject",
                "421 4.7.0 transient",
                "554 service unavailable",
                "connection reset",
            ])
        ) {
            let s = format!("{pad_left}{tmpl}{pad_right}");
            let a = classify(tmpl);
            let b = classify(&s);
            prop_assert_eq!(
                std::mem::discriminant(&a),
                std::mem::discriminant(&b),
            );
        }

        #[test]
        fn no_3_digit_substring_false_match(
            s in "[a-zA-Z !@#$%^&*(),.?:;'\"]{1,200}"
        ) {
            let f = classify(&s);
            let is_unstructured = is_unstructured(&f);
            prop_assert!(is_unstructured);
        }
    }

    // Small helpers because `matches!(_, Variant { .. })` inside
    // proptest's `prop_assert!` macro trips the format-string parser
    // (it sees `{ ..` as the start of a format placeholder).
    fn is_hard_bounce(f: &SmtpFailure) -> bool {
        matches!(f, SmtpFailure::HardBounce { .. })
    }
    fn is_unstructured(f: &SmtpFailure) -> bool {
        matches!(f, SmtpFailure::Unstructured { .. })
    }
}
