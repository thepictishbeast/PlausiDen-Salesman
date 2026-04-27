//! LLM-driven case study draft generator.
//!
//! BUG ASSUMPTION: input MUST be customer-supplied facts (problem,
//! solution, outcome). The LLM does not invent customers or numbers.
//! If a required fact is missing, the tool returns an error rather
//! than hallucinating.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
pub struct CaseStudyDraftTool {
    router: Arc<LlmRouter>,
    sender_company: String,
}

impl CaseStudyDraftTool {
    pub fn new(router: Arc<LlmRouter>, sender_company: impl Into<String>) -> Self {
        Self {
            router,
            sender_company: sender_company.into(),
        }
    }
}

#[async_trait]
impl Tool for CaseStudyDraftTool {
    fn name(&self) -> &str {
        "content.case_study"
    }

    fn description(&self) -> &str {
        "Draft a markdown case study from operator-supplied facts. \
         Refuses to invent details. Output: title, problem, approach, \
         outcome, quote (only if supplied), call-to-action."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "customer_name":      { "type": "string" },
                "customer_industry":  { "type": "string" },
                "customer_size":      { "type": ["string", "null"] },
                "problem":            { "type": "string" },
                "approach":           { "type": "string" },
                "outcome":            { "type": "string" },
                "outcome_metrics":    { "type": ["string", "null"] },
                "customer_quote":     { "type": ["string", "null"] },
                "quote_attribution":  { "type": ["string", "null"] },
                "products_used":      {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["customer_name", "problem", "approach", "outcome", "products_used"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let customer_name = require_str(&args, "customer_name")?;
        let problem = require_str(&args, "problem")?;
        let approach = require_str(&args, "approach")?;
        let outcome = require_str(&args, "outcome")?;
        let products_used: Vec<String> = args
            .0
            .get("products_used")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if products_used.is_empty() {
            return Err(Error::Validation(
                "case_study: at least one product must be in `products_used`".into(),
            ));
        }

        let optional = optional_args(&args);

        let system = [
            format!("You are a senior B2B writer for {}.", self.sender_company).as_str(),
            "Draft a case study from operator-supplied facts.",
            "HARD CONSTRAINTS (failing any makes the draft UNUSABLE):",
            "- Use ONLY the facts provided. Do NOT invent customer details, numbers,",
            "  metrics, or quotes that were not supplied.",
            "- If `customer_quote` is empty, OMIT the quote section entirely.",
            "- If `outcome_metrics` is empty, write the outcome WITHOUT specific numbers.",
            "- Avoid superlative marketing language. No 'industry-leading', 'best-in-class',",
            "  'unparalleled', 'revolutionary'.",
            "- Pure markdown. No JSON wrapper. No code fences.",
            "",
            "STRUCTURE:",
            "  # {Customer} {short framing}",
            "  ## The challenge",
            "  ## Our approach",
            "  ## Results",
            "  ## Quote          # only if customer_quote is non-empty",
            "  ## Want to talk?  # one-line CTA at the bottom",
        ]
        .join("\n");

        let mut user = format!(
            "Customer: {customer_name}\nProblem: {problem}\nApproach: {approach}\nOutcome: {outcome}\nProducts used: {}",
            products_used.join(", ")
        );
        for (k, v) in optional {
            user.push_str(&format!("\n{k}: {v}"));
        }
        user.push_str("\n\nWrite the case study now.");

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
            max_tokens: 2048,
            temperature: 0.4,
        };

        let resp = self.router.chat(RouteHint::Reasoning, req).await?;
        Ok(json!({
            "markdown": resp.message.content.trim(),
            "customer_name": customer_name,
            "products_used": products_used,
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

fn optional_args(args: &ToolArgs) -> Vec<(&'static str, String)> {
    let pairs: &[&str] = &[
        "customer_industry",
        "customer_size",
        "outcome_metrics",
        "customer_quote",
        "quote_attribution",
    ];
    pairs
        .iter()
        .filter_map(|k| {
            args.0
                .get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| (*k, s.to_string()))
        })
        .collect()
}
