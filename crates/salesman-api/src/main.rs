//! salesman-api binary — read + low-risk-write HTTP server.

mod handlers;
mod html;
mod middleware;

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use salesman_outreach::UnsubscribeTokens;
use salesman_state::State;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

#[derive(Clone, Debug)]
pub struct AppState {
    pub state: State,
    pub signing_key_id: String,
    /// Per-recipient one-click unsubscribe verifier. None disables the
    /// `/unsubscribe` routes (they 503 with a config-error message).
    pub unsubscribe_tokens: Option<UnsubscribeTokens>,
}

/// Build the full Axum router: the operator routes (`/healthz`,
/// `/pipeline/summary`, `/campaigns`, `/drafts`, `/drafts/:id/approve` +
/// `/reject`, `/receipts`) behind optional HTTP Basic auth, merged with the
/// always-public RFC 8058 `/unsubscribe` routes (which authenticate via
/// their own HMAC token, not credentials).
pub fn build_router(app_state: AppState, basic_auth: Option<String>) -> Router {
    // Unsubscribe routes are mounted on a sub-router that bypasses
    // basic auth — RFC 8058 one-click MUST work without credentials,
    // since the recipient cannot authenticate as anyone but themselves
    // and the HMAC token IS their authentication.
    let unsub_routes = Router::new()
        .route("/unsubscribe", get(handlers::unsubscribe_get))
        .route("/unsubscribe", post(handlers::unsubscribe_post))
        .with_state(Arc::new(app_state.clone()));

    let main_routes = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/pipeline/summary", get(handlers::pipeline_summary_json))
        .route("/campaigns", get(handlers::campaigns_json))
        .route("/drafts", get(handlers::drafts_html))
        .route("/drafts/:id/approve", post(handlers::draft_approve))
        .route("/drafts/:id/reject", post(handlers::draft_reject))
        .route("/receipts", get(handlers::receipts_html))
        .with_state(Arc::new(app_state));

    let main_routes = if let Some(creds) = basic_auth {
        main_routes.layer(axum::middleware::from_fn(move |req, next| {
            let creds = creds.clone();
            async move { middleware::basic_auth(creds, req, next).await }
        }))
    } else {
        main_routes
    };

    main_routes
        .merge(unsub_routes)
        .layer(TraceLayer::new_for_http())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=debug")),
        )
        .init();

    let database_url =
        std::env::var("SALESMAN_DATABASE_URL").context("SALESMAN_DATABASE_URL not set")?;
    let bind = std::env::var("SALESMAN_API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let basic_auth = std::env::var("SALESMAN_API_BASIC_AUTH").ok();
    let signing_key_id =
        std::env::var("SALESMAN_SIGNING_KEY_ID").unwrap_or_else(|_| "salesman-default-1".into());

    let state = State::connect(&database_url).await?;
    let unsubscribe_tokens = match UnsubscribeTokens::from_env() {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(reason = %e, "unsubscribe verifier disabled — /unsubscribe will 503 until SALESMAN_UNSUBSCRIBE_BASE_URL + SALESMAN_UNSUBSCRIBE_HMAC_SECRET are set");
            None
        }
    };
    let app = build_router(
        AppState {
            state,
            signing_key_id,
            unsubscribe_tokens,
        },
        basic_auth,
    );

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(bind = %bind, "salesman-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
