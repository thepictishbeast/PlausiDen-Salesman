//! IMAP poller skeleton. `poll_once()` is the unit of work; the
//! periodic loop sits above this in the orchestrator (or a CLI
//! `inbox-poll --once` invocation for ops).
//!
//! BUG ASSUMPTION: TLS only. Refuses to connect to plaintext IMAP.
//! BUG ASSUMPTION: only handles INBOX (or the configured mailbox);
//! does not walk subfolders.

use crate::{ImapConfig, ParsedReply};
use async_imap::Session;
use futures::{StreamExt, TryStreamExt};
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use salesman_core::{Error, Result};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, info, warn};

type ImapStream = tokio_rustls::client::TlsStream<TcpStream>;

#[derive(Debug)]
pub struct ImapPoller {
    config: ImapConfig,
}

impl ImapPoller {
    /// Build a poller bound to `config`. The TLS connection is opened
    /// lazily per [`Self::poll_once`], not here.
    pub fn new(config: ImapConfig) -> Self {
        Self { config }
    }

    /// Connect, login, select mailbox, fetch UNSEEN, hand each parsed
    /// message to `on_reply`, mark `\Seen` on success. Returns the
    /// number of messages handled.
    pub async fn poll_once<F, Fut>(&self, mut on_reply: F) -> Result<u32>
    where
        F: FnMut(ParsedReply) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let mut session = self.connect_and_login().await?;
        session
            .select(&self.config.mailbox)
            .await
            .map_err(|e| Error::Tool {
                tool: "imap.select".into(),
                message: format!("{e}"),
            })?;

        let unseen_uids = session
            .uid_search("UNSEEN")
            .await
            .map_err(|e| Error::Tool {
                tool: "imap.search".into(),
                message: format!("{e}"),
            })?;
        let unseen: Vec<u32> = unseen_uids.into_iter().collect();
        if unseen.is_empty() {
            info!("no new messages");
            let _ = session.logout().await;
            return Ok(0);
        }
        info!(count = unseen.len(), "fetching new messages");

        let mut handled = 0u32;
        for uid in &unseen {
            let stream = session
                .uid_fetch(uid.to_string(), "BODY.PEEK[]")
                .await
                .map_err(|e| Error::Tool {
                    tool: "imap.fetch".into(),
                    message: format!("uid={uid}: {e}"),
                })?;
            let messages: Vec<_> = stream.try_collect().await.map_err(|e| Error::Tool {
                tool: "imap.fetch_collect".into(),
                message: format!("uid={uid}: {e}"),
            })?;
            for m in messages {
                let body_bytes = match m.body() {
                    Some(b) => b.to_vec(),
                    None => {
                        warn!(uid, "FETCH returned no body");
                        continue;
                    }
                };
                let parsed = match ParsedReply::from_rfc5322(&body_bytes) {
                    Some(p) => p,
                    None => {
                        warn!(uid, "parse failed (no From or no body)");
                        continue;
                    }
                };
                debug!(uid, from = %parsed.from_address, "handing to callback");
                on_reply(parsed).await?;
                // mark \Seen
                let _ = session
                    .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
                    .await
                    .map_err(|e| Error::Tool {
                        tool: "imap.mark_seen".into(),
                        message: format!("uid={uid}: {e}"),
                    })?
                    .collect::<Vec<_>>()
                    .await;
                handled += 1;
            }
        }
        let _ = session.logout().await;
        Ok(handled)
    }

    async fn connect_and_login(&self) -> Result<Session<ImapStream>> {
        let addr = (self.config.host.as_str(), self.config.port);
        let tcp = TcpStream::connect(addr).await.map_err(|e| Error::Tool {
            tool: "imap.connect".into(),
            message: format!("tcp: {e}"),
        })?;

        let tls_config = build_tls_config();
        let connector = TlsConnector::from(Arc::new(tls_config));
        let server_name = ServerName::try_from(self.config.host.clone())
            .map_err(|e| Error::Config(format!("invalid IMAP host name: {e}")))?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::Tool {
                tool: "imap.tls".into(),
                message: format!("{e}"),
            })?;

        // async-imap is built with `runtime-tokio`, so it consumes the
        // tokio-rustls stream directly — no futures<->tokio compat shim.
        let client = async_imap::Client::new(tls);
        let session = client
            .login(&self.config.username, &*self.config.password)
            .await
            .map_err(|(e, _client)| Error::Tool {
                tool: "imap.login".into(),
                message: format!("{e}"),
            })?;
        Ok(session)
    }
}

fn build_tls_config() -> ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}
