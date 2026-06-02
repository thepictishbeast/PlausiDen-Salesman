use chrono::Utc;
use salesman_core::model::{Company, DiscoverySource, SizeBand};
use salesman_core::{CompanyId, Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use url::Url;

/// One row from an operator-supplied CSV.
#[derive(Debug, Deserialize)]
struct Row {
    display_name: String,
    homepage: Option<String>,
    legal_name: Option<String>,
    industry: Option<String>,
    region: Option<String>,
    description: Option<String>,
    size_band: Option<String>,
}

#[derive(Debug, Default)]
pub struct CsvSeed;

impl CsvSeed {
    /// Build a CSV-seed loader.
    pub fn new() -> Self {
        Self
    }

    /// Parse a CSV file into a list of `Company` values. Invalid rows
    /// are dropped and counted; the count is logged via tracing but
    /// never fails the call.
    pub fn read_path(&self, path: &Path) -> Result<Vec<Company>> {
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_path(path)
            .map_err(|e| Error::Validation(format!("csv open {}: {e}", path.display())))?;

        let mut companies = Vec::new();
        let mut skipped = 0u32;
        for (idx, row) in rdr.deserialize::<Row>().enumerate() {
            match row {
                Ok(r) => match Self::row_to_company(r) {
                    Ok(c) => companies.push(c),
                    Err(e) => {
                        tracing::warn!(row = idx + 2, "%e" = %e, "skipping invalid CSV row");
                        skipped += 1;
                    }
                },
                Err(e) => {
                    tracing::warn!(row = idx + 2, "%e" = %e, "skipping unparseable CSV row");
                    skipped += 1;
                }
            }
        }

        if skipped > 0 {
            tracing::info!(skipped, kept = companies.len(), "csv ingest summary");
        }
        Ok(companies)
    }

    fn row_to_company(r: Row) -> Result<Company> {
        if r.display_name.trim().is_empty() {
            return Err(Error::Validation("display_name is empty".into()));
        }
        let homepage = match r.homepage.as_deref() {
            Some(s) if !s.trim().is_empty() => {
                let with_scheme = if s.starts_with("http://") || s.starts_with("https://") {
                    s.to_string()
                } else {
                    format!("https://{s}")
                };
                Some(
                    Url::parse(&with_scheme)
                        .map_err(|e| Error::Validation(format!("homepage `{s}`: {e}")))?,
                )
            }
            _ => None,
        };
        let size_band = r
            .size_band
            .as_deref()
            .and_then(|s| SizeBand::from_str(s).ok());
        Ok(Company {
            id: CompanyId::new(),
            legal_name: r.legal_name,
            display_name: r.display_name.trim().to_string(),
            homepage,
            industry: r.industry,
            size_band,
            region: r.region,
            description: r.description,
            tech_signals: vec![],
            discovered_at: Utc::now(),
            last_enriched_at: None,
            source: DiscoverySource::OwnerSeed,
            raw: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn ingests_minimal_csv() {
        let dir = std::env::temp_dir();
        let path = dir.join("salesman_csv_test.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "display_name,homepage,industry").unwrap();
        writeln!(f, "Acme Inc,https://acme.example,Security").unwrap();
        writeln!(f, "Beta LLC,beta.example,Devtools").unwrap();
        writeln!(f, ",empty.example,").unwrap(); // invalid: missing display_name
        f.flush().unwrap();

        let cs = CsvSeed::new();
        let companies = cs.read_path(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(companies.len(), 2);
        assert_eq!(companies[0].display_name, "Acme Inc");
        assert_eq!(
            companies[1].homepage.as_ref().unwrap().as_str(),
            "https://beta.example/"
        );
    }
}
