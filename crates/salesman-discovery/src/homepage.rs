use salesman_core::model::TechSignal;
use salesman_core::{Error, Result};
use scraper::{Html, Selector};
use std::time::Duration;
use url::Url;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const TIMEOUT_S: u64 = 15;
const UA: &str = "PlausiDenSalesman/0.0 (+https://plausiden.com/bots; civic-research)";

/// Pulled facts about a company's homepage. Compact enough to fit in
/// a `description` + a small list of `TechSignal`s.
#[derive(Debug, Clone)]
pub struct HomepageFacts {
    pub final_url: Url,
    pub status: u16,
    pub title: Option<String>,
    pub meta_description: Option<String>,
    pub meta_keywords: Vec<String>,
    pub tech_signals: Vec<TechSignal>,
    pub html_bytes: usize,
}

#[derive(Debug)]
pub struct HomepageFetcher {
    http: reqwest::Client,
}

impl Default for HomepageFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HomepageFetcher {
    pub fn new() -> Self {
        // SAFETY: rustls-tls + UA + timeout + a known-good redirect
        // policy — none of these inputs can drive Client::build()
        // to fail in practice.
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(TIMEOUT_S))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("reqwest client construction is infallible with these settings");
        Self { http }
    }

    pub async fn fetch(&self, url: &Url) -> Result<HomepageFacts> {
        let resp = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|e| Error::Tool {
                tool: "homepage.fetch".into(),
                message: format!("transport: {e}"),
            })?;

        let status = resp.status().as_u16();
        let final_url = resp.url().clone();

        // Cap response size to avoid memory blowups on large landing pages.
        let bytes = resp.bytes().await.map_err(|e| Error::Tool {
            tool: "homepage.fetch".into(),
            message: format!("read body: {e}"),
        })?;
        if bytes.len() > MAX_BYTES {
            return Err(Error::Tool {
                tool: "homepage.fetch".into(),
                message: format!("body {} bytes exceeds cap {MAX_BYTES}", bytes.len()),
            });
        }

        let html = String::from_utf8_lossy(&bytes);
        let document = Html::parse_document(&html);

        let title = select_one_text(&document, "title");
        let meta_description = select_meta(&document, "description");
        let meta_keywords = select_meta(&document, "keywords")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let tech_signals = detect_tech_signals(&html);

        Ok(HomepageFacts {
            final_url,
            status,
            title,
            meta_description,
            meta_keywords,
            tech_signals,
            html_bytes: bytes.len(),
        })
    }
}

fn select_one_text(doc: &Html, sel: &str) -> Option<String> {
    let s = Selector::parse(sel).ok()?;
    doc.select(&s).next().map(|el| {
        el.text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn select_meta(doc: &Html, name: &str) -> Option<String> {
    let s = Selector::parse(&format!(r#"meta[name="{name}"]"#)).ok()?;
    if let Some(el) = doc.select(&s).next() {
        return el.value().attr("content").map(String::from);
    }
    let s = Selector::parse(&format!(r#"meta[property="og:{name}"]"#)).ok()?;
    doc.select(&s)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(String::from)
}

/// Lightweight tech-stack fingerprinting. Pattern-matches the page
/// source for vendor-specific markers. Keep this list short and
/// well-known — false positives waste LLM tokens later.
fn detect_tech_signals(html: &str) -> Vec<TechSignal> {
    let h = html.to_lowercase();
    let patterns: &[(&str, &str, &str, f32)] = &[
        // frameworks
        ("framework", "next.js", "/_next/", 0.9),
        ("framework", "react", "data-reactroot", 0.6),
        ("framework", "vue", "data-v-app", 0.6),
        ("framework", "angular", "ng-version=", 0.9),
        ("framework", "svelte", "svelte-", 0.5),
        ("framework", "remix", "/build/_assets/", 0.7),
        ("framework", "nuxt", "__nuxt", 0.85),
        ("framework", "astro", "/_astro/", 0.85),
        ("framework", "gatsby", "gatsby-image", 0.85),
        ("framework", "wordpress", "wp-content/", 0.95),
        ("framework", "django", "csrfmiddlewaretoken", 0.6),
        ("framework", "rails", "csrf-token", 0.5),
        // CMS / site builders
        ("cms", "shopify", "cdn.shopify.com", 0.95),
        ("cms", "webflow", "webflow.com/css", 0.9),
        ("cms", "ghost", "ghost-content", 0.6),
        ("cms", "framer", "framer.com/m/", 0.9),
        ("cms", "squarespace", "squarespace-cdn", 0.95),
        ("cms", "wix", "wix.com", 0.7),
        ("cms", "drupal", "drupal-static", 0.85),
        // hosting / CDN / edge
        ("hosting", "vercel", "vercel-id", 0.9),
        ("hosting", "netlify", "netlify", 0.6),
        ("hosting", "cloudflare", "cloudflare-static", 0.7),
        ("hosting", "fastly", "fastly-debug-", 0.9),
        ("hosting", "render", "onrender.com", 0.8),
        ("hosting", "fly_io", "fly-edge-", 0.85),
        ("hosting", "aws_cloudfront", ".cloudfront.net", 0.7),
        // analytics
        ("analytics", "google_analytics", "gtag(", 0.9),
        ("analytics", "plausible", "plausible.io/js", 0.95),
        ("analytics", "posthog", "posthog.com", 0.9),
        ("analytics", "fathom", "usefathom.com", 0.9),
        ("analytics", "amplitude", "api.amplitude.com", 0.9),
        ("analytics", "mixpanel", "cdn.mxpnl.com", 0.9),
        ("analytics", "segment", "cdn.segment.com", 0.9),
        ("analytics", "heap", "cdn.heapanalytics.com", 0.9),
        ("analytics", "matomo", "matomo.js", 0.85),
        ("analytics", "umami", "umami.is", 0.9),
        // auth / identity
        ("auth", "auth0", "auth0.com", 0.9),
        ("auth", "clerk", "clerk.dev", 0.9),
        ("auth", "okta", "okta.com", 0.9),
        ("auth", "supabase_auth", "supabase.co/auth", 0.85),
        ("auth", "firebase_auth", "firebaseauth", 0.8),
        ("auth", "workos", "workos.com", 0.85),
        // payments / billing
        ("payments", "stripe", "js.stripe.com", 0.95),
        ("payments", "paddle", "paddle.com", 0.85),
        ("payments", "lemon_squeezy", "lemonsqueezy.com", 0.85),
        ("payments", "braintree", "braintreegateway.com", 0.85),
        ("payments", "paypal", "paypal.com/sdk", 0.7),
        // crm / sales
        ("crm", "hubspot", "hubspot", 0.7),
        ("crm", "salesforce", "salesforce.com", 0.7),
        ("crm", "pipedrive", "pipedrive.com", 0.7),
        // chat / support
        ("chat", "intercom", "intercom.io", 0.9),
        ("chat", "zendesk", "zendesk", 0.7),
        ("chat", "drift", "drift.com", 0.85),
        ("chat", "crisp", "client.crisp.chat", 0.9),
        ("chat", "tidio", "tidiochat", 0.85),
        // ecommerce
        ("ecommerce", "bigcommerce", "bigcommerce.com", 0.9),
        // observability / monitoring
        ("observability", "datadog", "datadoghq.com", 0.9),
        ("observability", "sentry", "sentry.io", 0.9),
        ("observability", "rollbar", "rollbar.com", 0.85),
        ("observability", "logrocket", "logrocket.com", 0.85),
        ("observability", "fullstory", "fullstory.com", 0.85),
        ("observability", "honeycomb", "honeycomb.io", 0.85),
        ("observability", "new_relic", "newrelic.com", 0.9),
        // security / waf / dns
        (
            "security",
            "cloudflare_waf",
            "challenges.cloudflare.com",
            0.85,
        ),
        ("security", "akamai_bot_man", "ak-bmsc", 0.85),
        ("security", "snyk", "snyk.io", 0.6),
        // feature flags / experimentation
        ("feature_flags", "launchdarkly", "launchdarkly.com", 0.9),
        ("feature_flags", "growthbook", "growthbook.io", 0.85),
        ("feature_flags", "split_io", "split.io", 0.85),
        ("feature_flags", "unleash", "getunleash.io", 0.85),
        // marketing / email
        ("marketing", "mailchimp", "mailchimp.com", 0.7),
        ("marketing", "convertkit", "convertkit.com", 0.85),
        ("marketing", "loops", "loops.so", 0.85),
        // search
        ("search", "algolia", "algolia.net", 0.9),
        ("search", "typesense", "typesense.org", 0.85),
        ("search", "meilisearch", "meilisearch", 0.85),
        // database / backend-as-a-service tells (rare in HTML, but signal when present)
        ("baas", "supabase", "supabase.co", 0.85),
        ("baas", "firebase", "firebaseapp.com", 0.7),
    ];
    patterns
        .iter()
        .filter(|(_k, _v, marker, _c)| h.contains(marker))
        .map(|(k, v, _m, c)| TechSignal {
            kind: (*k).to_string(),
            value: (*v).to_string(),
            confidence: *c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_stacks() {
        let html = r#"
            <!doctype html>
            <html>
              <head>
                <title>  Acme   Security  </title>
                <meta name="description" content="We secure the world.">
                <meta name="keywords" content="security, edr, rust">
                <link href="/_next/static/x.css" />
                <script src="https://js.stripe.com/v3/"></script>
                <script>gtag('config', 'GA');</script>
              </head>
              <body><div data-reactroot></div></body>
            </html>
        "#;
        let doc = Html::parse_document(html);
        assert_eq!(
            select_one_text(&doc, "title").as_deref(),
            Some("Acme Security")
        );
        assert_eq!(
            select_meta(&doc, "description").as_deref(),
            Some("We secure the world."),
        );
        let signals = detect_tech_signals(html);
        let names: Vec<&str> = signals.iter().map(|s| s.value.as_str()).collect();
        assert!(names.contains(&"next.js"));
        assert!(names.contains(&"react"));
        assert!(names.contains(&"stripe"));
        assert!(names.contains(&"google_analytics"));
    }
}
