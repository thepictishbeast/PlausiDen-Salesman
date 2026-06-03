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
    let label = input.prospect_label.trim();
    let label = if label.is_empty() { "(unknown)" } else { label };

    let subject = format!("{SUBJECT_PREFIX} {label}");

    let dash = "—";
    let sent_subject = input.subject.map(str::trim).filter(|s| !s.is_empty());
    let campaign = input.campaign.map(str::trim).filter(|s| !s.is_empty());
    let receipt = input.receipt_id.map(str::trim).filter(|s| !s.is_empty());

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
        to = input.to_address.trim(),
        how = input.channel.trim(),
        when = input.sent_at.to_rfc3339(),
        campaign = campaign.unwrap_or(dash),
        receipt = receipt.unwrap_or(dash),
        subj = sent_subject.unwrap_or(dash),
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
}
