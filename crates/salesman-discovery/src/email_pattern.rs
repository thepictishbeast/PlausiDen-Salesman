//! Email-pattern guesser. Given (first_name, last_name, domain),
//! return ranked candidate addresses by pattern likelihood.
//!
//! Pattern priors are based on public B2B email-pattern surveys:
//! `first.last@` is the most common in modern B2B; legacy formats
//! (`flast@`, `firstl@`) are common in older organisations; single-
//! token formats (`first@`, `last@`) are common at small companies.
//!
//! BUG ASSUMPTION: this only generates candidates. It does NOT verify
//! deliverability — that's a separate (paid) external service.
//!
//! BUG ASSUMPTION: input is sanitised — punctuation removed, lower-
//! cased. Unicode names (e.g. accented characters) are not folded;
//! caller normalises if needed.

use async_trait::async_trait;
use salesman_core::{Error, Result, ToolArgs};
use salesman_tools::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuessedEmail {
    pub email: String,
    pub pattern: String,
    /// Prior probability, 0..1. Multiply by deliverability evidence
    /// (when verifier is wired in) to get a posterior.
    pub prior: f32,
}

#[derive(Debug, Default)]
pub struct EmailPatternGuesser;

impl EmailPatternGuesser {
    pub fn new() -> Self {
        Self
    }

    /// Returns up to N candidates ordered by descending prior.
    pub fn guess(&self, first: &str, last: &str, domain: &str) -> Vec<GuessedEmail> {
        let f = sanitise(first);
        let l = sanitise(last);
        let d = domain.trim().to_ascii_lowercase();
        if f.is_empty() || d.is_empty() {
            return vec![];
        }
        let fi = f.chars().next().unwrap_or('x');
        let li = l.chars().next().unwrap_or('x');

        let mut candidates: Vec<(String, &str, f32)> = vec![
            (format!("{f}.{l}@{d}"), "first.last", 0.42),
            (format!("{f}{l}@{d}"), "firstlast", 0.18),
            (format!("{fi}{l}@{d}"), "flast", 0.12),
            (format!("{f}{li}@{d}"), "firstl", 0.05),
            (format!("{f}@{d}"), "first", 0.10),
            (format!("{l}@{d}"), "last", 0.04),
            (format!("{l}.{f}@{d}"), "last.first", 0.03),
            (format!("{f}_{l}@{d}"), "first_last", 0.02),
            (format!("{f}-{l}@{d}"), "first-last", 0.02),
            (format!("{fi}.{l}@{d}"), "f.last", 0.02),
        ];
        // Drop any duplicates (single-letter first/last collapses).
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|(e, _, _)| seen.insert(e.clone()));
        // Drop any with empty pre-@ part.
        candidates.retain(|(e, _, _)| e.split('@').next().is_some_and(|p| !p.is_empty()));
        candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        candidates
            .into_iter()
            .map(|(e, p, prior)| GuessedEmail {
                email: e,
                pattern: p.to_string(),
                prior,
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub struct EmailPatternTool {
    guesser: EmailPatternGuesser,
}

impl EmailPatternTool {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Tool for EmailPatternTool {
    fn name(&self) -> &str {
        "discovery.email_pattern"
    }
    fn description(&self) -> &str {
        "Given (first, last, domain), return ranked candidate email \
         addresses by pattern likelihood. Does NOT verify deliverability."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "first":  { "type": "string" },
                "last":   { "type": "string" },
                "domain": { "type": "string" }
            },
            "required": ["first", "last", "domain"]
        })
    }
    async fn invoke(&self, args: ToolArgs) -> Result<Value> {
        let first = args.0.get("first").and_then(|v| v.as_str()).unwrap_or("");
        let last = args.0.get("last").and_then(|v| v.as_str()).unwrap_or("");
        let domain = args.0.get("domain").and_then(|v| v.as_str()).unwrap_or("");
        if first.is_empty() || domain.is_empty() {
            return Err(Error::Validation(
                "email_pattern: `first` and `domain` are required".into(),
            ));
        }
        let candidates = self.guesser.guess(first, last, domain);
        Ok(json!({ "count": candidates.len(), "candidates": candidates }))
    }
}

fn sanitise(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_first_last_first() {
        let g = EmailPatternGuesser::new();
        let out = g.guess("Jane", "Doe", "example.com");
        assert!(!out.is_empty());
        assert_eq!(out[0].email, "jane.doe@example.com");
        assert_eq!(out[0].pattern, "first.last");
    }

    #[test]
    fn handles_punctuation_in_names() {
        let g = EmailPatternGuesser::new();
        let out = g.guess("Anne-Marie", "O'Brien", "example.com");
        assert_eq!(out[0].email, "annemarie.obrien@example.com");
    }

    #[test]
    fn empty_first_returns_empty() {
        let g = EmailPatternGuesser::new();
        assert!(g.guess("", "doe", "example.com").is_empty());
    }

    #[test]
    fn missing_last_still_produces_first_only() {
        let g = EmailPatternGuesser::new();
        let out = g.guess("Jane", "", "example.com");
        assert!(out.iter().any(|c| c.email == "jane@example.com"));
    }
}
