//! HackerNews mention search via the Algolia-backed search API.
//! https://hn.algolia.com/api

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const HN_API: &str = "https://hn.algolia.com/api/v1/search";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnHit {
    pub object_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub points: Option<i64>,
    pub author: Option<String>,
    pub story_text: Option<String>,
    pub comment_text: Option<String>,
    pub created_at: String,
    pub story_url: String,
}

#[derive(Debug)]
pub struct HnClient {
    http: reqwest::Client,
}

impl Default for HnClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HnClient {
    /// Build a Hacker News (Firebase API) client.
    pub fn new() -> Self {
        Self {
            // SAFETY: rustls-tls backend with default options + a
            // single timeout setter — none of these can drive
            // Client::build() to fail.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest construction infallible"),
        }
    }

    /// Search HN. `query` is a free-text query; HN search treats it
    /// as keywords. Up to 1000 hits available; we cap at 100 by
    /// default to keep tokens sane.
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<HnHit>> {
        let resp = self
            .http
            .get(HN_API)
            .query(&[
                ("query", query),
                ("hitsPerPage", &limit.to_string()),
                ("tags", "story,comment"),
            ])
            .send()
            .await
            .map_err(|e| Error::Tool {
                tool: "osint.hn".into(),
                message: format!("transport: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(Error::Tool {
                tool: "osint.hn".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }
        let body: HnResponse = resp.json().await.map_err(|e| Error::Tool {
            tool: "osint.hn".into(),
            message: format!("decode: {e}"),
        })?;
        Ok(body
            .hits
            .into_iter()
            .map(|h| HnHit {
                object_id: h.object_id.clone(),
                title: h.title,
                url: h.url,
                points: h.points,
                author: h.author,
                story_text: h.story_text,
                comment_text: h.comment_text,
                created_at: h.created_at,
                story_url: format!("https://news.ycombinator.com/item?id={}", h.object_id),
            })
            .collect())
    }
}

#[derive(Debug)]
pub struct HnTool {
    inner: std::sync::Arc<HnClient>,
}

impl HnTool {
    /// Wrap a shared [`HnClient`] as an OSINT [`Tool`].
    pub fn new(inner: std::sync::Arc<HnClient>) -> Self {
        Self { inner }
    }
}

impl Default for HnTool {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(HnClient::new()))
    }
}

#[async_trait]
impl Tool for HnTool {
    fn name(&self) -> &str {
        "osint.hn_mentions"
    }
    fn description(&self) -> &str {
        "Search HackerNews stories + comments for mentions of a query. \
         Useful for brand-monitoring + warm-lead signal (someone \
         mentioning a competitor in a thread you can engage with)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 25 }
            },
            "required": ["query"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let q = args
            .0
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("osint.hn_mentions: missing query".into()))?;
        let limit = args
            .0
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(25)
            .clamp(1, 200) as u32;
        let hits = self.inner.search(q, limit).await?;
        Ok(json!({ "count": hits.len(), "hits": hits }))
    }
}

#[derive(Debug, Deserialize)]
struct HnResponse {
    hits: Vec<HnHitRaw>,
}

#[derive(Debug, Deserialize)]
struct HnHitRaw {
    #[serde(rename = "objectID")]
    object_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    story_text: Option<String>,
    #[serde(default)]
    comment_text: Option<String>,
    created_at: String,
}
