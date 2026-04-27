//! salesman-api binary — read + low-risk-write HTTP server.

mod handlers;
mod html;
mod middleware;

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use salesman_state::State;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

#[derive(Clone, Debug)]
pub struct AppState {
    pub state: State,
    pub signing_key_id: String,
}

pub fn build_router(app_state: AppState, basic_auth: Option<String>) -> Router {
    let app = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/pipeline/summary", get(handlers::pipeline_summary_json))
        .route("/campaigns", get(handlers::campaigns_json))
        .route("/drafts", get(handlers::drafts_html))
        .route("/drafts/:id/approve", post(handlers::draft_approve))
        .route("/drafts/:id/reject", post(handlers::draft_reject))
        .route("/receipts", get(handlers::receipts_html))
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(app_state));
    if let Some(creds) = basic_auth {
        app.layer(axum::middleware::from_fn(move |req, next| {
            let creds = creds.clone();
            async move { middleware::basic_auth(creds, req, next).await }
        }))
    } else {
        app
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=debug")),
        )
        .init();

    let database_url = std::env::var("SALESMAN_DATABASE_URL")
        .context("SALESMAN_DATABASE_URL not set")?;
    let bind = std::env::var("SALESMAN_API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let basic_auth = std::env::var("SALESMAN_API_BASIC_AUTH").ok();
    let signing_key_id =
        std::env::var("SALESMAN_SIGNING_KEY_ID").unwrap_or_else(|_| "salesman-default-1".into());

    let state = State::connect(&database_url).await?;
    let app = build_router(
        AppState {
            state,
            signing_key_id,
        },
        basic_auth,
    );

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(bind = %bind, "salesman-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
