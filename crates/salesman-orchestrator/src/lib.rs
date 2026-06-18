//! salesman-orchestrator — the agentic loop.
//!
//! Each iteration:
//! 1. Hand the conversation + available tools to the LLM router.
//! 2. If the model returns tool calls, dispatch them through the
//!    `ToolRegistry`.
//! 3. Append tool results to the conversation; loop.
//! 4. If the model returns plain text (no tool calls), emit it as the
//!    step output.
//!
//! BUG ASSUMPTION: a single agent step runs to completion before the
//! next is queued. We do not interleave two campaigns through the
//! same orchestrator instance — caller spawns one orchestrator per
//! campaign worker.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use salesman_core::Result;
use salesman_llm::{ChatRequest, ChatResponse, LlmRouter, Message, Role, RouteHint};
use salesman_tools::ToolRegistry;
use std::sync::Arc;
use tracing::{debug, info};

/// The agentic loop: drives an LLM conversation, dispatching tool calls
/// through the registry until the model returns a terminal answer or the
/// step cap is reached.
#[derive(Debug)]
pub struct Orchestrator {
    router: Arc<LlmRouter>,
    tools: Arc<ToolRegistry>,
    max_steps: u32,
}

impl Orchestrator {
    /// Build an orchestrator over an LLM router + tool registry, defaulting
    /// to a 16-step cap per run.
    pub fn new(router: Arc<LlmRouter>, tools: Arc<ToolRegistry>) -> Self {
        Self {
            router,
            tools,
            max_steps: 16,
        }
    }

    /// Override the maximum number of LLM turns per run (builder style).
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Drive a conversation forward up to `max_steps` LLM turns.
    /// Returns the final response (or the last response if `max_steps`
    /// was hit without a terminal answer).
    pub async fn run(&self, hint: RouteHint, mut messages: Vec<Message>) -> Result<ChatResponse> {
        let tool_schemas = self
            .tools
            .schemas()
            .into_iter()
            .map(|v| salesman_llm::types::ToolSchema {
                name: v["name"].as_str().unwrap_or_default().to_string(),
                description: v["description"].as_str().unwrap_or_default().to_string(),
                input_schema: v["input_schema"].clone(),
            })
            .collect::<Vec<_>>();

        for step in 0..self.max_steps {
            info!(step, "agent step start");
            let req = ChatRequest {
                messages: messages.clone(),
                tools: tool_schemas.clone(),
                max_tokens: 4096,
                temperature: 0.4,
            };
            let resp = self.router.chat(hint, req).await?;

            if resp.message.tool_calls.is_empty() {
                info!(step, "agent step terminated without tool calls");
                return Ok(resp);
            }

            // The model wants to invoke tools — execute them and
            // feed results back as a Tool message.
            let mut tool_results = Vec::new();
            for call in resp.message.tool_calls.iter() {
                debug!(tool = %call.tool, "invoking tool");
                let result = self.tools.invoke(call.clone()).await;
                tool_results.push(result);
            }

            messages.push(resp.message);
            messages.push(Message {
                role: Role::Tool,
                content: String::new(),
                tool_calls: Vec::new(),
                tool_results,
            });
        }

        // Hit max_steps without a terminal answer — return whatever
        // the last call produced. The orchestrator caller decides
        // what to do with that.
        let req = ChatRequest {
            messages,
            tools: tool_schemas,
            max_tokens: 1024,
            temperature: 0.0,
        };
        self.router.chat(hint, req).await
    }
}
