//! Claude backend (Anthropic Messages API).
//!
//! BUG ASSUMPTION: only `claude-opus-4-7` and `claude-sonnet-4-6`
//! are first-class targets right now. Older models work but we don't
//! exercise them.
//!
//! SECURITY: API key is held in `Zeroizing<String>`.

use crate::{BackendKind, ChatRequest, ChatResponse, LlmBackend, Message};
use async_trait::async_trait;
use salesman_core::{Error, Result};
use std::time::Duration;
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
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client construction is infallible with these settings"),
        }
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
        // STUB: real wire-format mapping lives here. For Phase 1.0 we
        // only need the trait to exist + compile so the orchestrator
        // can be wired up. The stub returns a "not implemented" error
        // so any caller that actually invokes this in tests fails
        // loud rather than silently succeeding.
        let _ = (&req, &self.api_key, &self.http, ANTHROPIC_API_URL, ANTHROPIC_VERSION);
        Err(Error::Llm {
            backend: "claude".into(),
            message: "wire format not implemented yet (Phase 1.0 scaffold)".into(),
        })
    }
}

// Convenience constructor used by the router default config.
impl ClaudeBackend {
    pub fn from_env(model: &str) -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| Error::Config("ANTHROPIC_API_KEY not set".into()))?;
        Ok(Self::new(model, key))
    }
}

#[allow(dead_code)]
fn _round_trip_msg_compiles(_m: Message) {}
