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
        for header in msg.headers() {
            let name = header.name();
            let value = header.value().as_text().map(|s| s.to_string());
            if let Some(v) = value {
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
}
