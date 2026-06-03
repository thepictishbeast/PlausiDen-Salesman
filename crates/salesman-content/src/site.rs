//! Static-site renderer. Reads a directory of markdown files and
//! produces:
//!   - one HTML file per source markdown (slug → slug.html)
//!   - sitemap.xml listing every output URL
//!   - index.html (auto-generated from the file list)
//!
//! Wraps each page in a minimal template (no JS, system fonts, dark
//! mode via prefers-color-scheme). The CSS lives inline so the
//! served file is fully self-contained.
//!
//! BUG ASSUMPTION: input is operator-curated markdown. We do NOT
//! sanitize; if you put untrusted content in, you'll get the raw
//! HTML out. That's intentional — these are owner-approved pages.
//!
//! BUG ASSUMPTION: sitemap.xml is generated unconditionally; if you
//! add a noindex page, exclude it from the source dir.

use chrono::Utc;
use pulldown_cmark::{Options, Parser, html};
use salesman_core::Result;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const PAGE_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>{{TITLE}}</title>
  <meta name="description" content="{{DESCRIPTION}}">
  <link rel="canonical" href="{{CANONICAL}}">
  <style>
    :root {
      color-scheme: light dark;
      --bg: #fff; --fg: #1a1a1a; --muted: #666; --link: #0064c8; --rule: #eee;
    }
    @media (prefers-color-scheme: dark) {
      :root { --bg: #0f0f10; --fg: #e8e6e3; --muted: #888; --link: #6db4ff; --rule: #2a2a2a; }
    }
    html, body { background: var(--bg); color: var(--fg); }
    body { font: 17px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif; max-width: 720px; margin: 2em auto; padding: 0 1em; }
    h1, h2, h3 { line-height: 1.25; margin-top: 2em; }
    a { color: var(--link); text-decoration: underline; text-underline-offset: 0.15em; }
    code { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 0.92em; background: var(--rule); padding: 0 0.3em; border-radius: 3px; }
    pre code { display: block; padding: 0.8em 1em; overflow-x: auto; }
    table { border-collapse: collapse; margin: 1em 0; }
    th, td { border: 1px solid var(--rule); padding: 0.4em 0.7em; text-align: left; }
    blockquote { border-left: 3px solid var(--rule); margin: 1em 0; padding: 0.2em 1em; color: var(--muted); }
    hr { border: 0; border-top: 1px solid var(--rule); }
    footer { color: var(--muted); font-size: 0.9em; margin-top: 4em; padding-top: 1em; border-top: 1px solid var(--rule); }
  </style>
</head>
<body>
{{BODY}}
<footer>
  <a href="/">All pages</a> · {{FOOTER}}
</footer>
</body>
</html>
"#;

/// Static site config used when rendering pages.
#[derive(Debug, Clone)]
pub struct SiteConfig {
    /// Site origin URL (e.g. `https://plausiden.com`).
    pub origin: String,
    /// Human-readable site name (e.g. `PlausiDen`).
    pub site_name: String,
    /// HTML footer appended to every rendered page.
    pub footer_html: String,
}

impl SiteConfig {
    /// Build a site config for the given `origin` URL and `site_name`.
    pub fn new(origin: impl Into<String>, site_name: impl Into<String>) -> Self {
        let site_name = site_name.into();
        Self {
            origin: origin.into(),
            footer_html: format!("Sovereign software for the rest of us. — {site_name}"),
            site_name,
        }
    }
}

/// Metadata about a page that was rendered to disk.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    /// URL slug of the page.
    pub slug: String,
    /// Source markdown/template path.
    pub source_path: PathBuf,
    /// Path the rendered HTML was written to.
    pub output_path: PathBuf,
    /// Page title.
    pub title: String,
    /// Page meta description.
    pub description: String,
}

/// Walk `src_dir` for `*.md` files; render each into `dst_dir/<slug>.html`.
/// Also writes `index.html` and `sitemap.xml`.
pub fn render_site(src_dir: &Path, dst_dir: &Path, cfg: &SiteConfig) -> Result<Vec<RenderedPage>> {
    fs::create_dir_all(dst_dir)?;
    let mut pages = Vec::new();

    for entry in WalkDir::new(src_dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        // `path` is walked from the operator-configured `src_dir`, not
        // from agent/network input — these are operator content files.
        let stem = path // nosemgrep
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();
        let slug = sanitise_slug(&stem);
        let md = fs::read_to_string(path)?; // nosemgrep
        let (title, description) = extract_title_and_description(&md);
        let html = render_markdown_to_html(&md);
        let canonical = format!("{}/{slug}", cfg.origin.trim_end_matches('/'));
        let page = PAGE_TEMPLATE
            .replace("{{TITLE}}", &escape_html(&title))
            .replace("{{DESCRIPTION}}", &escape_html(&description))
            .replace("{{CANONICAL}}", &canonical)
            .replace("{{BODY}}", &html)
            .replace("{{FOOTER}}", &cfg.footer_html);
        let out_path = dst_dir.join(format!("{slug}.html"));
        fs::write(&out_path, &page)?;
        pages.push(RenderedPage {
            slug,
            source_path: path.to_path_buf(),
            output_path: out_path,
            title,
            description,
        });
    }

    // Sort for deterministic index + sitemap output.
    pages.sort_by(|a, b| a.slug.cmp(&b.slug));

    // index.html
    let mut index_md = String::from("# ");
    index_md.push_str(&cfg.site_name);
    index_md.push_str("\n\n");
    for p in &pages {
        index_md.push_str(&format!(
            "- [{}]({}.html) — {}\n",
            p.title, p.slug, p.description
        ));
    }
    let index_html_inner = render_markdown_to_html(&index_md);
    let index_canonical = cfg.origin.trim_end_matches('/').to_string();
    let index = PAGE_TEMPLATE
        .replace("{{TITLE}}", &escape_html(&cfg.site_name))
        .replace(
            "{{DESCRIPTION}}",
            &escape_html(&format!("{} — pages", cfg.site_name)),
        )
        .replace("{{CANONICAL}}", &index_canonical)
        .replace("{{BODY}}", &index_html_inner)
        .replace("{{FOOTER}}", &cfg.footer_html);
    fs::write(dst_dir.join("index.html"), index)?;

    // sitemap.xml
    let now = Utc::now().to_rfc3339();
    let mut sitemap = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
"#,
    );
    sitemap.push_str(&format!(
        "  <url>\n    <loc>{origin}/</loc>\n    <lastmod>{now}</lastmod>\n  </url>\n",
        origin = cfg.origin.trim_end_matches('/'),
    ));
    for p in &pages {
        sitemap.push_str(&format!(
            "  <url>\n    <loc>{}/{}</loc>\n    <lastmod>{}</lastmod>\n  </url>\n",
            cfg.origin.trim_end_matches('/'),
            p.slug,
            now,
        ));
    }
    sitemap.push_str("</urlset>\n");
    fs::write(dst_dir.join("sitemap.xml"), sitemap)?;

    Ok(pages)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn render_markdown_to_html(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(md, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

fn extract_title_and_description(md: &str) -> (String, String) {
    let mut title = String::from("Untitled");
    let mut description = String::new();
    let mut found_title = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if !found_title && trimmed.starts_with("# ") {
            title = trimmed.trim_start_matches("# ").trim().to_string();
            found_title = true;
            continue;
        }
        if found_title && !trimmed.is_empty() && !trimmed.starts_with('#') {
            // First non-heading content line after the title becomes
            // the description (truncate to ~160 chars).
            description = trimmed.chars().take(160).collect();
            break;
        }
    }
    (title, description)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sanitise_slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .replace("--", "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn renders_basic_md() {
        let html = render_markdown_to_html("# hi\n\nbody **bold**\n");
        assert!(html.contains("<h1>hi</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn extracts_title_and_description() {
        let (t, d) = extract_title_and_description(
            "# My Title\n\nThis is the description line.\n\n## later",
        );
        assert_eq!(t, "My Title");
        assert!(d.starts_with("This is the description"));
    }

    #[test]
    fn sanitises_slug() {
        assert_eq!(
            sanitise_slug("Sentinel vs CrowdStrike"),
            "sentinel-vs-crowdstrike"
        );
        assert_eq!(sanitise_slug("AWS S3 (compared)"), "aws-s3-compared");
    }

    #[test]
    fn renders_full_site_to_tmp() {
        let dir = std::env::temp_dir().join(format!("salesman-render-test-{}", std::process::id()));
        let src = dir.join("src");
        let dst = dir.join("dst");
        fs::create_dir_all(&src).unwrap();
        let mut f = fs::File::create(src.join("Sentinel vs Falcon.md")).unwrap();
        writeln!(
            f,
            "# Sentinel vs Falcon\n\nWhen to pick which.\n\n## Pricing\n\nSentinel costs less."
        )
        .unwrap();
        let cfg = SiteConfig::new("https://test.example", "TestSite");
        let pages = render_site(&src, &dst, &cfg).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].slug, "sentinel-vs-falcon");
        assert!(dst.join("sentinel-vs-falcon.html").exists());
        assert!(dst.join("index.html").exists());
        assert!(dst.join("sitemap.xml").exists());
        let html = fs::read_to_string(&pages[0].output_path).unwrap();
        assert!(html.contains("Sentinel vs Falcon"));
        assert!(html.contains("https://test.example/sentinel-vs-falcon"));
        // cleanup
        fs::remove_dir_all(&dir).ok();
    }
}
