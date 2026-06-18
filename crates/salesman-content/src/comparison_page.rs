//! LLM-driven comparison-page generator.
//!
//! Output is markdown ready to render as a static page. Constraints
//! enforced via system prompt:
//! - Factual + verifiable claims only. No FUD against competitor.
//! - Concrete dimensions (price/architecture/data-residency/etc.)
//! - "Where Competitor wins" section is mandatory (credibility).
//! - Source links section at the bottom.
//!
//! BUG ASSUMPTION: pages ALWAYS land in the `pages` table (or
//! `awaiting_approval` queue, equivalent) — operator publishes
//! manually until phase 2.5+ adds an auto-publish path.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

/// Generates an honest "us vs. competitor" comparison page via the LLM.
#[derive(Debug)]
pub struct ComparisonPageTool {
    router: Arc<LlmRouter>,
    sender_company: String,
}

impl ComparisonPageTool {
    /// Build the comparison-page generator over the LLM `router`, writing
    /// on behalf of `sender_company`.
    pub fn new(router: Arc<LlmRouter>, sender_company: impl Into<String>) -> Self {
        Self {
            router,
            sender_company: sender_company.into(),
        }
    }
}

#[async_trait]
impl Tool for ComparisonPageTool {
    fn name(&self) -> &str {
        "content.comparison_page"
    }

    fn description(&self) -> &str {
        "Draft a comparison page (markdown) between a PlausiDen product \
         and a competitor product. Includes feature matrix, where each \
         wins, target segment fit, and source-link section. Owner \
         approves before publication."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "our_product":            { "type": "string" },
                "our_one_liner":          { "type": "string" },
                "competitor_product":     { "type": "string" },
                "competitor_one_liner":   { "type": "string" },
                "dimensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Comparison axes (e.g. 'pricing model', 'data residency', 'self-host vs SaaS')."
                },
                "target_segment": { "type": "string" }
            },
            "required": ["our_product", "competitor_product"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let our_product = require_str(&args, "our_product")?;
        let competitor_product = require_str(&args, "competitor_product")?;
        let our_one_liner = optional_str(&args, "our_one_liner").unwrap_or_default();
        let competitor_one_liner = optional_str(&args, "competitor_one_liner").unwrap_or_default();
        let target_segment =
            optional_str(&args, "target_segment").unwrap_or("SMB security teams".into());
        let dimensions: Vec<String> = args
            .0
            .get("dimensions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "deployment model".into(),
                    "pricing model".into(),
                    "data residency".into(),
                    "integration surface".into(),
                    "first-time setup time".into(),
                ]
            });

        let system = [
            format!("You are a senior B2B writer for {}.", self.sender_company).as_str(),
            "Write a HONEST comparison page between our product and the competitor's.",
            "Constraints (HARD — failing any of these makes the page UNUSABLE):",
            "- Factual + verifiable claims ONLY. No FUD. No 'their breach', no 'inferior',",
            "  no 'they're going out of business'.",
            "- Cite a source (URL or doc) for every numeric / specific claim.",
            "- Include a 'Where {competitor} wins' section (mandatory — credibility).",
            "- Include a 'Best fit' guide that explicitly says when the competitor is the",
            "  better choice for some prospects.",
            "- Acknowledge uncertainty where it exists ('as of YYYY-MM').",
            "",
            "Output FORMAT — strict markdown:",
            "  # {Our Product} vs {Competitor Product} for {target segment}",
            "  ## Summary (3-4 sentences, neutral)",
            "  ## Feature matrix (markdown table over the supplied dimensions)",
            "  ## Where we win (3 bullets, concrete)",
            "  ## Where {competitor} wins (3 bullets, concrete)",
            "  ## Best fit",
            "  ## Sources",
            "",
            "No JSON wrapper. Pure markdown.",
        ]
        .join("\n");

        let user = format!(
            "Our product: {our_product}\nOur one-liner: {our_one_liner}\n\
             Competitor product: {competitor_product}\nCompetitor one-liner: {competitor_one_liner}\n\
             Target segment: {target_segment}\n\
             Dimensions to cover (in this order): {}\n\nWrite the page.",
            dimensions.join(", ")
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
            max_tokens: 4096,
            temperature: 0.4,
        };

        let resp = self
            .router
            .chat_for(RouteHint::DeepReasoning, "comparison_page", req)
            .await?;
        let markdown = resp.message.content.trim().to_string();

        Ok(json!({
            "markdown": markdown,
            "our_product": our_product,
            "competitor_product": competitor_product,
            "model_latency_ms": resp.usage.latency_ms,
            "model_tokens_in":  resp.usage.prompt_tokens,
            "model_tokens_out": resp.usage.output_tokens,
        }))
    }
}

fn require_str(args: &ToolArgs, key: &str) -> Result<String> {
    args.0
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::Validation(format!("missing `{key}`")))
}

fn optional_str(args: &ToolArgs, key: &str) -> Option<String> {
    args.0.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use salesman_llm::LlmRouter;
    use std::sync::Arc;

    #[test]
    fn schema_round_trips_through_serde_json() {
        let t = ComparisonPageTool::new(Arc::new(LlmRouter::new()), "PlausiDen");
        let s = t.input_schema();
        let _ = serde_json::to_string(&s).unwrap();
        assert!(s["properties"]["our_product"].is_object());
    }
}
