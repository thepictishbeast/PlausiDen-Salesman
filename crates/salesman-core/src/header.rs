//! Header-value sanitization — defense against email header / CRLF
//! injection (CWE-93).
//!
//! Prospect-derived strings (names, business names, addresses) and
//! generated content flow into single-line email headers such as
//! `Subject:`. A raw carriage return or line feed embedded in such a
//! value would terminate the header and let an attacker inject
//! additional headers (e.g. `Bcc:`) or split the message into an
//! arbitrary body. Because the prospect data we carry is scraped /
//! imported (untrusted), every value bound for a header MUST pass
//! through [`sanitize_header_value`] first.
//!
//! This is a *defense-in-depth* boundary: the SMTP layer (`lettre`)
//! also encodes headers, but the values we mint and persist — owner
//! audit-notification subjects, for instance — are our responsibility
//! to keep single-line and injection-free at the point we build them.

/// Make `value` safe to place in a single-line email header.
///
/// Every ASCII control character (carriage return, line feed, NUL, tab,
/// and the rest of C0/DEL) and the Unicode line/paragraph separators
/// (`U+2028`, `U+2029`) is treated as folding whitespace: each run of
/// such characters — and of ordinary spaces — collapses to a single
/// space, and the result is trimmed. The returned string therefore
/// contains no line breaks and is safe to concatenate into one header
/// line.
///
/// Non-control, non-separator characters (including ordinary Unicode
/// letters and punctuation) are preserved verbatim — this is a
/// line-break/control filter, not a charset restriction; `lettre`
/// handles RFC 2047 encoding and line folding of the cleaned value.
///
/// ```
/// use salesman_core::sanitize_header_value;
/// assert_eq!(sanitize_header_value("Acme Inc"), "Acme Inc");
/// // A CRLF-injection attempt is flattened to harmless literal text.
/// assert_eq!(
///     sanitize_header_value("Acme\r\nBcc: evil@example.com"),
///     "Acme Bcc: evil@example.com"
/// );
/// assert_eq!(sanitize_header_value("\r\n\t  "), "");
/// ```
pub fn sanitize_header_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for c in value.chars() {
        // Every control char (CR/LF/NUL/TAB/other C0/DEL) and every kind
        // of whitespace — ordinary space, NBSP, and the Unicode line and
        // paragraph separators U+2028/U+2029 (`is_whitespace`) — folds to
        // a single space; collapse runs so the result has no line breaks
        // and no leading/trailing/double whitespace.
        let folds = c.is_control() || c.is_whitespace();
        if folds {
            // Defer emitting the space so leading runs and trailing
            // runs disappear via the final trim, and internal runs
            // collapse to exactly one space.
            if !out.is_empty() {
                pending_space = true;
            }
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_clean_values_through() {
        assert_eq!(sanitize_header_value("Acme Inc"), "Acme Inc");
        assert_eq!(sanitize_header_value("José's Café & Co."), "José's Café & Co.");
        assert_eq!(sanitize_header_value(""), "");
    }

    #[test]
    fn strips_crlf_injection() {
        // The canonical attack: a header injected after a CRLF.
        let got = sanitize_header_value("Acme\r\nBcc: attacker@evil.example");
        assert!(!got.contains('\r'), "{got:?}");
        assert!(!got.contains('\n'), "{got:?}");
        assert_eq!(got, "Acme Bcc: attacker@evil.example");
    }

    #[test]
    fn strips_lone_cr_and_lf_and_nul_and_tab() {
        assert_eq!(sanitize_header_value("a\rb"), "a b");
        assert_eq!(sanitize_header_value("a\nb"), "a b");
        assert_eq!(sanitize_header_value("a\0b"), "a b");
        assert_eq!(sanitize_header_value("a\tb"), "a b");
    }

    #[test]
    fn strips_unicode_line_separators() {
        assert_eq!(sanitize_header_value("a\u{2028}b"), "a b");
        assert_eq!(sanitize_header_value("a\u{2029}b"), "a b");
    }

    #[test]
    fn collapses_runs_and_trims() {
        assert_eq!(sanitize_header_value("  Acme \r\n\r\n  Inc  "), "Acme Inc");
        assert_eq!(sanitize_header_value("\r\n\t  "), "");
        assert_eq!(sanitize_header_value("a\r\n\r\n\r\nb"), "a b");
    }

    proptest::proptest! {
        /// For ANY input — including embedded control characters — the
        /// output must never contain a line break (the injection
        /// vector) and the function must never panic.
        #[test]
        fn output_is_always_single_line(chars in proptest::collection::vec(proptest::char::any(), 0..64)) {
            let input: String = chars.into_iter().collect();
            let got = sanitize_header_value(&input);
            proptest::prop_assert!(!got.contains('\r'), "{got:?}");
            proptest::prop_assert!(!got.contains('\n'), "{got:?}");
            proptest::prop_assert!(!got.contains('\0'), "{got:?}");
            proptest::prop_assert!(!got.contains('\u{2028}'), "{got:?}");
            proptest::prop_assert!(!got.contains('\u{2029}'), "{got:?}");
            // No leading/trailing whitespace, and no double spaces.
            proptest::prop_assert_eq!(got.trim(), got.as_str());
            proptest::prop_assert!(!got.contains("  "), "{got:?}");
        }
    }
}
