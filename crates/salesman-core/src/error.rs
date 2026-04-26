use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Top-level error type. Every other crate's error converts into one
/// of these via `From`. Keep variants stable — they're surfaced to
/// the CLI and the API.
#[derive(Debug, Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),

    #[error("database: {0}")]
    Db(String),

    #[error("llm backend `{backend}`: {message}")]
    Llm { backend: String, message: String },

    #[error("tool `{tool}`: {message}")]
    Tool { tool: String, message: String },

    #[error("validation: {0}")]
    Validation(String),

    #[error("rate limit: {scope} (retry in {retry_after_ms} ms)")]
    RateLimit { scope: String, retry_after_ms: u64 },

    #[error("suppressed: {reason}")]
    Suppressed { reason: String },

    #[error("not found: {0}")]
    NotFound(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde_json: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("internal: {0}")]
    Internal(String),
}
