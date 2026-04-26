//! Cross-crate tool envelope. The LLM router and tool registry both
//! depend on these shapes.
//!
//! BUG ASSUMPTION: tool args are JSON values for now. We'll tighten
//! to per-tool schemas once we wire the ToolRegistry validator.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub tool: String,
    pub args: ToolArgs,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolArgs(pub serde_json::Value);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub ok: bool,
    pub value: serde_json::Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}
