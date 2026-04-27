//! Wikipedia REST v1 summary adapter — no auth, generous rate limits.
//! https://en.wikipedia.org/api/rest_v1/page/summary/{title}
//!
//! Useful for company-background context: the LLM can use the summary
//! to qualify whether a prospect company is who we think it is.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const SUMMARY_BASE: &str = "https://en.wikipedia.org/api/rest_v1/page/summary/";
const UA: &str = "PlausiDenSalesman/0.0 (+https://plausiden.com/bots; civic-research)";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikipediaSummary {
    pub title: String,
    pub extract: String,
    pub url: String,
    pub disambiguation: bool,
}

#[derive(Debug)]
pub struct WikipediaClient {
    http: reqwest::Client,
}

impl Default for WikipediaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WikipediaClient {
    pub fn new() -> Self {
        Self {
            // SAFETY: rustls-tls + UA + timeout — known-good combo;
            // reqwest::Client::build() cannot fail on these inputs.
            http: reqwest::Client::builder()
                .user_agent(UA)
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest construction infallible"),
        }
    }

    pub async fn summary(&self, title: &str) -> Result<Option<WikipediaSummary>> {
        let url = format!("{SUMMARY_BASE}{}", urlencode_path(title));
        let resp = self.http.get(&url).send().await.map_err(|e| Error::Tool {
            tool: "osint.wikipedia".into(),
            message: format!("transport: {e}"),
        })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(Error::Tool {
                tool: "osint.wikipedia".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }
        let body: WikiResponse = resp.json().await.map_err(|e| Error::Tool {
            tool: "osint.wikipedia".into(),
            message: format!("decode: {e}"),
        })?;
        Ok(Some(WikipediaSummary {
            title: body.title,
            extract: body.extract.unwrap_or_default(),
            url: body
                .content_urls
                .and_then(|c| c.desktop)
                .map(|d| d.page)
                .unwrap_or_default(),
            disambiguation: body.r#type.as_deref() == Some("disambiguation"),
        }))
    }
}

#[derive(Debug)]
pub struct WikipediaTool {
    inner: std::sync::Arc<WikipediaClient>,
}

impl WikipediaTool {
    pub fn new(inner: std::sync::Arc<WikipediaClient>) -> Self {
        Self { inner }
    }
}

impl Default for WikipediaTool {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(WikipediaClient::new()))
    }
}

#[async_trait]
impl Tool for WikipediaTool {
    fn name(&self) -> &str {
        "osint.wikipedia"
    }
    fn description(&self) -> &str {
        "Look up a Wikipedia article summary by title (e.g. 'Stripe (company)'). \
         Returns the lede paragraph + URL. Disambiguation pages are flagged."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Wikipedia article title (case-sensitive)" }
            },
            "required": ["title"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let title = args
            .0
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("osint.wikipedia: missing title".into()))?;
        match self.inner.summary(title).await? {
            Some(s) => Ok(json!({
                "title": s.title,
                "extract": s.extract,
                "url": s.url,
                "disambiguation": s.disambiguation,
            })),
            None => Ok(json!({ "title": title, "found": false })),
        }
    }
}

fn urlencode_path(s: &str) -> String {
    // Conservative encode: replace spaces + a few reserved chars.
    s.chars()
        .flat_map(|c| match c {
            ' ' => "%20".chars().collect::<Vec<_>>(),
            '/' => "%2F".chars().collect::<Vec<_>>(),
            '?' => "%3F".chars().collect::<Vec<_>>(),
            '#' => "%23".chars().collect::<Vec<_>>(),
            '&' => "%26".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct WikiResponse {
    title: String,
    #[serde(default)]
    extract: Option<String>,
    #[serde(default)]
    content_urls: Option<ContentUrls>,
    #[serde(default)]
    r#type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentUrls {
    #[serde(default)]
    desktop: Option<DesktopUrls>,
}

#[derive(Debug, Deserialize)]
struct DesktopUrls {
    page: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_handles_spaces() {
        assert_eq!(urlencode_path("Stripe (company)"), "Stripe%20(company)");
        assert_eq!(urlencode_path("foo/bar"), "foo%2Fbar");
    }
}
