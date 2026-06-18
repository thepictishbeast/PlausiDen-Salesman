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
use zeroize::Zeroizing;

const GITHUB_API: &str = "https://api.github.com";
const UA: &str = "PlausiDenSalesman/0.0";

/// A GitHub repository summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRepo {
    /// Repository name.
    pub name: String,
    /// Web URL of the repo.
    pub html_url: String,
    /// Repo description, if any.
    pub description: Option<String>,
    /// Primary language, if detected.
    pub language: Option<String>,
    /// Star count.
    pub stargazers_count: u64,
    /// Last push timestamp (ISO 8601), if available.
    pub pushed_at: Option<String>,
    /// Whether the repo is a fork.
    pub fork: bool,
}

/// A GitHub organization profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubOrg {
    /// Org login/slug.
    pub login: String,
    /// Display name, if set.
    pub name: Option<String>,
    /// Org description, if set.
    pub description: Option<String>,
    /// Blog/website URL, if set.
    pub blog: Option<String>,
    /// Location, if set.
    pub location: Option<String>,
    /// Number of public repositories.
    pub public_repos: u64,
    /// Follower count.
    pub followers: u64,
    /// Org creation timestamp (ISO 8601).
    pub created_at: String,
}

/// Client for the GitHub REST org/repos endpoints.
pub struct GithubOrgClient {
    http: reqwest::Client,
    /// GitHub PAT. Zeroized on drop; never logged. A derived `Debug` would
    /// print the token (CLAUDE.md: no secrets in logs), so `Debug` is
    /// implemented manually below to redact it.
    token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for GithubOrgClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubOrgClient")
            .field("http", &self.http)
            // Reveal presence, never the value.
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
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
            token: token.map(Zeroizing::new),
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
            req = req.bearer_auth(t.as_str());
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

/// [`GithubOrgClient`] exposed as an agent-callable [`Tool`].
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_token() {
        let c = GithubOrgClient::new(Some("ghp_supersecrettoken123".into()));
        let dbg = format!("{c:?}");
        assert!(
            !dbg.contains("ghp_supersecrettoken123"),
            "Debug leaks token: {dbg}"
        );
        assert!(dbg.contains("<redacted>"));
        // The Tool wrapper's derived Debug delegates to the manual impl above,
        // so it must not leak the token either.
        let tool = GithubOrgTool::new(std::sync::Arc::new(c));
        assert!(!format!("{tool:?}").contains("ghp_supersecrettoken123"));
    }

    #[test]
    fn no_token_debug_shows_none() {
        let c = GithubOrgClient::new(None);
        assert!(format!("{c:?}").contains("token: None"));
    }
}
