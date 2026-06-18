//! salesman-tools — the action-surface the agent loop is allowed to
//! invoke.
//!
//! Each tool is a `Tool` impl that:
//! - declares a `name`, `description`, and JSON `input_schema`,
//! - validates incoming args against the schema,
//! - performs the action,
//! - returns a `ToolResult`.
//!
//! The `ToolRegistry` is what the orchestrator consults at every
//! agent step.
//!
//! BUG ASSUMPTION: tool implementations are stateless or hold their
//! own state via `Arc`s captured at construction. The registry does
//! not synchronize concurrent calls into a single tool — implementors
//! own their concurrency.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use async_trait::async_trait;
use salesman_core::{Result, ToolArgs, ToolCall, ToolResult};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// An action the agent loop is allowed to invoke. Each tool declares a
/// name + description + JSON input schema (advertised to the LLM as the
/// available surface), validates its args, performs the action, and
/// returns a JSON result. Implementors own their own concurrency.
#[async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    /// Stable identifier the LLM uses to call this tool; also the
    /// registry key, so it must be unique within a `ToolRegistry`.
    fn name(&self) -> &str;
    /// One-line, human/LLM-facing description of what the tool does.
    fn description(&self) -> &str;
    /// JSON Schema describing the tool's arguments, advertised to the LLM.
    fn input_schema(&self) -> serde_json::Value;
    /// Run the tool against `args`, returning its JSON output — or an
    /// error, which the registry converts into a failed `ToolResult`.
    async fn invoke(&self, args: ToolArgs) -> Result<serde_json::Value>;
}

/// The set of tools the orchestrator can dispatch to, keyed by name.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its `name()`. A later registration with the
    /// same name overwrites the earlier one.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a registered tool by name, cloning the `Arc` handle.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// The names of all registered tools (unordered).
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Get all registered tools as JSON-Schema descriptors suitable
    /// for handing to the LLM router as the available tool surface.
    pub fn schemas(&self) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "input_schema": t.input_schema(),
                })
            })
            .collect()
    }

    /// Dispatch a `ToolCall` to the named tool and return a `ToolResult`.
    /// An unknown tool name or a tool error both yield `ok: false` (never
    /// a panic); `duration_ms` is always recorded.
    pub async fn invoke(&self, call: ToolCall) -> ToolResult {
        let start = Instant::now();
        let tool = match self.get(&call.tool) {
            Some(t) => t,
            None => {
                return ToolResult {
                    call_id: call.id,
                    ok: false,
                    value: serde_json::Value::Null,
                    error: Some(format!("unknown tool: {}", call.tool)),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
        };
        match tool.invoke(call.args).await {
            Ok(value) => ToolResult {
                call_id: call.id,
                ok: true,
                value,
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
            },
            Err(e) => ToolResult {
                call_id: call.id,
                ok: false,
                value: serde_json::Value::Null,
                error: Some(e.to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            },
        }
    }
}

/// A simple no-op tool used by the dry-run / smoke tests so the
/// orchestrator can be exercised without any real outbound effects.
#[derive(Debug)]
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Returns the args verbatim. Used in dry-run + smoke tests."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "msg": { "type": "string" } },
            "required": ["msg"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<serde_json::Value> {
        Ok(args.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use salesman_core::ToolArgs;
    use serde_json::json;

    #[tokio::test]
    async fn echo_round_trips() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let res = reg
            .invoke(ToolCall {
                id: "abc".into(),
                tool: "echo".into(),
                args: ToolArgs(json!({ "msg": "hi" })),
            })
            .await;
        assert!(res.ok);
        assert_eq!(res.value, json!({ "msg": "hi" }));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let reg = ToolRegistry::new();
        let res = reg
            .invoke(ToolCall {
                id: "abc".into(),
                tool: "missing".into(),
                args: ToolArgs(json!({})),
            })
            .await;
        assert!(!res.ok);
        let _err: String = res.error.unwrap_or_default();
    }
}
