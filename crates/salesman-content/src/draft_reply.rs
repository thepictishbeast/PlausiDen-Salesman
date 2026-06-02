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
    /// Build the reply-drafting tool over the LLM `router`, signing as
    /// `sender_name` at `sender_company` with the `sender_one_liner` pitch.
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
            "- If `prospect.tags.interests` is non-empty, you MAY also bridge \
              to one of those interests when it naturally fits — they are \
              topics the prospect previously expressed interest in across \
              prior touches. Don't force it; only use when the bridge is \
              genuinely relevant.",
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

    /// System prompt for the objection-handling path. Threads the
    /// operator's pre-written talking points + posture guidance into
    /// the prompt so the response anchors in real facts the operator
    /// curated, instead of LLM-from-scratch generic-objection-handling.
    fn objection_system_prompt(&self, obj: &Value) -> String {
        let base = self.system_prompt();
        let key = obj.get("key").and_then(|v| v.as_str()).unwrap_or("?");
        let posture = obj
            .get("posture")
            .and_then(|v| v.as_str())
            .unwrap_or("calm; respectful");
        let talking_points = obj
            .get("talking_points")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let suffix = format!(
            "\n\n## OBJECTION-HANDLING MODE\n\
             The prospect raised a `{key}` objection. The operator has \
             pre-written talking points + posture guidance below. WEAVE \
             ONE OR TWO of the talking points naturally into your \
             response — do NOT list them all back at the prospect. \
             Match the posture.\n\n\
             intent should be `handle-objection`.\n\
             Length budget: 70-180 words.\n\n\
             ## POSTURE\n\
             {posture}\n\n\
             ## TALKING POINTS (pick 1-2 to weave in)\n\
             {talking_points}"
        );
        format!("{base}{suffix}")
    }

    /// System prompt for the meeting-shaped path. Adds the
    /// operator's REAL upcoming slots so the drafter proposes them
    /// specifically rather than asking "what time works."
    fn meeting_system_prompt(&self, calendar: &Value) -> String {
        let base = self.system_prompt();
        let cal_text = serde_json::to_string_pretty(calendar).unwrap_or_default();
        let suffix = format!(
            "\n\n## MEETING-SHAPED REPLY MODE\n\
             The prospect is asking to schedule. Propose THREE slots \
             from the operator's calendar below, in their stated \
             timezone. Format the slots as a small list the prospect \
             can pick from. Include the meeting duration + Zoom link \
             (or other join URL) when available. If the calendar has \
             no upcoming slots, fall back to asking what works for \
             them — do NOT invent times.\n\n\
             intent should be `propose-times`.\n\
             Length budget: 70-160 words.\n\n\
             ## OPERATOR CALENDAR\n\
             {cal_text}"
        );
        format!("{base}{suffix}")
    }

    /// System prompt for the pricing-shaped path. Adds the pricing
    /// catalog inline so the model has SPECIFIC numbers to quote
    /// rather than ranges.
    fn pricing_system_prompt(&self, catalog: &str) -> String {
        let base = self.system_prompt();
        let suffix = format!(
            "\n\n## PRICING-SHAPED REPLY MODE\n\
             The prospect is asking about price. Use the catalog below \
             to quote SPECIFIC tier names + monthly_usd numbers. Do \
             NOT invent prices. If the catalog doesn't cover the case, \
             say so honestly and offer to follow up with a custom \
             quote.\n\n\
             intent should be `send-pricing`.\n\
             Length budget: 80-220 words for the body (slightly higher \
             than default; pricing replies need a short table or \
             bullet list to be useful).\n\
             You may use a short bullet list of tiers if it makes the \
             reply scannable. Keep it terse.\n\n\
             ## PRICING CATALOG\n\
             {catalog}"
        );
        format!("{base}{suffix}")
    }

    fn user_prompt(&self, ctx: ReplyDraftContext<'_>) -> String {
        let mut out = String::new();
        out.push_str("Prospect facts (JSON):\n");
        out.push_str(&serde_json::to_string_pretty(ctx.prospect).unwrap_or_default());
        out.push_str("\n\n");
        if let Some(thread_v) = ctx.prior_thread
            && let Some(turns) = thread_v.as_array()
            && !turns.is_empty()
        {
            out.push_str(format_prior_thread(turns).as_str());
        }
        if let (Some(sub), Some(body)) = (ctx.outbound_subject, ctx.outbound_body) {
            out.push_str(&format!(
                "Original outbound (the message they're replying to):\n> Subject: {sub}\n> {}\n\n",
                body.lines().take(40).collect::<Vec<_>>().join("\n> ")
            ));
        }
        out.push_str(&format!(
            "Inbound reply (classified as `{kind}`):\n> Subject: {sub}\n> {body}\n\n",
            kind = ctx.inbound_kind,
            sub = ctx.inbound_subject.unwrap_or("(no subject)"),
            body = ctx
                .inbound_body
                .lines()
                .take(80)
                .collect::<Vec<_>>()
                .join("\n> "),
        ));
        out.push_str("Draft the reply now.\n");
        out
    }
}

/// Bundle of inputs for `DraftReplyTool::user_prompt`. Keeps the
/// method signature under the seven-arg lint and gives callers one
/// thing to construct instead of passing six positional Options.
#[derive(Debug, Clone, Copy)]
pub struct ReplyDraftContext<'a> {
    pub prospect: &'a Value,
    pub prior_thread: Option<&'a Value>,
    pub outbound_subject: Option<&'a str>,
    pub outbound_body: Option<&'a str>,
    pub inbound_subject: Option<&'a str>,
    pub inbound_body: &'a str,
    pub inbound_kind: &'a str,
}

/// Render a prior-thread JSON array (each element a
/// `{ role, at, subject, body, reply_kind? }` from
/// `State::list_thread_for_prospect`) into a compact
/// "CONVERSATION HISTORY" block for the reply-drafter prompt.
/// Excludes the most recent two turns (the inbound being replied
/// to and the outbound it's responding to) — those are already
/// supplied in detail elsewhere in the prompt. Each prior body
/// is line-truncated so the total prompt stays bounded.
pub fn format_prior_thread(turns: &[Value]) -> String {
    if turns.len() <= 2 {
        return String::new();
    }
    let earlier = &turns[..turns.len() - 2];
    let mut out = String::from(
        "Conversation history (oldest → newest, excluding the two most recent turns):\n",
    );
    for t in earlier {
        let role = t.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        let at = t.get("at").and_then(|v| v.as_str()).unwrap_or("?");
        let subject = t.get("subject").and_then(|v| v.as_str()).unwrap_or("");
        let body = t.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let kind = t
            .get("reply_kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let kind_tag = if kind.is_empty() {
            String::new()
        } else {
            format!(" ({kind})")
        };
        out.push_str(&format!("- [{role}{kind_tag} @ {at}] {subject}\n"));
        for line in body.lines().take(6) {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out.push('\n');
    out
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
                "inbound_kind":     { "type": "string" },
                "pricing_catalog":  {
                    "type": ["string", "null"],
                    "description": "Optional pricing catalog text (TOML/markdown). \
                                    When present + the inbound looks pricing-shaped, \
                                    the drafter includes specific numbers grounded \
                                    in the catalog instead of vague ranges."
                },
                "meeting_calendar": {
                    "type": ["object", "null"],
                    "description": "Optional MeetingCalendar.to_drafter_value() blob. \
                                    When present + inbound looks meeting-shaped, the \
                                    drafter proposes 3 specific slots from the operator's \
                                    real calendar instead of inviting a back-and-forth."
                },
                "objection_match": {
                    "type": ["object", "null"],
                    "description": "Optional ObjectionLibrary.to_drafter_value() blob. \
                                    When present + inbound is an objection, the drafter \
                                    weaves the operator's pre-written talking points + \
                                    posture into the response."
                }
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
        let pricing_catalog = args
            .0
            .get("pricing_catalog")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let meeting_calendar = args.0.get("meeting_calendar").cloned();
        let objection_match = args.0.get("objection_match").cloned();
        let prior_thread = args.0.get("prior_thread").cloned();

        // Refuse to draft on terminal kinds. The classifier already
        // suppressed optout/bounce; spam shouldn't get a reply.
        // legal_threat is treated even more strictly — operator MUST
        // handle it personally, never auto-draft a response.
        const TERMINAL: &[&str] =
            &["optout", "bounce", "spam", "out_of_office", "legal_threat"];
        if TERMINAL
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&inbound_kind))
        {
            // SECURITY: legal_threat surfaces a louder error so the
            // operator notices in the batch-drafter logs even if
            // alerts isn't running. The drafter MUST NOT compose
            // anything that could be construed as a legal response.
            if inbound_kind.eq_ignore_ascii_case("legal_threat") {
                tracing::warn!(
                    "draft_reply: REFUSING to draft a reply to a legal_threat \
                     classification — operator must respond personally"
                );
            }
            return Err(Error::Validation(format!(
                "draft_reply: refusing to draft a reply to kind=`{inbound_kind}` (terminal)"
            )));
        }

        // Pick the right system prompt for the kind of reply.
        // Priority: meeting (highest, time-sensitive) > objection
        // (matches > kind-label since prospect may say "we already
        // have X" inside a question shape) > pricing > default.
        let meeting_shaped = looks_like_meeting_question(&inbound_body);
        let pricing_shaped = looks_like_pricing_question(&inbound_body);
        let system = if meeting_shaped
            && let Some(cal_v) = meeting_calendar.as_ref()
            && let Some(slots) = cal_v.get("slots").and_then(|s| s.as_array())
            && !slots.is_empty()
        {
            self.meeting_system_prompt(cal_v)
        } else if let Some(obj) = objection_match.as_ref() {
            self.objection_system_prompt(obj)
        } else if pricing_shaped && let Some(cat) = pricing_catalog.as_deref() {
            self.pricing_system_prompt(cat)
        } else {
            self.system_prompt()
        };
        let user = self.user_prompt(ReplyDraftContext {
            prospect: &prospect,
            prior_thread: prior_thread.as_ref(),
            outbound_subject: outbound_subject.as_deref(),
            outbound_body: outbound_body.as_deref(),
            inbound_subject: inbound_subject.as_deref(),
            inbound_body: &inbound_body,
            inbound_kind: &inbound_kind,
        });

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

/// One entry in the operator's objection library.
#[derive(Debug, Clone, Deserialize)]
pub struct ObjectionEntry {
    pub key: String,
    /// Lower-case substrings the matcher looks for in the inbound.
    /// Any match → this entry is the threaded objection.
    pub matches: Vec<String>,
    /// Anchor facts the drafter weaves into the response. Not a
    /// full reply — the drafter still personalizes.
    pub talking_points: Vec<String>,
    /// Posture guidance ("calm", "respectful", "honest about being small")
    /// — included verbatim in the system prompt.
    #[serde(default)]
    pub posture: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObjectionLibrary {
    #[serde(default)]
    pub objections: Vec<ObjectionEntry>,
}

impl ObjectionLibrary {
    /// First entry whose matches[] substrings appear in the inbound
    /// body (case-insensitive). Returns None if no rule fires.
    pub fn match_inbound(&self, inbound: &str) -> Option<&ObjectionEntry> {
        let lc = inbound.to_ascii_lowercase();
        self.objections.iter().find(|o| {
            o.matches
                .iter()
                .any(|m| lc.contains(&m.to_ascii_lowercase()))
        })
    }

    /// Render the matched entry as a JSON object the drafter can
    /// thread into the prompt. None when nothing matched.
    pub fn to_drafter_value(&self, inbound: &str) -> Option<Value> {
        self.match_inbound(inbound).map(|o| {
            json!({
                "key": o.key,
                "talking_points": o.talking_points,
                "posture": o.posture,
            })
        })
    }
}

/// Load an objection library from TOML.
pub fn load_objections_toml(text: &str) -> Result<ObjectionLibrary> {
    let lib: ObjectionLibrary =
        toml::from_str(text).map_err(|e| Error::Validation(format!("objections parse: {e}")))?;
    Ok(lib)
}

/// One offered meeting slot. Operator-curated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingSlot {
    pub start: chrono::DateTime<chrono::FixedOffset>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Operator-curated calendar config. Read from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct MeetingCalendar {
    #[serde(default = "default_duration_minutes")]
    pub duration_minutes: u32,
    pub timezone: Option<String>,
    pub zoom_link: Option<String>,
    #[serde(default)]
    pub slots: Vec<MeetingSlot>,
}

fn default_duration_minutes() -> u32 {
    30
}

impl MeetingCalendar {
    /// Filter to upcoming slots (start > now) and take the first N.
    pub fn upcoming(&self, now: chrono::DateTime<chrono::Utc>, take: usize) -> Vec<&MeetingSlot> {
        self.slots
            .iter()
            .filter(|s| s.start.with_timezone(&chrono::Utc) > now)
            .take(take)
            .collect()
    }

    /// Render to a JSON value the drafter ingests.
    pub fn to_drafter_value(&self, now: chrono::DateTime<chrono::Utc>, take: usize) -> Value {
        let upcoming = self.upcoming(now, take);
        json!({
            "duration_minutes": self.duration_minutes,
            "timezone": self.timezone,
            "zoom_link": self.zoom_link,
            "slots": upcoming.iter().map(|s| json!({
                "start_iso": s.start.to_rfc3339(),
                "note": s.note,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Load a meeting-calendar from TOML.
pub fn load_calendar_toml(text: &str) -> Result<MeetingCalendar> {
    let cal: MeetingCalendar =
        toml::from_str(text).map_err(|e| Error::Validation(format!("calendar parse: {e}")))?;
    Ok(cal)
}

/// True if the inbound reply asks for a meeting time. Cheap keyword
/// check (no LLM). Caller uses this to decide whether to thread the
/// calendar into the drafter.
pub fn looks_like_meeting_question(body: &str) -> bool {
    let s = body.to_ascii_lowercase();
    const KEYWORDS: &[&str] = &[
        "when can we meet",
        "when can we talk",
        "when can we hop on",
        "let's meet",
        "let's hop on",
        "let's chat",
        "schedule a call",
        "schedule a time",
        "schedule a meeting",
        "what's a good time",
        "what time works",
        "send me a time",
        "set up a call",
        "set up a meeting",
        "book a call",
        "book a time",
        "happy to chat",
        "happy to talk",
    ];
    KEYWORDS.iter().any(|k| s.contains(k))
}

/// True if the inbound reply looks like a pricing question. Cheap
/// keyword check; no LLM call. The caller uses this to decide
/// whether to switch to the pricing-system-prompt path.
pub fn looks_like_pricing_question(body: &str) -> bool {
    let s = body.to_ascii_lowercase();
    const KEYWORDS: &[&str] = &[
        "what's the price",
        "how much does it cost",
        "how much is",
        "pricing",
        "pricelist",
        "send a quote",
        "send me a quote",
        "send pricing",
        "what does it cost",
        "what would it cost",
        "rate card",
        "monthly cost",
        "annual cost",
        "license cost",
        "license fee",
        "subscription cost",
    ];
    KEYWORDS.iter().any(|k| s.contains(k))
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
        let raw =
            r#"{"subject":"Re: pricing","body":"Hi.","intent":"send-pricing","confidence":0.7}"#;
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

    #[test]
    fn pricing_keyword_detector_fires() {
        assert!(looks_like_pricing_question("how much does it cost?"));
        assert!(looks_like_pricing_question("send me a quote, please"));
        assert!(looks_like_pricing_question("can you share pricing"));
        assert!(!looks_like_pricing_question("thanks for the demo"));
    }

    #[test]
    fn meeting_keyword_detector_fires() {
        assert!(looks_like_meeting_question("when can we meet?"));
        assert!(looks_like_meeting_question("Let's set up a call next week"));
        assert!(!looks_like_meeting_question("appreciate the writeup"));
    }

    #[test]
    fn objection_library_matches_first_keyword() {
        let toml = r#"
            [[objections]]
            key = "price-too-high"
            matches = ["too expensive", "out of budget"]
            talking_points = ["self-host is free"]
            posture = "calm"

            [[objections]]
            key = "not-now"
            matches = ["next quarter"]
            talking_points = ["leave a useful artifact"]
        "#;
        let lib = load_objections_toml(toml).unwrap();
        let m = lib.match_inbound("Honestly this is too expensive for us right now");
        assert!(m.is_some());
        assert_eq!(m.unwrap().key, "price-too-high");

        let none = lib.match_inbound("Thanks, will read the deck");
        assert!(none.is_none());
    }

    #[test]
    fn objection_to_drafter_value_includes_posture() {
        let toml = r#"
            [[objections]]
            key = "trust"
            matches = ["never heard of you"]
            talking_points = ["audit our source"]
            posture = "honest"
        "#;
        let lib = load_objections_toml(toml).unwrap();
        let v = lib
            .to_drafter_value("we've never heard of you")
            .expect("should match");
        assert_eq!(v["key"], "trust");
        assert_eq!(v["posture"], "honest");
    }

    // -----------------------------------------------------------------
    // U54: prior-thread formatting
    // -----------------------------------------------------------------

    #[test]
    fn format_prior_thread_skips_when_under_three_turns() {
        // Two turns: those are the inbound + outbound shown elsewhere
        // in the prompt, so we'd duplicate. Should produce empty.
        let turns = vec![
            serde_json::json!({"role":"outbound","at":"t1","subject":"hi","body":"a"}),
            serde_json::json!({"role":"reply","at":"t2","subject":"re: hi","body":"b"}),
        ];
        assert!(format_prior_thread(&turns).is_empty());
    }

    #[test]
    fn format_prior_thread_excludes_last_two() {
        let turns = vec![
            serde_json::json!({"role":"outbound","at":"t1","subject":"intro","body":"a1\na2"}),
            serde_json::json!({"role":"reply","at":"t2","subject":"re: intro","body":"b1","reply_kind":"engaged"}),
            serde_json::json!({"role":"outbound","at":"t3","subject":"re: intro","body":"c1"}),
            serde_json::json!({"role":"reply","at":"t4","subject":"re: intro","body":"d1"}),
        ];
        let out = format_prior_thread(&turns);
        // First two turns appear; last two excluded.
        assert!(out.contains("[outbound @ t1]"), "got: {out}");
        assert!(out.contains("[reply (engaged) @ t2]"), "got: {out}");
        assert!(!out.contains("@ t3]"));
        assert!(!out.contains("@ t4]"));
        // Body lines should be indented.
        assert!(out.contains("    a1"));
    }

    #[test]
    fn format_prior_thread_truncates_long_bodies() {
        let many_lines: String = (0..50).map(|i| format!("line{i}\n")).collect();
        let turns = vec![
            serde_json::json!({"role":"outbound","at":"t0","subject":"x","body":many_lines}),
            serde_json::json!({"role":"reply","at":"t1","subject":"x","body":"hi"}),
            serde_json::json!({"role":"outbound","at":"t2","subject":"x","body":"hi"}),
        ];
        let out = format_prior_thread(&turns);
        assert!(out.contains("line0"));
        assert!(out.contains("line5"));
        // Capped at 6 lines per turn per the implementation.
        assert!(!out.contains("line6"));
    }
}
