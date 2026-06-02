//! GitHub REST API adapter for org discovery.
//!
//! Works unauthenticated (60 req/h) or with a PAT (5000 req/h via
//! `GITHUB_TOKEN` env). Useful signal for tech-shop prospects:
//! activity level, languages, recent commits.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const GITHUB_API: &str = "https://api.github.com";
const UA: &str = "PlausiDenSalesman/0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRepo {
    pub name: String,
    pub html_url: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub stargazers_count: u64,
    pub pushed_at: Option<String>,
    pub fork: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubOrg {
    pub login: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub blog: Option<String>,
    pub location: Option<String>,
    pub public_repos: u64,
    pub followers: u64,
    pub created_at: String,
}

#[derive(Debug)]
pub struct GithubOrgClient {
    http: reqwest::Client,
    token: Option<String>,
}

impl Default for GithubOrgClient {
    fn default() -> Self {
        Self::new(std::env::var("GITHUB_TOKEN").ok())
    }
}

impl GithubOrgClient {
    /// Build a GitHub org client. A `token` raises the API rate limit;
    /// `None` uses unauthenticated requests.
    pub fn new(token: Option<String>) -> Self {
        Self {
            // SAFETY: rustls-tls + single timeout setter; reqwest's
            // Client::build() can only fail on TLS-backend mis-config,
            // not present in this configuration.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest construction infallible"),
            token,
        }
    }

    fn req(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{GITHUB_API}{path}");
        let mut req = self
            .http
            .get(&url)
            .header("User-Agent", UA)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        req
    }

    /// Fetch the [`GithubOrg`] profile for the org `slug`.
    pub async fn org(&self, slug: &str) -> Result<GithubOrg> {
        let resp = self
            .req(&format!("/orgs/{slug}"))
            .send()
            .await
            .map_err(|e| Error::Tool {
                tool: "osint.github_org".into(),
                message: format!("transport: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(Error::Tool {
                tool: "osint.github_org".into(),
                message: format!("org `{slug}`: HTTP {}", resp.status()),
            });
        }
        resp.json().await.map_err(|e| Error::Tool {
            tool: "osint.github_org".into(),
            message: format!("decode: {e}"),
        })
    }

    /// Fetch up to `limit` of org `slug`'s most-starred public repos.
    pub async fn top_repos(&self, slug: &str, limit: u32) -> Result<Vec<GithubRepo>> {
        let resp = self
            .req(&format!(
                "/orgs/{slug}/repos?per_page={limit}&sort=pushed&direction=desc"
            ))
            .send()
            .await
            .map_err(|e| Error::Tool {
                tool: "osint.github_org".into(),
                message: format!("transport: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(Error::Tool {
                tool: "osint.github_org".into(),
                message: format!("repos `{slug}`: HTTP {}", resp.status()),
            });
        }
        resp.json().await.map_err(|e| Error::Tool {
            tool: "osint.github_org".into(),
            message: format!("decode: {e}"),
        })
    }
}

#[derive(Debug)]
pub struct GithubOrgTool {
    inner: std::sync::Arc<GithubOrgClient>,
}

impl GithubOrgTool {
    /// Wrap a shared [`GithubOrgClient`] as an OSINT [`Tool`].
    pub fn new(inner: std::sync::Arc<GithubOrgClient>) -> Self {
        Self { inner }
    }
}

impl Default for GithubOrgTool {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(GithubOrgClient::default()))
    }
}

#[async_trait]
impl Tool for GithubOrgTool {
    fn name(&self) -> &str {
        "osint.github_org"
    }
    fn description(&self) -> &str {
        "Look up a GitHub org + its top recently-pushed repos. Returns \
         {org: {...}, repos: [...]} — useful for assessing tech-shop \
         prospects' activity level + tech stack."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug":  { "type": "string" },
                "repos": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 }
            },
            "required": ["slug"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let slug = args
            .0
            .get("slug")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("osint.github_org: missing slug".into()))?;
        let repos = args
            .0
            .get("repos")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 100) as u32;

        let org = self.inner.org(slug).await?;
        let top = self.inner.top_repos(slug, repos).await?;
        Ok(json!({
            "org": org,
            "repos": top,
        }))
    }
}
