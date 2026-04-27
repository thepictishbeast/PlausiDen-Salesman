//! Gemini backend (Google AI Studio v1beta — generateContent).
//!
//! Wire format reference:
//! https://ai.google.dev/api/generate-content
//!
//! Differences from Claude that drive the mapping:
//! - Roles: `user` and `model` (not `assistant`). System prompt goes
//!   in `systemInstruction`, not `contents`.
//! - Content is a list of `parts`. Each part is one of `text`,
//!   `functionCall`, `functionResponse`, `inlineData`.
//! - Tools are declared as `tools[*].functionDeclarations[*]` with
//!   the same `parameters` JSON-Schema shape we already produce.
//!
//! SECURITY: API key in `Zeroizing<String>`. Sent as a query param
//! per Google's docs; never logged.
//!
//! BUG ASSUMPTION: this targets `gemini-1.5-pro` and `gemini-1.5-flash`.
//! Newer Gemini 2.x model variants should work since the schema is
//! stable, but adjust `model` config when Google ships them.

use crate::types::{ChatRequest, ChatResponse, FinishReason, Message, Role, Usage};
use crate::{BackendKind, LlmBackend};
use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs, ToolCall};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use zeroize::Zeroizing;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Debug)]
pub struct GeminiBackend {
    model: String,
    api_key: Zeroizing<String>,
    http: reqwest::Client,
}

impl GeminiBackend {
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: Zeroizing::new(api_key.into()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .expect("reqwest client construction is infallible with these settings"),
        }
    }

    pub fn from_env(model: &str) -> Result<Self> {
        let key = std::env::var("GEMINI_API_KEY")
            .map_err(|_| Error::Config("GEMINI_API_KEY not set".into()))?;
        Ok(Self::new(model, key))
    }
}

#[async_trait]
impl LlmBackend for GeminiBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Gemini
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let started = Instant::now();
        let body = build_request(&req)?;
        let url = format!(
            "{GEMINI_API_BASE}/models/{model}:generateContent?key={key}",
            model = self.model,
            key = &**self.api_key,
        );
        debug!(model = %self.model, "gemini chat request");

        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Llm {
                backend: "gemini".into(),
                message: format!("transport: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Llm {
                backend: "gemini".into(),
                message: format!("HTTP {status}: {}", truncate(&text, 400)),
            });
        }

        let raw: ApiResponse = resp.json().await.map_err(|e| Error::Llm {
            backend: "gemini".into(),
            message: format!("decode: {e}"),
        })?;

        let latency_ms = started.elapsed().as_millis() as u64;
        let candidate = raw
            .candidates
            .into_iter()
            .next()
            .ok_or_else(|| Error::Llm {
                backend: "gemini".into(),
                message: "no candidates in response".into(),
            })?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for part in candidate.content.parts {
            if let Some(t) = part.text {
                text.push_str(&t);
            }
            if let Some(fc) = part.function_call {
                tool_calls.push(ToolCall {
                    id: format!("gemini-{}", uuid_v7_short()),
                    tool: fc.name,
                    args: ToolArgs(fc.args.unwrap_or(Value::Object(Default::default()))),
                });
            }
        }

        let finish_reason = match candidate.finish_reason.as_deref() {
            Some("STOP") => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::MaxTokens,
            Some(other) => {
                warn!(?other, "gemini unexpected finishReason");
                FinishReason::Stop
            }
            None if !tool_calls.is_empty() => FinishReason::ToolUse,
            None => FinishReason::Stop,
        };

        let usage = Usage {
            prompt_tokens: raw.usage_metadata.prompt_token_count.unwrap_or(0),
            output_tokens: raw.usage_metadata.candidates_token_count.unwrap_or(0),
            cache_hit_tokens: raw.usage_metadata.cached_content_token_count.unwrap_or(0),
            cost_micro_usd: 0,
            latency_ms,
        };

        // Promote ToolUse stop reason if model returned function calls
        // even with finish_reason=STOP (Gemini sometimes does this).
        let finish_reason = if !tool_calls.is_empty() && matches!(finish_reason, FinishReason::Stop)
        {
            FinishReason::ToolUse
        } else {
            finish_reason
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
        })
    }
}

// ---------------------------------------------------------------------------
// request builder
// ---------------------------------------------------------------------------

fn build_request(req: &ChatRequest) -> Result<Value> {
    let mut system_text = String::new();
    let mut contents: Vec<Value> = Vec::new();

    for m in &req.messages {
        match m.role {
            Role::System => {
                if !system_text.is_empty() {
                    system_text.push_str("\n\n");
                }
                system_text.push_str(&m.content);
            }
            Role::User => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{"text": m.content}],
                }));
            }
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for c in &m.tool_calls {
                    parts.push(json!({
                        "functionCall": {
                            "name": c.tool,
                            "args": c.args.0,
                        }
                    }));
                }
                contents.push(json!({
                    "role": "model",
                    "parts": parts,
                }));
            }
            Role::Tool => {
                let parts: Vec<Value> = m
                    .tool_results
                    .iter()
                    .map(|r| {
                        // Gemini wants the tool name in the response,
                        // but we only carry call_id. The orchestrator
                        // is responsible for matching call_id back to
                        // the tool that produced it. For now we use a
                        // generic label — the model will see the
                        // value JSON which is what matters.
                        json!({
                            "functionResponse": {
                                "name": "tool",
                                "response": {
                                    "name": "tool",
                                    "content": r.value,
                                    "ok": r.ok,
                                    "error": r.error,
                                },
                            }
                        })
                    })
                    .collect();
                contents.push(json!({
                    "role": "user",
                    "parts": parts,
                }));
            }
        }
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": {
            "maxOutputTokens": req.max_tokens,
            "temperature": req.temperature,
        },
    });

    if !system_text.is_empty() {
        body["systemInstruction"] = json!({
            "parts": [{"text": system_text}],
        });
    }

    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        body["tools"] = json!([{ "functionDeclarations": decls }]);
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

fn uuid_v7_short() -> String {
    // Gemini doesn't return per-call ids. Synthesize one so the
    // orchestrator can correlate request-result pairs in its logs.
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

// ---------------------------------------------------------------------------
// wire shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ApiResponse {
    candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: UsageMetadata,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: CandContent,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CandContent {
    parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FnCall>,
}

#[derive(Debug, Deserialize)]
struct FnCall {
    name: String,
    args: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: Option<u32>,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: Option<u32>,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<u32>,
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSchema;

    fn req() -> ChatRequest {
        ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "Be concise.".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: "search for X".into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            tools: vec![ToolSchema {
                name: "search".into(),
                description: "Web search.".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: 1024,
            temperature: 0.2,
        }
    }

    #[test]
    fn builds_request_with_system_instruction() {
        let body = build_request(&req()).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be concise.");
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "search for X");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "search"
        );
    }

    #[test]
    fn maps_assistant_tool_call_to_function_call_part() {
        let mut r = req();
        r.messages.push(Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "x".into(),
                tool: "search".into(),
                args: ToolArgs(json!({"q":"hi"})),
            }],
            tool_results: vec![],
        });
        let body = build_request(&r).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "search");
        assert_eq!(contents[1]["parts"][0]["functionCall"]["args"]["q"], "hi");
    }
}
