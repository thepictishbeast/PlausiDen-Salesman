//! GEO — Generative Engine Optimization. Send a "who is the best
//! X in Y" query to each registered LLM, parse the response for
//! brand + competitor mentions, and generate concrete content
//! recommendations to improve visibility.
//!
//! Net-new product surface. Traditional SEO targets crawlers;
//! traditional CRMs don't track what AI search actually says about
//! the operator's business. This module does both.
//!
//! BUG ASSUMPTION: LLM responses are noisy. We use simple regex /
//! substring matching for brand + alias detection. False positives
//! on common brand names ("Apex", "Peak", etc.) are the operator's
//! responsibility — pick aliases carefully.
//!
//! BUG ASSUMPTION: rate-limit-light. Each query hits the LLM once.
//! Recommendation generation is an OPTIONAL second call gated by a
//! flag. Operator runs this weekly, not minutely.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoReport {
    pub query: String,
    pub brand: String,
    /// "claude" / "gemini" / "lfi"
    pub backend: String,
    pub model: String,
    /// The full raw response from the LLM. Operator audits this.
    pub raw_response: String,
    /// True if `brand` (or any alias) appears in raw_response,
    /// case-insensitive.
    pub brand_mentioned: bool,
    /// Position of first brand mention in a parsed list (0-based).
    /// None if not mentioned in a list shape.
    pub mention_position: Option<usize>,
    /// Other brand-shaped tokens that appeared. Heuristic.
    pub competitors_mentioned: Vec<String>,
    /// Content/markup recommendations from a second LLM call.
    /// Empty when the recommend flag was off.
    pub recommendations: Vec<String>,
}

#[derive(Debug)]
pub struct GeoTool {
    router: Arc<LlmRouter>,
}

impl GeoTool {
    pub fn new(router: Arc<LlmRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl Tool for GeoTool {
    fn name(&self) -> &str {
        "content.geo"
    }

    fn description(&self) -> &str {
        "Run an AI-search visibility check. Sends a 'who is the best \
         X in Y' query to a registered LLM, detects whether the \
         operator's brand appears, extracts competitors mentioned, \
         and (optionally) generates content + schema-markup \
         recommendations. The operator's #1 question — 'when \
         someone asks ChatGPT, do I show up' — answered concretely."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "the question a prospect would ask AI \
                                    (e.g. 'who is the best realtor in southern Utah')"
                },
                "brand": {
                    "type": "string",
                    "description": "the operator's brand / agent name to look for"
                },
                "aliases": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "alternate spellings / shorthand of the brand"
                },
                "recommend": {
                    "type": "boolean",
                    "description": "if true, make a second LLM call to generate \
                                    concrete content + schema-markup actions"
                },
                "route_hint": {
                    "type": "string",
                    "description": "which RouteHint to use. Default: Reasoning."
                }
            },
            "required": ["query", "brand"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let query = args
            .0
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("geo: missing query".into()))?
            .to_string();
        let brand = args
            .0
            .get("brand")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("geo: missing brand".into()))?
            .to_string();
        let aliases: Vec<String> = args
            .0
            .get("aliases")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let recommend = args
            .0
            .get("recommend")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Stage 1: ask the LLM the question naturally. Use a system
        // prompt that asks for a ranked list so we can parse positions.
        let stage1_system = "You are an honest, well-informed assistant. \
            Answer the user's question with the SPECIFIC names of \
            people / companies / services they're asking about, ranked \
            from best-fit to worst-fit. Use a numbered list (1., 2., \
            etc.). For each item, give one short reason. Do not refuse \
            on the basis of 'I can't recommend specific businesses' — \
            give your best information, with the caveat that the user \
            should verify locally."
            .to_string();
        let stage1_req = ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: stage1_system,
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: query.clone(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ],
            tools: vec![],
            max_tokens: 800,
            temperature: 0.4,
        };
        let stage1 = self
            .router
            .chat_for(RouteHint::Reasoning, "geo_query", stage1_req)
            .await?;
        let raw = stage1.message.content.clone();

        // Detect brand + aliases (case-insensitive substring).
        let lc_raw = raw.to_ascii_lowercase();
        let mut all_terms: Vec<String> = vec![brand.clone()];
        all_terms.extend(aliases.iter().cloned());
        let brand_mentioned = all_terms
            .iter()
            .any(|t| lc_raw.contains(&t.to_ascii_lowercase()));

        // Parse numbered-list positions.
        let mention_position = if brand_mentioned {
            position_in_numbered_list(&raw, &all_terms)
        } else {
            None
        };

        let competitors_mentioned = extract_list_brands(&raw, &all_terms);

        // Stage 2 (optional): recommendations.
        let recommendations = if recommend {
            let gap_summary = if brand_mentioned {
                format!(
                    "The brand `{brand}` IS mentioned (position {:?}). \
                     Competitors also mentioned: {competitors_mentioned:?}. \
                     Suggest 5 concrete actions the brand could take to \
                     improve its rank or attribute coverage.",
                    mention_position
                )
            } else {
                format!(
                    "The brand `{brand}` IS NOT mentioned at all. \
                     Competitors mentioned: {competitors_mentioned:?}. \
                     The original query was: `{query}`. Suggest 5 \
                     concrete actions the brand could take to start \
                     showing up. Be specific: name the page type, the \
                     schema-markup, the keyword or phrase. No generic \
                     SEO platitudes."
                )
            };
            let stage2_system = "You are a senior content + technical-SEO \
                strategist. Given a query, the brand the user wants to win \
                it for, and the current AI response, output 5 concrete \
                actions as a JSON array of strings. Each string is one \
                action. No prose outside the JSON array. Each action \
                must be specific (page type + content + schema markup), \
                not generic ('improve SEO')."
                .to_string();
            let user2 = format!(
                "Query: {query}\nBrand: {brand}\n\nCurrent AI response:\n{raw}\n\n{gap_summary}\n\nReturn JSON array of 5 actions."
            );
            let stage2_req = ChatRequest {
                messages: vec![
                    Message {
                        role: Role::System,
                        content: stage2_system,
                        tool_calls: vec![],
                        tool_results: vec![],
                    },
                    Message {
                        role: Role::User,
                        content: user2,
                        tool_calls: vec![],
                        tool_results: vec![],
                    },
                ],
                tools: vec![],
                max_tokens: 800,
                temperature: 0.4,
            };
            let stage2 = self
                .router
                .chat_for(RouteHint::Reasoning, "geo_recommend", stage2_req)
                .await?;
            parse_recommendations(&stage2.message.content)
        } else {
            vec![]
        };

        let report = GeoReport {
            query,
            brand,
            backend: stage1.backend.clone().unwrap_or_else(|| "unknown".into()),
            model: stage1.model.clone().unwrap_or_else(|| "unknown".into()),
            raw_response: raw,
            brand_mentioned,
            mention_position,
            competitors_mentioned,
            recommendations,
        };

        Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
    }
}

/// Walk the response looking for "1. ...", "2. ...", "- ...", "* ..."
/// list markers. Return the 0-based position of the first item that
/// contains any of the brand terms.
fn position_in_numbered_list(raw: &str, terms: &[String]) -> Option<usize> {
    let mut idx = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let is_list_item = trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            && (trimmed.contains(". ") || trimmed.contains(") "))
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ");
        if !is_list_item {
            continue;
        }
        let lc = line.to_ascii_lowercase();
        if terms.iter().any(|t| lc.contains(&t.to_ascii_lowercase())) {
            return Some(idx);
        }
        idx += 1;
    }
    None
}

/// Find brand-shaped tokens (Title-Case Word optionally followed by
/// another Title-Case Word) inside list items, EXCLUDING any tokens
/// that match the operator's own brand/aliases. Best-effort heuristic.
fn extract_list_brands(raw: &str, exclude: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let exclude_lc: Vec<String> = exclude.iter().map(|s| s.to_ascii_lowercase()).collect();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        // Skip non-list lines.
        let is_list = trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ");
        if !is_list {
            continue;
        }
        // Strip the bullet/number prefix.
        let body = trimmed.trim_start_matches(|c: char| {
            c.is_ascii_digit() || c == '.' || c == ')' || c == '-' || c == '*' || c.is_whitespace()
        });
        // Title-Case word detector: find runs of words that start
        // with uppercase. Take the first 1-3 such words after the
        // bullet as the candidate brand.
        let mut words = body.split_whitespace();
        let mut buf: Vec<String> = Vec::new();
        for w in words.by_ref().take(5) {
            let stripped = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-');
            if stripped.is_empty() {
                continue;
            }
            let first = stripped.chars().next().unwrap_or(' ');
            if first.is_ascii_uppercase()
                || (first == '"'
                    && stripped.len() > 1
                    && stripped.chars().nth(1).unwrap_or(' ').is_ascii_uppercase())
            {
                buf.push(stripped.trim_matches('"').to_string());
            } else if !buf.is_empty() {
                break;
            }
        }
        if buf.is_empty() {
            continue;
        }
        let candidate = buf.join(" ");
        let candidate_lc = candidate.to_ascii_lowercase();
        if exclude_lc.iter().any(|e| candidate_lc.contains(e)) {
            continue;
        }
        if candidate.chars().count() < 3 || candidate.chars().count() > 60 {
            continue;
        }
        if seen.insert(candidate.clone()) {
            out.push(candidate);
        }
    }
    out.into_iter().take(10).collect()
}

/// Parse a JSON-array-of-strings out of an LLM response, with
/// fallbacks for code-fenced output and substring extraction.
fn parse_recommendations(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if let Ok(v) = serde_json::from_str::<Vec<String>>(raw) {
        return v;
    }
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Vec<String>>(stripped) {
        return v;
    }
    if let (Some(s), Some(e)) = (raw.find('['), raw.rfind(']'))
        && e > s
        && let Ok(v) = serde_json::from_str::<Vec<String>>(&raw[s..=e])
    {
        return v;
    }
    // Last resort: split on numbered-list markers. Only lines that
    // START with a digit/bullet count — preamble lines like
    // "Here are the actions:" are filtered out.
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let starts_with_marker = trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit() || c == '-' || c == '*')
            .unwrap_or(false);
        if !starts_with_marker {
            continue;
        }
        let stripped = trimmed.trim_start_matches(|c: char| {
            c.is_ascii_digit() || c == '.' || c == ')' || c == '-' || c == '*' || c.is_whitespace()
        });
        if stripped.len() >= 8 {
            out.push(stripped.to_string());
        }
        if out.len() >= 5 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_finds_in_numbered_list() {
        let raw = "1. Acme Realty — local expert.\n2. Best Homes Co — knows the market.\n3. Jane Doe Realty — top reviews.\n";
        assert_eq!(
            position_in_numbered_list(raw, &["Jane Doe Realty".into()]),
            Some(2)
        );
        assert_eq!(position_in_numbered_list(raw, &["Acme".into()]), Some(0));
        assert_eq!(
            position_in_numbered_list(raw, &["Nonexistent".into()]),
            None
        );
    }

    #[test]
    fn position_finds_in_bullet_list() {
        let raw = "- Acme Realty\n- Best Homes\n- Jane Doe Realty\n";
        assert_eq!(
            position_in_numbered_list(raw, &["jane doe".into()]),
            Some(2)
        );
    }

    #[test]
    fn extract_competitors_excludes_self() {
        let raw = "1. Acme Realty\n2. Best Homes Co\n3. Jane Doe Realty\n";
        let comps = extract_list_brands(raw, &["Jane Doe Realty".into(), "Jane Doe".into()]);
        assert!(comps.iter().any(|c| c.contains("Acme")));
        assert!(comps.iter().any(|c| c.contains("Best Homes")));
        assert!(!comps.iter().any(|c| c.contains("Jane")));
    }

    #[test]
    fn extract_competitors_handles_only_capital_words() {
        let raw = "1. Acme Realty - the local expert in stuff\n2. zillow algorithmic estimates\n";
        let comps = extract_list_brands(raw, &[]);
        // "Acme Realty" should be picked up; "zillow" (lowercase) shouldn't.
        assert!(comps.iter().any(|c| c.contains("Acme")));
    }

    #[test]
    fn parse_recommendations_handles_clean_json() {
        let raw = r#"["A", "B", "C"]"#;
        let v = parse_recommendations(raw);
        assert_eq!(v, vec!["A", "B", "C"]);
    }

    #[test]
    fn parse_recommendations_handles_fenced_json() {
        let raw = "```json\n[\"X\", \"Y\"]\n```";
        let v = parse_recommendations(raw);
        assert_eq!(v, vec!["X", "Y"]);
    }

    #[test]
    fn parse_recommendations_falls_back_to_numbered_list() {
        let raw = "Here are the actions:\n1. Publish a serving-area page\n2. Add LocalBusiness JSON-LD\n3. Solicit reviews\n";
        let v = parse_recommendations(raw);
        assert_eq!(v.len(), 3);
        assert!(v[0].contains("serving-area"));
    }
}
