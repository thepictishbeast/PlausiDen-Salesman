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
#![deny(missing_docs)]

pub mod bounce;
pub mod owner_notify;
pub mod unsubscribe;
pub use bounce::{SmtpFailure, classify as classify_smtp_failure};
pub use owner_notify::{OwnerNotification, OwnerNotifyInput, build_owner_notification};
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
///
/// `Debug` is implemented manually to REDACT the password — the derived
/// impl would print it (Zeroizing<String>'s Debug delegates to String),
/// leaking the secret on any `{:?}` of this or `SmtpSender`.
#[derive(Clone)]
pub struct SmtpConfig {
    /// SMTP relay hostname.
    pub host: String,
    /// SMTP relay port (e.g. 587 for STARTTLS submission).
    pub port: u16,
    /// SASL username. None when relaying via a local / cluster-
    /// internal MTA that allowlists the sender by IP (no AUTH
    /// required). The two are coupled — must be either both Some
    /// or both None.
    pub username: Option<String>,
    /// SASL password, paired with `username` (see above). Zeroized on drop.
    pub password: Option<Zeroizing<String>>,
    /// Display name used in the From header.
    pub from_name: String,
    /// Envelope/From email address.
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

impl std::fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the password; everything else is safe to show.
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("from_name", &self.from_name)
            .field("from_email", &self.from_email)
            .field("reply_to", &self.reply_to)
            .field("compliance_footer", &self.compliance_footer)
            .field("list_unsubscribe", &self.list_unsubscribe)
            .field("unsubscribe_tokens", &self.unsubscribe_tokens)
            .finish()
    }
}

impl SmtpConfig {
    /// Build an [`SmtpConfig`] from the `SALESMAN_SMTP_*` env vars. The
    /// RFC 8058 unsubscribe minter is best-effort: if its env vars are
    /// missing, sends fall back to a static List-Unsubscribe and
    /// `salesman doctor` flags it as a deliverability warning rather than
    /// erroring here.
    pub fn from_env() -> Result<Self> {
        let env = |k: &str| std::env::var(k).map_err(|_| Error::Config(format!("env {k} not set")));
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
        // Username + password are paired. Either both set (SASL
        // auth path — typical for hosted SMTP like SendGrid /
        // Mailgun) or both unset (cluster-internal relay where the
        // upstream MTA allowlists the sender by IP and skips AUTH).
        // Mixed half-set state is a config error — fail loud.
        let (username, password) = pair_credentials(
            std::env::var("SALESMAN_SMTP_USERNAME").ok(),
            std::env::var("SALESMAN_SMTP_PASSWORD").ok(),
        )?;
        Ok(Self {
            host: env("SALESMAN_SMTP_HOST")?,
            port: env("SALESMAN_SMTP_PORT")?
                .parse()
                .map_err(|_| Error::Config("SALESMAN_SMTP_PORT not a valid u16".into()))?,
            username,
            password,
            from_name: env("SALESMAN_FROM_NAME")?,
            from_email: env("SALESMAN_FROM_EMAIL")?,
            reply_to: std::env::var("SALESMAN_REPLY_TO").ok(),
            compliance_footer: std::env::var("SALESMAN_COMPLIANCE_FOOTER").unwrap_or_else(|_| {
                "PlausiDen — sovereign data tools.\n\
                     Reply STOP to opt out of further messages."
                    .to_string()
            }),
            list_unsubscribe: std::env::var("SALESMAN_LIST_UNSUBSCRIBE").ok(),
            unsubscribe_tokens,
        })
    }
}

/// An SMTP sender: an [`SmtpConfig`] plus a built transport, used to
/// send a single email and produce a [`SendOutcome`].
#[derive(Debug)]
pub struct SmtpSender {
    config: SmtpConfig,
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpSender {
    /// Build a STARTTLS SMTP sender from `config`. SASL AUTH is added
    /// only when credentials are configured; a cluster-internal relay
    /// (IP-trusted via `mynetworks`) sends without AUTH. Errors if the
    /// transport cannot be constructed.
    pub fn new(config: SmtpConfig) -> Result<Self> {
        // Build the transport once. AUTH is added only when the
        // operator configured SASL credentials. Cluster-internal
        // relay (web-01 trusts openclaw's IP via mynetworks) skips
        // AUTH entirely; lettre's `relay` builder issues no AUTH
        // command in that case.
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
            .map_err(|e| Error::Config(format!("smtp transport: {e}")))?
            .port(config.port);
        if let (Some(u), Some(p)) = (config.username.as_ref(), config.password.as_ref()) {
            builder = builder.credentials(Credentials::new(u.clone(), (**p).clone()));
        }
        let transport = builder.build();
        Ok(Self { config, transport })
    }

    /// The configured envelope / From address used for outbound mail.
    pub fn from_email(&self) -> &str {
        &self.config.from_email
    }

    /// Send a single email. Returns the data needed to construct a
    /// receipt. Caller MUST have already done a suppression check.
    pub async fn send_email(
        &self,
        to_email: &str,
        subject: &str,
        body: &str,
    ) -> Result<SendOutcome> {
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
            Some(url) if self.config.unsubscribe_tokens.is_some() => {
                format!("{}\nUnsubscribe: {}", self.config.compliance_footer, url)
            }
            _ => self.config.compliance_footer.clone(),
        };
        let full_body = format!("{body}\n\n--\n{footer_with_unsub}");

        let mut builder = Message::builder().from(from).to(to).subject(subject);

        if let Some(reply) = &self.config.reply_to {
            let reply_mb =
                Mailbox::from_str(reply).map_err(|e| Error::Config(format!("reply_to: {e}")))?;
            builder = builder.reply_to(reply_mb);
        }
        if let Some(url) = &unsub_url {
            builder = builder.header(ListUnsubscribe(format!("<{url}>")));
            builder = builder.header(ListUnsubscribePost(
                "List-Unsubscribe=One-Click".to_string(),
            ));
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
            smtp_message_id: extract_message_id(
                resp.message().collect::<Vec<_>>().join("\n").as_str(),
            ),
            smtp_response_code: resp.code().to_string(),
            sent_at: chrono::Utc::now(),
            from: self.config.from_email.clone(),
            to: to_email.to_string(),
            subject: subject.to_string(),
        })
    }
}

/// The result of a successful send, carrying the data needed to build
/// a receipt and update the touch record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendOutcome {
    /// Queue/message id parsed from the SMTP response, if the server gave one.
    pub smtp_message_id: Option<String>,
    /// The SMTP response code (e.g. `250`).
    pub smtp_response_code: String,
    /// When the send completed.
    pub sent_at: chrono::DateTime<chrono::Utc>,
    /// The From address used.
    pub from: String,
    /// The recipient address.
    pub to: String,
    /// The subject line sent.
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

/// Validate the SASL credential pair: username + password must be BOTH
/// set (authenticated relay, e.g. SendGrid/Mailgun) or BOTH unset
/// (IP-trusted internal relay that skips AUTH). A half-set pair is a
/// configuration error and fails loud.
///
/// Extracted from [`SmtpConfig::from_env`] so this safety invariant is
/// unit-testable without mutating the process environment.
fn pair_credentials(
    username: Option<String>,
    password: Option<String>,
) -> Result<(Option<String>, Option<Zeroizing<String>>)> {
    match (username, password) {
        (Some(u), Some(p)) => Ok((Some(u), Some(Zeroizing::new(p)))),
        (None, None) => Ok((None, None)),
        (Some(_), None) => Err(Error::Config(
            "SALESMAN_SMTP_USERNAME set but SALESMAN_SMTP_PASSWORD missing — \
             SASL credentials must be both set or both unset"
                .into(),
        )),
        (None, Some(_)) => Err(Error::Config(
            "SALESMAN_SMTP_PASSWORD set but SALESMAN_SMTP_USERNAME missing — \
             SASL credentials must be both set or both unset"
                .into(),
        )),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_smtp_password() {
        let cfg = SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            username: Some("user".into()),
            password: Some(zeroize::Zeroizing::new("hunter2-supersecret".to_string())),
            from_name: "Sender".into(),
            from_email: "s@example.com".into(),
            reply_to: None,
            compliance_footer: "footer".into(),
            list_unsubscribe: None,
            unsubscribe_tokens: None,
        };
        let dbg = format!("{cfg:?}");
        assert!(
            !dbg.contains("hunter2-supersecret"),
            "Debug must not leak the SMTP password: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "password should be marked redacted"
        );
        // Non-secret fields remain visible for debuggability.
        assert!(dbg.contains("smtp.example.com"));
    }

    #[test]
    fn pair_credentials_accepts_both_set() {
        let (u, p) = pair_credentials(Some("user".into()), Some("pw".into())).unwrap();
        assert_eq!(u.as_deref(), Some("user"));
        assert_eq!(p.unwrap().as_str(), "pw");
    }

    #[test]
    fn pair_credentials_accepts_both_unset() {
        let (u, p) = pair_credentials(None, None).unwrap();
        assert!(u.is_none() && p.is_none());
    }

    #[test]
    fn pair_credentials_rejects_username_without_password() {
        let err = pair_credentials(Some("user".into()), None).unwrap_err();
        assert!(format!("{err}").contains("both set or both unset"));
    }

    #[test]
    fn pair_credentials_rejects_password_without_username() {
        let err = pair_credentials(None, Some("pw".into())).unwrap_err();
        assert!(format!("{err}").contains("both set or both unset"));
    }

    #[test]
    fn escape_html_escapes_ampersand_before_angle_brackets() {
        assert_eq!(escape_html("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        // Ampersand must be escaped first: `<` becomes `&lt;`, and if `&`
        // were escaped afterwards it would double-escape into `&amp;lt;`.
        assert_eq!(escape_html("<&>"), "&lt;&amp;&gt;");
        assert_eq!(escape_html("no special chars"), "no special chars");
    }

    #[test]
    fn extract_message_id_takes_trailing_token() {
        assert_eq!(
            extract_message_id("250 2.0.0 Ok: queued as ABC123").as_deref(),
            Some("ABC123")
        );
        assert_eq!(extract_message_id("").as_deref(), None);
        assert_eq!(extract_message_id("250").as_deref(), Some("250"));
    }
}
