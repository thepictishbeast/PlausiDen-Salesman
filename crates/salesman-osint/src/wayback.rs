//! Wayback Machine availability adapter.
//! https://archive.org/wayback/available?url=...&timestamp=...
//!
//! Useful when qualifying a prospect: how long has their site been
//! up? Does the homepage look stable, or have they been pivoting?

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const URL: &str = "https://archive.org/wayback/available";
const UA: &str = "PlausiDenSalesman/0.0 (+https://plausiden.com/bots; civic-research)";

/// A Wayback Machine snapshot reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaybackSnapshot {
    /// URL of the archived snapshot.
    pub url: String,
    /// Snapshot timestamp (YYYYMMDDhhmmss).
    pub timestamp: String,
    /// Whether the snapshot is available.
    pub available: bool,
}

/// Client for the Internet Archive Wayback availability API.
#[derive(Debug)]
pub struct WaybackClient {
    http: reqwest::Client,
}

impl Default for WaybackClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WaybackClient {
    /// Build a Wayback Machine (web.archive.org) client.
    pub fn new() -> Self {
        Self {
            // SAFETY: reqwest::Client::builder().build() only fails on
            // TLS backend mis-configuration or unsupported feature
            // combos. We use the workspace's rustls-tls backend with
            // default options that are known-good — the only inputs
            // are user_agent + timeout, neither of which can drive a
            // build failure.
            http: reqwest::Client::builder()
                .user_agent(UA)
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest construction infallible"),
        }
    }

    /// `target_url`: the URL to look up.
    /// `timestamp`: optional YYYYMMDD or longer. If omitted, returns
    /// the most recent snapshot.
    pub async fn snapshot(
        &self,
        target_url: &str,
        timestamp: Option<&str>,
    ) -> Result<Option<WaybackSnapshot>> {
        let mut req = self.http.get(URL).query(&[("url", target_url)]);
        if let Some(ts) = timestamp {
            req = req.query(&[("timestamp", ts)]);
        }
        let resp = req.send().await.map_err(|e| Error::Tool {
            tool: "osint.wayback".into(),
            message: format!("transport: {e}"),
        })?;
        if !resp.status().is_success() {
            return Err(Error::Tool {
                tool: "osint.wayback".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }
        let body: WaybackResponse = resp.json().await.map_err(|e| Error::Tool {
            tool: "osint.wayback".into(),
            message: format!("decode: {e}"),
        })?;
        Ok(body
            .archived_snapshots
            .and_then(|s| s.closest)
            .map(|c| WaybackSnapshot {
                url: c.url,
                timestamp: c.timestamp,
                available: c.available,
            }))
    }
}

/// [`WaybackClient`] exposed as an agent-callable [`Tool`].
#[derive(Debug)]
pub struct WaybackTool {
    inner: std::sync::Arc<WaybackClient>,
}

impl WaybackTool {
    /// Wrap a shared [`WaybackClient`] as an OSINT [`Tool`].
    pub fn new(inner: std::sync::Arc<WaybackClient>) -> Self {
        Self { inner }
    }
}

impl Default for WaybackTool {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(WaybackClient::new()))
    }
}

#[async_trait]
impl Tool for WaybackTool {
    fn name(&self) -> &str {
        "osint.wayback"
    }
    fn description(&self) -> &str {
        "Look up the closest Internet Archive Wayback Machine snapshot \
         for a URL. Optional timestamp (YYYYMMDD) selects the closest \
         snapshot to that date — omit for most recent. Useful for \
         qualifying how long a prospect's site has been live."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url":       { "type": "string", "format": "uri" },
                "timestamp": { "type": "string", "description": "Optional YYYYMMDD" }
            },
            "required": ["url"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let target = args
            .0
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("osint.wayback: missing url".into()))?;
        let ts = args.0.get("timestamp").and_then(|v| v.as_str());
        match self.inner.snapshot(target, ts).await? {
            Some(s) => Ok(json!({
                "found": true,
                "url": s.url,
                "timestamp": s.timestamp,
                "available": s.available,
            })),
            None => Ok(json!({ "found": false })),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WaybackResponse {
    #[serde(default)]
    archived_snapshots: Option<ArchivedSnapshots>,
}

#[derive(Debug, Deserialize)]
struct ArchivedSnapshots {
    #[serde(default)]
    closest: Option<ClosestSnapshot>,
}

#[derive(Debug, Deserialize)]
struct ClosestSnapshot {
    url: String,
    timestamp: String,
    #[serde(default)]
    available: bool,
}
