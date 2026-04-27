//! Minimal server-side HTML rendering. No JS. System fonts.

const HEAD: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>{TITLE}</title>
  <style>
    :root { color-scheme: light dark; --bg:#fff; --fg:#1a1a1a; --muted:#666; --rule:#eee; --good:#1f883d; --bad:#cf222e; }
    @media (prefers-color-scheme: dark) { :root { --bg:#0f0f10; --fg:#e8e6e3; --muted:#888; --rule:#2a2a2a; --good:#3fb950; --bad:#f85149; } }
    body { background: var(--bg); color: var(--fg); font: 16px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; max-width: 980px; margin: 1.5em auto; padding: 0 1em; }
    h1, h2 { line-height: 1.25; }
    a { color: inherit; }
    nav { margin-bottom: 1em; padding-bottom: 0.5em; border-bottom: 1px solid var(--rule); }
    nav a { margin-right: 1em; }
    table { border-collapse: collapse; width: 100%; }
    th, td { border: 1px solid var(--rule); padding: 0.4em 0.7em; text-align: left; vertical-align: top; }
    .ok { color: var(--good); }
    .bad { color: var(--bad); }
    .small { color: var(--muted); font-size: 0.9em; }
    code { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
    button { font: inherit; padding: 0.3em 0.7em; cursor: pointer; }
    form { display: inline; }
  </style>
</head>
<body>
<nav>
  <strong>salesman</strong>
  <a href="/drafts">drafts</a>
  <a href="/receipts">receipts</a>
  <a href="/pipeline/summary">summary</a>
  <a href="/campaigns">campaigns</a>
</nav>
"#;

const FOOT: &str = "\n</body></html>\n";

fn page(title: &str, body: &str) -> String {
    let head = HEAD.replace("{TITLE}", title);
    let mut out = String::with_capacity(head.len() + body.len() + FOOT.len());
    out.push_str(&head);
    out.push_str(body);
    out.push_str(FOOT);
    out
}

pub fn drafts_index(awaiting_count: i64) -> String {
    let body = format!(
        r#"<h1>Drafts awaiting approval ({n})</h1>
<p class="small">Per-draft review with body + approve/reject buttons lands in J2.b.
For now, list-all-across-campaigns needs a state op
(<code>list_all_drafts_awaiting_approval</code>) — pending. Use the
CLI: <code>salesman review --campaign &lt;name&gt;</code> to see drafts.</p>
"#,
        n = awaiting_count,
    );
    page("drafts — salesman", &body)
}

pub fn receipts_table(
    rows: &[(chrono::DateTime<chrono::Utc>, String, String, bool)],
) -> String {
    let mut body = String::from("<h1>Receipts (most recent 100)</h1>\n");
    if rows.is_empty() {
        body.push_str("<p class=\"small\">No receipts yet.</p>\n");
    } else {
        body.push_str(
            "<table><thead><tr><th>created</th><th>event</th><th>hash (8b)</th><th>verify</th></tr></thead><tbody>\n",
        );
        for (ts, kind, hash_hex, ok) in rows {
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td><code>{}</code></td><td class=\"{}\">{}</td></tr>\n",
                ts.to_rfc3339(),
                kind,
                hash_hex,
                if *ok { "ok" } else { "bad" },
                if *ok { "OK" } else { "FAIL" },
            ));
        }
        body.push_str("</tbody></table>\n");
    }
    page("receipts — salesman", &body)
}
