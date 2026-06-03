use crate::AppState;
use crate::html;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use salesman_core::{Result as SR, TouchId};
use salesman_receipts::{Signer, default_seed_path, verify_receipt};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// `GET /healthz` — liveness probe; always 200 `ok`.
pub async fn healthz() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain")],
        "ok\n",
    )
}

/// `GET /pipeline/summary` — JSON snapshot of the last 24h: counts by
/// funnel state, recent sends/replies/opt-outs/receipts, and suppression
/// totals.
pub async fn pipeline_summary_json(
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let s = app.state.pipeline_summary(24).await?;
    Ok(Json(json!({
        "since_hours": s.since_hours,
        "companies": s.companies,
        "prospects": s.prospects,
        "by_state": {
            "new": s.new_prospects,
            "contacted": s.contacted,
            "engaged": s.engaged,
            "won": s.won,
            "lost": s.lost,
            "suppressed": s.suppressed_prospects,
        },
        "awaiting_approval": s.awaiting_approval,
        "recent": {
            "sends": s.sent_recent,
            "replies": s.replies_recent,
            "optouts": s.optout_recent,
            "receipts": s.receipts_recent,
        },
        "suppressions": s.suppressions,
    })))
}

/// `GET /campaigns` — JSON list of all campaigns.
pub async fn campaigns_json(
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let campaigns = app.state.list_campaigns().await?;
    Ok(Json(json!({ "campaigns": campaigns })))
}

/// `GET /drafts` — operator HTML table of every draft awaiting approval
/// across all campaigns (company + subject are HTML-escaped).
pub async fn drafts_html(State(app): State<Arc<AppState>>) -> Result<Html<String>, ApiError> {
    let drafts = app.state.list_all_drafts_awaiting_approval().await?;
    let rows: Vec<(uuid::Uuid, String, Option<String>, chrono::DateTime<chrono::Utc>)> = drafts
        .into_iter()
        .map(|d| (d.touch_id.0, d.company, d.subject, d.queued_at))
        .collect();
    Ok(Html(html::drafts_index(&rows)))
}

/// `POST /drafts/:id/approve` — move the touch `awaiting_approval` →
/// `approved`. 404 if it is not awaiting approval; otherwise redirect to
/// `/drafts`.
pub async fn draft_approve(
    State(app): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let touch_id = TouchId(id);
    let n = app.state.approve_touch(touch_id).await?;
    if n == 0 {
        return Ok((StatusCode::NOT_FOUND, "touch not in awaiting_approval").into_response());
    }
    Ok(Redirect::to("/drafts").into_response())
}

/// `POST /drafts/:id/reject` — move the touch `awaiting_approval` →
/// `rejected`. 404 if it is not awaiting approval; otherwise redirect to
/// `/drafts`.
pub async fn draft_reject(
    State(app): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let touch_id = TouchId(id);
    let n = app.state.reject_touch(touch_id).await?;
    if n == 0 {
        return Ok((StatusCode::NOT_FOUND, "touch not in awaiting_approval").into_response());
    }
    Ok(Redirect::to("/drafts").into_response())
}

/// `GET /receipts` — HTML table of the 100 most recent signed receipts,
/// each marked verified/unverified against the loaded signing key.
pub async fn receipts_html(State(app): State<Arc<AppState>>) -> Result<Html<String>, ApiError> {
    let receipts = app.state.list_recent_receipts(100).await?;
    // Try to load signing key for verify; if missing, mark all as "unverified".
    let signer = Signer::load_or_generate(&default_seed_path(), &app.signing_key_id).ok();
    let vk = signer.as_ref().map(|s| s.verifying_key());

    let mut rows = Vec::with_capacity(receipts.len());
    for r in &receipts {
        let verified = match &vk {
            Some(vk) => verify_receipt(r, vk).is_ok(),
            None => false,
        };
        rows.push((
            r.created_at,
            r.event_kind.clone(),
            hex::encode(&r.hash[..8.min(r.hash.len())]),
            verified,
        ));
    }
    Ok(Html(html::receipts_table(&rows)))
}

// ---------------------------------------------------------------------------
// unsubscribe (RFC 8058 one-click)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UnsubQuery {
    /// HMAC token of the form "{email_b64}.{mac_b64}".
    t: Option<String>,
}

/// GET /unsubscribe?t=...
///
/// Renders a small confirmation page. We DO NOT auto-suppress on GET —
/// many mail clients (and link prefetchers!) hit GET on every link.
/// The visible button on the page POSTs to the same URL, which is the
/// path that actually records the suppression.
///
/// Exception: if the recipient is already suppressed, we show a
/// success message — the action they wanted is already done.
pub async fn unsubscribe_get(
    State(app): State<Arc<AppState>>,
    Query(q): Query<UnsubQuery>,
) -> Response {
    let Some(verifier) = &app.unsubscribe_tokens else {
        return service_unconfigured();
    };
    let token = match q.t.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html::unsubscribe_error("Missing or empty unsubscribe link parameter."),
            )
                .into_response();
        }
    };
    let email = match verifier.verify_token(token) {
        Ok(e) => e,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html::unsubscribe_error(
                    "This unsubscribe link is invalid or has been tampered with. \
                     If you keep getting our messages, reply with the word STOP.",
                ),
            )
                .into_response();
        }
    };
    let already = app.state.is_suppressed(&email).await.unwrap_or(false);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html::unsubscribe_confirm(&email, token, already),
    )
        .into_response()
}

/// POST /unsubscribe?t=...
///
/// Either RFC 8058 one-click (mail clients POST `List-Unsubscribe=One-Click`
/// form-encoded) OR the visible-button submission from the GET page.
/// Always idempotent — a second POST with the same email returns 200.
pub async fn unsubscribe_post(
    State(app): State<Arc<AppState>>,
    Query(q): Query<UnsubQuery>,
) -> Response {
    let Some(verifier) = &app.unsubscribe_tokens else {
        return service_unconfigured();
    };
    let token = match q.t.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html::unsubscribe_error("Missing unsubscribe token."),
            )
                .into_response();
        }
    };
    let email = match verifier.verify_token(token) {
        Ok(e) => e,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html::unsubscribe_error(
                    "This unsubscribe link is invalid or has been tampered with.",
                ),
            )
                .into_response();
        }
    };
    if let Err(e) = app
        .state
        .add_suppression(&email, "email", "one-click unsubscribe", "one_click")
        .await
    {
        tracing::error!(error = %e, "add_suppression failed for one-click");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html::unsubscribe_error(
                "Could not record your unsubscribe right now. Please try again, \
                 or reply with the word STOP.",
            ),
        )
            .into_response();
    }
    tracing::info!(email = %email, "one-click unsubscribe recorded");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html::unsubscribe_done(&email),
    )
        .into_response()
}

fn service_unconfigured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "unsubscribe service is not configured on this host\n",
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError(salesman_core::Error);

impl From<salesman_core::Error> for ApiError {
    fn from(e: salesman_core::Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!("%e" = %self.0, "api error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("error: {}", self.0),
        )
            .into_response()
    }
}

#[allow(dead_code)]
fn _swallow_unused<T>(_: T) -> SR<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use salesman_outreach::UnsubscribeTokens;
    use salesman_state::State;

    async fn test_app() -> Arc<AppState> {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("set TEST_DATABASE_URL to a writable postgres URL");
        let state = State::connect(&url).await.expect("connect");
        let tokens = UnsubscribeTokens::new(vec![7u8; 32], "https://outreach.plausiden.com/unsubscribe")
            .expect("tokens");
        Arc::new(AppState {
            state,
            signing_key_id: "test".into(),
            unsubscribe_tokens: Some(tokens),
        })
    }

    fn nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    /// The public RFC 8058 one-click endpoint: a valid token records the
    /// opt-out; a forged token is rejected and suppresses nothing; GET is
    /// prefetch-safe (never suppresses); a missing token is a 400.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
    async fn one_click_unsubscribe_suppresses_only_with_a_valid_token() {
        let app = test_app().await;
        let tokens = app.unsubscribe_tokens.clone().unwrap();
        let n = nanos();

        // 1) Valid POST → 200 and the address is now suppressed.
        let email = format!("unsub{n}@example.com");
        let token = tokens.token_for(&email);
        let resp = unsubscribe_post(
            State(app.clone()),
            Query(UnsubQuery { t: Some(token) }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(app.state.is_suppressed(&email).await.unwrap());

        // 2) Forged token → 400 and it must NOT suppress an arbitrary
        //    address (else anyone could opt out our whole prospect list).
        let email2 = format!("unsub{n}b@example.com");
        let forged = format!("{}A", tokens.token_for(&email2)); // corrupt the MAC
        let resp = unsubscribe_post(
            State(app.clone()),
            Query(UnsubQuery { t: Some(forged) }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            !app.state.is_suppressed(&email2).await.unwrap(),
            "a forged token must not suppress an arbitrary address"
        );

        // 3) GET with a valid token → 200 but records NOTHING (link
        //    prefetchers hit GET; only the POST button suppresses).
        let email3 = format!("unsub{n}c@example.com");
        let token3 = tokens.token_for(&email3);
        let resp = unsubscribe_get(
            State(app.clone()),
            Query(UnsubQuery { t: Some(token3) }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            !app.state.is_suppressed(&email3).await.unwrap(),
            "GET must not record a suppression"
        );

        // 4) Missing token → 400.
        let resp =
            unsubscribe_post(State(app.clone()), Query(UnsubQuery { t: None })).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // cleanup (canonical form; normalize is idempotent).
        for e in [&email, &email2, &email3] {
            let canon = salesman_core::normalize_email_for_match(e);
            let _ = app.state.remove_suppression(&canon).await;
        }
    }

    /// The human-in-the-loop gate: draft_approve/draft_reject move a touch
    /// out of awaiting_approval exactly once, and 404 on anything not
    /// currently awaiting (so a touch can't be approved twice or after
    /// rejection).
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
    async fn draft_approve_and_reject_are_one_shot_gate() {
        use salesman_core::CompanyId;
        use salesman_core::model::{Company, DiscoverySource, TouchChannel};
        use std::collections::BTreeMap;

        let app = test_app().await;
        let n = nanos();
        let campaign_name = format!("salesman-gatetest-{n}");

        let mk_company = || Company {
            id: CompanyId::new(),
            legal_name: None,
            display_name: format!("GateTest {n}"),
            homepage: None,
            industry: None,
            size_band: None,
            region: None,
            description: None,
            tech_signals: vec![],
            discovered_at: chrono::Utc::now(),
            last_enriched_at: None,
            source: DiscoverySource::OwnerSeed,
            raw: BTreeMap::new(),
        };

        // Two prospects so we can approve one and reject the other.
        let c1 = mk_company();
        let c2 = mk_company();
        app.state
            .insert_companies(&[c1.clone(), c2.clone()])
            .await
            .expect("insert companies");
        let cid = app
            .state
            .ensure_campaign(&campaign_name, "test", "test")
            .await
            .expect("ensure_campaign");
        app.state
            .upsert_prospects_for_campaign(cid, &[c1.id, c2.id])
            .await
            .expect("upsert");
        let listed = app
            .state
            .list_prospects_with_facts_for_campaign(cid)
            .await
            .expect("list");
        let pid = |company: &Company| {
            listed
                .iter()
                .find(|p| p.company_id == company.id)
                .expect("prospect")
                .prospect_id
        };

        let approve_touch = app
            .state
            .insert_touch_draft(pid(&c1), TouchChannel::Email, Some("hi"), "body")
            .await
            .expect("draft 1");
        let reject_touch = app
            .state
            .insert_touch_draft(pid(&c2), TouchChannel::Email, Some("hi"), "body")
            .await
            .expect("draft 2");

        // Helper: is a touch currently awaiting approval?
        let awaiting = |tid: salesman_core::TouchId| {
            let app = app.clone();
            async move {
                app.state
                    .list_all_drafts_awaiting_approval()
                    .await
                    .expect("list drafts")
                    .iter()
                    .any(|d| d.touch_id == tid)
            }
        };

        // Both start awaiting.
        assert!(awaiting(approve_touch).await);
        assert!(awaiting(reject_touch).await);

        // Approve → redirect, and the touch leaves the awaiting list.
        let resp = draft_approve(State(app.clone()), Path(approve_touch.0))
            .await
            .expect("approve");
        assert!(resp.status().is_redirection(), "approve should redirect");
        assert!(!awaiting(approve_touch).await, "approved touch must leave awaiting");

        // Re-approving the same touch → 404 (one-shot).
        let resp = draft_approve(State(app.clone()), Path(approve_touch.0))
            .await
            .expect("re-approve");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Reject the other → redirect, leaves awaiting; re-reject → 404.
        let resp = draft_reject(State(app.clone()), Path(reject_touch.0))
            .await
            .expect("reject");
        assert!(resp.status().is_redirection(), "reject should redirect");
        assert!(!awaiting(reject_touch).await, "rejected touch must leave awaiting");
        let resp = draft_reject(State(app.clone()), Path(reject_touch.0))
            .await
            .expect("re-reject");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Unknown touch id → 404.
        let resp = draft_approve(State(app.clone()), Path(uuid::Uuid::now_v7()))
            .await
            .expect("approve unknown");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // No explicit teardown: every identifier is unique per run (keyed
        // off `n`) and the test DB is ephemeral, so leftover rows neither
        // affect the touch-id-specific assertions above nor other runs.
        // (The api crate has no sqlx dep for raw DELETEs, and adding State
        // delete methods just for teardown isn't worth the surface.)
    }
}
