use chrono::{DateTime, Utc};
use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Normalised representation of an inbound message ready to be
/// persisted as a `replies` row. Keeps original headers so downstream
/// can correlate by Message-Id / In-Reply-To / References.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedReply {
    pub from_address: String,
    pub from_name: Option<String>,
    pub subject: Option<String>,
    pub body_plain: String,
    pub body_html: Option<String>,
    pub received_at: DateTime<Utc>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub raw_headers: BTreeMap<String, String>,
    /// Raw `Authentication-Results:` header values, in the order
    /// they appeared in the message. A message can have several (one
    /// per server it traversed). Empty when absent.
    ///
    /// SECURITY: caller MUST verify the authserv-id of each result
    /// against the trusted MX hostname before honoring it — an
    /// attacker can include their own AR header. See
    /// `is_from_authenticated`.
    #[serde(default)]
    pub authentication_results_raw: Vec<String>,
}

impl ParsedReply {
    /// Domain part of `from_address`, lowercased. Empty string when
    /// from_address has no `@`. Convenience for the
    /// `is_from_authenticated` consumer.
    pub fn from_domain(&self) -> String {
        self.from_address
            .rsplit_once('@')
            .map(|(_, d)| d.trim().to_ascii_lowercase())
            .unwrap_or_default()
    }

    /// Best-effort: did our trusted mail server stamp an
    /// `Authentication-Results:` header that says the From: domain
    /// passed SPF / DKIM / DMARC alignment?
    ///
    /// `trusted_authserv_id` MUST be the operator's own MX hostname
    /// (lowercased). We IGNORE Authentication-Results headers from
    /// any other authserv-id — they're either from upstream relays
    /// (we can't verify those) OR forgeries.
    ///
    /// Returns:
    ///   - `Some(true)` when at least one trusted AR header passes
    ///     the domain alignment check (DMARC pass, OR aligned DKIM
    ///     pass, OR aligned SPF pass).
    ///   - `Some(false)` when at least one trusted AR header was
    ///     present but did NOT pass alignment.
    ///   - `None` when NO trusted AR header was present at all.
    ///     The caller's policy should distinguish "untrusted because
    ///     authentication failed" from "untrusted because the
    ///     mail server wasn't configured" — both are unsafe to
    ///     act on for suppression, but the operator should be
    ///     warned only once for a missing-stamp deployment.
    pub fn is_from_authenticated(&self, trusted_authserv_id: &str) -> Option<bool> {
        let trusted = trusted_authserv_id.trim().to_ascii_lowercase();
        if trusted.is_empty() {
            return None;
        }
        let from_domain = self.from_domain();
        let mut saw_trusted_header = false;
        for raw in &self.authentication_results_raw {
            let Some(parsed) = crate::AuthResults::parse(raw) else {
                continue;
            };
            // Only honor headers from our own MX. An attacker can
            // include their own AR header claiming pass.
            if parsed.authserv_id != trusted {
                continue;
            }
            saw_trusted_header = true;
            if parsed.is_from_authenticated(&from_domain) {
                return Some(true);
            }
        }
        if saw_trusted_header { Some(false) } else { None }
    }
}

impl ParsedReply {
    /// Parse raw RFC 5322 bytes (typically from IMAP FETCH BODY[]) into
    /// a `ParsedReply`. Returns `None` if the message has no usable
    /// From header or no extractable body.
    pub fn from_rfc5322(bytes: &[u8]) -> Option<Self> {
        let msg = MessageParser::default().parse(bytes)?;

        let (from_address, from_name) =
            msg.from()
                .and_then(|addr_list| addr_list.first())
                .map(|a| {
                    (
                        a.address().unwrap_or_default().to_string(),
                        a.name().map(|s| s.to_string()),
                    )
                })?;
        if from_address.is_empty() {
            return None;
        }

        let subject = msg.subject().map(|s| s.to_string());
        let body_plain = msg.body_text(0).map(|c| c.to_string()).unwrap_or_default();
        let body_html = msg.body_html(0).map(|c| c.to_string());

        let received_at = msg
            .date()
            .and_then(|d| chrono::DateTime::<Utc>::from_timestamp(d.to_timestamp(), 0))
            .unwrap_or_else(Utc::now);

        let message_id = msg.message_id().map(|s| s.to_string());
        let in_reply_to = msg.in_reply_to().as_text().map(|s| s.to_string());
        let references = msg
            .references()
            .as_text_list()
            .map(|v| v.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        let mut raw_headers = BTreeMap::new();
        let mut authentication_results_raw: Vec<String> = Vec::new();
        for header in msg.headers() {
            let name = header.name();
            let value = header.value().as_text().map(|s| s.to_string());
            if let Some(v) = value {
                // Authentication-Results may appear multiple times
                // (one per server). raw_headers collapses to one
                // entry; we keep the full list separately so the
                // consumer can verify each independently.
                if name.eq_ignore_ascii_case("Authentication-Results") {
                    authentication_results_raw.push(v.clone());
                }
                raw_headers.insert(name.to_string(), v);
            }
        }

        Some(Self {
            from_address,
            from_name,
            subject,
            body_plain,
            body_html,
            received_at,
            message_id,
            in_reply_to,
            references,
            raw_headers,
            authentication_results_raw,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_message() {
        let raw = b"From: Alice <alice@example.com>\r\n\
                    To: bob@example.com\r\n\
                    Subject: hello\r\n\
                    Date: Mon, 01 Jan 2024 12:00:00 +0000\r\n\
                    Message-ID: <abc@x>\r\n\
                    In-Reply-To: <orig@x>\r\n\
                    \r\n\
                    Hi Bob, this is the body.\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        assert_eq!(p.from_address, "alice@example.com");
        assert_eq!(p.from_name.as_deref(), Some("Alice"));
        assert_eq!(p.subject.as_deref(), Some("hello"));
        assert!(p.body_plain.contains("this is the body"));
        assert_eq!(p.message_id.as_deref(), Some("abc@x"));
        assert_eq!(p.in_reply_to.as_deref(), Some("orig@x"));
    }

    #[test]
    fn returns_none_on_no_from() {
        let raw = b"To: bob@example.com\r\nSubject: nope\r\n\r\nbody\r\n";
        assert!(ParsedReply::from_rfc5322(raw).is_none());
    }

    #[test]
    fn from_domain_extracts_lowercased_domain() {
        let raw = b"From: Alice <Alice@Example.COM>\r\n\
                    To: bob@example.com\r\n\
                    Subject: x\r\n\
                    \r\nbody\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        assert_eq!(p.from_domain(), "example.com");
    }

    #[test]
    fn captures_authentication_results_header() {
        let raw = b"From: alice@example.com\r\n\
                    Authentication-Results: mx.plausiden.com; spf=pass smtp.mailfrom=alice@example.com; dkim=pass header.d=example.com\r\n\
                    To: bob@plausiden.com\r\n\
                    Subject: x\r\n\
                    \r\nbody\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        assert_eq!(p.authentication_results_raw.len(), 1);
        assert!(p.authentication_results_raw[0].contains("dkim=pass"));
    }

    #[test]
    fn is_from_authenticated_honors_trusted_authserv_id() {
        let raw = b"From: alice@example.com\r\n\
                    Authentication-Results: mx.plausiden.com; spf=pass smtp.mailfrom=alice@example.com; dkim=pass header.d=example.com; dmarc=pass\r\n\
                    To: bob@plausiden.com\r\n\
                    Subject: x\r\n\
                    \r\nbody\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        // Trusted server name matches: pass.
        assert_eq!(p.is_from_authenticated("mx.plausiden.com"), Some(true));
        // Trust a different server: that AR header is from a
        // different server, so we got NO trusted AR header at all.
        assert_eq!(p.is_from_authenticated("mx.other.com"), None);
    }

    #[test]
    fn is_from_authenticated_rejects_attacker_supplied_ar_header() {
        // Attacker sends a forged email and includes their own
        // Authentication-Results claiming pass — but the
        // authserv-id is "attacker.evil.com", not our trusted MX.
        let raw = b"From: alice@bigprospect.com\r\n\
                    Authentication-Results: attacker.evil.com; spf=pass smtp.mailfrom=alice@bigprospect.com; dkim=pass header.d=bigprospect.com; dmarc=pass\r\n\
                    To: bob@plausiden.com\r\n\
                    Subject: please remove me\r\n\
                    \r\nremove me from your list\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        // We trust ONLY mx.plausiden.com. The attacker's AR header
        // is ignored — we got no trusted AR header at all.
        assert_eq!(p.is_from_authenticated("mx.plausiden.com"), None);
    }

    #[test]
    fn is_from_authenticated_with_failing_trusted_header() {
        // Our MX stamped a fail. Distinguishable from "no header at
        // all" so the operator can be alerted differently.
        let raw = b"From: alice@bigprospect.com\r\n\
                    Authentication-Results: mx.plausiden.com; spf=fail smtp.mailfrom=alice@bigprospect.com; dkim=fail header.d=bigprospect.com\r\n\
                    To: bob@plausiden.com\r\n\
                    Subject: please remove me\r\n\
                    \r\nremove me\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        assert_eq!(p.is_from_authenticated("mx.plausiden.com"), Some(false));
    }

    #[test]
    fn is_from_authenticated_returns_none_for_empty_trust() {
        let raw = b"From: alice@example.com\r\n\
                    Authentication-Results: mx.plausiden.com; dmarc=pass\r\n\
                    To: bob@plausiden.com\r\nSubject: x\r\n\r\nbody\r\n";
        let p = ParsedReply::from_rfc5322(raw).unwrap();
        assert_eq!(p.is_from_authenticated(""), None);
        assert_eq!(p.is_from_authenticated("   "), None);
    }

    proptest::proptest! {
        // An inbound reply is fully attacker-controlled, so from_rfc5322 is a
        // trust boundary: it must NEVER panic — only ever return Some/None.
        #[test]
        fn from_rfc5322_never_panics_on_arbitrary_bytes(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)
        ) {
            let _ = ParsedReply::from_rfc5322(&bytes);
        }

        // Header-shaped fuzz: tabs/CR/LF/colons + printable ASCII exercise the
        // header-splitting paths specifically.
        #[test]
        fn from_rfc5322_never_panics_on_headerish_text(
            s in "[\\x09\\x0a\\x0d\\x20-\\x7e]{0,2000}"
        ) {
            if let Some(p) = ParsedReply::from_rfc5322(s.as_bytes()) {
                // Downstream calls on a parsed-from-garbage reply must also be
                // panic-free.
                let _ = p.from_domain();
                let _ = p.is_from_authenticated("plausiden.com");
            }
        }
    }
}
