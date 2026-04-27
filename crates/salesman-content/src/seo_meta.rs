//! SEO meta-tag generator. Given a draft page + target query, produces
//! title (≤60 chars), meta description (≤160 chars), open-graph
//! fields, and JSON-LD Article schema.
//!
//! Pure deterministic where possible — only the LLM call generates the
//! title + description. JSON-LD is templated.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeoMeta {
    pub title: String,
    pub description: String,
}

#[derive(Debug)]
pub struct SeoMetaTool {
    router: Arc<LlmRouter>,
    site_name: String,
    site_origin: String,
}

impl SeoMetaTool {
    pub fn new(
        router: Arc<LlmRouter>,
        site_name: impl Into<String>,
        site_origin: impl Into<String>,
    ) -> Self {
        Self {
            router,
            site_name: site_name.into(),
            site_origin: site_origin.into(),
        }
    }
}

#[async_trait]
impl Tool for SeoMetaTool {
    fn name(&self) -> &str {
        "content.seo_meta"
    }

    fn description(&self) -> &str {
        "Generate SEO meta tags for a page. Returns: { title (<=60 \
         chars), description (<=160 chars), opengraph fields, JSON-LD \
         Article schema }. Title + description are LLM-generated; \
         JSON-LD is templated."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "page_title":   { "type": "string" },
                "page_summary": { "type": "string" },
                "target_query": { "type": "string", "description": "the search query the page should rank for" },
                "slug":         { "type": "string" },
                "published":    { "type": ["string", "null"], "description": "ISO-8601 date" }
            },
            "required": ["page_title", "page_summary", "target_query", "slug"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let page_title = require_str(&args, "page_title")?;
        let page_summary = require_str(&args, "page_summary")?;
        let target_query = require_str(&args, "target_query")?;
        let slug = require_str(&args, "slug")?;
        let published = args.0.get("published").and_then(|v| v.as_str()).unwrap_or("");

        let system = [
            "You write SEO meta tags. Output STRICT JSON.",
            "Constraints:",
            "- title <= 60 chars (HARD).",
            "- description <= 160 chars (HARD).",
            "- title MUST contain the target query (or a near match).",
            "- description MUST be a real preview, not marketing fluff.",
            "- No emoji. No clickbait. No all-caps.",
            "- One brand mention is fine. No repeating it.",
            "Output: {\"title\": string, \"description\": string}",
        ]
        .join("\n");

        let user = format!(
            "Page title: {page_title}\nPage summary: {page_summary}\nTarget query: {target_query}\n\
             Brand: {brand}\n\nWrite the meta tags now.",
            brand = self.site_name,
        );

        let req = ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: system,
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: user,
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            tools: vec![],
            max_tokens: 256,
            temperature: 0.2,
        };

        let resp = self.router.chat_for(RouteHint::Reasoning, "seo_meta", req).await?;
        let raw = resp.message.content.trim();
        let meta = parse_seo(raw)
            .map_err(|e| Error::Tool {
                tool: "content.seo_meta".into(),
                message: format!("parse: {e}"),
            })?;

        // Hard-truncate just in case.
        let title = truncate(&meta.title, 60);
        let description = truncate(&meta.description, 160);

        let url = format!("{}/{}", self.site_origin.trim_end_matches('/'), slug);
        let json_ld = json!({
            "@context": "https://schema.org",
            "@type": "Article",
            "headline": title,
            "description": description,
            "url": url,
            "datePublished": published,
            "publisher": { "@type": "Organization", "name": self.site_name },
        });

        Ok(json!({
            "title": title,
            "description": description,
            "opengraph": {
                "og:type":        "article",
                "og:title":       title,
                "og:description": description,
                "og:url":         url,
                "og:site_name":   self.site_name,
            },
            "twitter": {
                "twitter:card":        "summary_large_image",
                "twitter:title":       title,
                "twitter:description": description,
            },
            "json_ld": json_ld,
            "model_latency_ms": resp.usage.latency_ms,
        }))
    }
}

fn parse_seo(raw: &str) -> std::result::Result<SeoMeta, String> {
    if let Ok(s) = serde_json::from_str::<SeoMeta>(raw) {
        return Ok(s);
    }
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(s) = serde_json::from_str::<SeoMeta>(stripped) {
        return Ok(s);
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}')) {
        if e > s {
            if let Ok(parsed) = serde_json::from_str::<SeoMeta>(&raw[s..=e]) {
                return Ok(parsed);
            }
        }
    }
    Err("not parseable as SeoMeta JSON".into())
}

fn require_str(args: &ToolArgs, key: &str) -> Result<String> {
    args.0
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::Validation(format!("missing `{key}`")))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i >= n - 1 { break; }
            out.push(c);
        }
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_chars_not_bytes() {
        assert_eq!(truncate("hello", 10), "hello");
        let t = truncate("abcdefghijklmnop", 5);
        assert_eq!(t.chars().count(), 5);
    }

    #[test]
    fn parses_seo_json() {
        let raw = r#"{"title":"x","description":"y"}"#;
        let s = parse_seo(raw).unwrap();
        assert_eq!(s.title, "x");
    }
}
