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

use async_trait::async_trait;
use salesman_core::{Result, ToolArgs, ToolCall, ToolResult};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

#[async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn invoke(&self, args: ToolArgs) -> Result<serde_json::Value>;
}

#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

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
