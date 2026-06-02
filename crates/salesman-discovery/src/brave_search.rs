//! Brave Search REST adapter.
//!
//! Wire format: https://api.search.brave.com/res/v1/web/search
//! Auth: `X-Subscription-Token: <key>` header.
//!
//! BUG ASSUMPTION: free-tier quota is ~2000 queries/month at 1 QPS.
//! We don't enforce quota here (the caller has to track), but we DO
//! self-throttle to 1 QPS via a tokio interval. Bursts will block
//! rather than 429 the API.
//!
//! SECURITY: API key in `Zeroizing<String>`, sent only as a header,
//! never logged.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const SELF_THROTTLE: Duration = Duration::from_millis(1100);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub description: String,
    /// Some hits are FAQs / videos / news; for B2B prospect discovery
    /// we typically want `web` results only, but we surface this so
    /// the caller can filter.
    pub kind: String,
}

#[derive(Debug)]
pub struct BraveSearch {
    api_key: Zeroizing<String>,
    http: reqwest::Client,
    last_request: Mutex<Option<std::time::Instant>>,
}

impl BraveSearch {
    /// Build a Brave Search client with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: Zeroizing::new(api_key.into()),
            // SAFETY: rustls + single timeout — build() cannot fail.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client construction is infallible"),
            last_request: Mutex::new(None),
        }
    }

    /// Build a Brave Search client, reading the key from
    /// `BRAVE_SEARCH_API_KEY` (errors if unset).
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("BRAVE_SEARCH_API_KEY")
            .map_err(|_| Error::Config("BRAVE_SEARCH_API_KEY not set".into()))?;
        Ok(Self::new(key))
    }

    /// One web search call. Returns up to `count` hits.
    pub async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        // self-throttle to 1 QPS
        {
            let mut last = self.last_request.lock().await;
            if let Some(t) = *last {
                let elapsed = t.elapsed();
                if elapsed < SELF_THROTTLE {
                    tokio::time::sleep(SELF_THROTTLE - elapsed).await;
                }
            }
            *last = Some(std::time::Instant::now());
        }

        let resp = self
            .http
            .get(BRAVE_SEARCH_URL)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "gzip")
            .header("X-Subscription-Token", &**self.api_key)
            .query(&[
                ("q", query),
                ("count", &count.to_string()),
                ("safesearch", "moderate"),
            ])
            .send()
            .await
            .map_err(|e| Error::Tool {
                tool: "brave_search".into(),
                message: format!("transport: {e}"),
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Tool {
                tool: "brave_search".into(),
                message: format!("HTTP {status}: {}", truncate(&text, 300)),
            });
        }

        let body: BraveResponse = resp.json().await.map_err(|e| Error::Tool {
            tool: "brave_search".into(),
            message: format!("decode: {e}"),
        })?;

        let mut out = Vec::new();
        for r in body.web.results {
            out.push(SearchHit {
                title: strip_html(&r.title),
                url: r.url,
                description: strip_html(&r.description),
                kind: "web".into(),
            });
        }
        Ok(out)
    }
}

#[derive(Debug)]
pub struct BraveSearchTool {
    inner: std::sync::Arc<BraveSearch>,
}

impl BraveSearchTool {
    /// Wrap a shared [`BraveSearch`] client as a discovery [`Tool`].
    pub fn new(inner: std::sync::Arc<BraveSearch>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Tool for BraveSearchTool {
    fn name(&self) -> &str {
        "discovery.brave_search"
    }
    fn description(&self) -> &str {
        "Search the web via the Brave Search API. Returns a list of \
         {title, url, description}. Self-throttled to 1 QPS."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "q":     { "type": "string", "description": "search query" },
                "count": { "type": "integer", "minimum": 1, "maximum": 20, "default": 10 }
            },
            "required": ["q"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let q = args
            .0
            .get("q")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("brave_search: missing q".into()))?;
        let count = args
            .0
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 20) as u32;
        let hits = self.inner.search(q, count).await?;
        Ok(json!({ "count": hits.len(), "hits": hits }))
    }
}

// ---------------------------------------------------------------------------
// wire shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: WebResults,
}

#[derive(Debug, Default, Deserialize)]
struct WebResults {
    #[serde(default)]
    results: Vec<WebResult>,
}

#[derive(Debug, Deserialize)]
struct WebResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

fn strip_html(s: &str) -> String {
    // Brave returns HTML-tagged snippets ("<strong>x</strong>"); strip
    // tags for clean storage. Cheap regex-free implementation.
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}...", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_html_tags() {
        assert_eq!(strip_html("<strong>hi</strong> there"), "hi there");
        assert_eq!(strip_html("plain text"), "plain text");
        assert_eq!(strip_html("<a href=\"x\">link</a>"), "link");
    }
}
