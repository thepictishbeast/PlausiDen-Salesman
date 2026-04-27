//! Claude backend (Anthropic Messages API).
//!
//! Wire format reference: https://docs.anthropic.com/en/api/messages
//!
//! Highlights:
//! - System prompt is hoisted out of `messages` into the top-level
//!   `system` field (Anthropic does not accept a system role inside
//!   the messages array).
//! - Our `Role::Tool` messages translate to user-role messages whose
//!   content is a list of `tool_result` blocks.
//! - Our `Role::Assistant` messages with tool_calls translate to a
//!   content block list containing optional `text` + one `tool_use`
//!   per call.
//! - Prompt caching: we mark the system block + the tools block with
//!   `cache_control = ephemeral` so cold-start cost amortizes after
//!   the first call in a session.
//!
//! SECURITY: API key is held in `Zeroizing<String>`. Never logged.
//! SECURITY: latency / token counts are logged; prompts and outputs
//! are not (avoid leaking PII into journald).

use crate::types::{ChatRequest, ChatResponse, FinishReason, Message, Role, Usage};
use crate::{BackendKind, LlmBackend};
use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs, ToolCall};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use zeroize::Zeroizing;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug)]
pub struct ClaudeBackend {
    model: String,
    api_key: Zeroizing<String>,
    http: reqwest::Client,
}

impl ClaudeBackend {
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: Zeroizing::new(api_key.into()),
            // SAFETY: rustls-tls + single timeout setter; the
            // configured combination cannot drive Client::build()
            // to fail in practice. If the rustls feature is ever
            // changed, this expect needs revisiting.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .expect("reqwest client construction is infallible with these settings"),
        }
    }

    pub fn from_env(model: &str) -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| Error::Config("ANTHROPIC_API_KEY not set".into()))?;
        Ok(Self::new(model, key))
    }
}

#[async_trait]
impl LlmBackend for ClaudeBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Claude
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let started = Instant::now();
        let body = build_request(&self.model, &req)?;
        debug!(model = %self.model, "claude chat request");

        let resp = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &**self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Llm {
                backend: "claude".into(),
                message: format!("transport: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Llm {
                backend: "claude".into(),
                message: format!("HTTP {status}: {}", truncate(&text, 400)),
            });
        }

        let raw: ApiResponse = resp.json().await.map_err(|e| Error::Llm {
            backend: "claude".into(),
            message: format!("decode: {e}"),
        })?;

        let latency_ms = started.elapsed().as_millis() as u64;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for block in raw.content {
            match block {
                ContentBlock::Text { text: t } => text.push_str(&t),
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        tool: name,
                        args: ToolArgs(input),
                    });
                }
            }
        }

        let finish_reason = match raw.stop_reason.as_deref() {
            Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
            Some("tool_use") => FinishReason::ToolUse,
            Some("max_tokens") => FinishReason::MaxTokens,
            other => {
                warn!(?other, "unexpected stop_reason");
                FinishReason::Stop
            }
        };

        let usage = Usage {
            prompt_tokens: raw.usage.input_tokens,
            output_tokens: raw.usage.output_tokens,
            cache_hit_tokens: raw.usage.cache_read_input_tokens.unwrap_or(0),
            cost_micro_usd: 0, // computed by salesman-state from per-model rates
            latency_ms,
        };

        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: text,
                tool_calls,
                tool_results: vec![],
            },
            usage,
            finish_reason,
            backend: Some("claude".into()),
            model: Some(self.model.clone()),
            via_fallback: false,
        })
    }
}

// ---------------------------------------------------------------------------
// request builder
// ---------------------------------------------------------------------------

fn build_request(model: &str, req: &ChatRequest) -> Result<Value> {
    // Hoist system messages out of the message stream.
    let mut system_text = String::new();
    let mut wire_messages: Vec<Value> = Vec::new();

    for m in &req.messages {
        match m.role {
            Role::System => {
                if !system_text.is_empty() {
                    system_text.push_str("\n\n");
                }
                system_text.push_str(&m.content);
            }
            Role::User => {
                wire_messages.push(json!({
                    "role": "user",
                    "content": m.content,
                }));
            }
            Role::Assistant => {
                let mut blocks: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    blocks.push(json!({"type": "text", "text": m.content}));
                }
                for c in &m.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.tool,
                        "input": c.args.0,
                    }));
                }
                wire_messages.push(json!({
                    "role": "assistant",
                    "content": blocks,
                }));
            }
            Role::Tool => {
                // Translate to a user message with tool_result blocks.
                let blocks: Vec<Value> = m
                    .tool_results
                    .iter()
                    .map(|r| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": r.call_id,
                            "content": r.value.to_string(),
                            "is_error": !r.ok,
                        })
                    })
                    .collect();
                wire_messages.push(json!({
                    "role": "user",
                    "content": blocks,
                }));
            }
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
        "messages": wire_messages,
    });

    if !system_text.is_empty() {
        // Ephemeral cache mark amortizes the system prompt cost.
        body["system"] = json!([{
            "type": "text",
            "text": system_text,
            "cache_control": { "type": "ephemeral" },
        }]);
    }

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }

    Ok(body)
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}...", &s[..n])
    }
}

// ---------------------------------------------------------------------------
// wire shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    usage: ApiUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    cache_creation_input_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// tests (build-only — no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, Role, ToolSchema};
    use salesman_core::{ToolArgs, ToolCall, ToolResult};

    fn req() -> ChatRequest {
        ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "You are a helpful agent.".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: "Find me 3 prospects.".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            tools: vec![ToolSchema {
                name: "search".into(),
                description: "Web search.".into(),
                input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            }],
            max_tokens: 1024,
            temperature: 0.4,
        }
    }

    #[test]
    fn builds_request_hoists_system() {
        let body = build_request("claude-opus-4-7", &req()).unwrap();
        assert_eq!(body["model"], "claude-opus-4-7");
        assert!(body["system"].is_array());
        assert_eq!(body["system"][0]["text"], "You are a helpful agent.");
        // System message must NOT appear in the messages array.
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        // Tool exposed.
        assert_eq!(body["tools"][0]["name"], "search");
    }

    #[test]
    fn round_trips_tool_use_and_result() {
        let mut r = req();
        r.messages.push(Message {
            role: Role::Assistant,
            content: "calling search".into(),
            tool_calls: vec![ToolCall {
                id: "tool_1".into(),
                tool: "search".into(),
                args: ToolArgs(json!({"q": "rust security"})),
            }],
            tool_results: vec![],
        });
        r.messages.push(Message {
            role: Role::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "tool_1".into(),
                ok: true,
                value: json!({"hits": ["a", "b"]}),
                error: None,
                duration_ms: 10,
            }],
        });
        let body = build_request("claude-sonnet-4-6", &r).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "text");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][1]["id"], "tool_1");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "tool_1");
    }
}
