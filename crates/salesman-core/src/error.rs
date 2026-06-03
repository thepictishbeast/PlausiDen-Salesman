//! The crate-wide error and result types.

use thiserror::Error;

/// Convenience alias: `Result<T>` is `std::result::Result<T, Error>`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Top-level error type. Every other crate's error converts into one
/// of these via `From`. Keep variants stable — they're surfaced to
/// the CLI and the API.
#[derive(Debug, Error)]
pub enum Error {
    /// Configuration is missing or invalid (e.g. an unset env var).
    #[error("config: {0}")]
    Config(String),

    /// A database operation failed (connection, query, or migration).
    #[error("database: {0}")]
    Db(String),

    /// An LLM backend call failed.
    #[error("llm backend `{backend}`: {message}")]
    Llm {
        /// The backend that failed (e.g. `claude`, `gemini`).
        backend: String,
        /// Human-readable failure detail.
        message: String,
    },

    /// A tool invocation failed.
    #[error("tool `{tool}`: {message}")]
    Tool {
        /// The tool name that failed (e.g. `osint.wikipedia`).
        tool: String,
        /// Human-readable failure detail.
        message: String,
    },

    /// Input failed validation before any side effect occurred.
    #[error("validation: {0}")]
    Validation(String),

    /// A rate limit was hit; the caller should retry after a delay.
    #[error("rate limit: {scope} (retry in {retry_after_ms} ms)")]
    RateLimit {
        /// What was limited (e.g. a per-domain or per-recipient scope).
        scope: String,
        /// Suggested backoff before retrying, in milliseconds.
        retry_after_ms: u64,
    },

    /// The target is on the suppression list and must not be contacted.
    #[error("suppressed: {reason}")]
    Suppressed {
        /// Why the target is suppressed (e.g. `reply_optout`).
        reason: String,
    },

    /// A requested entity does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// An underlying I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failed.
    #[error("serde_json: {0}")]
    SerdeJson(#[from] serde_json::Error),

    /// An invariant was violated — a bug, not bad input.
    #[error("internal: {0}")]
    Internal(String),
}
