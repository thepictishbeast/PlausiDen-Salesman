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

/// Operator view: every draft awaiting approval, across all campaigns.
/// `rows` is (touch_id, company, subject, queued_at). Company and
/// subject originate from scraped/LLM data, so both are HTML-escaped.
pub fn drafts_index(
    rows: &[(
        uuid::Uuid,
        String,
        Option<String>,
        chrono::DateTime<chrono::Utc>,
    )],
) -> String {
    let mut body = format!("<h1>Drafts awaiting approval ({n})</h1>\n", n = rows.len(),);
    if rows.is_empty() {
        body.push_str("<p class=\"small\">No drafts awaiting approval.</p>\n");
    } else {
        body.push_str(
            "<table><thead><tr><th>queued</th><th>company</th><th>subject</th><th>touch id</th></tr></thead><tbody>\n",
        );
        for (id, company, subject, queued_at) in rows {
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>\n",
                queued_at.to_rfc3339(),
                h(company),
                h(subject.as_deref().unwrap_or("(no subject)")),
                id,
            ));
        }
        body.push_str("</tbody></table>\n");
    }
    page("drafts — salesman", &body)
}

/// Render the receipts page from rows of
/// `(created_at, event_kind, short-hash-hex, verified)` — the `verified`
/// flag drives the OK/FAIL styling per row.
pub fn receipts_table(rows: &[(chrono::DateTime<chrono::Utc>, String, String, bool)]) -> String {
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

// ---------------------------------------------------------------------------
// Unsubscribe pages — no admin nav, recipient-facing copy.
// ---------------------------------------------------------------------------

const UNSUB_HEAD: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="robots" content="noindex,nofollow">
  <title>{TITLE}</title>
  <style>
    :root { color-scheme: light dark; --bg:#fff; --fg:#1a1a1a; --muted:#666; --rule:#eee; --accent:#1f6feb; }
    @media (prefers-color-scheme: dark) { :root { --bg:#0f0f10; --fg:#e8e6e3; --muted:#888; --rule:#2a2a2a; --accent:#58a6ff; } }
    body { background: var(--bg); color: var(--fg); font: 16px/1.55 system-ui, -apple-system, "Segoe UI", sans-serif; max-width: 560px; margin: 4em auto; padding: 0 1.5em; }
    h1 { font-size: 1.5em; margin-bottom: 0.6em; }
    p { margin: 0.6em 0; }
    button { font: inherit; background: var(--accent); color: #fff; border: 0; border-radius: 4px; padding: 0.6em 1em; cursor: pointer; }
    button:hover { filter: brightness(1.1); }
    code { font-family: ui-monospace, "SF Mono", Menlo, monospace; background: var(--rule); padding: 1px 4px; border-radius: 3px; }
    .small { color: var(--muted); font-size: 0.9em; }
  </style>
</head>
<body>
"#;

fn unsub_page(title: &str, body: &str) -> String {
    let head = UNSUB_HEAD.replace("{TITLE}", title);
    let mut out = String::with_capacity(head.len() + body.len() + FOOT.len());
    out.push_str(&head);
    out.push_str(body);
    out.push_str(FOOT);
    out
}

fn h(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// GET /unsubscribe — confirmation page with a POST button. The token
/// is echoed into the form action (already URL-safe — pure base64url
/// + `.`) so submitting hits the same `t=`.
pub fn unsubscribe_confirm(email: &str, token: &str, already: bool) -> String {
    let body = if already {
        format!(
            r#"<h1>You are already unsubscribed</h1>
<p>The address <code>{email}</code> will not receive further messages from us.</p>
<p class="small">If you continue to get email, reply with the word STOP and we will hand-investigate.</p>
"#,
            email = h(email),
        )
    } else {
        format!(
            r#"<h1>Confirm unsubscribe</h1>
<p>Click the button to stop receiving messages from PlausiDen at <code>{email}</code>.</p>
<form method="post" action="/unsubscribe?t={token}">
  <button type="submit">Unsubscribe {email}</button>
</form>
<p class="small">This adds your address to a global do-not-contact list. We do not sell, share, or use it for any other purpose.</p>
"#,
            email = h(email),
            token = h(token),
        )
    };
    unsub_page("Unsubscribe — PlausiDen", &body)
}

/// POST /unsubscribe success.
pub fn unsubscribe_done(email: &str) -> String {
    let body = format!(
        r#"<h1>Unsubscribed</h1>
<p>The address <code>{email}</code> will not receive further messages from us.</p>
<p class="small">Action recorded. If you ever change your mind, reply directly to a previous message and ask to be re-added — we will only do so on explicit request.</p>
"#,
        email = h(email),
    );
    unsub_page("Unsubscribed — PlausiDen", &body)
}

/// 4xx / 5xx page when the link is invalid or the service is offline.
pub fn unsubscribe_error(message: &str) -> String {
    let body = format!(
        r#"<h1>Could not unsubscribe</h1>
<p>{message}</p>
<p class="small">As a fallback, replying with the word STOP to any of our messages will also opt you out.</p>
"#,
        message = h(message),
    );
    unsub_page("Unsubscribe error — PlausiDen", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h_escapes_html_metachars() {
        let r = h(r#"<script>alert("x")&'</script>"#);
        assert_eq!(
            r,
            "&lt;script&gt;alert(&quot;x&quot;)&amp;&#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn confirm_escapes_user_email() {
        let r = unsubscribe_confirm("\"><script>x</script>@x", "TOKEN", false);
        // The raw script tag MUST NOT appear unescaped.
        assert!(!r.contains("<script>x</script>"));
        // The escaped version MUST appear.
        assert!(r.contains("&lt;script&gt;x&lt;/script&gt;"));
        // Form points back at /unsubscribe.
        assert!(r.contains(r#"action="/unsubscribe?t=TOKEN""#));
    }

    #[test]
    fn confirm_branches_on_already_suppressed() {
        let normal = unsubscribe_confirm("a@b.com", "TOK", false);
        assert!(normal.contains("Confirm unsubscribe"));
        assert!(normal.contains("<form"));

        let already = unsubscribe_confirm("a@b.com", "TOK", true);
        assert!(already.contains("already unsubscribed"));
        // The "already" path MUST NOT show a button — there is nothing
        // left to do.
        assert!(!already.contains("<form"));
    }

    #[test]
    fn done_renders_email() {
        let r = unsubscribe_done("alice@example.com");
        assert!(r.contains("alice@example.com"));
        assert!(r.contains("Unsubscribed"));
    }

    #[test]
    fn error_page_offers_stop_fallback() {
        let r = unsubscribe_error("link is invalid");
        assert!(r.contains("link is invalid"));
        assert!(r.contains("STOP"));
    }

    #[test]
    fn unsub_pages_have_no_admin_nav() {
        // Recipient-facing pages must not advertise the admin
        // /drafts /receipts /campaigns links — the recipient is not
        // an operator. Defensive check against accidentally reusing
        // the wrong page() helper later.
        let r = unsubscribe_done("a@b.com");
        assert!(!r.contains(r#"href="/drafts""#));
        assert!(!r.contains(r#"href="/receipts""#));
    }

    #[test]
    fn unsub_pages_set_robots_noindex() {
        // Public unsubscribe pages should not be indexed.
        let r = unsubscribe_done("a@b.com");
        assert!(r.contains(r#"name="robots" content="noindex,nofollow""#));
    }

    #[test]
    fn drafts_index_escapes_company_and_subject() {
        let rows = vec![(
            uuid::Uuid::nil(),
            "<script>evil</script>".to_string(),
            Some("Re: <b>hi</b>".to_string()),
            chrono::DateTime::from_timestamp(0, 0).unwrap(),
        )];
        let r = drafts_index(&rows);
        // Raw scraped/LLM strings must never reach the page unescaped.
        assert!(!r.contains("<script>evil</script>"));
        assert!(r.contains("&lt;script&gt;evil&lt;/script&gt;"));
        assert!(r.contains("&lt;b&gt;hi&lt;/b&gt;"));
        assert!(r.contains("Drafts awaiting approval (1)"));
    }

    #[test]
    fn drafts_index_empty_has_no_table() {
        let r = drafts_index(&[]);
        assert!(r.contains("Drafts awaiting approval (0)"));
        assert!(r.contains("No drafts awaiting approval"));
        assert!(!r.contains("<table>"));
    }
}
