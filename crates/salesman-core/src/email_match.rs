//! Email normalization for suppression matching.
//!
//! `is_suppressed("john@example.com")` should return true if the
//! suppression list contains:
//!   - exact "john@example.com",
//!   - "John@Example.com" (case-folding — local-part is case-
//!     sensitive per RFC 5321 but every major provider treats it
//!     case-insensitively in practice),
//!   - "john+anything@example.com" (plus-addressing — RFC 5233,
//!     honored by Gmail / Fastmail / Outlook / Proton / Yahoo
//!     and most enterprise mail servers),
//!   - For Gmail / Googlemail specifically: "j.o.h.n@gmail.com"
//!     (Gmail ignores dots in the local-part — `john@gmail.com`
//!     and `j.o.h.n@gmail.com` deliver to the same mailbox).
//!
//! Without this, an opt-out from `john+sales@gmail.com` would not
//! protect `john@gmail.com` and the auto-drafter would happily
//! email the same person again.
//!
//! SECURITY: this is a *broadening* of the suppression check.
//! False positives (someone gets blocked who shouldn't) are
//! preferable to false negatives (someone gets emailed who
//! opted out). The helpers below intentionally err generous.
//!
//! BUG ASSUMPTION: this is purely best-effort for common
//! providers. We do NOT try to canonicalize across exotic
//! providers' quirks (e.g. iCloud's hide-my-email aliases, or
//! corporate aliases that route both `firstname.lastname@` and
//! `flastname@` to the same person — these vary per-deployment
//! and there's no reliable way to know without DNS / API
//! lookups outside this module's scope).

/// Compute the canonical form of an email for suppression
/// storage and matching. Lowercases everything, strips the
/// `+suffix` from the local-part (universal plus-addressing),
/// and removes dots from the local-part for Gmail addresses.
///
/// Returns the input unchanged if the address has no `@`.
/// Empty input returns empty string.
pub fn normalize_email_for_match(email: &str) -> String {
    let trimmed = email.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_lowercase();
    let (local, domain) = match lower.rsplit_once('@') {
        Some(x) => x,
        None => return lower,
    };
    if local.is_empty() || domain.is_empty() {
        return lower;
    }
    // Strip plus-suffix universally (RFC 5233; honored by Gmail,
    // Fastmail, Outlook, Proton, Yahoo, most enterprise servers).
    let local_no_plus = local.split_once('+').map_or(local, |(a, _)| a);
    // Gmail-specific: dots in local-part are ignored.
    let local_canonical = if matches!(domain, "gmail.com" | "googlemail.com") {
        let no_dots: String = local_no_plus.chars().filter(|c| *c != '.').collect();
        // gmail.com is the canonical form; both gmail.com and
        // googlemail.com route to the same mailbox.
        return format!("{no_dots}@gmail.com");
    } else {
        local_no_plus.to_string()
    };
    format!("{local_canonical}@{domain}")
}

/// All candidate forms an inbound email could match against the
/// suppression list. Used by `is_suppressed` to broaden the lookup
/// without requiring a SQL function.
///
/// Always includes:
/// 1. The verbatim input (handles legacy rows stored as-typed).
/// 2. The lowercased input.
/// 3. The fully-normalized form (`normalize_email_for_match`).
///
/// Duplicates are removed; order is preserved (verbatim first so
/// exact matches win on the cheapest comparison).
///
/// SECURITY: callers MUST treat presence of ANY candidate in the
/// suppression list as a hit — never combine these for a partial
/// match against a different field, only equality on the same
/// `target` column.
pub fn email_match_candidates(email: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(3);
    let verbatim = email.trim().to_string();
    if !verbatim.is_empty() {
        out.push(verbatim.clone());
    }
    let lower = verbatim.to_lowercase();
    if !lower.is_empty() && !out.contains(&lower) {
        out.push(lower);
    }
    let canonical = normalize_email_for_match(email);
    if !canonical.is_empty() && !out.contains(&canonical) {
        out.push(canonical);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases() {
        assert_eq!(
            normalize_email_for_match("John.Smith@Acme.COM"),
            "john.smith@acme.com"
        );
    }

    #[test]
    fn normalize_strips_plus_suffix_universally() {
        assert_eq!(
            normalize_email_for_match("alice+work@example.com"),
            "alice@example.com"
        );
        assert_eq!(
            normalize_email_for_match("ops+ticket-1234@enterprise.io"),
            "ops@enterprise.io"
        );
    }

    #[test]
    fn normalize_strips_dots_for_gmail_only() {
        assert_eq!(
            normalize_email_for_match("j.o.h.n@gmail.com"),
            "john@gmail.com"
        );
        assert_eq!(
            normalize_email_for_match("a.l.i.c.e@googlemail.com"),
            "alice@gmail.com"
        );
        // Non-Gmail: dots preserved (firstname.lastname@ is a
        // distinct mailbox at most providers).
        assert_eq!(
            normalize_email_for_match("john.smith@acme.com"),
            "john.smith@acme.com"
        );
    }

    #[test]
    fn normalize_combines_plus_and_dots_for_gmail() {
        assert_eq!(
            normalize_email_for_match("J.O.H.N+work@gmail.com"),
            "john@gmail.com"
        );
        assert_eq!(
            normalize_email_for_match("J.O.H.N+work@GoogleMail.com"),
            "john@gmail.com"
        );
    }

    #[test]
    fn normalize_handles_no_at_sign() {
        assert_eq!(normalize_email_for_match("noatsymbol"), "noatsymbol");
    }

    #[test]
    fn normalize_handles_empty_and_whitespace() {
        assert_eq!(normalize_email_for_match(""), "");
        assert_eq!(normalize_email_for_match("   "), "");
        assert_eq!(normalize_email_for_match("  john@acme.com  "), "john@acme.com");
    }

    #[test]
    fn normalize_handles_empty_local_or_domain() {
        // Garbage in — preserve the lowercased verbatim form so
        // the caller sees something to log, but don't crash.
        assert_eq!(normalize_email_for_match("@acme.com"), "@acme.com");
        assert_eq!(normalize_email_for_match("john@"), "john@");
    }

    #[test]
    fn candidates_dedupe_simple() {
        // Already lowercase + no plus + non-gmail: only one candidate.
        let c = email_match_candidates("john@acme.com");
        assert_eq!(c, vec!["john@acme.com".to_string()]);
    }

    #[test]
    fn candidates_include_verbatim_lower_and_canonical() {
        let c = email_match_candidates("John+Sales@Gmail.com");
        // Verbatim, lowercased, canonical — three distinct values.
        assert!(c.contains(&"John+Sales@Gmail.com".to_string()));
        assert!(c.contains(&"john+sales@gmail.com".to_string()));
        assert!(c.contains(&"john@gmail.com".to_string()));
    }

    #[test]
    fn candidates_preserve_verbatim_first() {
        let c = email_match_candidates("John+Sales@Gmail.com");
        assert_eq!(c.first().unwrap(), "John+Sales@Gmail.com");
    }

    #[test]
    fn candidates_skip_empty_input() {
        assert!(email_match_candidates("").is_empty());
        assert!(email_match_candidates("   ").is_empty());
    }

    #[test]
    fn round_trip_idempotent() {
        // Normalizing a normalized address must not change it —
        // important for add_suppression: if we store the canonical
        // form once, future inserts of equivalent addresses
        // converge on the same row.
        let inputs = [
            "john@acme.com",
            "alice+x@example.com",
            "j.o.h.n@gmail.com",
            "John.Smith@Acme.COM",
        ];
        for input in inputs {
            let once = normalize_email_for_match(input);
            let twice = normalize_email_for_match(&once);
            assert_eq!(once, twice, "normalize must be idempotent for {input}");
        }
    }
}
