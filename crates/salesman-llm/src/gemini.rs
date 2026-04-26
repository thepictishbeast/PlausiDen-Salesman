//! Gemini backend (Google AI Studio / Vertex AI).
//!
//! BUG ASSUMPTION: targets `gemini-1.5-pro` (grounded) and
//! `gemini-1.5-flash` (cheap bulk). Updated as Google ships.

use crate::{BackendKind, ChatRequest, ChatResponse, LlmBackend};
use async_trait::async_trait;
use salesman_core::{Error, Result};
use std::time::Duration;
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
                .timeout(Duration::from_secs(120))
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
        let _ = (&req, &self.api_key, &self.http, GEMINI_API_BASE);
        Err(Error::Llm {
            backend: "gemini".into(),
            message: "wire format not implemented yet (Phase 1.0 scaffold)".into(),
        })
    }
}
