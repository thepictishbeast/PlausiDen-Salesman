//! Decision-maker finder — scrape a company's public team / about /
//! leadership pages for (name, role) pairs, then combine with the
//! email-pattern guesser to produce ranked buyer candidates.
//!
//! BUG ASSUMPTION: the public web is messy. We use simple HTML +
//! text heuristics (look for role keywords near capitalized names),
//! not headless-browser DOM analysis. Recall is what matters; the
//! operator reviews before any address is used. False positives
//! that don't pass operator review do no damage.
//!
//! BUG ASSUMPTION: bot-detection on team pages varies. We send the
//! same User-Agent as HomepageFetcher and respect a 20s timeout.
//! Sites that aggressively block (Cloudflare, etc.) just yield no
//! candidates and the operator falls back to manual research.

use crate::EmailPatternGuesser;
use reqwest::Client;
use salesman_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Duration;
use url::Url;

const UA: &str = "Mozilla/5.0 (compatible; PlausiDen-Salesman/0.1; +https://plausiden.com)";
const TIMEOUT_S: u64 = 20;

/// A single found buyer candidate with role + email guess + confidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuyerCandidate {
    /// Person's name.
    pub name: String,
    /// Their role/title as found on the team page.
    pub role: String,
    /// Best-guess email address.
    pub email: String,
    /// The pattern used to guess `email` (e.g. `first.last`).
    pub email_pattern: String,
    /// 0..1 — combination of role priority + email-pattern prior +
    /// scrape-source confidence.
    pub confidence: f32,
    /// Where we found this person — full URL of the team page.
    pub source_url: String,
    /// Free-form rationale the operator sees in the review.
    pub rationale: String,
}

/// Tiered list of HTML paths we try, after the homepage. First-hit
/// wins per company; we don't crawl deeper.
const TEAM_PATHS: &[&str] = &[
    "/team",
    "/about",
    "/about-us",
    "/leadership",
    "/people",
    "/company",
    "/our-team",
    "/founders",
];

/// Role keywords scored by buyer-fitness for B2B SaaS. Higher score
/// = more likely the actual decision-maker for our pitch. Reorder
/// or extend per vertical if needed (TODO: vertical-pack overrides).
const ROLE_PRIORITY: &[(&str, f32)] = &[
    ("ceo", 0.95),
    ("chief executive", 0.95),
    ("founder", 0.90),
    ("co-founder", 0.90),
    ("cto", 0.85),
    ("chief technology", 0.85),
    ("ciso", 0.92), // security buyer for our cyber pitch
    ("chief information security", 0.92),
    ("vp engineering", 0.80),
    ("vp of engineering", 0.80),
    ("head of engineering", 0.78),
    ("head of security", 0.85),
    ("director of security", 0.78),
    ("president", 0.85),
    ("coo", 0.70),
    ("chief operating", 0.70),
    ("director of it", 0.70),
    ("head of it", 0.70),
    ("vp it", 0.65),
    ("it manager", 0.55),
    ("engineering manager", 0.50),
    // De-prioritize gatekeepers
    ("sales", 0.10),
    ("marketing", 0.05),
    ("hr", 0.05),
    ("recruiter", 0.05),
    ("support", 0.05),
];

/// Scrapes a company team/about page for buyer candidates, pairing
/// each found person with an email guess + confidence.
#[derive(Debug)]
pub struct TeamScraper {
    http: Client,
    guesser: EmailPatternGuesser,
}

impl Default for TeamScraper {
    fn default() -> Self {
        Self::new()
    }
}

impl TeamScraper {
    /// Build a team-page scraper with the crate's default HTTP client
    /// (custom UA, timeout, limited redirects).
    pub fn new() -> Self {
        // SAFETY: rustls + UA + timeout + redirect = build() infallible.
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(TIMEOUT_S))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("reqwest construction infallible with these settings");
        Self {
            http,
            guesser: EmailPatternGuesser::new(),
        }
    }

    /// Find buyer candidates for a company. Tries each TEAM_PATHS
    /// suffix on the homepage URL, scrapes (name, role) pairs from
    /// the first one that returns 200, and joins with the
    /// email-pattern guesser for the company's domain.
    /// Returns at most `max` candidates ranked by confidence.
    pub async fn find_for_company(
        &self,
        company_name: &str,
        homepage: &Url,
        max: usize,
    ) -> Result<Vec<BuyerCandidate>> {
        let domain = match homepage.host_str() {
            Some(h) => h.trim_start_matches("www.").to_string(),
            None => return Ok(vec![]),
        };

        for path in TEAM_PATHS {
            let candidate_url = match build_url(homepage, path) {
                Some(u) => u,
                None => continue,
            };
            let html = match self.fetch(&candidate_url).await {
                Ok(Some(h)) => h,
                _ => continue,
            };
            let pairs = extract_name_role_pairs(&html);
            if pairs.is_empty() {
                continue;
            }
            // We have hits. Build candidates.
            let mut out: Vec<BuyerCandidate> = Vec::new();
            for (name, role) in &pairs {
                let role_score = score_role(role);
                if role_score < 0.10 {
                    // Skip likely gatekeepers / irrelevant roles
                    // unless we have nothing else (handled by max).
                    continue;
                }
                let (first, last) = split_name(name);
                if first.is_empty() {
                    continue;
                }
                let guesses = self.guesser.guess(&first, &last, &domain);
                if let Some(top) = guesses.first() {
                    let confidence = (role_score * 0.6 + top.prior * 0.4).clamp(0.0, 1.0);
                    out.push(BuyerCandidate {
                        name: name.clone(),
                        role: role.clone(),
                        email: top.email.clone(),
                        email_pattern: top.pattern.clone(),
                        confidence,
                        source_url: candidate_url.to_string(),
                        rationale: format!(
                            "Role `{role}` scored {role_score:.2}; pattern `{}` prior {:.2}; \
                             scraped from `{}`. Email is a GUESS — verify before sending.",
                            top.pattern,
                            top.prior,
                            candidate_url.path(),
                        ),
                    });
                }
            }
            out.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            out.truncate(max);
            tracing::info!(
                company = %company_name,
                hits = pairs.len(),
                kept = out.len(),
                "team scraper found candidates"
            );
            return Ok(out);
        }
        Ok(vec![])
    }

    async fn fetch(&self, url: &Url) -> Result<Option<String>> {
        let resp = match self.http.get(url.as_str()).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(url = %url, "%e" = %e, "fetch failed");
                return Ok(None);
            }
        };
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body = resp.text().await.map_err(|e| Error::Tool {
            tool: "discovery.team_scraper".into(),
            message: format!("body: {e}"),
        })?;
        // Cap body to 1 MB to bound work + memory
        let trimmed = if body.len() > 1_048_576 {
            body[..1_048_576].to_string()
        } else {
            body
        };
        Ok(Some(trimmed))
    }
}

fn build_url(base: &Url, path: &str) -> Option<Url> {
    // Resolve the path against the homepage URL (which itself may be
    // a non-trivial URL). We use Url::join to keep scheme/host.
    base.join(path).ok()
}

/// Strip HTML tags and collapse whitespace. Cheap; not a full parser.
/// Good enough to find role keywords near name-shaped tokens.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut prev_space = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
            continue;
        }
        if c == '>' {
            in_tag = false;
            continue;
        }
        if in_tag {
            continue;
        }
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

/// Heuristic: find tokens that look like "Title-Cased Full Name"
/// (2-3 words, first letter caps) and a role keyword within ±60
/// chars. Returns (name, role) pairs deduplicated.
fn extract_name_role_pairs(html: &str) -> Vec<(String, String)> {
    let text = strip_html(html);
    let lc = text.to_ascii_lowercase();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for (role_kw, _score) in ROLE_PRIORITY {
        let mut start = 0usize;
        while let Some(rel) = lc[start..].find(role_kw) {
            let abs = start + rel;
            // Window: 60 chars before, 60 after.
            let lo = abs.saturating_sub(60);
            let hi = (abs + role_kw.len() + 60).min(text.len());
            // We need to clip to char boundaries; brute search.
            let mut a = lo;
            while !text.is_char_boundary(a) && a > 0 {
                a -= 1;
            }
            let mut b = hi;
            while !text.is_char_boundary(b) && b < text.len() {
                b += 1;
            }
            let window = &text[a..b];
            if let Some(name) = first_titlecase_pair_or_triple(window) {
                let key = (name.clone(), role_kw.to_string());
                if seen.insert(key.clone()) {
                    out.push((name, role_kw.to_string()));
                }
            }
            start = abs + role_kw.len();
        }
    }
    out
}

/// Find the first occurrence of "Firstname Lastname" or
/// "Firstname Middlename Lastname" — title-cased ASCII tokens.
fn first_titlecase_pair_or_triple(s: &str) -> Option<String> {
    let toks: Vec<&str> = s
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'')
        .filter(|t| !t.is_empty())
        .collect();
    let is_titlecase = |t: &str| {
        let mut chars = t.chars();
        match chars.next() {
            Some(c) if c.is_ascii_uppercase() => {
                let rest_ok = chars.clone().count() >= 1
                    && chars.all(|c| c.is_ascii_lowercase() || c == '-' || c == '\'');
                rest_ok && t.len() <= 24 && t.len() >= 2
            }
            _ => false,
        }
    };
    let bad_word = |t: &str| {
        let lc = t.to_ascii_lowercase();
        matches!(
            lc.as_str(),
            "the"
                | "and"
                | "for"
                | "our"
                | "your"
                | "our team"
                | "team"
                | "company"
                | "about"
                | "leadership"
                | "founder"
                | "co-founder"
                | "ceo"
                | "cto"
                | "ciso"
                | "vp"
                | "head"
                | "director"
                | "manager"
                | "president"
                | "officer"
        )
    };
    for i in 0..toks.len() {
        if is_titlecase(toks[i]) && !bad_word(toks[i]) {
            // try triple (Firstname Middle Lastname)
            if i + 2 < toks.len()
                && is_titlecase(toks[i + 1])
                && is_titlecase(toks[i + 2])
                && !bad_word(toks[i + 1])
                && !bad_word(toks[i + 2])
            {
                return Some(format!("{} {} {}", toks[i], toks[i + 1], toks[i + 2]));
            }
            // pair
            if i + 1 < toks.len() && is_titlecase(toks[i + 1]) && !bad_word(toks[i + 1]) {
                return Some(format!("{} {}", toks[i], toks[i + 1]));
            }
        }
    }
    None
}

fn split_name(full: &str) -> (String, String) {
    let parts: Vec<&str> = full.split_whitespace().collect();
    if parts.is_empty() {
        return (String::new(), String::new());
    }
    if parts.len() == 1 {
        return (parts[0].to_string(), String::new());
    }
    let first = parts[0].to_string();
    let last = parts[parts.len() - 1].to_string();
    (first, last)
}

fn score_role(role: &str) -> f32 {
    let lc = role.to_ascii_lowercase();
    ROLE_PRIORITY
        .iter()
        .find(|(kw, _)| lc.contains(kw))
        .map(|(_, s)| *s)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_name_handles_three_part() {
        let (f, l) = split_name("Jane Doe Smith");
        assert_eq!(f, "Jane");
        assert_eq!(l, "Smith");
    }

    #[test]
    fn split_name_single_token() {
        let (f, l) = split_name("Madonna");
        assert_eq!(f, "Madonna");
        assert_eq!(l, "");
    }

    #[test]
    fn role_priority_orders_correctly() {
        assert!(score_role("CEO and Founder") > score_role("Marketing Director"));
        assert!(score_role("Chief Information Security Officer") > 0.85);
        assert_eq!(score_role("Janitor"), 0.0);
    }

    #[test]
    fn extracts_name_near_role_keyword() {
        let html = r#"
            <html><body>
                <div class="leadership">
                    <h3>Jane Smith</h3>
                    <p>CEO and Co-Founder</p>
                </div>
                <div class="leadership">
                    <h3>Bob Jones</h3>
                    <p>Marketing Director</p>
                </div>
            </body></html>
        "#;
        let pairs = extract_name_role_pairs(html);
        // Jane should be in the list (CEO match).
        assert!(pairs.iter().any(|(n, r)| n == "Jane Smith" && r == "ceo"));
    }

    #[test]
    fn skips_lone_role_words_without_name() {
        let html = "<div>About our CEO and our team.</div>";
        let pairs = extract_name_role_pairs(html);
        // No name → no pair returned for "ceo".
        assert!(!pairs.iter().any(|(_, r)| r == "ceo"));
    }

    #[test]
    fn strip_html_keeps_text_only() {
        assert_eq!(strip_html("<p>Hi <b>there</b></p>"), "Hi there");
    }

    #[test]
    fn strip_html_collapses_internal_whitespace() {
        assert_eq!(strip_html("<p>Hi  \n\t there</p>"), "Hi there");
    }
}
