//! Tool wrappers so the agent loop can call CSV seeding and homepage
//! fetching as first-class actions.

use crate::{CsvSeed, HomepageFetcher, IMPORT_DIR_ENV, ImportRoot};
use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde_json::{Value, json};
use std::sync::Arc;
use url::Url;

/// [`CsvSeed`] exposed as an agent-callable [`Tool`], confined to the
/// operator import directory.
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
        "Read an operator-supplied CSV of companies by filename. The \
         file must live inside the operator's import directory \
         (SALESMAN_IMPORT_DIR); `name` is resolved relative to it and \
         cannot escape it. Required column: `display_name`. Optional: \
         `homepage`, `industry`, `region`, `description`, `legal_name`, \
         `size_band`. Returns the parsed companies as JSON; the \
         orchestrator decides which to persist."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "CSV filename relative to SALESMAN_IMPORT_DIR (no absolute paths, no `..`)."
                }
            },
            "required": ["name"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        // The agent picks this name, so it is untrusted: confine it to
        // the operator's import directory. Unset dir => fail closed.
        let name = args
            .0
            .get("name")
            // Back-compat: accept the legacy `path` key as an alias.
            .or_else(|| args.0.get("path"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("discovery.csv_seed: missing `name`".into()))?;
        let resolved = ImportRoot::from_env()?.resolve(name)?;
        let companies = self.seed.read_path(&resolved)?;
        Ok(json!({
            "count": companies.len(),
            "import_dir_env": IMPORT_DIR_ENV,
            "companies": companies,
        }))
    }
}

/// [`HomepageFetcher`] exposed as an agent-callable [`Tool`].
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
