//! Auto-angle picker — given a prospect's facts and a product
//! catalog, pick the best (product, angle) pair to pitch.
//!
//! Today the operator has to specify `--product X` and `--angle-hint
//! Y` per draft run. That's friction at exactly the place where the
//! system should take over: matching prospect signals to the right
//! offer is a pattern problem the LLM solves well.
//!
//! The output goes straight into DraftColdEmailTool: pass
//! `picked_product` as `product` and `picked_angle` as `angle_hint`.
//!
//! BUG ASSUMPTION: the catalog is small + curated (5-20 products
//! max). The picker reads the whole catalog into the prompt; very
//! large catalogs would need a retrieve-then-rank step that we
//! don't have yet.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_llm::{ChatRequest, LlmRouter, Message, Role, RouteHint};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// One row of the product catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductEntry {
    /// Product name.
    pub name: String,
    /// One-line pitch.
    pub one_liner: String,
    /// Free-form description of who this is for. The picker uses
    /// this to match against the prospect's industry / description.
    pub ideal_customer: String,
    /// Pre-canned angle phrases. The picker can pick one verbatim
    /// or generate a riff. Empty list = LLM picks any angle.
    #[serde(default)]
    pub key_angles: Vec<String>,
}

/// What the picker returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnglePick {
    /// Name of the product the picker chose.
    pub picked_product: String,
    /// The angle the picker chose (verbatim or a riff).
    pub picked_angle: String,
    /// Why the picker chose this product + angle.
    pub rationale: String,
    /// Model confidence, 0..=1.
    #[serde(default)]
    pub confidence: Option<f32>,
}

/// Picks the best product + outreach angle for a prospect via the LLM.
#[derive(Debug)]
pub struct AnglePickerTool {
    router: Arc<LlmRouter>,
    sender_company: String,
}

impl AnglePickerTool {
    /// Build the angle-picker tool over the LLM `router`, choosing outreach
    /// angles on behalf of `sender_company`.
    pub fn new(router: Arc<LlmRouter>, sender_company: impl Into<String>) -> Self {
        Self {
            router,
            sender_company: sender_company.into(),
        }
    }
}

#[async_trait]
impl Tool for AnglePickerTool {
    fn name(&self) -> &str {
        "content.angle_picker"
    }

    fn description(&self) -> &str {
        "Given a prospect's facts and a product catalog, pick the \
         best product to pitch + the best angle. Returns JSON \
         { picked_product, picked_angle, rationale, confidence }. \
         Eliminates the operator-must-pick step before draft."
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
                        "description":  { "type": ["string", "null"] },
                        "tech_signals": { "type": "array" }
                    },
                    "required": ["display_name"]
                },
                "catalog": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name":           { "type": "string" },
                            "one_liner":      { "type": "string" },
                            "ideal_customer": { "type": "string" },
                            "key_angles":     { "type": "array" }
                        },
                        "required": ["name", "one_liner", "ideal_customer"]
                    }
                }
            },
            "required": ["prospect", "catalog"]
        })
    }

    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let prospect = args.0.get("prospect").cloned().unwrap_or(Value::Null);
        let catalog_v = args
            .0
            .get("catalog")
            .cloned()
            .ok_or_else(|| Error::Validation("angle_picker: missing catalog".into()))?;
        let catalog: Vec<ProductEntry> = serde_json::from_value(catalog_v.clone())
            .map_err(|e| Error::Validation(format!("angle_picker: bad catalog: {e}")))?;
        if catalog.is_empty() {
            return Err(Error::Validation(
                "angle_picker: catalog is empty — nothing to pick from".into(),
            ));
        }

        let system = format!(
            "You are a senior B2B sales operator at {}. You match a \
             prospect's facts to the most-fit product + angle from a \
             curated catalog. Be specific. Pick the SINGLE best match.\n\
             \n\
             HARD CONSTRAINTS:\n\
             - You MUST pick a `picked_product` whose `name` is one of \
               the entries in the catalog (case-sensitive match).\n\
             - You MAY pick a `picked_angle` from the entry's `key_angles` \
               OR write a new short angle phrase grounded in the prospect's \
               facts. ≤80 chars.\n\
             - `rationale`: 1-2 sentences citing the prospect signal that \
               drove the pick. No marketing fluff.\n\
             - `confidence`: 0..1, how sure you are.\n\
             \n\
             Output STRICT JSON: {{\"picked_product\": string, \
             \"picked_angle\": string, \"rationale\": string, \
             \"confidence\": number}}.\n\
             No prose outside JSON. No code fences.",
            self.sender_company
        );

        let user = format!(
            "Prospect facts (JSON):\n{}\n\nProduct catalog (JSON):\n{}\n\nPick now.",
            serde_json::to_string_pretty(&prospect).unwrap_or_default(),
            serde_json::to_string_pretty(&catalog_v).unwrap_or_default(),
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
            // Tiny output — picker JSON is small.
            max_tokens: 256,
            temperature: 0.2,
        };

        let resp = self
            .router
            .chat_for(RouteHint::Reasoning, "angle_picker", req)
            .await?;
        let pick = parse_pick(&resp.message.content).map_err(|e| Error::Tool {
            tool: "content.angle_picker".into(),
            message: format!("parse: {e}"),
        })?;

        // Validate that the picked_product is in the catalog. If
        // not, fall back to the FIRST catalog entry — defensive,
        // because a bad pick downstream would mis-pitch the prospect.
        let valid = catalog.iter().any(|p| p.name == pick.picked_product);
        let final_product = if valid {
            pick.picked_product.clone()
        } else {
            tracing::warn!(
                picked = %pick.picked_product,
                "angle_picker picked product not in catalog; falling back to catalog[0]"
            );
            catalog[0].name.clone()
        };

        Ok(json!({
            "picked_product": final_product,
            "picked_angle": pick.picked_angle,
            "rationale": pick.rationale,
            "confidence": pick.confidence,
            "valid_pick": valid,
            "model_latency_ms": resp.usage.latency_ms,
            "model_tokens_in":  resp.usage.prompt_tokens,
            "model_tokens_out": resp.usage.output_tokens,
        }))
    }
}

fn parse_pick(raw: &str) -> std::result::Result<AnglePick, String> {
    let raw = raw.trim();
    if let Ok(p) = serde_json::from_str::<AnglePick>(raw) {
        return Ok(p);
    }
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(p) = serde_json::from_str::<AnglePick>(stripped) {
        return Ok(p);
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}'))
        && e > s
        && let Ok(p) = serde_json::from_str::<AnglePick>(&raw[s..=e])
    {
        return Ok(p);
    }
    Err("output was not valid JSON in any expected shape".into())
}

/// Load a product catalog from a TOML file. Format:
///
/// ```toml
/// [[products]]
/// name = "Sentinel"
/// one_liner = "Self-hosted log + threat aggregator"
/// ideal_customer = "5-50 person security teams"
/// key_angles = ["compliance audit", "log volume cost"]
/// ```
pub fn load_catalog_toml(text: &str) -> Result<Vec<ProductEntry>> {
    #[derive(Deserialize)]
    struct Wrapped {
        #[serde(default)]
        products: Vec<ProductEntry>,
    }
    let parsed: Wrapped =
        toml::from_str(text).map_err(|e| Error::Validation(format!("catalog parse: {e}")))?;
    if parsed.products.is_empty() {
        return Err(Error::Validation("catalog has no products".into()));
    }
    Ok(parsed.products)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let raw = r#"{"picked_product":"Sentinel","picked_angle":"compliance audit","rationale":"They're SOC2-prepping","confidence":0.8}"#;
        let p = parse_pick(raw).unwrap();
        assert_eq!(p.picked_product, "Sentinel");
        assert_eq!(p.picked_angle, "compliance audit");
        assert_eq!(p.confidence, Some(0.8));
    }

    #[test]
    fn parse_fenced_json() {
        let raw =
            "```json\n{\"picked_product\":\"X\",\"picked_angle\":\"y\",\"rationale\":\"z\"}\n```";
        let p = parse_pick(raw).unwrap();
        assert_eq!(p.picked_product, "X");
    }

    #[test]
    fn parse_substring_recovery() {
        let raw = "Sure: {\"picked_product\":\"X\",\"picked_angle\":\"y\",\"rationale\":\"z\"}\nGood luck!";
        let p = parse_pick(raw).unwrap();
        assert_eq!(p.picked_product, "X");
    }

    #[test]
    fn parse_failure() {
        assert!(parse_pick("nope").is_err());
    }

    #[test]
    fn load_catalog_basic() {
        let toml = r#"
            [[products]]
            name = "Sentinel"
            one_liner = "Logs"
            ideal_customer = "Security"
            key_angles = ["audit", "cost"]

            [[products]]
            name = "Tidy"
            one_liner = "Cleaner"
            ideal_customer = "Privacy"
        "#;
        let cat = load_catalog_toml(toml).unwrap();
        assert_eq!(cat.len(), 2);
        assert_eq!(cat[0].name, "Sentinel");
        assert_eq!(cat[0].key_angles.len(), 2);
        assert!(cat[1].key_angles.is_empty());
    }

    #[test]
    fn load_catalog_empty_fails() {
        let toml = "";
        assert!(load_catalog_toml(toml).is_err());
    }
}
