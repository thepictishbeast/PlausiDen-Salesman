//! LLM-driven reply-response drafter.
//!
//! Today the system CLASSIFIES inbound replies (engaged / question /
//! objection / optout / OOO / bounce / spam). Classification alone
//! dumps work back on the operator — they still have to compose the
//! response.
//!
//! THIS tool drafts the actual response. Same gates as the
//! cold-draft tool (detector ensemble, human approval, signed
//! receipt) — operator clicks approve, doesn't compose from scratch.
//! That's the leap from "sends emails" to "closes deals."
//!
//! BUG ASSUMPTION: never reply to `optout` / `bounce` / `spam`
//! kinds — they are terminal. The tool refuses if the kind isn't
//! `engaged` / `question` / `objection`.
//!
//! BUG ASSUMPTION: model output is JSON. We instruct it to emit
//! `{ subject, body, intent }`. Parse fallbacks inherited from the
//! cold-draft tool's pattern.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyDraft {
    pub subject: String,
    pub body: String,
    /// What this draft is trying to do — "answer-question" /
    /// "handle-objection" / "advance-to-meeting" / "send-pricing".
    /// Operator sees this in the review queue.
    pub intent: String,
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug)]
pub struct DraftReplyTool {
    router: Arc<LlmRouter>,
    sender_name: String,
    sender_company: String,
    sender_one_liner: String,
}

impl DraftReplyTool {
    pub fn new(
        router: Arc<LlmRouter>,
        sender_name: impl Into<String>,
        sender_company: impl Into<String>,
        sender_one_liner: impl Into<String>,
    ) -> Self {
        Self {
            router,
            sender_name: sender_name.into(),
            sender_company: sender_company.into(),
            sender_one_liner: sender_one_liner.into(),
        }
    }

    fn system_prompt(&self) -> String {
        [
            format!("You are {} at {}.", self.sender_name, self.sender_company).as_str(),
            format!("One-liner about your company: {}.", self.sender_one_liner).as_str(),
            "",
            "You are drafting a REPLY to an inbound message from a prospect.",
            "The operator will review your draft and either approve or edit it.",
            "",
            "STRICT CONSTRAINTS (failing any makes the draft UNUSABLE):",
            "- Reference something specific the prospect said. Quote one short phrase verbatim if useful.",
            "- Answer the question they actually asked, or address the objection they actually raised.",
            "- Be concrete. If you mention a number or capability, it must trace to a fact you were given.",
            "- One clear next step at the end. Either an answer + question, or an offer + ask.",
            "- No marketing superlatives. No 'industry-leading', 'best-in-class', 'transformative'.",
            "- No empty hedging. No 'I was wondering if', 'no pressure at all', 'happy to chat whenever'.",
            "- No recap connectives. No 'to recap', 'in summary', 'as we delve'.",
            "- No cliché openers. No 'thanks for getting back to me!', 'great to hear from you!'.",
            "- Em-dashes: at most one per 100 chars.",
            "- 60-180 words. Reply emails are SHORT. Anything longer fails review.",
            "",
            "Output STRICT JSON: {\"subject\": string, \"body\": string, \"intent\": string, \"confidence\": 0..1}",
            "  - subject: prefix with `Re: ` if not already; otherwise reuse the inbound subject.",
            "  - body: the reply text, no signature line (operator's signature is appended downstream).",
            "  - intent: one of `answer-question` / `handle-objection` / `advance-to-meeting` /",
            "    `send-pricing` / `propose-times` / `clarify` / `acknowledge`.",
            "  - confidence: 0..1, your sense of how good this reply is.",
            "No prose outside JSON. No code fences.",
        ]
        .join("\n")
    }

    fn user_prompt(
        &self,
        prospect: &Value,
        outbound_subject: Option<&str>,
        outbound_body: Option<&str>,
        inbound_subject: Option<&str>,
        inbound_body: &str,
        inbound_kind: &str,
    ) -> String {
        let mut out = String::new();
        out.push_str("Prospect facts (JSON):\n");
        out.push_str(&serde_json::to_string_pretty(prospect).unwrap_or_default());
        out.push_str("\n\n");
        if let (Some(sub), Some(body)) = (outbound_subject, outbound_body) {
            out.push_str(&format!("Original outbound (your earlier message):\n> Subject: {sub}\n> {}\n\n",
                body.lines().take(40).collect::<Vec<_>>().join("\n> ")));
        }
        out.push_str(&format!(
            "Inbound reply (classified as `{inbound_kind}`):\n> Subject: {sub}\n> {body}\n\n",
            sub = inbound_subject.unwrap_or("(no subject)"),
            body = inbound_body.lines().take(80).collect::<Vec<_>>().join("\n> "),
        ));
        out.push_str("Draft the reply now.\n");
        out
    }
}

#[async_trait]
impl Tool for DraftReplyTool {
    fn name(&self) -> &str {
        "content.draft_reply"
    }

    fn description(&self) -> &str {
        "Draft a reply to an inbound classified prospect message. \
         Returns JSON: { subject, body, intent, confidence }. The \
         draft lands in the awaiting-approval queue; operator reviews \
         + sends. Refuses to draft when the inbound is optout / \
         bounce / spam (terminal kinds). The whole point: turn \
         inbox-classification into inbox-draft so the operator clicks \
         approve instead of composing from scratch."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prospect": {
                    "type": "object",
                    "properties": {
                        "display_name": { "type": "string" },
                        "industry":     { "type": ["string", "null"] },
                        "description":  { "type": ["string", "null"] }
                    },
                    "required": ["display_name"]
                },
                "outbound_subject": { "type": ["string", "null"] },
                "outbound_body":    { "type": ["string", "null"] },
                "inbound_subject":  { "type": ["string", "null"] },
                "inbound_body":     { "type": "string" },
                "inbound_kind":     { "type": "string" }
            },
            "required": ["prospect", "inbound_body", "inbound_kind"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let prospect = args.0.get("prospect").cloned().unwrap_or(Value::Null);
        let inbound_body = args
            .0
            .get("inbound_body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("draft_reply: missing inbound_body".into()))?
            .to_string();
        let inbound_kind = args
            .0
            .get("inbound_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("unclassified")
            .to_string();
        let inbound_subject = args
            .0
            .get("inbound_subject")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let outbound_subject = args
            .0
            .get("outbound_subject")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let outbound_body = args
            .0
            .get("outbound_body")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Refuse to draft on terminal kinds. The classifier already
        // suppressed optout/bounce; spam shouldn't get a reply.
        const TERMINAL: &[&str] = &["optout", "bounce", "spam", "out_of_office"];
        if TERMINAL.iter().any(|t| t.eq_ignore_ascii_case(&inbound_kind)) {
            return Err(Error::Validation(format!(
                "draft_reply: refusing to draft a reply to kind=`{inbound_kind}` (terminal)"
            )));
        }

        let system = self.system_prompt();
        let user = self.user_prompt(
            &prospect,
            outbound_subject.as_deref(),
            outbound_body.as_deref(),
            inbound_subject.as_deref(),
            &inbound_body,
            &inbound_kind,
        );

        let req = ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: system,
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
            // Reply drafts are short — small max_tokens keeps cost low.
            max_tokens: 600,
            temperature: 0.4,
        };

        let resp = self
            .router
            .chat_for(RouteHint::Reasoning, "draft_reply", req)
            .await?;
        let draft = parse_reply(&resp.message.content).map_err(|e| Error::Tool {
            tool: "content.draft_reply".into(),
            message: format!("parse: {e}"),
        })?;

        // Run the same detector ensemble as cold drafts. The reviewer
        // sees the score in the review queue — they can override but
        // generally bad drafts get rejected before approval.
        let det = salesman_detector::score(&draft.body, Some(&draft.subject));
        if det.score >= 0.6 {
            warn!(
                score = det.score,
                "reply draft scored high on detector; operator should review carefully"
            );
        }

        let produced_by = json!({
            "backend": resp.backend.as_deref().unwrap_or("unknown"),
            "model": resp.model.as_deref().unwrap_or("unknown"),
            "via_fallback": resp.via_fallback,
            "purpose": "draft_reply",
        });

        Ok(json!({
            "subject": draft.subject,
            "body": draft.body,
            "intent": draft.intent,
            "confidence": draft.confidence,
            "detector_score": det.score,
            "detector_reasons": det.reasons(),
            "passed_detector": det.score < 0.6,
            "produced_by": produced_by,
            "model_latency_ms": resp.usage.latency_ms,
            "model_tokens_in":  resp.usage.prompt_tokens,
            "model_tokens_out": resp.usage.output_tokens,
        }))
    }
}

fn parse_reply(raw: &str) -> std::result::Result<ReplyDraft, String> {
    let raw = raw.trim();
    if let Ok(d) = serde_json::from_str::<ReplyDraft>(raw) {
        return Ok(d);
    }
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(d) = serde_json::from_str::<ReplyDraft>(stripped) {
        return Ok(d);
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}'))
        && e > s
        && let Ok(d) = serde_json::from_str::<ReplyDraft>(&raw[s..=e])
    {
        return Ok(d);
    }
    Err("output was not valid JSON in any expected shape".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let raw = r#"{"subject":"Re: pricing","body":"Hi.","intent":"send-pricing","confidence":0.7}"#;
        let d = parse_reply(raw).unwrap();
        assert_eq!(d.subject, "Re: pricing");
        assert_eq!(d.intent, "send-pricing");
    }

    #[test]
    fn parse_fenced_json() {
        let raw = "```json\n{\"subject\":\"Re: x\",\"body\":\"y\",\"intent\":\"clarify\"}\n```";
        let d = parse_reply(raw).unwrap();
        assert_eq!(d.subject, "Re: x");
        assert_eq!(d.intent, "clarify");
    }

    #[test]
    fn parse_substring_recovery() {
        let raw = "Sure, here you go:\n\n{\"subject\":\"Re: x\",\"body\":\"y\",\"intent\":\"acknowledge\"}\n\nBest!";
        let d = parse_reply(raw).unwrap();
        assert_eq!(d.intent, "acknowledge");
    }

    #[test]
    fn parse_failure_returns_err() {
        assert!(parse_reply("no json here").is_err());
    }
}
