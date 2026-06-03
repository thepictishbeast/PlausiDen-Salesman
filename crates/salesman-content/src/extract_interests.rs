//! LLM-driven interest extractor. Given a positive reply (engaged
//! or question), returns 1–5 SHORT topic tags the prospect cared
//! enough about to mention. Tags merge into prospects.tags['interests']
//! via state::add_prospect_interest, so the next touch in the
//! sequence can cite the interest directly.
//!
//! Routing: Bulk — these run on every classified reply and need to
//! be cheap. Schema is small + strict so a flash-class model is fine.
//!
//! BUG ASSUMPTION: extractor returns STRICT JSON
//! `{ "interests": ["..", ".."] }`. Parse fallbacks (code-fence
//! strip + substring brace search) match the rest of the content
//! crate's pattern.
//!
//! BUG ASSUMPTION: shaping rules — each tag ≤ 4 words, lowercase,
//! no trailing punctuation, no PII (no first names / company
//! names / dollar amounts / URLs). The shaping is enforced after
//! the LLM returns; the LLM is a hint, not a source of truth.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::warn;

/// Interests extracted from a prospect's public signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedInterests {
    /// The extracted interest tags.
    pub interests: Vec<String>,
}

/// Extracts prospect interests from supplied text via the LLM.
#[derive(Debug)]
pub struct InterestExtractTool {
    router: Arc<LlmRouter>,
}

impl InterestExtractTool {
    /// Build the interest-extraction tool over the LLM `router`.
    pub fn new(router: Arc<LlmRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl Tool for InterestExtractTool {
    fn name(&self) -> &str {
        "reply.extract_interests"
    }

    fn description(&self) -> &str {
        "Extract 1-5 short interest tags from a positive prospect \
         reply (engaged / question). Each tag <= 4 words, lowercase, \
         no PII. Returns { \"interests\": [..] }."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "body": { "type": "string" }
            },
            "required": ["body"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let body = args
            .0
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("reply.extract_interests: missing body".into()))?
            .to_string();

        let system = "You extract interest tags from a B2B prospect's \
                      reply. The prospect engaged or asked a question; \
                      identify the SUBSTANTIVE topics they mentioned.\n\
                      \n\
                      Rules:\n\
                      - Output STRICT JSON: { \"interests\": [\"tag\", ..] }\n\
                      - 1 to 5 tags. Fewer is better than weaker tags.\n\
                      - Each tag: <= 4 words, lowercase, no punctuation\n\
                      - No PII: no first names, company names, dollar amounts, URLs, dates\n\
                      - Prefer NOUN PHRASES that name a topic, feature, \
                        concern, or use case (e.g. \"data residency\", \
                        \"security questionnaire\", \"on-prem deployment\")\n\
                      - If the reply is purely a greeting / acknowledgement \
                        with no substantive topic, return { \"interests\": [] }\n\
                      - No prose outside JSON.";

        let user = format!("Reply body:\n{body}\n");

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

        let resp = self
            .router
            .chat_for(RouteHint::Bulk, "extract_interests", req)
            .await?;
        let parsed = parse_extraction(&resp.message.content).unwrap_or_else(|e| {
            warn!("%e" = %e, "interest extractor output unparseable; returning empty");
            ExtractedInterests { interests: vec![] }
        });
        let cleaned = shape_tags(parsed.interests);

        Ok(json!({
            "interests": cleaned,
            "model_latency_ms": resp.usage.latency_ms,
            "model_tokens_in":  resp.usage.prompt_tokens,
            "model_tokens_out": resp.usage.output_tokens,
        }))
    }
}

/// Shape the LLM's tag list into the canonical form: lowercase,
/// trimmed, no trailing punctuation, ≤ 4 words, ≤ 40 chars,
/// deduped, max 5. Drops anything that fails any rule.
pub fn shape_tags(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for t in raw {
        let pre = t.trim().to_ascii_lowercase();
        // Reject obvious PII shapes BEFORE we strip surrounding
        // punctuation: $-prefixed amounts, @-addresses, URLs.
        if pre.contains('@') || pre.contains("http") || pre.starts_with('$') {
            continue;
        }
        let cleaned = pre
            .trim_matches(|c: char| c.is_ascii_punctuation() && c != '-')
            .to_string();
        if cleaned.is_empty() {
            continue;
        }
        if cleaned.chars().count() > 40 {
            continue;
        }
        let word_count = cleaned.split_whitespace().count();
        if word_count == 0 || word_count > 4 {
            continue;
        }
        if seen.insert(cleaned.clone()) {
            out.push(cleaned);
            if out.len() >= 5 {
                break;
            }
        }
    }
    out
}

fn parse_extraction(raw: &str) -> std::result::Result<ExtractedInterests, String> {
    let raw = raw.trim();
    if let Ok(c) = serde_json::from_str::<ExtractedInterests>(raw) {
        return Ok(c);
    }
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(c) = serde_json::from_str::<ExtractedInterests>(stripped) {
        return Ok(c);
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}'))
        && e > s
        && let Ok(c) = serde_json::from_str::<ExtractedInterests>(&raw[s..=e])
    {
        return Ok(c);
    }
    Err("could not parse extractor output".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_drops_empty_and_dedupes() {
        let raw = vec![
            "Data Residency".into(),
            "data residency".into(),
            "".into(),
            "  ".into(),
            "SECURITY QUESTIONNAIRE".into(),
        ];
        let out = shape_tags(raw);
        assert_eq!(out, vec!["data residency", "security questionnaire"]);
    }

    #[test]
    fn shape_caps_at_five_tags() {
        let raw: Vec<String> = (0..10).map(|i| format!("tag {i}")).collect();
        let out = shape_tags(raw);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn shape_drops_overlong_words() {
        let raw = vec![
            "fine".into(),
            "way too many words to count as a tag here".into(),
            "ok now".into(),
        ];
        let out = shape_tags(raw);
        assert_eq!(out, vec!["fine", "ok now"]);
    }

    #[test]
    fn shape_strips_trailing_punctuation_keeps_hyphens() {
        let raw = vec!["on-prem deployment.".into(), "case study!".into()];
        let out = shape_tags(raw);
        assert_eq!(out, vec!["on-prem deployment", "case study"]);
    }

    #[test]
    fn shape_rejects_pii_shapes() {
        let raw = vec![
            "alice@acme.com".into(),
            "https://acme.com".into(),
            "$50K".into(),
            "fine tag".into(),
        ];
        let out = shape_tags(raw);
        assert_eq!(out, vec!["fine tag"]);
    }

    #[test]
    fn parse_fenced_json() {
        let raw = "```json\n{\"interests\":[\"data residency\"]}\n```";
        let p = parse_extraction(raw).unwrap();
        assert_eq!(p.interests, vec!["data residency"]);
    }

    #[test]
    fn parse_with_preamble_substring_match() {
        let raw = "Sure! Here's the JSON:\n\n{\"interests\":[\"a\",\"b\"]}\n";
        let p = parse_extraction(raw).unwrap();
        assert_eq!(p.interests, vec!["a", "b"]);
    }

    #[test]
    fn parse_empty_list_is_valid() {
        let raw = r#"{"interests":[]}"#;
        let p = parse_extraction(raw).unwrap();
        assert!(p.interests.is_empty());
    }

    #[test]
    fn parse_missing_field_errors() {
        let raw = r#"{"other":"stuff"}"#;
        assert!(parse_extraction(raw).is_err());
    }
}
