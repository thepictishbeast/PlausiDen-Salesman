//! Tool wrappers so the agent loop can call CSV seeding and homepage
//! fetching as first-class actions.

use crate::{CsvSeed, HomepageFetcher};
use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

#[derive(Debug)]
pub struct CsvSeedTool {
    seed: CsvSeed,
}

impl Default for CsvSeedTool {
    fn default() -> Self {
        Self::new()
    }
}

impl CsvSeedTool {
    /// Build the CSV-seed discovery [`Tool`].
    pub fn new() -> Self {
        Self {
            seed: CsvSeed::new(),
        }
    }
}

#[async_trait]
impl Tool for CsvSeedTool {
    fn name(&self) -> &str {
        "discovery.csv_seed"
    }

    fn description(&self) -> &str {
        "Read an operator-supplied CSV of companies. Required column: \
         `display_name`. Optional: `homepage`, `industry`, `region`, \
         `description`, `legal_name`, `size_band`. Returns the parsed \
         companies as JSON; the orchestrator decides which to persist."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Filesystem path to the CSV." }
            },
            "required": ["path"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let path = args
            .0
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("discovery.csv_seed: missing `path`".into()))?;
        let companies = self.seed.read_path(&PathBuf::from(path))?;
        Ok(json!({ "count": companies.len(), "companies": companies }))
    }
}

#[derive(Debug)]
pub struct HomepageFetchTool {
    fetcher: Arc<HomepageFetcher>,
}

impl Default for HomepageFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl HomepageFetchTool {
    /// Build the homepage-fetch discovery [`Tool`].
    pub fn new() -> Self {
        Self {
            fetcher: Arc::new(HomepageFetcher::new()),
        }
    }
}

#[async_trait]
impl Tool for HomepageFetchTool {
    fn name(&self) -> &str {
        "discovery.homepage_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a company's homepage and extract title, meta description, \
         meta keywords, and tech-stack fingerprints. Returns JSON."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" }
            },
            "required": ["url"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let url_str =
            args.0.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::Validation("discovery.homepage_fetch: missing `url`".into())
            })?;
        let url = Url::parse(url_str).map_err(|e| Error::Validation(format!("url: {e}")))?;
        let facts = self.fetcher.fetch(&url).await?;
        Ok(json!({
            "final_url": facts.final_url.as_str(),
            "status": facts.status,
            "title": facts.title,
            "meta_description": facts.meta_description,
            "meta_keywords": facts.meta_keywords,
            "tech_signals": facts.tech_signals,
            "html_bytes": facts.html_bytes,
        }))
    }
}
