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

pub async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain")], "ok\n")
}

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

pub async fn campaigns_json(
    State(_app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Phase 1.4 placeholder — stub returns empty until we add
    // list_campaigns to salesman-state.
    Ok(Json(json!({"campaigns": []})))
}

pub async fn drafts_html(State(app): State<Arc<AppState>>) -> Result<Html<String>, ApiError> {
    // Until we have list_all_drafts_awaiting_approval (across all
    // campaigns), iterate per-campaign would need that state op.
    // For now we expose a minimal shell with a TODO note.
    let summary = app.state.pipeline_summary(24).await?;
    Ok(Html(html::drafts_index(summary.awaiting_approval)))
}

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

pub async fn receipts_html(
    State(app): State<Arc<AppState>>,
) -> Result<Html<String>, ApiError> {
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
        rows.push((r.created_at, r.event_kind.clone(), hex::encode(&r.hash[..8.min(r.hash.len())]), verified));
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
    let already = app
        .state
        .is_suppressed(&email)
        .await
        .unwrap_or(false);
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

pub struct ApiError(salesman_core::Error);

impl From<salesman_core::Error> for ApiError {
    fn from(e: salesman_core::Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!("%e" = %self.0, "api error");
        (StatusCode::INTERNAL_SERVER_ERROR, format!("error: {}", self.0)).into_response()
    }
}

#[allow(dead_code)]
fn _swallow_unused<T>(_: T) -> SR<()> { Ok(()) }
