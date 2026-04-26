//! Routing rules over the registered backends.
//!
//! Callers pass a `RouteHint` describing the workload shape; the
//! router picks a backend. Defaults match the strategy in PLAN.md
//! ("Claude for reasoning, Gemini for bulk/grounding").

use crate::{BackendKind, ChatRequest, ChatResponse, LlmBackend};
use salesman_core::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

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
        }
    }

    pub fn register(&mut self, backend: Arc<dyn LlmBackend>) {
        self.backends.insert(backend.kind(), backend);
    }

    pub async fn chat(&self, hint: RouteHint, req: ChatRequest) -> Result<ChatResponse> {
        let kind = match hint {
            RouteHint::Reasoning => self.default_reasoning,
            RouteHint::DeepReasoning => self.default_deep_reasoning,
            RouteHint::Bulk => self.default_bulk,
            RouteHint::Grounded => self.default_grounded,
            RouteHint::Sovereign => self.default_sovereign,
            RouteHint::Backend(k) => k,
        };
        let backend = self.backends.get(&kind).ok_or_else(|| Error::Config(
            format!("LLM backend `{kind}` not registered"),
        ))?;
        backend.chat(req).await
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
