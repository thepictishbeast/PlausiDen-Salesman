//! LLM-driven cold-email draft generation.
//!
//! Single-shot LLM call: in goes (prospect facts + product fit
//! summary), out comes (subject, body). Reasoning routing hint by
//! default — drafting needs the bigger model for tone control.
//!
//! BUG ASSUMPTION: model output is JSON. We instruct it to emit a
//! strict shape and parse with serde_json. If parsing fails we
//! fall back to extracting subject/body via heuristic markers; on
//! second failure we error and the touch is NOT created.
//!
//! Output is ALWAYS routed to `outcome = AwaitingApproval`.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;

/// One template loaded from `templates/cold/*.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ColdTemplate {
    pub key: String,
    pub description: String,
    #[serde(default)]
    pub segment: Option<String>,
    #[serde(default)]
    pub delay_days: Option<u32>,
    pub subject_seed: String,
    pub body_seed: String,
    #[serde(default)]
    pub mandatory_phrases: Vec<String>,
    #[serde(default)]
    pub forbidden_phrases: Vec<String>,
}

impl ColdTemplate {
    /// Load a template by key from a templates directory. Looks for
    /// `<dir>/<key>.toml`. Returns `None` if file is missing; bubbles
    /// errors only on parse failure.
    pub fn load(templates_dir: &std::path::Path, key: &str) -> Result<Option<Self>> {
        let path = templates_dir.join(format!("{key}.toml"));
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| Error::Io(e))?;
        let parsed: ColdTemplate = toml::from_str(&text)
            .map_err(|e| Error::Validation(format!("template `{key}` parse: {e}")))?;
        Ok(Some(parsed))
    }
}

#[allow(dead_code)]
fn _toml_dep_visible() {
    // PathBuf used in callers only; touch it here so the `std::path::PathBuf` import isn't unused.
    let _: PathBuf = PathBuf::new();
}

/// What the LLM returns. Mirrors the JSON schema in the system prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColdEmailDraft {
    pub subject: String,
    pub body: String,
    /// Short ad-hoc note from the model on why it picked this angle.
    /// Surfaced to the operator in the review queue.
    #[serde(default)]
    pub angle: Option<String>,
    /// Confidence the model has in the personalization.
    /// 0..=1. Used by the dashboard to triage.
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug)]
pub struct DraftColdEmailTool {
    router: Arc<LlmRouter>,
    sender_name: String,
    sender_company: String,
    sender_one_liner: String,
}

impl DraftColdEmailTool {
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
}

#[async_trait]
impl Tool for DraftColdEmailTool {
    fn name(&self) -> &str {
        "content.draft_cold_email"
    }

    fn description(&self) -> &str {
        "Draft a personalized cold outreach email from PlausiDen to a \
         prospect, given prospect facts (display_name, industry, \
         description, tech_signals) and the PlausiDen product to pitch. \
         Returns JSON with subject, body, angle, confidence. Drafts ALWAYS \
         land in the awaiting-approval queue — they are never sent \
         without operator review."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prospect": {
                    "type": "object",
                    "properties": {
                        "display_name":  { "type": "string" },
                        "homepage":      { "type": ["string", "null"] },
                        "industry":      { "type": ["string", "null"] },
                        "description":   { "type": ["string", "null"] },
                        "tech_signals":  { "type": "array" }
                    },
                    "required": ["display_name"]
                },
                "product": {
                    "type": "string",
                    "description": "The PlausiDen product to pitch (Sentinel, Tidy, Atrium, AppGuard, etc.)"
                },
                "angle_hint": {
                    "type": "string",
                    "description": "Optional steering — e.g. 'lead with the compliance angle'."
                }
            },
            "required": ["prospect", "product"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let prospect = args.0.get("prospect").cloned().unwrap_or(Value::Null);
        let product = args
            .0
            .get("product")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("draft_cold_email: missing `product`".into()))?
            .to_string();
        let angle_hint = args
            .0
            .get("angle_hint")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let template_key = args
            .0
            .get("template_key")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let detector_threshold = args
            .0
            .get("detector_threshold")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.6) as f32;
        let max_retries = args
            .0
            .get("max_retries")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as u32;

        // Load template if key + dir provided.
        let template = match (&template_key, std::env::var("SALESMAN_TEMPLATES_DIR").ok()) {
            (Some(key), Some(dir)) => {
                ColdTemplate::load(std::path::Path::new(&dir), key)?
            }
            _ => None,
        };

        let system = self.system_prompt(template.as_ref());
        let user_initial = self.user_prompt(&prospect, &product, angle_hint.as_deref(), template.as_ref());

        // Auto-rewrite-and-retry loop: max_retries + 1 total attempts.
        // Each attempt that fails the detector gets the detector's
        // reasons folded into the next prompt as explicit feedback.
        let mut feedback: Option<String> = None;
        let mut last_resp = None;
        let mut last_draft = None;
        let mut last_score = 0.0f32;
        let mut last_reasons: Vec<String> = vec![];

        for attempt in 0..=max_retries {
            let mut user = user_initial.clone();
            if let Some(fb) = &feedback {
                user.push_str("\n\nPREVIOUS DRAFT FAILED THE AI-DETECTOR. Reasons:\n");
                user.push_str(fb);
                user.push_str("\nWrite a new version that avoids those tells. Same JSON shape.");
            }

            let req = ChatRequest {
                messages: vec![
                    Message {
                        role: Role::System,
                        content: system.clone(),
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
                max_tokens: 1024,
                // Slightly higher temperature on retries — explore.
                temperature: 0.55 + (attempt as f32) * 0.05,
            };

            let resp = self.router.chat_for(RouteHint::Reasoning, "draft_cold_email", req).await?;
            let raw = resp.message.content.trim();
            let draft = parse_draft(raw).map_err(|e| Error::Tool {
                tool: "content.draft_cold_email".into(),
                message: format!("attempt {attempt}: parse: {e} -- raw: {}", truncate(raw, 200)),
            })?;

            let score = salesman_detector::score(&draft.body, Some(&draft.subject));
            last_resp = Some(resp);
            last_draft = Some(draft);
            last_score = score.score;
            last_reasons = score.reasons();

            if score.passes(detector_threshold) {
                break;
            }
            tracing::warn!(attempt, score = score.score, "draft failed detector; retrying");
            feedback = Some(score.reasons().join("\n  "));
        }

        let draft = last_draft.expect("loop runs at least once");
        let resp = last_resp.expect("loop runs at least once");

        Ok(json!({
            "subject": draft.subject,
            "body": draft.body,
            "angle": draft.angle,
            "confidence": draft.confidence,
            "detector_score": last_score,
            "detector_reasons": last_reasons,
            "passed_detector": last_score < detector_threshold,
            "model_latency_ms": resp.usage.latency_ms,
            "model_tokens_in":  resp.usage.prompt_tokens,
            "model_tokens_out": resp.usage.output_tokens,
        }))
    }
}

impl DraftColdEmailTool {
    fn system_prompt(&self, template: Option<&ColdTemplate>) -> String {
        let header = format!(
            "You are a senior B2B sales writer for {}. {}",
            self.sender_company, self.sender_one_liner,
        );
        let from_line = format!("- First-person from {}. Plain text, no markdown.", self.sender_name);
        let mut lines: Vec<String> = vec![
            header,
            String::new(),
            "Write a personalized cold-outreach email. Constraints:".into(),
            from_line,
            "- Subject < 60 chars, no clickbait, no all-caps.".into(),
            "- Body 80-180 words. One short paragraph of personalization (must reference \
              at least one specific fact about the prospect), one short pitch paragraph \
              (one concrete benefit, not feature dump), one explicit ask (low-friction CTA - \
              15-min call, demo link, reply with interest).".into(),
            "- No emoji. No fake urgency. No fake social proof. No 'I noticed' / \
              'I came across' opener cliches. No 'just wanted to' / \
              'hope this finds you well'.".into(),
            "- End with a clear opt-out: 'Reply STOP and I won't follow up.'".into(),
            "- Do NOT promise things the product doesn't do.".into(),
        ];

        if let Some(t) = template {
            lines.push(String::new());
            lines.push(format!("TEMPLATE GUIDANCE (`{}` — {}):", t.key, t.description));
            lines.push("Use this subject seed as a tonal reference (don't paste verbatim):".into());
            lines.push(format!("  Subject seed: {}", t.subject_seed));
            lines.push("Use this body seed as a STRUCTURE + TONE reference. Do not paste it verbatim; rewrite each section using the prospect facts:".into());
            lines.push(t.body_seed.trim().to_string());
            if !t.mandatory_phrases.is_empty() {
                lines.push(String::new());
                lines.push("Mandatory phrases (MUST appear verbatim in the body):".into());
                for p in &t.mandatory_phrases {
                    lines.push(format!("  - {p}"));
                }
            }
            if !t.forbidden_phrases.is_empty() {
                lines.push(String::new());
                lines.push("FORBIDDEN phrases (MUST NOT appear; the model has been observed reaching for these):".into());
                for p in &t.forbidden_phrases {
                    lines.push(format!("  - {p}"));
                }
            }
        }

        lines.push(String::new());
        lines.push("Output STRICT JSON only, this exact shape:".into());
        lines.push(r#"{"subject": string, "body": string, "angle": short string explaining the personalization angle, "confidence": number 0..1}"#.into());
        lines.push("No prose outside the JSON. No code fences.".into());
        lines.join("\n")
    }

    fn user_prompt(
        &self,
        prospect: &Value,
        product: &str,
        angle_hint: Option<&str>,
        template: Option<&ColdTemplate>,
    ) -> String {
        let prospect_pretty =
            serde_json::to_string_pretty(prospect).unwrap_or_else(|_| prospect.to_string());
        let mut s = format!(
            "PROSPECT FACTS (JSON):\n{prospect_pretty}\n\n\
             PRODUCT TO PITCH: {product}\n"
        );
        if let Some(t) = template {
            s.push_str(&format!("TEMPLATE: {} ({})\n", t.key, t.description));
        }
        if let Some(h) = angle_hint {
            s.push_str(&format!("ANGLE HINT: {h}\n"));
        }
        s.push_str("\nWrite the draft. Output STRICT JSON only.");
        s
    }
}

fn parse_draft(raw: &str) -> std::result::Result<ColdEmailDraft, String> {
    if let Ok(d) = serde_json::from_str::<ColdEmailDraft>(raw) {
        return Ok(d);
    }
    // Strip markdown code fences if the model added them despite instructions.
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(d) = serde_json::from_str::<ColdEmailDraft>(stripped) {
        return Ok(d);
    }
    // Last-ditch: try to find the first {...} block.
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
        if end > start {
            let slice = &raw[start..=end];
            if let Ok(d) = serde_json::from_str::<ColdEmailDraft>(slice) {
                warn!("draft parse: fell back to substring extraction");
                return Ok(d);
            }
        }
    }
    Err("output was not valid JSON in any expected shape".into())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}...", &s[..n]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let raw = r#"{"subject":"x","body":"y","angle":"z","confidence":0.7}"#;
        let d = parse_draft(raw).unwrap();
        assert_eq!(d.subject, "x");
        assert_eq!(d.body, "y");
        assert_eq!(d.confidence, Some(0.7));
    }

    #[test]
    fn parses_code_fenced_json() {
        let raw = "```json\n{\"subject\":\"x\",\"body\":\"y\"}\n```";
        let d = parse_draft(raw).unwrap();
        assert_eq!(d.subject, "x");
    }

    #[test]
    fn parses_substring_json() {
        let raw =
            "Sure! Here is the draft:\n\n{\"subject\":\"x\",\"body\":\"y\"}\n\nLet me know if you want changes.";
        let d = parse_draft(raw).unwrap();
        assert_eq!(d.subject, "x");
    }

    #[test]
    fn errors_on_garbage() {
        assert!(parse_draft("totally not json").is_err());
    }
}
