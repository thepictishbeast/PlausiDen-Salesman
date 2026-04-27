//! Typed queries over the schema. We hand-roll these instead of using
//! sqlx::query_as! macros so we don't require the database to be live
//! at compile time. Trade-off: slightly more boilerplate, no
//! compile-time SQL checking.

use crate::State;
use chrono::Utc;
use salesman_core::model::{CampaignStatus, TechSignal};
use salesman_core::{Campaign, CampaignId, Company, CompanyId, Error, Prospect, ProspectId, Result};
use sqlx::Row;

impl State {
    /// Insert a new company. Returns the assigned id (caller-supplied).
    pub async fn insert_company(&self, c: &Company) -> Result<CompanyId> {
        sqlx::query(
            "INSERT INTO companies
             (id, legal_name, display_name, homepage, industry,
              size_band, region, description, tech_signals,
              discovered_at, last_enriched_at, source, raw)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(c.id.0)
        .bind(&c.legal_name)
        .bind(&c.display_name)
        .bind(c.homepage.as_ref().map(|u| u.as_str()))
        .bind(&c.industry)
        .bind(c.size_band.as_ref().map(|s| s.to_string()))
        .bind(&c.region)
        .bind(&c.description)
        .bind(serde_json::to_value(&c.tech_signals)?)
        .bind(c.discovered_at)
        .bind(c.last_enriched_at)
        .bind(c.source.to_string())
        .bind(serde_json::to_value(&c.raw)?)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(c.id)
    }

    /// Insert many companies inside a single transaction. Idempotent
    /// per id (skips rows whose id already exists). Returns the number
    /// inserted.
    pub async fn insert_companies(&self, companies: &[Company]) -> Result<u64> {
        if companies.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool().begin().await.map_err(|e| Error::Db(e.to_string()))?;
        let mut inserted = 0u64;
        for c in companies {
            let result = sqlx::query(
                "INSERT INTO companies
                 (id, legal_name, display_name, homepage, industry,
                  size_band, region, description, tech_signals,
                  discovered_at, last_enriched_at, source, raw)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(c.id.0)
            .bind(&c.legal_name)
            .bind(&c.display_name)
            .bind(c.homepage.as_ref().map(|u| u.as_str()))
            .bind(&c.industry)
            .bind(c.size_band.as_ref().map(|s| s.to_string()))
            .bind(&c.region)
            .bind(&c.description)
            .bind(serde_json::to_value(&c.tech_signals)?)
            .bind(c.discovered_at)
            .bind(c.last_enriched_at)
            .bind(c.source.to_string())
            .bind(serde_json::to_value(&c.raw)?)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            inserted += result.rows_affected();
        }
        tx.commit().await.map_err(|e| Error::Db(e.to_string()))?;
        Ok(inserted)
    }

    /// Count companies in the database.
    pub async fn count_companies(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*)::BIGINT AS n FROM companies")
            .fetch_one(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("n").map_err(|e| Error::Db(e.to_string()))?)
    }

    /// Find or create a campaign by name. Returns the id either way.
    /// Created campaigns start in `Draft` status with the supplied goal.
    pub async fn ensure_campaign(
        &self,
        name: &str,
        goal: &str,
        target_segment: &str,
    ) -> Result<CampaignId> {
        if let Some(id) = self.find_campaign_id_by_name(name).await? {
            return Ok(id);
        }
        let c = Campaign {
            id: CampaignId::new(),
            name: name.to_string(),
            goal: goal.to_string(),
            target_segment: target_segment.to_string(),
            status: CampaignStatus::Draft,
            created_at: Utc::now(),
            paused_at: None,
            paused_reason: None,
        };
        sqlx::query(
            "INSERT INTO campaigns
             (id, name, goal, target_segment, status, created_at)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(c.id.0)
        .bind(&c.name)
        .bind(&c.goal)
        .bind(&c.target_segment)
        .bind(c.status.to_string())
        .bind(c.created_at)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(c.id)
    }

    async fn find_campaign_id_by_name(&self, name: &str) -> Result<Option<CampaignId>> {
        let row = sqlx::query("SELECT id FROM campaigns WHERE name = $1")
            .bind(name)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.map(|r| {
            let raw: uuid::Uuid = r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil());
            CampaignId(raw)
        }))
    }

    /// Add (campaign, company) pairs as Prospects in the `New` state.
    /// Idempotent — re-running for the same pair is a no-op.
    pub async fn upsert_prospects_for_campaign(
        &self,
        campaign_id: CampaignId,
        company_ids: &[CompanyId],
    ) -> Result<u64> {
        if company_ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool().begin().await.map_err(|e| Error::Db(e.to_string()))?;
        let mut inserted = 0u64;
        for cid in company_ids {
            let p = Prospect {
                id: ProspectId::new(),
                campaign_id,
                company_id: *cid,
                primary_contact_id: None,
                state: salesman_core::FunnelState::New,
                state_reason: None,
                state_changed_at: Utc::now(),
                fit_score: None,
                notes: None,
            };
            let result = sqlx::query(
                "INSERT INTO prospects
                 (id, campaign_id, company_id, primary_contact_id,
                  state, state_reason, state_changed_at, fit_score, notes)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                 ON CONFLICT (campaign_id, company_id) DO NOTHING",
            )
            .bind(p.id.0)
            .bind(p.campaign_id.0)
            .bind(p.company_id.0)
            .bind(p.primary_contact_id.map(|x| x.0))
            .bind(p.state.to_string())
            .bind(&p.state_reason)
            .bind(p.state_changed_at)
            .bind(p.fit_score)
            .bind(&p.notes)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            inserted += result.rows_affected();
        }
        tx.commit().await.map_err(|e| Error::Db(e.to_string()))?;
        Ok(inserted)
    }

    /// Persist enrichment facts back onto a company. Currently we
    /// merge: title/meta_description into `description` (if absent),
    /// tech_signals replace, last_enriched_at = now.
    pub async fn update_company_enrichment(
        &self,
        company_id: CompanyId,
        title: Option<&str>,
        meta_description: Option<&str>,
        tech_signals: &[TechSignal],
    ) -> Result<()> {
        // Pick the best description we have: prefer existing, then
        // meta_description, then title.
        let desc = meta_description.or(title).map(str::to_string);
        sqlx::query(
            "UPDATE companies
             SET description = COALESCE(description, $2),
                 tech_signals = $3,
                 last_enriched_at = $4
             WHERE id = $1",
        )
        .bind(company_id.0)
        .bind(desc)
        .bind(serde_json::to_value(tech_signals)?)
        .bind(Utc::now())
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// List companies linked to a campaign via `prospects`.
    pub async fn list_companies_for_campaign(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Vec<(CompanyId, String, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT c.id, c.display_name, c.homepage
             FROM companies c
             JOIN prospects p ON p.company_id = c.id
             WHERE p.campaign_id = $1
             ORDER BY c.display_name",
        )
        .bind(campaign_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    CompanyId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil())),
                    r.try_get("display_name").unwrap_or_default(),
                    r.try_get::<Option<String>, _>("homepage").unwrap_or(None),
                )
            })
            .collect())
    }
}
