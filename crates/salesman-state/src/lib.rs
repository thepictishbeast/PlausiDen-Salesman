//! salesman-state — Postgres persistence for the Salesman pipeline.
//!
//! BUG ASSUMPTION: connection string comes from config; Postgres is
//! reachable at construction time. We don't lazily reconnect — caller
//! handles `Db` errors by failing the operation, not by retrying
//! inside the call.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use salesman_core::{Error, Result};
use salesman_llm::{BackendKind, LlmCallSink};
use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod query;
pub use query::{
    CampaignCostRow, CostSummaryRow, DueProspect, LlmCallRecord, PipelineSummary,
    ProspectWithFacts, ReplyRow, SequenceStepInput, TemplateStat, TouchSummary, UnclassifiedReply,
};

/// Thin wrapper around a Postgres connection pool. Created at startup,
/// shared across the process.
#[derive(Debug, Clone)]
pub struct State {
    pool: PgPool,
}

impl State {
    /// Connect to the database and apply pending migrations.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .connect(database_url)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| Error::Db(format!("migrations: {e}")))?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl LlmCallSink for State {
    async fn record_call(
        &self,
        backend: BackendKind,
        model: String,
        prompt_tokens: u32,
        output_tokens: u32,
        cache_hit_tokens: u32,
        latency_ms: u64,
        cost_micro_usd: u64,
        purpose: String,
    ) {
        let rec = query::LlmCallRecord {
            backend: backend.to_string(),
            model,
            prompt_tokens,
            output_tokens,
            cache_hit_tokens,
            latency_ms,
            cost_micro_usd,
            purpose,
            related_id: None,
            related_kind: None,
        };
        if let Err(e) = self.insert_llm_call(&rec).await {
            tracing::warn!("%e" = %e, "llm cost ledger insert failed (non-fatal)");
        }
    }
}
