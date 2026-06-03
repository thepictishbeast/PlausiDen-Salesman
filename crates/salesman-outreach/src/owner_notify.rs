//! Owner audit-notification: a per-contact summary the operator receives
//! whenever Salesman reaches out to a prospect.
//!
//! Purpose (operator's words): "if I get a phone call saying you reached
//! out to me, I know exactly who and how and what was said." Every
//! outbound contact produces one of these so the operator has a durable,
//! receipt-backed record keyed by the prospect's name/business.
//!
//! This module is the pure *formatter* — it builds the notification's
//! subject + body from a sent-contact summary. It does NOT send anything
//! (delivery reuses the gated [`crate::SmtpSender`]) and does NOT persist
//! anything (that is the state layer's job). Keeping it pure makes the
//! exact wording unit-testable without a mailbox or a database.

use chrono::{DateTime, Utc};
use salesman_core::sanitize_header_value;

/// Everything needed to render an owner audit-notification for one
/// outbound contact.
#[derive(Debug, Clone)]
pub struct OwnerNotifyInput<'a> {
    /// The prospect's name or business — placed verbatim in the subject
    /// so the operator can find the contact by who it was about.
    pub prospect_label: &'a str,
    /// The recipient address that was contacted.
    pub to_address: &'a str,
    /// The channel used (e.g. `email`).
    pub channel: &'a str,
    /// When the contact was sent.
    pub sent_at: DateTime<Utc>,
    /// The subject line that was sent, if any.
    pub subject: Option<&'a str>,
    /// The body that was sent.
    pub body: &'a str,
    /// The signed receipt id for the send, if one was recorded.
    pub receipt_id: Option<&'a str>,
    /// The campaign the contact belonged to, if any.
    pub campaign: Option<&'a str>,
}

/// A rendered owner audit-notification, ready to be queued to the
/// operator's mailbox (subject + plain-text body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerNotification {
    /// Subject line — carries the prospect's name/business.
    pub subject: String,
    /// Plain-text body capturing who / how / what-was-said.
    pub body: String,
}

/// Subject prefix so the operator can filter these in their inbox.
const SUBJECT_PREFIX: &str = "[Salesman] Reached out to";

/// Build the owner audit-notification for one outbound contact.
///
/// The subject is `"[Salesman] Reached out to {prospect_label}"` and the
/// body is a fixed who/how/what layout followed by the verbatim message
/// that was sent. Missing optional fields render as a literal `—` rather
/// than being omitted, so the layout (and any log scraping over it) is
/// stable across contacts.
pub fn build_owner_notification(input: &OwnerNotifyInput) -> OwnerNotification {
    // The subject is an email header and the Who/How/… lines are each a
    // single body line. Prospect-derived values (label, address, channel)
    // and the echoed sent-subject are untrusted (scraped/imported), so a
    // raw CR/LF in any of them would inject headers or split the message
    // (CWE-93). Sanitize every single-line field; the multi-line message
    // body is the one place line breaks are intended, so it is left
    // intact (only right-trimmed).
    let label = sanitize_header_value(input.prospect_label);
    let label = if label.is_empty() {
        "(unknown)".to_string()
    } else {
        label
    };

    let subject = format!("{SUBJECT_PREFIX} {label}");

    let to = sanitize_header_value(input.to_address);
    let how = sanitize_header_value(input.channel);
    let sent_subject = input
        .subject
        .map(sanitize_header_value)
        .filter(|s| !s.is_empty());
    let campaign = input
        .campaign
        .map(sanitize_header_value)
        .filter(|s| !s.is_empty());
    let receipt = input
        .receipt_id
        .map(sanitize_header_value)
        .filter(|s| !s.is_empty());

    let body = format!(
        "Salesman contacted a prospect on your behalf.\n\
         \n\
         Who:      {label} <{to}>\n\
         How:      {how}\n\
         When:     {when}\n\
         Campaign: {campaign}\n\
         Receipt:  {receipt}\n\
         Subject:  {subj}\n\
         \n\
         --- message sent ---\n\
         {body}\n",
        label = label,
        to = to,
        how = how,
        when = input.sent_at.to_rfc3339(),
        campaign = campaign.unwrap_or_else(|| "—".to_string()),
        receipt = receipt.unwrap_or_else(|| "—".to_string()),
        subj = sent_subject.unwrap_or_else(|| "—".to_string()),
        body = input.body.trim_end(),
    );

    OwnerNotification { subject, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample<'a>() -> OwnerNotifyInput<'a> {
        OwnerNotifyInput {
            prospect_label: "Acme Security Inc",
            to_address: "sales@acme.example",
            channel: "email",
            sent_at: Utc.with_ymd_and_hms(2026, 6, 2, 15, 4, 5).unwrap(),
            subject: Some("On your security workflow"),
            body: "Hi — quick note about X.\n\nReply STOP and I won't follow up.",
            receipt_id: Some("rcpt-123"),
            campaign: Some("local-security-q2"),
        }
    }

    #[test]
    fn subject_carries_prospect_label() {
        let n = build_owner_notification(&sample());
        assert!(
            n.subject.contains("Acme Security Inc"),
            "subject must name the prospect: {}",
            n.subject
        );
        assert!(n.subject.starts_with(SUBJECT_PREFIX));
    }

    #[test]
    fn body_captures_who_how_what() {
        let n = build_owner_notification(&sample());
        // who
        assert!(n.body.contains("Acme Security Inc"));
        assert!(n.body.contains("sales@acme.example"));
        // how
        assert!(n.body.contains("email"));
        // when (RFC3339)
        assert!(n.body.contains("2026-06-02T15:04:05"));
        // what
        assert!(n.body.contains("On your security workflow"));
        assert!(n.body.contains("quick note about X"));
        // provenance
        assert!(n.body.contains("rcpt-123"));
        assert!(n.body.contains("local-security-q2"));
    }

    #[test]
    fn missing_optionals_render_as_dash_not_omitted() {
        let mut input = sample();
        input.subject = None;
        input.receipt_id = None;
        input.campaign = None;
        let n = build_owner_notification(&input);
        // The labelled lines are still present (stable layout).
        assert!(n.body.contains("Receipt:  —"));
        assert!(n.body.contains("Campaign: —"));
        assert!(n.body.contains("Subject:  —"));
    }

    #[test]
    fn empty_optionals_treated_as_missing() {
        let mut input = sample();
        input.subject = Some("   ");
        input.receipt_id = Some("");
        let n = build_owner_notification(&input);
        assert!(n.body.contains("Subject:  —"));
        assert!(n.body.contains("Receipt:  —"));
    }

    #[test]
    fn blank_label_falls_back_to_unknown() {
        let mut input = sample();
        input.prospect_label = "   ";
        let n = build_owner_notification(&input);
        assert!(n.subject.contains("(unknown)"));
    }

    #[test]
    fn subject_resists_crlf_header_injection() {
        // A scraped business name carrying a CRLF + an injected header.
        let mut input = sample();
        input.prospect_label = "Acme\r\nBcc: attacker@evil.example";
        let n = build_owner_notification(&input);
        // The subject is a header line — it must never contain a break.
        assert!(!n.subject.contains('\r'), "subject: {:?}", n.subject);
        assert!(!n.subject.contains('\n'), "subject: {:?}", n.subject);
        // The injected text survives only as harmless flattened literal
        // text on the subject line, not as a real header.
        assert!(n.subject.contains("Acme Bcc: attacker@evil.example"));
        // The Who: body line that echoes the label is likewise flattened.
        let who = n.body.lines().find(|l| l.starts_with("Who:")).unwrap();
        assert!(who.contains("Acme Bcc: attacker@evil.example"), "{who:?}");
    }

    #[test]
    fn injected_address_and_channel_stay_single_line() {
        let mut input = sample();
        input.to_address = "x@y.z\r\nSubject: spoofed";
        input.channel = "email\nX-Evil: 1";
        let n = build_owner_notification(&input);
        // Each labelled body line stays exactly one physical line.
        let who = n.body.lines().find(|l| l.starts_with("Who:")).unwrap();
        let how = n.body.lines().find(|l| l.starts_with("How:")).unwrap();
        assert!(who.contains("x@y.z Subject: spoofed"), "{who:?}");
        assert!(how.contains("email X-Evil: 1"), "{how:?}");
    }

    proptest::proptest! {
        /// The formatter is fed prospect-derived strings — including, in
        /// the wild, ones with embedded control characters. It must never
        /// panic, the subject (an email header) must never carry a line
        /// break, and the contact must stay findable: the sanitized label
        /// (or the "(unknown)" fallback) appears in the subject.
        #[test]
        fn subject_is_single_line_and_identifies_prospect(
            label_chars in proptest::collection::vec(proptest::char::any(), 0..40),
            to in ".{0,40}",
            channel in ".{0,12}",
            subject in proptest::option::of(".{0,40}"),
            body_chars in proptest::collection::vec(proptest::char::any(), 0..200),
        ) {
            let label: String = label_chars.into_iter().collect();
            let body: String = body_chars.into_iter().collect();
            let input = OwnerNotifyInput {
                prospect_label: &label,
                to_address: &to,
                channel: &channel,
                sent_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                subject: subject.as_deref(),
                body: &body,
                receipt_id: None,
                campaign: None,
            };
            let n = build_owner_notification(&input);
            proptest::prop_assert!(!n.subject.contains('\r'), "subject: {:?}", n.subject);
            proptest::prop_assert!(!n.subject.contains('\n'), "subject: {:?}", n.subject);
            let sanitized = sanitize_header_value(&label);
            let expected = if sanitized.is_empty() {
                "(unknown)".to_string()
            } else {
                sanitized
            };
            proptest::prop_assert!(
                n.subject.contains(&expected),
                "subject {:?} must contain {:?}", n.subject, expected
            );
        }
    }
}
