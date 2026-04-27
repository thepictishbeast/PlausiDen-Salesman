//! Competitor-mention detector — scan an inbound reply for
//! mentions of any competitor in the operator's catalog. Cheap
//! substring match (no LLM), case-insensitive, alias-aware.
//!
//! BUG ASSUMPTION: false positives on common-word competitor names
//! ("element", "node", etc. are bad picks for a `name` or `alias`).
//! The catalog is operator-curated; that's the right place to keep
//! the list precise.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompetitorEntry {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub positioning: Option<String>,
    #[serde(default)]
    pub where_we_win: Vec<String>,
    #[serde(default)]
    pub where_they_win: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompetitorCatalog {
    #[serde(default)]
    pub competitors: Vec<CompetitorEntry>,
}

impl CompetitorCatalog {
    /// Detect mentions in `body`. Returns the list of canonical
    /// competitor names found (deduplicated). Case-insensitive.
    /// Aliases are matched as substrings; the canonical name is
    /// what's reported.
    pub fn detect(&self, body: &str) -> Vec<String> {
        let lc = body.to_ascii_lowercase();
        let mut hits: Vec<String> = Vec::new();
        for c in &self.competitors {
            let mut found = false;
            // Match canonical name.
            if lc.contains(&c.name.to_ascii_lowercase()) {
                found = true;
            }
            // Match any alias.
            for a in &c.aliases {
                if lc.contains(&a.to_ascii_lowercase()) {
                    found = true;
                    break;
                }
            }
            if found && !hits.iter().any(|h| h == &c.name) {
                hits.push(c.name.clone());
            }
        }
        hits
    }
}

pub fn load_competitors_toml(text: &str) -> Result<CompetitorCatalog, String> {
    toml::from_str(text).map_err(|e| format!("competitor catalog parse: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> CompetitorCatalog {
        CompetitorCatalog {
            competitors: vec![
                CompetitorEntry {
                    name: "Outreach".into(),
                    aliases: vec!["outreach.io".into(), "outreach".into()],
                    positioning: None,
                    where_we_win: vec![],
                    where_they_win: vec![],
                },
                CompetitorEntry {
                    name: "Splunk".into(),
                    aliases: vec!["splunk".into()],
                    positioning: None,
                    where_we_win: vec![],
                    where_they_win: vec![],
                },
            ],
        }
    }

    #[test]
    fn detects_canonical_name() {
        let c = fixture();
        let hits = c.detect("We're evaluating Outreach next quarter.");
        assert_eq!(hits, vec!["Outreach"]);
    }

    #[test]
    fn detects_alias() {
        let c = fixture();
        let hits = c.detect("Currently using outreach.io but considering alternatives");
        assert_eq!(hits, vec!["Outreach"]);
    }

    #[test]
    fn case_insensitive() {
        let c = fixture();
        let hits = c.detect("we're on SPLUNK and it's painful");
        assert_eq!(hits, vec!["Splunk"]);
    }

    #[test]
    fn dedupes_canonical_when_multiple_aliases_hit() {
        let c = fixture();
        let hits = c.detect("Outreach and outreach.io are the same thing");
        assert_eq!(hits, vec!["Outreach"]);
    }

    #[test]
    fn returns_multiple_distinct_competitors() {
        let c = fixture();
        let hits = c.detect("comparing Outreach to Splunk... wait, those aren't comparable");
        assert!(hits.contains(&"Outreach".to_string()));
        assert!(hits.contains(&"Splunk".to_string()));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn no_false_positive_on_unrelated_text() {
        let c = fixture();
        let hits = c.detect("Just a thank-you note. Looking forward to next steps.");
        assert!(hits.is_empty());
    }

    #[test]
    fn parses_toml_round_trip() {
        let toml = r#"
            [[competitors]]
            name = "Outreach"
            aliases = ["outreach.io", "outreach"]
            positioning = "SaaS, holds your data"

            [[competitors]]
            name = "Lemlist"
            aliases = ["lemlist"]
        "#;
        let c = load_competitors_toml(toml).unwrap();
        assert_eq!(c.competitors.len(), 2);
        assert_eq!(c.competitors[0].name, "Outreach");
        assert_eq!(c.competitors[0].aliases.len(), 2);
    }
}
