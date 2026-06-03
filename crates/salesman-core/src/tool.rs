//! Cross-crate tool envelope. The LLM router and tool registry both
//! depend on these shapes.
//!
//! BUG ASSUMPTION: tool args are JSON values for now. We'll tighten
//! to per-tool schemas once we wire the ToolRegistry validator.

use serde::{Deserialize, Serialize};

/// A request from the LLM to invoke a named tool with arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique id for this call, echoed back in the matching [`ToolResult`].
    pub id: String,
    /// The tool name to invoke (e.g. `osint.wikipedia`).
    pub tool: String,
    /// The tool arguments.
    pub args: ToolArgs,
}

/// Tool arguments, currently an untyped JSON value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolArgs(
    /// The raw JSON arguments object.
    pub serde_json::Value,
);

/// The outcome of a [`ToolCall`], returned to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The [`ToolCall::id`] this result corresponds to.
    pub call_id: String,
    /// Whether the tool succeeded.
    pub ok: bool,
    /// The tool's JSON output (or an error payload when `ok` is false).
    pub value: serde_json::Value,
    /// Error message when `ok` is false; `None` on success.
    pub error: Option<String>,
    /// Wall-clock duration of the invocation, in milliseconds.
    pub duration_ms: u64,
}
