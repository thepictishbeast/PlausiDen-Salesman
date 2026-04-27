use crate::AppState;
use crate::html;
use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use salesman_core::{Result as SR, TouchId};
use salesman_receipts::{Signer, default_seed_path, verify_receipt};
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
