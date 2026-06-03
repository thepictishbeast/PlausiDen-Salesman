//! IMAP connection configuration for the reply poller.

use salesman_core::{Error, Result};
use zeroize::Zeroizing;

/// IMAP connection config for the reply poller. Only implicit-TLS
/// port 993 is supported.
///
/// SECURITY: the IMAP `password` is held in `Zeroizing<String>` and
/// `Debug` is implemented manually to REDACT it. The derived `Debug`
/// would print the password verbatim — `Zeroizing`'s `Debug` delegates
/// to the inner `String` — and `ImapPoller` derives `Debug` while
/// holding an `ImapConfig`, so a stray `{:?}` anywhere up the stack
/// would leak the mailbox credential into logs (CLAUDE.md: no secrets
/// in logs).
#[derive(Clone)]
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

impl std::fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the password; everything else is safe to show. Listing
        // fields explicitly (rather than deriving) is fail-safe: a field
        // added later is omitted from output until consciously included.
        f.debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("mailbox", &self.mailbox)
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_imap_password() {
        let cfg = ImapConfig {
            host: "imap.example.com".into(),
            port: 993,
            username: "agent@example.com".into(),
            password: Zeroizing::new("hunter2-super-secret".into()),
            mailbox: "INBOX".into(),
        };
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("hunter2-super-secret"),
            "Debug must not leak the IMAP password: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        // Non-secret fields stay visible for debugging.
        assert!(rendered.contains("imap.example.com"), "{rendered}");
        assert!(rendered.contains("agent@example.com"), "{rendered}");
    }
}
