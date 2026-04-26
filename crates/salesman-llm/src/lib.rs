//! salesman-llm — multi-backend LLM router for the agent loop.
//!
//! Backends implement `LlmBackend`. The router picks one based on
//! task profile (reasoning / bulk / sovereign / grounded). Cost +
//! latency are recorded for every call so we can audit and tune.
//!
//! BUG ASSUMPTION: all backends are HTTP. We don't depend on any
//! vendor SDK — keeps dep graph small and lets us pin behavior.
//!
//! SECURITY: API keys live in `BackendCreds` wrapped in `Zeroizing`
//! so they're zeroed on drop. Never log a key; never include one in
//! an error.
#![forbid(unsafe_code)]

pub mod claude;
pub mod gemini;
pub mod router;
pub mod types;

pub use router::{LlmRouter, RouteHint};
pub use types::{BackendKind, ChatRequest, ChatResponse, Message, Role, Usage};

use async_trait::async_trait;
use salesman_core::Result;

/// All LLM backends implement this trait.
///
/// Implementations are responsible for:
/// - mapping the unified `ChatRequest` to their wire format,
/// - calling the backend's HTTP endpoint,
/// - mapping the response (and any tool-use blocks) back to
///   `ChatResponse`,
/// - reporting usage metrics on every call.
#[async_trait]
pub trait LlmBackend: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> BackendKind;
    fn model(&self) -> &str;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
}
