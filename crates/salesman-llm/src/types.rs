use salesman_core::{ToolCall, ToolResult};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    Claude,
    Gemini,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
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
    pub messages: Vec<Message>,
    /// Tools the model is allowed to call (may be empty).
    pub tools: Vec<ToolSchema>,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// JSON-Schema-shaped tool descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub output_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cost_micro_usd: u64,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    MaxTokens,
    ToolUse,
    Error,
}
