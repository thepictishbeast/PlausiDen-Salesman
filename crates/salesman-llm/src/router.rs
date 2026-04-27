//! Routing rules over the registered backends.
//!
//! Callers pass a `RouteHint` describing the workload shape; the
//! router picks a backend. Defaults match the strategy in PLAN.md
//! ("Claude for reasoning, Gemini for bulk/grounding").

use crate::rates::compute_cost_micro_usd;
use crate::{BackendKind, ChatRequest, ChatResponse, LlmBackend};
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
}

impl LlmRouter {
    pub fn new() -> Self {
        Self {
            backends: HashMap::new(),
            default_reasoning: BackendKind::Claude,
            default_deep_reasoning: BackendKind::Claude,
            default_bulk: BackendKind::Gemini,
            default_grounded: BackendKind::Gemini,
            default_sovereign: BackendKind::Lfi,
            sink: None,
        }
    }

    pub fn register(&mut self, backend: Arc<dyn LlmBackend>) {
        self.backends.insert(backend.kind(), backend);
    }

    /// Attach a cost-recording sink. Optional — if absent, calls
    /// don't get logged to the ledger (useful for dry-run / tests).
    pub fn with_sink(mut self, sink: Arc<dyn LlmCallSink>) -> Self {
        self.sink = Some(sink);
        self
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

    pub fn registered_kinds(&self) -> Vec<BackendKind> {
        self.backends.keys().copied().collect()
    }
}

impl Default for LlmRouter {
    fn default() -> Self {
        Self::new()
    }
}
