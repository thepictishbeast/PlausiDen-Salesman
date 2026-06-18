//! Cross-backend chat request/response types used by the router.

use salesman_core::{ToolCall, ToolResult};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which LLM backend served (or should serve) a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    /// Anthropic Claude.
    Claude,
    /// Google Gemini.
    Gemini,
    /// PlausiDen's local LFI backend.
    Lfi,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendKind::Claude => f.write_str("claude"),
            BackendKind::Gemini => f.write_str("gemini"),
            BackendKind::Lfi => f.write_str("lfi"),
        }
    }
}

/// The author role of a chat [`Message`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Role {
    /// System / instruction message.
    System,
    /// End-user message.
    User,
    /// Model-authored message.
    Assistant,
    /// Tool-result message returned to the model.
    Tool,
}

/// One message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Who authored the message.
    pub role: Role,
    /// The text content.
    pub content: String,
    /// Set when role is Assistant and the model wants to call tools.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Set when role is Tool and we're returning a result to the model.
    #[serde(default)]
    pub tool_results: Vec<ToolResult>,
}

/// One inference request from the orchestrator to the router.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// Tools the model is allowed to call (may be empty).
    pub tools: Vec<ToolSchema>,
    /// Maximum tokens the model may generate.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
}

/// JSON-Schema-shaped tool descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Tool name advertised to the model.
    pub name: String,
    /// One-line description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: serde_json::Value,
}

/// The model's reply plus usage/provenance metadata.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The assistant message returned.
    pub message: Message,
    /// Token + cost + latency accounting for this call.
    pub usage: Usage,
    /// Why generation stopped.
    pub finish_reason: FinishReason,
    /// Which backend served the request — populated by the router
    /// after dispatch so callers can record provenance per
    /// MODEL_RESILIENCE.md.
    #[doc(hidden)]
    pub backend: Option<String>,
    /// Which model name served the request. May differ from the
    /// router's declared model when fallback is in play.
    #[doc(hidden)]
    pub model: Option<String>,
    /// True if this response came from a fallback in the
    /// preference chain (primary was unavailable / rate-limited).
    #[doc(hidden)]
    pub via_fallback: bool,
}

/// Token, cost, and latency accounting for one inference call.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    /// Tokens in the prompt.
    pub prompt_tokens: u32,
    /// Tokens generated in the response.
    pub output_tokens: u32,
    /// Prompt tokens served from cache (if the backend reports it).
    pub cache_hit_tokens: u32,
    /// Estimated cost of the call, in micro-USD.
    pub cost_micro_usd: u64,
    /// Wall-clock latency of the call, in milliseconds.
    pub latency_ms: u64,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Natural stop / end of turn.
    Stop,
    /// Hit the `max_tokens` cap.
    MaxTokens,
    /// Stopped to emit tool calls.
    ToolUse,
    /// Generation errored.
    Error,
}
