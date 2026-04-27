//! salesman-state — Postgres persistence for the Salesman pipeline.
//!
//! BUG ASSUMPTION: connection string comes from config; Postgres is
//! reachable at construction time. We don't lazily reconnect — caller
//! handles `Db` errors by failing the operation, not by retrying
//! inside the call.
#![forbid(unsafe_code)]

use salesman_core::{Error, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod query;
pub use query::{
    DueProspect, PipelineSummary, ProspectWithFacts, ReplyRow, SequenceStepInput, TouchSummary,
    UnclassifiedReply,
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
