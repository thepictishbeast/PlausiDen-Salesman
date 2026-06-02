//! Routing rules over the registered backends.
//!
//! Callers pass a `RouteHint` describing the workload shape; the
//! router picks a backend. Defaults match the strategy in PLAN.md
//! ("Claude for reasoning, Gemini for bulk/grounding").

use crate::rates::compute_cost_micro_usd;
use crate::{BackendKind, ChatRequest, ChatResponse, LlmBackend, Message, Role};
use async_trait::async_trait;
use salesman_core::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Sink for one llm_calls row. Decoupled so salesman-llm doesn't
/// depend on salesman-state. State implements this in its own crate.
#[async_trait]
pub trait LlmCallSink: Send + Sync + std::fmt::Debug {
    #[allow(clippy::too_many_arguments)]
    async fn record_call(
        &self,
        backend: BackendKind,
        model: String,
        prompt_tokens: u32,
        output_tokens: u32,
        cache_hit_tokens: u32,
        latency_ms: u64,
        cost_micro_usd: u64,
        purpose: String,
    );
}

/// Tells the router what kind of work this is. The router maps it to
/// a registered backend. Hints are advisory — caller can also pin a
/// specific backend with `RouteHint::Backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteHint {
    /// Heavy reasoning, planning, draft generation. Default: Claude Sonnet.
    Reasoning,
    /// Hard reasoning that benefits from a larger model. Default: Claude Opus.
    DeepReasoning,
    /// Bulk classification, batch enrichment. Default: Gemini Flash.
    Bulk,
    /// Need grounded / web-aware answer. Default: Gemini Pro grounded.
    Grounded,
    /// Sovereignty mode — never leave the cluster. Default: LFI.
    Sovereign,
    /// Pin a specific backend.
    Backend(BackendKind),
}

#[derive(Debug)]
pub struct LlmRouter {
    backends: HashMap<BackendKind, Arc<dyn LlmBackend>>,
    default_reasoning: BackendKind,
    default_deep_reasoning: BackendKind,
    default_bulk: BackendKind,
    default_grounded: BackendKind,
    default_sovereign: BackendKind,
    sink: Option<Arc<dyn LlmCallSink>>,
    /// Operator-supplied prefix prepended to every system message
    /// before dispatch. See docs/MODEL_RESILIENCE.md §5 — keeps
    /// fallback models tone-aligned with the brand.
    operator_brief: Option<String>,
}

impl LlmRouter {
    /// Build an empty router with the default per-route backend choices
    /// (Claude for reasoning/deep-reasoning, Gemini for bulk/grounded,
    /// LFI for sovereign). Register backends before routing.
    pub fn new() -> Self {
        Self {
            backends: HashMap::new(),
            default_reasoning: BackendKind::Claude,
            default_deep_reasoning: BackendKind::Claude,
            default_bulk: BackendKind::Gemini,
            default_grounded: BackendKind::Gemini,
            default_sovereign: BackendKind::Lfi,
            sink: None,
            operator_brief: None,
        }
    }

    /// Register a backend under its `kind()`, replacing any existing one.
    pub fn register(&mut self, backend: Arc<dyn LlmBackend>) {
        self.backends.insert(backend.kind(), backend);
    }

    /// Attach a cost-recording sink. Optional — if absent, calls
    /// don't get logged to the ledger (useful for dry-run / tests).
    pub fn with_sink(mut self, sink: Arc<dyn LlmCallSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Prepend `brief` to every system message at chat-dispatch time.
    /// Owner-curated 200-300 word context — company name, sender
    /// identity, banned phrases, tone guide. Keeps swapped-in models
    /// tone-aligned. Idempotent: passing twice replaces.
    pub fn with_operator_brief(mut self, brief: impl Into<String>) -> Self {
        let s = brief.into();
        self.operator_brief = if s.trim().is_empty() { None } else { Some(s) };
        self
    }

    /// Construct a router and load the operator brief from the path
    /// in `SALESMAN_OPERATOR_BRIEF`. Missing file → no brief, no
    /// error (the env var is optional). Read failure → log and
    /// proceed without — the brief is a quality nudge, not a
    /// correctness requirement.
    pub fn with_operator_brief_from_env(mut self) -> Self {
        match std::env::var("SALESMAN_OPERATOR_BRIEF") {
            Ok(path) if !path.trim().is_empty() => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        self.operator_brief = Some(trimmed);
                        tracing::info!(path = %path, "operator brief loaded");
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %path, "%e" = %e, "operator brief read failed; proceeding without");
                }
            },
            _ => {}
        }
        self
    }

    /// The operator brand/voice brief injected into prompts, if configured.
    pub fn operator_brief(&self) -> Option<&str> {
        self.operator_brief.as_deref()
    }

    /// Route + record-cost wrapper. `purpose` is recorded for audit
    /// (e.g. "draft", "qualify", "classify").
    pub async fn chat_for(
        &self,
        hint: RouteHint,
        purpose: impl Into<String>,
        req: ChatRequest,
    ) -> Result<ChatResponse> {
        let kind = match hint {
            RouteHint::Reasoning => self.default_reasoning,
            RouteHint::DeepReasoning => self.default_deep_reasoning,
            RouteHint::Bulk => self.default_bulk,
            RouteHint::Grounded => self.default_grounded,
            RouteHint::Sovereign => self.default_sovereign,
            RouteHint::Backend(k) => k,
        };
        let backend = self
            .backends
            .get(&kind)
            .ok_or_else(|| Error::Config(format!("LLM backend `{kind}` not registered")))?;
        let model = backend.model().to_string();
        // Inject the operator brief into the FIRST system message of
        // the request, if present. We mutate a clone so the caller's
        // request isn't side-channel modified.
        let mut req = req;
        if let Some(brief) = &self.operator_brief {
            apply_operator_brief(&mut req, brief);
        }
        let resp = backend.chat(req).await?;

        // Record cost (best-effort; sink failure does not fail the call).
        if let Some(sink) = &self.sink {
            let cost = compute_cost_micro_usd(
                kind,
                &model,
                resp.usage.prompt_tokens,
                resp.usage.output_tokens,
                resp.usage.cache_hit_tokens,
            );
            sink.record_call(
                kind,
                model,
                resp.usage.prompt_tokens,
                resp.usage.output_tokens,
                resp.usage.cache_hit_tokens,
                resp.usage.latency_ms,
                cost,
                purpose.into(),
            )
            .await;
        }
        Ok(resp)
    }

    /// Backwards-compatible — calls chat_for with purpose="unspecified".
    pub async fn chat(&self, hint: RouteHint, req: ChatRequest) -> Result<ChatResponse> {
        self.chat_for(hint, "unspecified", req).await
    }

    /// The backend kinds currently registered (unordered).
    pub fn registered_kinds(&self) -> Vec<BackendKind> {
        self.backends.keys().copied().collect()
    }
}

impl Default for LlmRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Inject the operator brief into the first System message of the
/// request, or insert one at index 0 if there isn't one. Subsequent
/// system messages (rare — e.g. multi-stage instructions) are left
/// alone; the brief is a project-level preamble, not a per-call
/// override.
fn apply_operator_brief(req: &mut ChatRequest, brief: &str) {
    let preamble = format!(
        "## Operator brief (do not echo verbatim; absorb the tone and constraints)\n{brief}\n\n## Task instructions follow.\n"
    );
    if let Some(first_system) = req
        .messages
        .iter_mut()
        .find(|m| matches!(m.role, Role::System))
    {
        first_system.content = format!("{preamble}{}", first_system.content);
    } else {
        req.messages.insert(
            0,
            Message {
                role: Role::System,
                content: preamble,
                tool_calls: vec![],
                tool_results: vec![],
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(msgs: Vec<Message>) -> ChatRequest {
        ChatRequest {
            messages: msgs,
            tools: vec![],
            max_tokens: 100,
            temperature: 0.0,
        }
    }

    fn sys(content: &str) -> Message {
        Message {
            role: Role::System,
            content: content.into(),
            tool_calls: vec![],
            tool_results: vec![],
        }
    }

    fn user(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.into(),
            tool_calls: vec![],
            tool_results: vec![],
        }
    }

    #[test]
    fn brief_prepended_to_existing_system() {
        let mut r = req(vec![sys("Be concise."), user("Say hi.")]);
        apply_operator_brief(&mut r, "PlausiDen sells security tools.");
        assert!(matches!(r.messages[0].role, Role::System));
        assert!(r.messages[0].content.contains("Operator brief"));
        assert!(
            r.messages[0]
                .content
                .contains("PlausiDen sells security tools.")
        );
        assert!(r.messages[0].content.contains("Be concise.")); // original kept
        assert_eq!(r.messages.len(), 2); // didn't insert a new one
    }

    #[test]
    fn brief_inserted_when_no_system_present() {
        let mut r = req(vec![user("Say hi.")]);
        apply_operator_brief(&mut r, "PlausiDen sells X.");
        assert_eq!(r.messages.len(), 2);
        assert!(matches!(r.messages[0].role, Role::System));
        assert!(r.messages[0].content.contains("Operator brief"));
        assert!(matches!(r.messages[1].role, Role::User));
    }

    #[test]
    fn brief_only_touches_first_system() {
        let mut r = req(vec![
            sys("First system."),
            user("Hi."),
            sys("Second system."),
        ]);
        apply_operator_brief(&mut r, "Brief.");
        assert!(r.messages[0].content.contains("Brief."));
        assert!(!r.messages[2].content.contains("Brief.")); // untouched
    }
}
