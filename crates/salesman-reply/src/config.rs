//! IMAP connection configuration for the reply poller.

use salesman_core::{Error, Result};
use zeroize::Zeroizing;

/// IMAP connection config for the reply poller. Only implicit-TLS
/// port 993 is supported.
#[derive(Debug, Clone)]
pub struct ImapConfig {
    /// IMAP server hostname.
    pub host: String,
    /// IMAP server port (must be 993 — implicit TLS).
    pub port: u16,
    /// Login username.
    pub username: String,
    /// Login password. Zeroized on drop.
    pub password: Zeroizing<String>,
    /// Mailbox to poll (defaults to `INBOX`).
    pub mailbox: String,
}

impl ImapConfig {
    /// Build an [`ImapConfig`] from the `SALESMAN_IMAP_*` environment
    /// variables. Errors if a required var is missing or the port is not
    /// a valid `u16`.
    pub fn from_env() -> Result<Self> {
        let env = |k: &str| std::env::var(k).map_err(|_| Error::Config(format!("env {k} not set")));
        let port: u16 = env("SALESMAN_IMAP_PORT")?
            .parse()
            .map_err(|_| Error::Config("SALESMAN_IMAP_PORT not a valid u16".into()))?;
        if port != 993 {
            // We refuse plaintext IMAP. If you need a non-standard
            // TLS port, set it to 993 anyway and use SNI — or modify
            // this check. We do NOT support port 143 STARTTLS.
            return Err(Error::Config(format!(
                "IMAP port {port}: only 993 (implicit TLS) is supported"
            )));
        }
        Ok(Self {
            host: env("SALESMAN_IMAP_HOST")?,
            port,
            username: env("SALESMAN_IMAP_USERNAME")?,
            password: Zeroizing::new(env("SALESMAN_IMAP_PASSWORD")?),
            mailbox: std::env::var("SALESMAN_IMAP_MAILBOX").unwrap_or_else(|_| "INBOX".into()),
        })
    }
}
