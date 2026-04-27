//! RFC 3464 delivery status notification (DSN) detection + parsing.
//!
//! Most mail systems accept-then-bounce: they 250-OK the message at
//! SMTP time, then asynchronously send a Mailer-Daemon DSN back if
//! delivery fails. The synchronous bounce classifier in
//! `salesman-outreach` only catches the FIRST kind. This module
//! catches the SECOND.
//!
//! BUG ASSUMPTION: we work from a `ParsedReply` (already through
//! mail-parser). DSNs are structured (multipart/report with a
//! message/delivery-status part) but we deliberately use a HEURISTIC
//! over the plain-text body — the structured form is fragile across
//! bouncing MTAs that re-wrap or re-encode the report.
//!
//! Detection signals (need ≥2 of 3 to treat as DSN):
//! 1. From: contains `mailer-daemon` / `postmaster`
//! 2. Subject: matches a known DSN preamble
//! 3. Body contains `Final-Recipient:` or `Status: <X.Y.Z>`
//!
//! Once classified, we extract the failed recipient + the enhanced
//! status code, hand them to `salesman_outreach::bounce::classify` to
//! decide hard vs soft, and let the caller suppress accordingly.

use crate::ParsedReply;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnInfo {
    /// The recipient address that failed (extracted from
    /// `Final-Recipient:` or fallback heuristics on the body).
    pub recipient: String,
    /// Enhanced status code, e.g. "5.1.1". Best-effort.
    pub status: Option<String>,
    /// Free-form summary line — used for the suppression reason.
    pub summary: String,
}

impl ParsedReply {
    /// Attempt DSN detection. Returns Some when the message looks like
    /// a Mailer-Daemon delivery report and a recipient address could
    /// be extracted.
    pub fn detect_dsn(&self) -> Option<DsnInfo> {
        let from_l = self.from_address.to_ascii_lowercase();
        let subj_l = self.subject.as_deref().unwrap_or("").to_ascii_lowercase();
        let body = &self.body_plain;
        let body_l = body.to_ascii_lowercase();

        let from_signal = from_l.contains("mailer-daemon")
            || from_l.starts_with("postmaster@")
            || from_l.contains("mail-daemon");
        let subject_signal = SUBJECT_PATTERNS.iter().any(|p| subj_l.contains(p));
        let body_signal = body_l.contains("final-recipient:")
            || body_l.contains("\nstatus: 5.")
            || body_l.contains("\nstatus: 4.")
            || body_l.contains("delivery to the following recipient failed")
            || body_l.contains("the following message could not be delivered");

        let signals = (from_signal as u8) + (subject_signal as u8) + (body_signal as u8);
        if signals < 2 {
            return None;
        }

        let recipient = extract_recipient(body)?;
        let status = extract_status(body);
        let summary = first_diagnostic_line(body)
            .unwrap_or_else(|| {
                self.subject
                    .clone()
                    .unwrap_or_else(|| "delivery failed".into())
            })
            .chars()
            .take(280)
            .collect();

        Some(DsnInfo {
            recipient,
            status,
            summary,
        })
    }
}

const SUBJECT_PATTERNS: &[&str] = &[
    "delivery status notification",
    "undelivered mail returned",
    "mail delivery failed",
    "mail delivery failure",
    "returned mail",
    "failure notice",
    "delivery has failed",
    "couldn't be delivered",
    "could not be delivered",
    "delivery report",
];

fn extract_recipient(body: &str) -> Option<String> {
    // RFC 3464 form: "Final-Recipient: rfc822; user@host" — be tolerant
    // of casing, whitespace, and missing address-type prefix.
    for line in body.lines() {
        let l = line.trim();
        if let Some(rest) = strip_prefix_ci(l, "final-recipient:") {
            return parse_recipient_value(rest);
        }
        if let Some(rest) = strip_prefix_ci(l, "original-recipient:") {
            return parse_recipient_value(rest);
        }
    }
    // Fallback: <email@host> on a "could not deliver to" line.
    for line in body.lines() {
        let l_lc = line.to_ascii_lowercase();
        if (l_lc.contains("recipient")
            || l_lc.contains("could not")
            || l_lc.contains("undelivered"))
            && let Some(addr) = extract_angle_addr(line)
        {
            return Some(addr);
        }
    }
    // Last-resort fallback: the first standalone <email@host> in the
    // body. We've already confirmed (by signal count) that this IS a
    // DSN, so any angle-addr is much more likely to be the failed
    // recipient than anything else (DSN templates rarely include
    // unrelated addresses).
    for line in body.lines() {
        if let Some(addr) = extract_angle_addr(line) {
            return Some(addr);
        }
    }
    None
}

fn parse_recipient_value(rest: &str) -> Option<String> {
    // Forms we tolerate:
    //   "rfc822; user@host"
    //   "rfc822;user@host"
    //   "user@host"
    //   "<user@host>"
    let v = rest.trim();
    let after_semi = v.split_once(';').map(|(_, r)| r.trim()).unwrap_or(v);
    if let Some(angled) = extract_angle_addr(after_semi) {
        return Some(angled);
    }
    let candidate = after_semi.trim_matches(|c: char| c.is_whitespace() || c == ',');
    if looks_like_email(candidate) {
        return Some(candidate.to_string());
    }
    None
}

fn extract_angle_addr(s: &str) -> Option<String> {
    let start = s.find('<')?;
    let end = s[start..].find('>')?;
    let inner = &s[start + 1..start + end];
    if looks_like_email(inner) {
        Some(inner.to_string())
    } else {
        None
    }
}

fn looks_like_email(s: &str) -> bool {
    // Cheap structural check; bounces sometimes carry mangled
    // addresses, but if it has exactly one '@' with non-empty local
    // and domain parts and no whitespace, that's good enough.
    if s.contains(' ') || s.contains('\t') {
        return false;
    }
    let (local, domain) = match s.split_once('@') {
        Some(p) => p,
        None => return false,
    };
    !local.is_empty() && !domain.is_empty() && domain.contains('.') && !domain.contains('@')
}

fn extract_status(body: &str) -> Option<String> {
    for line in body.lines() {
        let l = line.trim();
        if let Some(rest) = strip_prefix_ci(l, "status:") {
            let rest = rest.trim();
            // Accept just the X.Y.Z token at the start.
            let token = rest.split_whitespace().next()?;
            if is_enhanced_status_code(token) {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn is_enhanced_status_code(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && (parts[0] == "4" || parts[0] == "5" || parts[0] == "2")
}

fn first_diagnostic_line(body: &str) -> Option<String> {
    for line in body.lines() {
        let l = line.trim();
        if let Some(rest) = strip_prefix_ci(l, "diagnostic-code:") {
            return Some(rest.trim().to_string());
        }
    }
    // Fallback: first non-empty line that looks like a remote
    // server's complaint.
    for line in body.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let lc = l.to_ascii_lowercase();
        if lc.starts_with("550 ")
            || lc.starts_with("554 ")
            || lc.starts_with("the following message")
            || lc.starts_with("delivery to")
            || lc.contains("user unknown")
            || lc.contains("address rejected")
        {
            return Some(l.to_string());
        }
    }
    None
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() {
        return None;
    }
    if s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn reply(from: &str, subject: &str, body: &str) -> ParsedReply {
        ParsedReply {
            from_address: from.into(),
            from_name: None,
            subject: Some(subject.into()),
            body_plain: body.into(),
            body_html: None,
            received_at: Utc::now(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            raw_headers: BTreeMap::new(),
        }
    }

    const POSTFIX_DSN: &str = "This is the mail system at host mail.example.com.\n\
\n\
I'm sorry to have to inform you that your message could not\n\
be delivered to one or more recipients. It's attached below.\n\
\n\
                   The mail system\n\
\n\
<bob@nonexistent.example>: host nonexistent.example[1.2.3.4] said: 550 5.1.1\n\
    <bob@nonexistent.example>: Recipient address rejected: User unknown in\n\
    virtual mailbox table (in reply to RCPT TO command)\n\
\n\
Reporting-MTA: dns; mail.example.com\n\
X-Postfix-Queue-ID: ABC123\n\
\n\
Final-Recipient: rfc822; bob@nonexistent.example\n\
Original-Recipient: rfc822;bob@nonexistent.example\n\
Action: failed\n\
Status: 5.1.1\n\
Diagnostic-Code: smtp; 550 5.1.1 <bob@nonexistent.example>: Recipient address rejected: User unknown\n";

    const GMAIL_DSN: &str = "Address not found\n\
\n\
Your message wasn't delivered to nobody@gmail.example because the address couldn't be found, or is unable to receive mail.\n\
\n\
The response was:\n\
550 5.1.1 The email account that you tried to reach does not exist.\n\
\n\
Final-Recipient: rfc822; nobody@gmail.example\n\
Action: failed\n\
Status: 5.1.1\n\
Remote-MTA: dns; gmail-smtp-in.l.google.com\n\
Diagnostic-Code: smtp; 550-5.1.1 The email account that you tried to reach does not exist.\n";

    #[test]
    fn detects_postfix_dsn() {
        let r = reply(
            "MAILER-DAEMON@mail.example.com",
            "Undelivered Mail Returned to Sender",
            POSTFIX_DSN,
        );
        let d = r.detect_dsn().expect("should detect");
        assert_eq!(d.recipient, "bob@nonexistent.example");
        assert_eq!(d.status.as_deref(), Some("5.1.1"));
        assert!(d.summary.to_lowercase().contains("user unknown"));
    }

    #[test]
    fn detects_gmail_dsn() {
        let r = reply(
            "Mail Delivery Subsystem <mailer-daemon@googlemail.com>",
            "Delivery Status Notification (Failure)",
            GMAIL_DSN,
        );
        let d = r.detect_dsn().expect("should detect");
        assert_eq!(d.recipient, "nobody@gmail.example");
        assert_eq!(d.status.as_deref(), Some("5.1.1"));
    }

    #[test]
    fn ignores_normal_reply() {
        let r = reply(
            "alice@customer.example",
            "Re: about your product",
            "Sounds interesting, can you tell me more?",
        );
        assert!(r.detect_dsn().is_none());
    }

    #[test]
    fn ignores_message_that_only_mentions_delivery() {
        // A real reply that talks about delivery shouldn't be misread
        // as a DSN — needs ≥2 signals.
        let r = reply(
            "alice@customer.example",
            "Re: Delivery times for your product",
            "What's your typical delivery status?",
        );
        assert!(r.detect_dsn().is_none());
    }

    #[test]
    fn handles_dsn_without_subject_match() {
        // From-signal + body-signal alone is enough.
        let r = reply(
            "MAILER-DAEMON@x.example",
            "(no subject)",
            "Final-Recipient: rfc822; lost@gone.example\n\
             Action: failed\n\
             Status: 5.1.10\n",
        );
        let d = r.detect_dsn().expect("should detect");
        assert_eq!(d.recipient, "lost@gone.example");
        assert_eq!(d.status.as_deref(), Some("5.1.10"));
    }

    #[test]
    fn handles_dsn_without_explicit_final_recipient() {
        // Some lazy MTAs only put the address in the body text.
        let r = reply(
            "MAILER-DAEMON@x.example",
            "Mail delivery failed: returning message to sender",
            "The following recipient could not be reached:\n\
            \n\
             <bob@dead.example>\n\
             550 user unknown\n",
        );
        let d = r.detect_dsn();
        assert!(d.is_some());
        assert_eq!(d.unwrap().recipient, "bob@dead.example");
    }

    #[test]
    fn rejects_bare_postmaster_with_no_body_signal() {
        // postmaster@ may also send legitimate ops mail. Without a
        // body signal we should not classify as DSN.
        let r = reply(
            "postmaster@example.com",
            "Service announcement",
            "Mail server maintenance scheduled for Friday.",
        );
        assert!(r.detect_dsn().is_none());
    }

    #[test]
    fn parses_4xx_status_too() {
        let r = reply(
            "MAILER-DAEMON@x.example",
            "Mail delivery failed",
            "Final-Recipient: rfc822; busy@example.com\n\
             Action: delayed\n\
             Status: 4.2.0\n",
        );
        let d = r.detect_dsn().unwrap();
        assert_eq!(d.status.as_deref(), Some("4.2.0"));
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        // detect_dsn never panics on arbitrary header / body content.
        #[test]
        fn detect_never_panics(
            from in ".{0,64}",
            subject in ".{0,128}",
            body in ".{0,512}",
        ) {
            let r = reply(&from, &subject, &body);
            let _ = r.detect_dsn();
        }

        // Replies that look nothing like a DSN never get classified
        // as one. We use a printable-ASCII generator that excludes
        // the trigger keywords.
        #[test]
        fn benign_reply_never_detected(
            // exclude DSN signal words
            body in "[a-zA-Z0-9 .,!?\\n]{0,400}",
        ) {
            let body_lc = body.to_ascii_lowercase();
            let has_signal = body_lc.contains("final-recipient")
                || body_lc.contains("status: 5.")
                || body_lc.contains("status: 4.")
                || body_lc.contains("could not be delivered")
                || body_lc.contains("recipient failed");
            if has_signal {
                return Ok(()); // not benign
            }
            let r = reply("alice@customer.example", "Re: hello", &body);
            let detected = r.detect_dsn().is_some();
            prop_assert!(!detected);
        }
    }
}
