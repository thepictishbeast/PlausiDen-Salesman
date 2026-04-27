//! salesman-outreach — multi-channel sender. Phase 1.3 ships the
//! email channel via `lettre`. LinkedIn / forms / X DMs land later.
//!
//! BUG ASSUMPTION: every send goes through `SmtpSender::send_email`
//! which:
//! 1. validates suppression check has been done by caller (we accept
//!    `pre_suppressed` so caller can short-circuit),
//! 2. constructs the message,
//! 3. sends via SMTP-over-TLS,
//! 4. on success, returns a `SendOutcome` containing the receipt
//!    payload — the State layer is responsible for writing the
//!    receipt + updating the touch.
//!
//! SECURITY: SMTP password held in `Zeroizing<String>`.
#![forbid(unsafe_code)]

pub mod bounce;
pub mod unsubscribe;
pub use bounce::{SmtpFailure, classify as classify_smtp_failure};
pub use unsubscribe::UnsubscribeTokens;

use lettre::message::header::{Header, HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use salesman_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use zeroize::Zeroizing;

/// SMTP connection + identity config. Read once at startup.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Zeroizing<String>,
    pub from_name: String,
    pub from_email: String,
    /// Optional Reply-To if different from From.
    pub reply_to: Option<String>,
    /// Footer appended to every body. Should include physical address
    /// + opt-out language for CAN-SPAM.
    pub compliance_footer: String,
    /// Static fallback List-Unsubscribe URL (mailto: or https://).
    /// Used only when `unsubscribe_tokens` is None — otherwise the
    /// per-recipient minted URL takes precedence.
    pub list_unsubscribe: Option<String>,
    /// Per-recipient one-click unsubscribe minter (RFC 8058).
    /// When present, the sender mints a recipient-specific URL on
    /// every send and emits both `List-Unsubscribe` +
    /// `List-Unsubscribe-Post` headers, and appends the URL to the
    /// compliance footer in plain text for older mail clients.
    pub unsubscribe_tokens: Option<UnsubscribeTokens>,
}

impl SmtpConfig {
    pub fn from_env() -> Result<Self> {
        let env = |k: &str| {
            std::env::var(k).map_err(|_| Error::Config(format!("env {k} not set")))
        };
        // The minter is best-effort: if either env var is missing we
        // fall back to the static `list_unsubscribe` (if any). A
        // missing minter is logged as a deliverability warning by
        // `salesman doctor`, not a hard error here.
        let unsubscribe_tokens = match UnsubscribeTokens::from_env() {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::debug!(reason = %e, "no per-recipient unsubscribe minter configured");
                None
            }
        };
        Ok(Self {
            host: env("SALESMAN_SMTP_HOST")?,
            port: env("SALESMAN_SMTP_PORT")?.parse().map_err(|_| {
                Error::Config("SALESMAN_SMTP_PORT not a valid u16".into())
            })?,
            username: env("SALESMAN_SMTP_USERNAME")?,
            password: Zeroizing::new(env("SALESMAN_SMTP_PASSWORD")?),
            from_name: env("SALESMAN_FROM_NAME")?,
            from_email: env("SALESMAN_FROM_EMAIL")?,
            reply_to: std::env::var("SALESMAN_REPLY_TO").ok(),
            compliance_footer: std::env::var("SALESMAN_COMPLIANCE_FOOTER")
                .unwrap_or_else(|_| {
                    "PlausiDen — sovereign data tools.\n\
                     Reply STOP to opt out of further messages.".to_string()
                }),
            list_unsubscribe: std::env::var("SALESMAN_LIST_UNSUBSCRIBE").ok(),
            unsubscribe_tokens,
        })
    }
}

#[derive(Debug)]
pub struct SmtpSender {
    config: SmtpConfig,
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpSender {
    pub fn new(config: SmtpConfig) -> Result<Self> {
        let creds = Credentials::new(config.username.clone(), (*config.password).clone());
        let transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
            .map_err(|e| Error::Config(format!("smtp transport: {e}")))?
            .port(config.port)
            .credentials(creds)
            .build();
        Ok(Self { config, transport })
    }

    pub fn from_email(&self) -> &str {
        &self.config.from_email
    }

    /// Send a single email. Returns the data needed to construct a
    /// receipt. Caller MUST have already done a suppression check.
    pub async fn send_email(&self, to_email: &str, subject: &str, body: &str) -> Result<SendOutcome> {
        let from = Mailbox::new(
            Some(self.config.from_name.clone()),
            self.config
                .from_email
                .parse()
                .map_err(|e| Error::Config(format!("from_email: {e}")))?,
        );
        let to: Mailbox = to_email
            .parse()
            .map_err(|e| Error::Validation(format!("to address: {e}")))?;

        // Resolve unsubscribe URL: per-recipient minter wins, static
        // fallback otherwise. When neither is set we ship without the
        // header (gateway reputation will suffer; `salesman doctor`
        // surfaces this).
        let unsub_url: Option<String> = self
            .config
            .unsubscribe_tokens
            .as_ref()
            .map(|m| m.url_for(to_email))
            .or_else(|| self.config.list_unsubscribe.clone());

        // When we have a real minted URL, surface it in the visible
        // footer too — RFC 8058 only covers the headers and many MUAs
        // (Apple Mail, Thunderbird older versions, plain-text CLI mail)
        // will not render the header link.
        let footer_with_unsub = match &unsub_url {
            Some(url) if self.config.unsubscribe_tokens.is_some() => format!(
                "{}\nUnsubscribe: {}",
                self.config.compliance_footer,
                url
            ),
            _ => self.config.compliance_footer.clone(),
        };
        let full_body = format!("{body}\n\n--\n{footer_with_unsub}");

        let mut builder = Message::builder()
            .from(from)
            .to(to)
            .subject(subject);

        if let Some(reply) = &self.config.reply_to {
            let reply_mb = Mailbox::from_str(reply)
                .map_err(|e| Error::Config(format!("reply_to: {e}")))?;
            builder = builder.reply_to(reply_mb);
        }
        if let Some(url) = &unsub_url {
            builder = builder.header(ListUnsubscribe(format!("<{url}>")));
            builder = builder.header(ListUnsubscribePost("List-Unsubscribe=One-Click".to_string()));
        }

        let msg = builder
            .multipart(MultiPart::alternative_plain_html(
                full_body.clone(),
                escape_html(&full_body),
            ))
            .map_err(|e| Error::Internal(format!("message build: {e}")))?;

        let resp = self.transport.send(msg).await.map_err(|e| Error::Tool {
            tool: "outreach.smtp".into(),
            message: format!("send: {e}"),
        })?;

        Ok(SendOutcome {
            smtp_message_id: extract_message_id(resp.message().collect::<Vec<_>>().join("\n").as_str()),
            smtp_response_code: resp.code().to_string(),
            sent_at: chrono::Utc::now(),
            from: self.config.from_email.clone(),
            to: to_email.to_string(),
            subject: subject.to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendOutcome {
    pub smtp_message_id: Option<String>,
    pub smtp_response_code: String,
    pub sent_at: chrono::DateTime<chrono::Utc>,
    pub from: String,
    pub to: String,
    pub subject: String,
}

/// RFC 2369 / RFC 8058 List-Unsubscribe header.
#[derive(Clone, Debug)]
struct ListUnsubscribe(String);

impl Header for ListUnsubscribe {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("List-Unsubscribe")
    }
    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.to_string()))
    }
    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.0.clone())
    }
}

/// RFC 8058 one-click unsubscribe marker.
#[derive(Clone, Debug)]
struct ListUnsubscribePost(String);

impl Header for ListUnsubscribePost {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("List-Unsubscribe-Post")
    }
    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.to_string()))
    }
    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.0.clone())
    }
}

fn extract_message_id(s: &str) -> Option<String> {
    // Some servers echo the queue id in the 250 response: "250 2.0.0 Ok: queued as ABC123".
    s.split_whitespace().last().map(str::to_string)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
