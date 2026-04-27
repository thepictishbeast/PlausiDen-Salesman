use salesman_core::{Error, Result};
use zeroize::Zeroizing;

#[derive(Debug, Clone)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Zeroizing<String>,
    pub mailbox: String,
}

impl ImapConfig {
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
