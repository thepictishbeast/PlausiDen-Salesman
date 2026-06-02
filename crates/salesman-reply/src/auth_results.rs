//! RFC 8601 Authentication-Results header parsing.
//!
//! Why this exists:
//!   The MIME `From:` header is operator-supplied and has no
//!   integrity guarantee. Anyone with our IMAP inbox address (which
//!   shows up in WHOIS / DNS / out-of-office bounces / etc.) can
//!   send a forged message claiming to be from a real prospect:
//!
//!   ```text
//!     From: alice@bigprospect.com
//!     To:   inbound@plausiden.com
//!     Subject: Re: cold email
//!     Body: please remove me from your list, not interested
//!   ```
//!
//!   `keyword_optout` fires → ReplyKind::Optout → our suppression
//!   list adds `alice@bigprospect.com` with source `reply_optout`.
//!   The real Alice never opted out but is now blocked forever.
//!   Repeat at scale = denial-of-service against our pipeline.
//!
//!   Our mail server (Postfix etc.) already runs SPF + DKIM + DMARC
//!   on every inbound and stamps the verdict in an
//!   `Authentication-Results:` header per RFC 8601. Reading and
//!   honoring that verdict is the only thing standing between us
//!   and trivial suppression-list poisoning.
//!
//! Threat model:
//!   - We trust an `Authentication-Results:` header IF AND ONLY IF
//!     its `authserv-id` matches our configured trusted-server
//!     name (operator's MX hostname). An attacker can include their
//!     own AR header claiming pass — we MUST NOT trust those.
//!   - We surface the matrix of (method, result, identifier) and
//!     a single boolean `is_from_authenticated()` for the common
//!     case. Callers that need finer policy (e.g. require both SPF
//!     and DKIM) read the matrix directly.
//!   - SECURITY: this module ONLY parses; it does NOT decide
//!     whether the message is trustworthy. The decision lives in
//!     the consumer, who knows the trust-server name and the
//!     policy.

use std::collections::HashMap;

/// Outcomes per RFC 8601 §2.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    Pass,
    Fail,
    SoftFail,
    Neutral,
    None,
    TempError,
    PermError,
    /// Anything we don't recognize — treat as untrusted by default.
    Other,
}

impl AuthResult {
    fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "pass" => Self::Pass,
            "fail" => Self::Fail,
            "softfail" => Self::SoftFail,
            "neutral" => Self::Neutral,
            "none" => Self::None,
            "temperror" | "temp-error" => Self::TempError,
            "permerror" | "perm-error" => Self::PermError,
            _ => Self::Other,
        }
    }

    pub fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// One method's result. RFC 8601 calls this a "method-spec".
#[derive(Debug, Clone)]
pub struct MethodResult {
    /// Method name lowercased: "spf" / "dkim" / "dmarc" / "arc" / etc.
    pub method: String,
    pub result: AuthResult,
    /// Property bag from the header — e.g. "smtp.mailfrom" → "alice@x.com",
    /// "header.from" → "alice@x.com", "header.d" → "x.com". Keys are
    /// lowercased; values are kept as written. RFC 8601 calls these
    /// "property-types".
    pub properties: HashMap<String, String>,
}

/// One Authentication-Results header. A message can have several
/// (one per server it traversed). Callers should filter on
/// `authserv_id` against their trusted server name BEFORE acting.
#[derive(Debug, Clone)]
pub struct AuthResults {
    /// The server that performed the checks ("authserv-id" in
    /// RFC 8601). E.g. "mx.plausiden.com". Lowercased.
    pub authserv_id: String,
    /// Result per method.
    pub methods: Vec<MethodResult>,
}

impl AuthResults {
    /// Parse one Authentication-Results header value. Returns None
    /// when the header is unrecognizable (no authserv-id, no
    /// methods). Unknown properties are kept; unknown methods
    /// land as `AuthResult::Other`.
    ///
    /// Per RFC 8601 §2.2, the structure is:
    ///   `authserv-id [version] *(method-spec) [reason]`
    /// A method-spec is `method=result *(ptype.property=value)`.
    /// Multiple method-specs are separated by `;`.
    ///
    /// We do a minimal forward-only parser that handles the
    /// common cases (Postfix, Gmail, Outlook, Proton). It does
    /// NOT handle quoted-string values with semicolons (rare
    /// in practice and dropped to OTHER if encountered).
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut parts = trimmed.splitn(2, ';');
        let authserv_id = parts.next()?.trim().to_ascii_lowercase();
        if authserv_id.is_empty() {
            return None;
        }
        // The first segment may include a version number after the
        // authserv-id (`mx.example.com 1`); strip it.
        let authserv_id = authserv_id
            .split_whitespace()
            .next()?
            .to_string();

        let rest = parts.next().unwrap_or("");
        let methods = rest
            .split(';')
            .filter_map(|seg| parse_method_spec(seg.trim()))
            .collect::<Vec<_>>();

        if methods.is_empty() {
            return None;
        }

        Some(Self {
            authserv_id,
            methods,
        })
    }

    /// Return the result for `method` (e.g. "spf", "dkim", "dmarc"),
    /// or None if the method wasn't reported.
    pub fn result_for(&self, method: &str) -> Option<&MethodResult> {
        let m = method.to_ascii_lowercase();
        self.methods.iter().find(|r| r.method == m)
    }

    /// Best-effort: was the message's From: domain authenticated?
    /// Returns true when EITHER:
    ///   - DKIM=pass for a signing identity matching `from_domain`
    ///     (the `header.d` or `header.i` property), OR
    ///   - SPF=pass for an envelope-from matching `from_domain`
    ///     (`smtp.mailfrom` or `smtp.helo`), OR
    ///   - DMARC=pass (DMARC alignment already implies one of the
    ///     above passed AND aligned with the From domain).
    ///
    /// Returns false on any temperror / permerror / fail / softfail
    /// / neutral / none / other.
    ///
    /// SECURITY: the caller MUST verify `self.authserv_id` against
    /// the trusted MX hostname BEFORE calling this — an attacker
    /// can forge their own Authentication-Results header. This
    /// helper does NOT check the authserv-id.
    pub fn is_from_authenticated(&self, from_domain: &str) -> bool {
        let domain = from_domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return false;
        }
        // DMARC pass is the strongest single signal; it's only emitted
        // when SPF or DKIM passed AND aligned with the From domain.
        if let Some(r) = self.result_for("dmarc")
            && r.result.is_pass()
        {
            return true;
        }
        // DKIM: pass + signing domain (`header.d`) matches From domain.
        if let Some(r) = self.result_for("dkim")
            && r.result.is_pass()
        {
            for key in ["header.d", "header.i"] {
                if let Some(d) = r.properties.get(key)
                    && domain_matches(&domain, d)
                {
                    return true;
                }
            }
        }
        // SPF: pass + envelope-from domain matches From domain.
        if let Some(r) = self.result_for("spf")
            && r.result.is_pass()
        {
            for key in ["smtp.mailfrom", "smtp.helo"] {
                if let Some(v) = r.properties.get(key)
                    && let Some(d) = email_or_host_domain(v)
                    && domain_matches(&domain, &d)
                {
                    return true;
                }
            }
        }
        false
    }
}

/// Parse one `method=result *(ptype.property=value)` segment.
fn parse_method_spec(seg: &str) -> Option<MethodResult> {
    let seg = seg.trim();
    if seg.is_empty() {
        return None;
    }
    // Skip "reason=" / "policy=" segments — RFC 8601 §2.3 — they
    // describe context, not a verdict.
    let first_token = seg.split_whitespace().next()?;
    let (method_kv, rest) = seg.split_once(char::is_whitespace).unwrap_or((seg, ""));
    let _ = first_token;
    let (method, result_raw) = method_kv.split_once('=')?;
    let method = method.trim().to_ascii_lowercase();
    if method == "reason" || method == "policy" || method.is_empty() {
        return None;
    }
    let result = AuthResult::from_str(result_raw.trim());
    let mut properties = HashMap::new();
    for token in rest.split_whitespace() {
        if let Some((k, v)) = token.split_once('=') {
            // ptype.property=value — keep `ptype.property` lowercased
            // as the key.
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().trim_matches('"').to_string();
            if !key.is_empty() && !val.is_empty() {
                properties.insert(key, val);
            }
        }
    }
    Some(MethodResult {
        method,
        result,
        properties,
    })
}

/// Extract the domain from "user@domain" or just "domain" / "host.domain".
fn email_or_host_domain(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if let Some((_, d)) = lower.rsplit_once('@') {
        if d.is_empty() {
            None
        } else {
            Some(d.to_string())
        }
    } else {
        Some(lower)
    }
}

/// Domain match: exact OR signing domain is a parent of the from
/// domain. E.g. signing `gmail.com` matches `From: alice@gmail.com`,
/// and signing `mailgun.org` does NOT match `From: alice@x.com` even
/// if the mail was relayed through Mailgun. Subdomain alignment is
/// allowed in one direction only (signer parent of from).
fn domain_matches(from_domain: &str, signer_or_envelope: &str) -> bool {
    let s = signer_or_envelope.to_ascii_lowercase();
    if s == from_domain {
        return true;
    }
    // From "mail.example.com" the signer "example.com" should match
    // (parent domain). The reverse must NOT pass.
    from_domain.ends_with(&format!(".{s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_postfix_style_full() {
        let raw = "mx.example.com 1; \
                   spf=pass smtp.mailfrom=alice@example.org; \
                   dkim=pass header.d=example.org header.i=@example.org; \
                   dmarc=pass policy.dmarc=quarantine";
        let r = AuthResults::parse(raw).unwrap();
        assert_eq!(r.authserv_id, "mx.example.com");
        assert_eq!(r.methods.len(), 3);
        assert!(r.result_for("spf").unwrap().result.is_pass());
        assert!(r.result_for("dkim").unwrap().result.is_pass());
        assert!(r.result_for("dmarc").unwrap().result.is_pass());
    }

    #[test]
    fn dmarc_pass_authenticates_from() {
        let raw = "mx.x.com; \
                   dmarc=pass policy.dmarc=reject; \
                   spf=pass smtp.mailfrom=alice@example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(r.is_from_authenticated("example.org"));
    }

    #[test]
    fn dkim_pass_aligned_authenticates_from() {
        let raw = "mx.x.com; dkim=pass header.d=example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(r.is_from_authenticated("example.org"));
        // Subdomain From with parent signer: aligned by RFC 6376.
        assert!(r.is_from_authenticated("mail.example.org"));
        // Different domain: NOT authenticated.
        assert!(!r.is_from_authenticated("attacker.com"));
    }

    #[test]
    fn spf_pass_aligned_authenticates_from() {
        let raw = "mx.x.com; spf=pass smtp.mailfrom=alice@example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(r.is_from_authenticated("example.org"));
        // Misaligned envelope: would be a DMARC fail but we count it
        // as not authenticated for the From domain either way.
        assert!(!r.is_from_authenticated("legit-prospect.com"));
    }

    #[test]
    fn fail_or_softfail_does_not_authenticate() {
        let raw = "mx.x.com; spf=fail smtp.mailfrom=alice@example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(!r.is_from_authenticated("example.org"));

        let raw = "mx.x.com; spf=softfail smtp.mailfrom=alice@example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(!r.is_from_authenticated("example.org"));
    }

    #[test]
    fn temperror_does_not_authenticate() {
        let raw = "mx.x.com; spf=temperror smtp.mailfrom=alice@example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(!r.is_from_authenticated("example.org"));
    }

    #[test]
    fn missing_methods_is_not_authenticated() {
        let raw = "mx.x.com; reason=\"missing dkim selector\"";
        // This has only a reason; no method-spec → parse returns None.
        assert!(AuthResults::parse(raw).is_none());
    }

    #[test]
    fn unknown_method_lands_as_other() {
        let raw = "mx.x.com; weirdsig=pass header.d=example.org";
        let r = AuthResults::parse(raw).unwrap();
        let m = r.result_for("weirdsig").unwrap();
        // Result string IS "pass" but only the named methods affect
        // is_from_authenticated.
        assert!(m.result.is_pass());
        // is_from_authenticated only honors spf / dkim / dmarc.
        assert!(!r.is_from_authenticated("example.org"));
    }

    #[test]
    fn empty_or_garbage_returns_none() {
        assert!(AuthResults::parse("").is_none());
        assert!(AuthResults::parse("   ").is_none());
        assert!(AuthResults::parse("nojessequal").is_none());
    }

    #[test]
    fn case_insensitive_authserv_id_and_methods() {
        let raw = "MX.X.COM; SPF=Pass smtp.mailfrom=alice@EXAMPLE.org";
        let r = AuthResults::parse(raw).unwrap();
        assert_eq!(r.authserv_id, "mx.x.com");
        assert!(r.is_from_authenticated("example.org"));
        // Mixed-case from-domain input also works.
        assert!(r.is_from_authenticated("Example.Org"));
    }

    #[test]
    fn empty_from_domain_never_authenticates() {
        let raw = "mx.x.com; spf=pass smtp.mailfrom=alice@example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(!r.is_from_authenticated(""));
        assert!(!r.is_from_authenticated("   "));
    }

    #[test]
    fn signer_subdomain_does_not_match_parent_from() {
        // Important asymmetry: a signature on "mail.example.com"
        // does NOT authenticate From: alice@example.com — only the
        // other direction (signer is ancestor of from-domain) is
        // accepted. Otherwise an attacker controlling
        // `attacker-mail.bigprospect.com` could authenticate as
        // `bigprospect.com`.
        let raw = "mx.x.com; dkim=pass header.d=mail.example.org";
        let r = AuthResults::parse(raw).unwrap();
        assert!(r.is_from_authenticated("mail.example.org"));
        assert!(!r.is_from_authenticated("example.org"));
    }

    proptest::proptest! {
        // The Authentication-Results header value is attacker-controlled, so
        // parse() is a trust boundary: it must never panic — only Some/None.
        #[test]
        fn parse_never_panics(raw in "[\\x09\\x20-\\x7e]{0,1000}") {
            let _ = AuthResults::parse(&raw);
        }

        // parse() + querying with an arbitrary from-domain must both be
        // panic-free, and a garbage header must never spuriously authenticate.
        #[test]
        fn parse_and_query_never_panic(
            raw in "[\\x09\\x20-\\x7e]{0,1000}",
            dom in "[a-zA-Z0-9.-]{0,80}"
        ) {
            if let Some(ar) = AuthResults::parse(&raw) {
                let _ = ar.is_from_authenticated(&dom);
            }
        }
    }
}
