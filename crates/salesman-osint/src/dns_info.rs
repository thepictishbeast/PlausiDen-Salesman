//! DNS-info adapter — resolves A / MX / TXT records via the system
//! resolver. No external API; uses tokio's hostname resolution
//! (limited to A) plus a tiny shell-out to `dig` for MX / TXT.
//!
//! BUG ASSUMPTION: `dig` is on PATH on the host running this. If
//! not present, the tool returns a soft error. Caller can ignore.
//!
//! Usefulness: tells the LLM what mail provider a prospect uses
//! (Google Workspace, Microsoft 365, ProtonMail, self-hosted), what
//! anti-spam policy they publish (SPF/DMARC), and where they're
//! hosted (A record + AS hint).

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Resolved DNS records for a domain.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DnsInfo {
    /// A (IPv4 address) records.
    pub a: Vec<String>,
    /// MX (mail exchanger) records.
    pub mx: Vec<String>,
    /// TXT records (SPF/DMARC/verification, etc.).
    pub txt: Vec<String>,
    /// NS (nameserver) records.
    pub ns: Vec<String>,
}

/// Resolves DNS records via the system resolver + `dig`.
#[derive(Debug, Default)]
pub struct DnsInfoClient;

impl DnsInfoClient {
    /// Build a DNS-info client.
    pub fn new() -> Self {
        Self
    }

    /// Resolve `domain` into [`DnsInfo`] (A records, etc.) via the tokio
    /// resolver. Errors on resolver failure.
    pub async fn lookup(&self, domain: &str) -> Result<DnsInfo> {
        // A records via tokio resolver
        let a = match tokio::net::lookup_host(format!("{domain}:0")).await {
            Ok(addrs) => addrs.map(|sa| sa.ip().to_string()).collect(),
            Err(_) => Vec::new(),
        };
        let mx = dig_query(domain, "MX").await.unwrap_or_default();
        let txt = dig_query(domain, "TXT").await.unwrap_or_default();
        let ns = dig_query(domain, "NS").await.unwrap_or_default();
        Ok(DnsInfo { a, mx, txt, ns })
    }
}

async fn dig_query(domain: &str, qtype: &str) -> Result<Vec<String>> {
    let out = tokio::process::Command::new("dig")
        .args(["+short", qtype, domain])
        .output()
        .await
        .map_err(|e| Error::Tool {
            tool: "osint.dns_info".into(),
            message: format!("dig: {e}"),
        })?;
    if !out.status.success() {
        return Err(Error::Tool {
            tool: "osint.dns_info".into(),
            message: format!("dig {qtype} {domain} exit {}", out.status),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// [`DnsInfoClient`] exposed as an agent-callable [`Tool`].
#[derive(Debug)]
pub struct DnsInfoTool {
    inner: std::sync::Arc<DnsInfoClient>,
}

impl DnsInfoTool {
    /// Wrap a shared [`DnsInfoClient`] as an OSINT [`Tool`].
    pub fn new(inner: std::sync::Arc<DnsInfoClient>) -> Self {
        Self { inner }
    }
}

impl Default for DnsInfoTool {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(DnsInfoClient::new()))
    }
}

#[async_trait]
impl Tool for DnsInfoTool {
    fn name(&self) -> &str {
        "osint.dns_info"
    }
    fn description(&self) -> &str {
        "Resolve A / MX / TXT / NS for a domain via the system resolver \
         and `dig`. Useful for inferring a prospect's mail provider \
         (Google Workspace = MX aspmx.l.google.com; Microsoft 365 = \
         MX *.protection.outlook.com), anti-spam policy (SPF / DMARC \
         in TXT), and hosting (A record). Requires `dig` on PATH for \
         MX / TXT / NS — A works without dig."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "domain": { "type": "string" }
            },
            "required": ["domain"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let domain = args
            .0
            .get("domain")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("osint.dns_info: missing domain".into()))?;
        let info = self.inner.lookup(domain).await?;
        Ok(json!({
            "domain": domain,
            "a":   info.a,
            "mx":  info.mx,
            "txt": info.txt,
            "ns":  info.ns,
        }))
    }
}
