//! GDELT 2.0 DOC API — global news mentions, no auth required.
//! https://api.gdeltproject.org/api/v2/doc/doc

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const GDELT_URL: &str = "https://api.gdeltproject.org/api/v2/doc/doc";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsHit {
    pub title: String,
    pub url: String,
    pub seen_at: String,
    pub source_country: Option<String>,
}

#[derive(Debug)]
pub struct GdeltClient {
    http: reqwest::Client,
}

impl Default for GdeltClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GdeltClient {
    /// Build a GDELT (global news) API client.
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest construction infallible"),
        }
    }

    /// Search news articles. `query` should already be quoted if it
    /// contains spaces (e.g. `"plausiden ai"`). `count` 1..=250.
    pub async fn search_news(&self, query: &str, count: u32) -> Result<Vec<NewsHit>> {
        let resp = self
            .http
            .get(GDELT_URL)
            .query(&[
                ("query", query),
                ("mode", "ArtList"),
                ("format", "json"),
                ("maxrecords", &count.to_string()),
                ("sort", "DateDesc"),
            ])
            .send()
            .await
            .map_err(|e| Error::Tool {
                tool: "osint.gdelt".into(),
                message: format!("transport: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(Error::Tool {
                tool: "osint.gdelt".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }
        let body: GdeltResponse = resp.json().await.map_err(|e| Error::Tool {
            tool: "osint.gdelt".into(),
            message: format!("decode: {e}"),
        })?;
        Ok(body
            .articles
            .unwrap_or_default()
            .into_iter()
            .map(|a| NewsHit {
                title: a.title,
                url: a.url,
                seen_at: a.seendate,
                source_country: a.sourcecountry,
            })
            .collect())
    }
}

#[derive(Debug)]
pub struct GdeltTool {
    inner: std::sync::Arc<GdeltClient>,
}

impl GdeltTool {
    /// Wrap a shared [`GdeltClient`] as an OSINT [`Tool`].
    pub fn new(inner: std::sync::Arc<GdeltClient>) -> Self {
        Self { inner }
    }
}

impl Default for GdeltTool {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(GdeltClient::new()))
    }
}

#[async_trait]
impl Tool for GdeltTool {
    fn name(&self) -> &str {
        "osint.recent_news"
    }
    fn description(&self) -> &str {
        "Fetch recent news articles mentioning a query, via GDELT 2.0 (no auth, free)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "count": { "type": "integer", "minimum": 1, "maximum": 250, "default": 25 }
            },
            "required": ["query"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let q = args
            .0
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("osint.recent_news: missing query".into()))?;
        let count = args
            .0
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(25)
            .clamp(1, 250) as u32;
        let hits = self.inner.search_news(q, count).await?;
        Ok(json!({ "count": hits.len(), "hits": hits }))
    }
}

#[derive(Debug, Deserialize)]
struct GdeltResponse {
    #[serde(default)]
    articles: Option<Vec<GdeltArticle>>,
}

#[derive(Debug, Deserialize)]
struct GdeltArticle {
    title: String,
    url: String,
    seendate: String,
    #[serde(default)]
    sourcecountry: Option<String>,
}
