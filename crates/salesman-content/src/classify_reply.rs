//! LLM-driven reply classifier. Given a reply body (and optional
//! subject), returns the ReplyKind plus a short reason and a clear
//! optout signal.
//!
//! Routing: defaults to Bulk (Gemini Flash) — these calls run at high
//! volume and the classification is structured + cheap.
//!
//! BUG ASSUMPTION: model returns strict JSON matching `ClassifyReply`.
//! Parse fallbacks (code-fence strip + substring) inherited from the
//! draft tool's pattern.
//!
//! BUG ASSUMPTION: opt-out detection is BOTH heuristic (fast keyword
//! check) AND LLM-classified. Either signal triggers suppression.

use async_trait::async_trait;
use salesman_core::model::ReplyKind;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::str::FromStr;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifyReply {
    pub kind: String,
    pub optout_detected: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug)]
pub struct ReplyClassifyTool {
    router: Arc<LlmRouter>,
}

impl ReplyClassifyTool {
    pub fn new(router: Arc<LlmRouter>) -> Self {
        Self { router }
    }

    /// Quick keyword-only optout check (no LLM call). Used as a
    /// safety net so we never miss an obvious unsubscribe even if the
    /// LLM mis-classifies.
    pub fn keyword_optout(body: &str) -> bool {
        let s = body.to_ascii_lowercase();
        const KEYWORDS: &[&str] = &[
            "unsubscribe",
            "remove me",
            "opt out",
            "opt-out",
            "stop emailing",
            "do not email",
            "do not contact",
            "stop contacting",
            "take me off",
            "no thanks",
            "not interested",
        ];
        KEYWORDS.iter().any(|k| s.contains(k))
    }
}

#[async_trait]
impl Tool for ReplyClassifyTool {
    fn name(&self) -> &str {
        "reply.classify"
    }

    fn description(&self) -> &str {
        "Classify an inbound reply into one of: \
         engaged | question | objection | optout | out_of_office | \
         bounce | spam | unclassified. Also returns optout_detected \
         (bool) and a short reason."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subject": { "type": ["string", "null"] },
                "body":    { "type": "string" }
            },
            "required": ["body"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let body = args
            .0
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("reply.classify: missing body".into()))?
            .to_string();
        let subject = args
            .0
            .get("subject")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let keyword_hit = Self::keyword_optout(&body);

        let system = "You classify B2B outbound-reply emails into ONE of:\n\
                      engaged | question | objection | optout | out_of_office | bounce | spam | unclassified\n\
                      \n\
                      Also detect explicit unsubscribe/opt-out language.\n\
                      Output STRICT JSON: { \"kind\": <category>, \"optout_detected\": bool, \"reason\": short string, \"confidence\": 0..1 }\n\
                      No prose outside JSON.";
        let mut user = String::new();
        if let Some(s) = subject.as_deref() {
            user.push_str(&format!("Subject: {s}\n"));
        }
        user.push_str(&format!("Body:\n{body}\n"));

        let req = ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: system.into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: user,
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            tools: vec![],
            max_tokens: 256,
            temperature: 0.0,
        };

        let resp = self.router.chat(RouteHint::Bulk, req).await?;
        let parsed = parse_classification(&resp.message.content)
            .unwrap_or_else(|e| {
                warn!("%e" = %e, "classifier output unparseable; falling back to heuristic");
                ClassifyReply {
                    kind: if keyword_hit { "optout".into() } else { "unclassified".into() },
                    optout_detected: keyword_hit,
                    reason: Some("LLM output unparseable".into()),
                    confidence: Some(0.4),
                }
            });

        // Force optout if either signal fires — never under-suppress.
        let optout_detected = parsed.optout_detected || keyword_hit;
        let kind = if optout_detected {
            "optout".to_string()
        } else {
            parsed.kind.clone()
        };

        // Validate kind is a known ReplyKind.
        let _ok = ReplyKind::from_str(&kind).map_err(|_| Error::Tool {
            tool: "reply.classify".into(),
            message: format!("LLM returned unknown ReplyKind `{kind}`"),
        })?;

        Ok(json!({
            "kind": kind,
            "optout_detected": optout_detected,
            "reason": parsed.reason,
            "confidence": parsed.confidence,
            "model_latency_ms": resp.usage.latency_ms,
            "model_tokens_in":  resp.usage.prompt_tokens,
            "model_tokens_out": resp.usage.output_tokens,
        }))
    }
}

fn parse_classification(raw: &str) -> std::result::Result<ClassifyReply, String> {
    let raw = raw.trim();
    if let Ok(c) = serde_json::from_str::<ClassifyReply>(raw) {
        return Ok(c);
    }
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(c) = serde_json::from_str::<ClassifyReply>(stripped) {
        return Ok(c);
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}')) {
        if e > s {
            if let Ok(c) = serde_json::from_str::<ClassifyReply>(&raw[s..=e]) {
                return Ok(c);
            }
        }
    }
    Err("could not parse classifier output".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_catches_unsubscribe() {
        assert!(ReplyClassifyTool::keyword_optout("Please unsubscribe me."));
        assert!(ReplyClassifyTool::keyword_optout("remove me from this list"));
        assert!(ReplyClassifyTool::keyword_optout("Not interested, thanks"));
    }

    #[test]
    fn keyword_lets_engaged_through() {
        assert!(!ReplyClassifyTool::keyword_optout("Sure, let's chat next week."));
        assert!(!ReplyClassifyTool::keyword_optout("What's the price?"));
    }

    #[test]
    fn parses_clean_json() {
        let raw = r#"{"kind":"engaged","optout_detected":false,"reason":"asked for demo","confidence":0.9}"#;
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, "engaged");
        assert!(!c.optout_detected);
    }

    #[test]
    fn parses_substring_json() {
        let raw = "Here you go:\n\n{\"kind\":\"optout\",\"optout_detected\":true,\"reason\":\"unsubscribe\"}\n";
        let c = parse_classification(raw).unwrap();
        assert_eq!(c.kind, "optout");
        assert!(c.optout_detected);
    }
}
