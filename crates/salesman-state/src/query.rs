//! Typed queries over the schema. We hand-roll these instead of using
//! sqlx::query_as! macros so we don't require the database to be live
//! at compile time. Trade-off: slightly more boilerplate, no
//! compile-time SQL checking.

use crate::State;
use chrono::Utc;
use salesman_core::model::{CampaignStatus, TechSignal};
use salesman_core::model::ReplyKind;
use salesman_core::{
    Campaign, CampaignId, Company, CompanyId, Error, Prospect, ProspectId, Result, TouchId,
    TouchOutcome,
};
use salesman_receipts::Receipt;
use sqlx::Row;

/// Fire a Postgres NOTIFY on the `salesman_event` channel. Any LISTEN-er
/// (e.g. PlausiDen-CRM ingest) gets the JSON payload. Best-effort; a
/// failure here does NOT fail the calling write — we log + continue.
pub(crate) async fn notify_event(pool: &sqlx::PgPool, kind: &str, payload: serde_json::Value) {
    let envelope = serde_json::json!({
        "kind": kind,
        "at": chrono::Utc::now().to_rfc3339(),
        "payload": payload,
    });
    // pg_notify safely escapes the string; max payload 8000 bytes per
    // pg_notify limits. We truncate aggressively.
    let json = envelope.to_string();
    let json_truncated = if json.len() > 7800 {
        let mut s = json[..7800].to_string();
        s.push_str("...TRUNCATED");
        s
    } else {
        json
    };
    let result = sqlx::query("SELECT pg_notify('salesman_event', $1)")
        .bind(&json_truncated)
        .execute(pool)
        .await;
    if let Err(e) = result {
        tracing::warn!("%e" = %e, kind, "notify_event failed (non-fatal)");
    }
}

fn random_pick(keys: &[String], default_key: &str) -> String {
    if keys.is_empty() {
        return default_key.to_string();
    }
    let idx = (uuid::Uuid::now_v7().as_u128() as usize) % keys.len();
    keys[idx].clone()
}

#[derive(Debug, Clone)]
pub struct ProspectWithFacts {
    pub prospect_id: ProspectId,
    pub company_id: CompanyId,
    pub display_name: String,
    pub homepage: Option<String>,
    pub industry: Option<String>,
    pub description: Option<String>,
    pub tech_signals: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct SequenceStepInput {
    pub channel: String,
    pub template_key: String,
    pub delay_days: u32,
}

#[derive(Debug, Clone)]
pub struct DueProspect {
    pub prospect_id: ProspectId,
    pub sequence_id: uuid::Uuid,
    pub current_step: u32,
    pub template_key: String,
    pub channel: String,
}

#[derive(Debug, Clone)]
pub struct TemplateStat {
    pub template_key: String,
    pub drafted: i64,
    pub sent: i64,
    pub replied: i64,
    pub engaged_replied: i64,
}

impl TemplateStat {
    pub fn reply_rate(&self) -> f32 {
        if self.sent == 0 { 0.0 } else { self.replied as f32 / self.sent as f32 }
    }
    pub fn engaged_rate(&self) -> f32 {
        if self.sent == 0 { 0.0 } else { self.engaged_replied as f32 / self.sent as f32 }
    }
}

#[derive(Debug, Clone)]
pub struct CampaignCostRow {
    pub id: CampaignId,
    pub name: String,
    pub status: String,
    pub cost_cap_micro_usd: Option<i64>,
    pub spent_micro_usd: i64,
    pub calls: i64,
}

impl CampaignCostRow {
    pub fn over_cap(&self) -> bool {
        self.cost_cap_micro_usd
            .map(|cap| self.spent_micro_usd >= cap)
            .unwrap_or(false)
    }
    pub fn pct_used(&self) -> Option<f32> {
        self.cost_cap_micro_usd.and_then(|cap| {
            if cap <= 0 { None } else { Some((self.spent_micro_usd as f32) / (cap as f32) * 100.0) }
        })
    }
}

#[derive(Debug, Clone)]
pub struct LlmCallRecord {
    pub backend: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub output_tokens: u32,
    pub cache_hit_tokens: u32,
    pub latency_ms: u64,
    pub cost_micro_usd: u64,
    pub purpose: String,
    pub related_id: Option<uuid::Uuid>,
    pub related_kind: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CostSummaryRow {
    pub backend: String,
    pub model: String,
    pub count: i64,
    pub prompt_tokens: i64,
    pub output_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cost_micro_usd: i64,
    pub avg_latency_ms: i64,
    pub p95_latency_ms: i64,
}

#[derive(Debug, Clone)]
pub struct PipelineSummary {
    pub companies: i64,
    pub prospects: i64,
    pub new_prospects: i64,
    pub contacted: i64,
    pub engaged: i64,
    pub won: i64,
    pub lost: i64,
    pub suppressed_prospects: i64,
    pub awaiting_approval: i64,
    pub sent_recent: i64,
    pub replies_recent: i64,
    pub optout_recent: i64,
    pub suppressions: i64,
    pub receipts_recent: i64,
    pub since_hours: i64,
}

impl PipelineSummary {
    pub fn render_text(&self) -> String {
        format!(
            "PlausiDen-Salesman pipeline summary ({}h window)\n\
             ============================================\n\
             \n\
             Companies discovered:  {:>6}\n\
             Prospects (total):     {:>6}\n\
             \n\
             By funnel state:\n\
               new                  {:>6}\n\
               contacted            {:>6}\n\
               engaged              {:>6}\n\
               won                  {:>6}\n\
               lost                 {:>6}\n\
               suppressed           {:>6}\n\
             \n\
             Last {}h activity:\n\
               sends                {:>6}\n\
               replies              {:>6}   (opt-outs: {})\n\
               receipts             {:>6}\n\
             \n\
             Drafts awaiting approval:  {}\n\
             Suppression list size:     {}\n",
            self.since_hours,
            self.companies, self.prospects,
            self.new_prospects, self.contacted, self.engaged,
            self.won, self.lost, self.suppressed_prospects,
            self.since_hours,
            self.sent_recent, self.replies_recent, self.optout_recent,
            self.receipts_recent,
            self.awaiting_approval, self.suppressions
        )
    }
}

#[derive(Debug, Clone)]
pub struct ReplyRow {
    pub from_address: String,
    pub subject: Option<String>,
    pub body: String,
    pub kind: String,
    pub received_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct UnclassifiedReply {
    pub reply_id: uuid::Uuid,
    pub prospect_id: ProspectId,
    pub campaign_id: CampaignId,
    pub from_address: String,
    pub subject: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct TouchSummary {
    pub touch_id: salesman_core::TouchId,
    pub prospect_id: ProspectId,
    pub company: String,
    pub channel: String,
    pub subject: Option<String>,
    pub body: String,
    pub queued_at: chrono::DateTime<chrono::Utc>,
}

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

    /// List prospects-with-facts for a campaign. Returns enough to
    /// drive draft generation without a second round-trip.
    pub async fn list_prospects_with_facts_for_campaign(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Vec<ProspectWithFacts>> {
        let rows = sqlx::query(
            "SELECT p.id AS prospect_id, c.id AS company_id,
                    c.display_name, c.homepage, c.industry,
                    c.description, c.tech_signals
             FROM prospects p
             JOIN companies c ON c.id = p.company_id
             WHERE p.campaign_id = $1
             ORDER BY c.display_name",
        )
        .bind(campaign_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let prospect_id = ProspectId(r.try_get("prospect_id").unwrap_or_else(|_| uuid::Uuid::nil()));
            let company_id = CompanyId(r.try_get("company_id").unwrap_or_else(|_| uuid::Uuid::nil()));
            let display_name: String = r.try_get("display_name").unwrap_or_default();
            let homepage: Option<String> = r.try_get("homepage").unwrap_or(None);
            let industry: Option<String> = r.try_get("industry").unwrap_or(None);
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let tech_signals: serde_json::Value = r.try_get("tech_signals").unwrap_or(serde_json::Value::Array(vec![]));
            out.push(ProspectWithFacts {
                prospect_id,
                company_id,
                display_name,
                homepage,
                industry,
                description,
                tech_signals,
            });
        }
        Ok(out)
    }

    /// Insert a draft Touch in `awaiting_approval` outcome. The
    /// caller chose the channel + content; we just persist. Optional
    /// template_key threads through for the L4 stats query.
    pub async fn insert_touch_draft(
        &self,
        prospect_id: ProspectId,
        channel: salesman_core::TouchChannel,
        subject: Option<&str>,
        body: &str,
    ) -> Result<salesman_core::TouchId> {
        self.insert_touch_draft_with_template(prospect_id, channel, subject, body, None)
            .await
    }

    /// Same as `insert_touch_draft`, but also records `template_key`.
    pub async fn insert_touch_draft_with_template(
        &self,
        prospect_id: ProspectId,
        channel: salesman_core::TouchChannel,
        subject: Option<&str>,
        body: &str,
        template_key: Option<&str>,
    ) -> Result<salesman_core::TouchId> {
        let id = salesman_core::TouchId::new();
        sqlx::query(
            "INSERT INTO touches
             (id, prospect_id, channel, subject, body, queued_at, sent_at, outcome, receipt_id, template_key)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(id.0)
        .bind(prospect_id.0)
        .bind(channel.to_string())
        .bind(subject)
        .bind(body)
        .bind(Utc::now())
        .bind(None::<chrono::DateTime<chrono::Utc>>)
        .bind(salesman_core::TouchOutcome::AwaitingApproval.to_string())
        .bind(None::<uuid::Uuid>)
        .bind(template_key)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(id)
    }

    /// Pick a template via epsilon-greedy. With probability `1-epsilon`
    /// we pick the template with the best `engaged_rate`; with
    /// probability `epsilon` we pick a random other template.
    /// If there are no template-tagged sends yet, returns
    /// `default_key`.
    pub async fn pick_template_via_bandit(
        &self,
        epsilon: f32,
        default_key: &str,
        candidate_keys: &[String],
    ) -> Result<String> {
        if candidate_keys.is_empty() {
            return Ok(default_key.to_string());
        }
        let stats = self.template_stats().await?;
        // Filter to candidates only.
        let mut applicable: Vec<&TemplateStat> = stats
            .iter()
            .filter(|s| candidate_keys.contains(&s.template_key) && s.sent > 0)
            .collect();
        if applicable.is_empty() {
            // No data yet — pick a random candidate to start exploring.
            return Ok(random_pick(candidate_keys, default_key));
        }
        applicable.sort_by(|a, b| {
            b.engaged_rate()
                .partial_cmp(&a.engaged_rate())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let best = applicable[0].template_key.clone();
        // ε-explore.
        let r: f32 = (uuid::Uuid::now_v7().as_u128() as u32 as f32) / (u32::MAX as f32);
        if r < epsilon && candidate_keys.len() > 1 {
            // Random other.
            let others: Vec<&String> = candidate_keys.iter().filter(|k| **k != best).collect();
            if others.is_empty() {
                return Ok(best);
            }
            let idx = (uuid::Uuid::now_v7().as_u128() as usize) % others.len();
            return Ok(others[idx].clone());
        }
        Ok(best)
    }

    /// Per-template performance: drafted, sent, replied (any kind),
    /// engaged-replied. The bandit reads this to weight template
    /// selection.
    pub async fn template_stats(&self) -> Result<Vec<TemplateStat>> {
        let rows = sqlx::query(
            "SELECT
               t.template_key,
               COUNT(*) FILTER (WHERE t.outcome != 'rejected')::BIGINT AS drafted,
               COUNT(*) FILTER (WHERE t.outcome = 'sent')::BIGINT     AS sent,
               COUNT(DISTINCT r.id) FILTER (WHERE r.id IS NOT NULL)::BIGINT AS replied,
               COUNT(DISTINCT r.id) FILTER (WHERE r.kind = 'engaged')::BIGINT AS engaged_replied
             FROM touches t
             LEFT JOIN replies r ON r.touch_id = t.id
             WHERE t.template_key IS NOT NULL
             GROUP BY t.template_key
             ORDER BY drafted DESC",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| TemplateStat {
                template_key: r.try_get("template_key").unwrap_or_default(),
                drafted: r.try_get("drafted").unwrap_or(0),
                sent: r.try_get("sent").unwrap_or(0),
                replied: r.try_get("replied").unwrap_or(0),
                engaged_replied: r.try_get("engaged_replied").unwrap_or(0),
            })
            .collect())
    }

    /// List touches in awaiting-approval state for a campaign.
    pub async fn list_drafts_awaiting_approval(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Vec<TouchSummary>> {
        let rows = sqlx::query(
            "SELECT t.id, t.prospect_id, t.subject, t.body, t.channel, t.queued_at,
                    c.display_name AS company
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             JOIN companies c ON c.id = p.company_id
             WHERE p.campaign_id = $1 AND t.outcome = 'awaiting_approval'
             ORDER BY t.queued_at",
        )
        .bind(campaign_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(TouchSummary {
                touch_id: salesman_core::TouchId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil())),
                prospect_id: ProspectId(r.try_get("prospect_id").unwrap_or_else(|_| uuid::Uuid::nil())),
                company: r.try_get("company").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                queued_at: r.try_get("queued_at").unwrap_or_else(|_| chrono::Utc::now()),
            });
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // touch transitions
    // -----------------------------------------------------------------

    /// Move a touch from `awaiting_approval` → `approved`. No-op (returns
    /// 0) if the touch is in any other state. Caller checks rows-affected.
    pub async fn approve_touch(&self, touch_id: TouchId) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE touches SET outcome = $2 \
             WHERE id = $1 AND outcome = 'awaiting_approval'",
        )
        .bind(touch_id.0)
        .bind(TouchOutcome::Approved.to_string())
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Move a touch from `awaiting_approval` → `rejected`.
    pub async fn reject_touch(&self, touch_id: TouchId) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE touches SET outcome = $2 \
             WHERE id = $1 AND outcome = 'awaiting_approval'",
        )
        .bind(touch_id.0)
        .bind(TouchOutcome::Rejected.to_string())
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Move a touch from `approved` → `sent`. Records sent_at and a
    /// receipt_id linkage. Strict — does not transition from any other
    /// state. Fires a `touch.sent` NOTIFY on success.
    pub async fn mark_touch_sent(
        &self,
        touch_id: TouchId,
        receipt_id: salesman_core::ReceiptId,
        sent_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE touches SET outcome = $2, sent_at = $3, receipt_id = $4 \
             WHERE id = $1 AND outcome = 'approved'",
        )
        .bind(touch_id.0)
        .bind(TouchOutcome::Sent.to_string())
        .bind(sent_at)
        .bind(receipt_id.0)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        if result.rows_affected() > 0 {
            notify_event(
                self.pool(),
                "touch.sent",
                serde_json::json!({
                    "touch_id": touch_id.to_string(),
                    "receipt_id": receipt_id.to_string(),
                    "sent_at": sent_at.to_rfc3339(),
                }),
            )
            .await;
        }
        Ok(result.rows_affected())
    }

    /// List touches in `approved` state for a campaign — these are
    /// the work queue for `send-pending`.
    pub async fn list_approved_touches(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Vec<TouchSummary>> {
        let rows = sqlx::query(
            "SELECT t.id, t.prospect_id, t.subject, t.body, t.channel, t.queued_at,
                    c.display_name AS company
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             JOIN companies c ON c.id = p.company_id
             WHERE p.campaign_id = $1 AND t.outcome = 'approved'
             ORDER BY t.queued_at",
        )
        .bind(campaign_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(TouchSummary {
                touch_id: TouchId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil())),
                prospect_id: ProspectId(r.try_get("prospect_id").unwrap_or_else(|_| uuid::Uuid::nil())),
                company: r.try_get("company").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                queued_at: r.try_get("queued_at").unwrap_or_else(|_| chrono::Utc::now()),
            });
        }
        Ok(out)
    }

    /// Pull the (subject, body, outcome) for a touch — used by the
    /// approve flow to score the draft against the AI detector before
    /// changing state.
    pub async fn get_touch_for_review(
        &self,
        touch_id: TouchId,
    ) -> Result<Option<(Option<String>, String, String)>> {
        let row = sqlx::query("SELECT subject, body, outcome FROM touches WHERE id = $1")
            .bind(touch_id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.map(|r| {
            (
                r.try_get::<Option<String>, _>("subject").unwrap_or(None),
                r.try_get::<String, _>("body").unwrap_or_default(),
                r.try_get::<String, _>("outcome").unwrap_or_default(),
            )
        }))
    }

    /// Look up the to-address for a touch via the prospect's primary
    /// contact (or fall back to None — caller decides what to do).
    pub async fn touch_to_address(&self, touch_id: TouchId) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT ct.email AS to_email
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             LEFT JOIN contacts ct ON ct.id = p.primary_contact_id
             WHERE t.id = $1",
        )
        .bind(touch_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.and_then(|r| r.try_get::<Option<String>, _>("to_email").unwrap_or(None)))
    }

    // -----------------------------------------------------------------
    // replies
    // -----------------------------------------------------------------

    /// Insert a raw inbound reply. Caller has already parsed the
    /// MIME. We try to thread the reply to a prospect by matching
    /// from_address against any prospect's primary contact email.
    /// If no match, the reply is dropped (warns + returns Ok(None)).
    pub async fn insert_reply_threaded(
        &self,
        from_address: &str,
        subject: Option<&str>,
        body: &str,
        raw_headers: &serde_json::Value,
    ) -> Result<Option<uuid::Uuid>> {
        let row = sqlx::query(
            "SELECT p.id AS prospect_id
             FROM prospects p
             JOIN contacts c ON c.id = p.primary_contact_id
             WHERE c.email = $1
             ORDER BY p.state_changed_at DESC
             LIMIT 1",
        )
        .bind(from_address)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let Some(row) = row else {
            tracing::warn!(%from_address, "no prospect matches reply from-address — dropping");
            return Ok(None);
        };
        let prospect_id_uuid: uuid::Uuid = row.try_get("prospect_id")
            .map_err(|e| Error::Db(e.to_string()))?;

        let reply_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO replies
             (id, prospect_id, touch_id, from_address, subject, body, kind, raw_headers)
             VALUES ($1, $2, NULL, $3, $4, $5, $6, $7)",
        )
        .bind(reply_id)
        .bind(prospect_id_uuid)
        .bind(from_address)
        .bind(subject)
        .bind(body)
        .bind(ReplyKind::Unclassified.to_string())
        .bind(raw_headers)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        notify_event(
            self.pool(),
            "reply.received",
            serde_json::json!({
                "reply_id": reply_id.to_string(),
                "prospect_id": prospect_id_uuid.to_string(),
                "from_address": from_address,
            }),
        )
        .await;
        Ok(Some(reply_id))
    }

    /// List replies in `unclassified` state (queue for the classifier).
    pub async fn list_unclassified_replies(
        &self,
        limit: i64,
    ) -> Result<Vec<UnclassifiedReply>> {
        let rows = sqlx::query(
            "SELECT r.id, r.prospect_id, r.from_address, r.subject, r.body, p.campaign_id
             FROM replies r
             JOIN prospects p ON p.id = r.prospect_id
             WHERE r.kind = 'unclassified'
             ORDER BY r.received_at
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(UnclassifiedReply {
                reply_id: r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                prospect_id: ProspectId(r.try_get("prospect_id").unwrap_or_else(|_| uuid::Uuid::nil())),
                campaign_id: CampaignId(r.try_get("campaign_id").unwrap_or_else(|_| uuid::Uuid::nil())),
                from_address: r.try_get("from_address").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
            });
        }
        Ok(out)
    }

    pub async fn update_reply_kind(
        &self,
        reply_id: uuid::Uuid,
        kind: ReplyKind,
    ) -> Result<()> {
        sqlx::query("UPDATE replies SET kind = $2 WHERE id = $1")
            .bind(reply_id)
            .bind(kind.to_string())
            .execute(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// Apply a reply-kind decision to the prospect's funnel state and
    /// (when applicable) auto-suppress + pause sequence. Returns a
    /// human-readable summary of what changed.
    pub async fn apply_reply_to_prospect(
        &self,
        reply_id: uuid::Uuid,
        prospect_id: ProspectId,
        from_address: &str,
        kind: ReplyKind,
    ) -> Result<String> {
        let mut tx = self.pool().begin().await.map_err(|e| Error::Db(e.to_string()))?;
        let mut summary = String::new();

        // Always update reply.kind first.
        sqlx::query("UPDATE replies SET kind = $2 WHERE id = $1")
            .bind(reply_id)
            .bind(kind.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;

        // Map ReplyKind → FunnelState transition.
        let new_state: Option<&str> = match kind {
            ReplyKind::Engaged | ReplyKind::Question => Some("engaged"),
            ReplyKind::Optout => Some("suppressed"),
            ReplyKind::Bounce => Some("lost"),
            // Objection / OOO / Spam / Unclassified — leave funnel state.
            _ => None,
        };
        if let Some(target) = new_state {
            sqlx::query(
                "UPDATE prospects SET state = $2, state_changed_at = NOW(), \
                  state_reason = $3 WHERE id = $1",
            )
            .bind(prospect_id.0)
            .bind(target)
            .bind(format!("auto: reply classified {kind}"))
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            summary.push_str(&format!("prospect → {target}; "));
        }

        // Optout: also add to suppressions + reject any in-flight touches.
        if matches!(kind, ReplyKind::Optout) {
            sqlx::query(
                "INSERT INTO suppressions (id, target, target_kind, reason, source) \
                 VALUES ($1, $2, 'email', 'reply optout', 'reply_optout') \
                 ON CONFLICT (target) DO NOTHING",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(from_address)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            summary.push_str("added to suppressions; ");

            sqlx::query(
                "UPDATE touches SET outcome = 'suppressed' \
                 WHERE prospect_id = $1 AND outcome IN ('awaiting_approval', 'approved')",
            )
            .bind(prospect_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            summary.push_str("in-flight touches suppressed; ");
        }

        // Bounce: mark contact email as not verified.
        if matches!(kind, ReplyKind::Bounce) {
            sqlx::query(
                "UPDATE contacts SET email_verified = FALSE \
                 FROM prospects p \
                 WHERE contacts.id = p.primary_contact_id AND p.id = $1",
            )
            .bind(prospect_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            summary.push_str("contact marked unverified; ");
        }

        tx.commit().await.map_err(|e| Error::Db(e.to_string()))?;
        if summary.is_empty() {
            summary.push_str("no transition (kind doesn't drive a state change)");
        }
        // Notify after the tx commits.
        notify_event(
            self.pool(),
            "reply.classified",
            serde_json::json!({
                "reply_id": reply_id.to_string(),
                "prospect_id": prospect_id.to_string(),
                "kind": kind.to_string(),
                "summary": summary,
            }),
        )
        .await;
        Ok(summary)
    }

    // -----------------------------------------------------------------
    // sequences
    // -----------------------------------------------------------------

    /// Create a sequence + its steps in one transaction.
    pub async fn create_sequence(
        &self,
        campaign_id: CampaignId,
        name: &str,
        steps: &[SequenceStepInput],
    ) -> Result<uuid::Uuid> {
        if steps.is_empty() {
            return Err(Error::Validation("sequence must have at least one step".into()));
        }
        let mut tx = self.pool().begin().await.map_err(|e| Error::Db(e.to_string()))?;
        let sequence_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO sequences (id, campaign_id, name) VALUES ($1, $2, $3)",
        )
        .bind(sequence_id)
        .bind(campaign_id.0)
        .bind(name)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        for (idx, s) in steps.iter().enumerate() {
            sqlx::query(
                "INSERT INTO sequence_steps (id, sequence_id, position, channel, template_key, delay_days) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(sequence_id)
            .bind(idx as i32)
            .bind(&s.channel)
            .bind(&s.template_key)
            .bind(s.delay_days as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        }
        tx.commit().await.map_err(|e| Error::Db(e.to_string()))?;
        Ok(sequence_id)
    }

    /// Assign every prospect in a campaign to a sequence at step 0.
    pub async fn assign_sequence_to_campaign(
        &self,
        campaign_id: CampaignId,
        sequence_id: uuid::Uuid,
    ) -> Result<u64> {
        let result = sqlx::query(
            "INSERT INTO prospect_sequence_state \
             (prospect_id, sequence_id, current_step, next_due_at, last_advanced_at) \
             SELECT p.id, $2, 0, NOW(), NOW() \
             FROM prospects p \
             WHERE p.campaign_id = $1 \
             ON CONFLICT (prospect_id) DO NOTHING",
        )
        .bind(campaign_id.0)
        .bind(sequence_id)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Advance a prospect through its sequence after a successful send.
    /// Loads the step's delay, schedules next_due_at = NOW + delay.
    /// Returns true if advanced, false if already at the last step.
    pub async fn advance_prospect_in_sequence(
        &self,
        prospect_id: ProspectId,
    ) -> Result<bool> {
        let row = sqlx::query(
            "SELECT pss.sequence_id, pss.current_step,
                    (SELECT MAX(position) FROM sequence_steps WHERE sequence_id = pss.sequence_id) AS max_pos,
                    (SELECT delay_days FROM sequence_steps
                     WHERE sequence_id = pss.sequence_id AND position = pss.current_step + 1
                     LIMIT 1) AS next_delay
             FROM prospect_sequence_state pss
             WHERE pss.prospect_id = $1",
        )
        .bind(prospect_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let Some(row) = row else { return Ok(false) };
        let current: i32 = row.try_get("current_step").unwrap_or(0);
        let max_pos: Option<i32> = row.try_get("max_pos").ok();
        let next_delay: Option<i32> = row.try_get("next_delay").ok();

        if Some(current) >= max_pos {
            return Ok(false); // already at last step
        }
        let delay = next_delay.unwrap_or(0).max(0) as i64;

        sqlx::query(
            "UPDATE prospect_sequence_state \
             SET current_step = current_step + 1, \
                 next_due_at = NOW() + ($2 || ' days')::INTERVAL, \
                 last_advanced_at = NOW() \
             WHERE prospect_id = $1",
        )
        .bind(prospect_id.0)
        .bind(delay.to_string())
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(true)
    }

    /// Pause a prospect's sequence (e.g. on negative reply).
    pub async fn pause_prospect_sequence(
        &self,
        prospect_id: ProspectId,
        reason: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE prospect_sequence_state \
             SET paused = TRUE, paused_reason = $2 \
             WHERE prospect_id = $1",
        )
        .bind(prospect_id.0)
        .bind(reason)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// List prospect-sequence-states whose next_due_at has passed and
    /// are not paused — the work queue for the sequence scheduler.
    pub async fn list_due_prospects(
        &self,
        limit: i64,
    ) -> Result<Vec<DueProspect>> {
        let rows = sqlx::query(
            "SELECT pss.prospect_id, pss.sequence_id, pss.current_step,
                    s.template_key, s.channel, s.delay_days
             FROM prospect_sequence_state pss
             JOIN sequence_steps s
               ON s.sequence_id = pss.sequence_id AND s.position = pss.current_step
             WHERE pss.next_due_at <= NOW() AND NOT pss.paused
             ORDER BY pss.next_due_at
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| DueProspect {
                prospect_id: ProspectId(r.try_get("prospect_id").unwrap_or_else(|_| uuid::Uuid::nil())),
                sequence_id: r.try_get("sequence_id").unwrap_or_else(|_| uuid::Uuid::nil()),
                current_step: r.try_get::<i32, _>("current_step").unwrap_or(0) as u32,
                template_key: r.try_get("template_key").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
            })
            .collect())
    }

    // -----------------------------------------------------------------
    // suppressions
    // -----------------------------------------------------------------

    /// Idempotent insert. `target` is either a full email or a domain.
    /// `target_kind` MUST be "email" or "domain".
    pub async fn add_suppression(
        &self,
        target: &str,
        target_kind: &str,
        reason: &str,
        source: &str,
    ) -> Result<()> {
        if target_kind != "email" && target_kind != "domain" {
            return Err(Error::Validation(format!(
                "target_kind must be 'email' or 'domain', got `{target_kind}`"
            )));
        }
        sqlx::query(
            "INSERT INTO suppressions (id, target, target_kind, reason, source) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (target) DO NOTHING",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(target)
        .bind(target_kind)
        .bind(reason)
        .bind(source)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// True if either the full email is suppressed OR its domain is.
    /// Case-insensitive thanks to CITEXT on the column.
    pub async fn is_suppressed(&self, email: &str) -> Result<bool> {
        let domain = email.rsplit_once('@').map(|(_, d)| d).unwrap_or(email);
        let row = sqlx::query(
            "SELECT EXISTS (
                SELECT 1 FROM suppressions
                WHERE (target_kind = 'email'  AND target = $1)
                   OR (target_kind = 'domain' AND target = $2)
             ) AS hit",
        )
        .bind(email)
        .bind(domain)
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<bool, _>("hit").unwrap_or(false))
    }

    /// Count suppressions (for operator visibility).
    pub async fn count_suppressions(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*)::BIGINT AS n FROM suppressions")
            .fetch_one(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("n").map_err(|e| Error::Db(e.to_string()))?)
    }

    // -----------------------------------------------------------------
    // receipts
    // -----------------------------------------------------------------

    /// Insert a receipt row. Caller already constructed + signed it.
    pub async fn insert_receipt(&self, r: &Receipt) -> Result<()> {
        sqlx::query(
            "INSERT INTO receipts \
             (id, event_kind, event_payload, prev_hash, hash, signature, signing_key_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(r.id.0)
        .bind(&r.event_kind)
        .bind(&r.event_payload)
        .bind(&r.prev_hash)
        .bind(&r.hash)
        .bind(&r.signature)
        .bind(&r.signing_key_id)
        .bind(r.created_at)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// Latest receipt's hash, scoped to a signing key (chains are
    /// per-key). Returns 32-zero bytes if no prior receipt for this key.
    pub async fn get_last_hash(&self, signing_key_id: &str) -> Result<Vec<u8>> {
        let row = sqlx::query(
            "SELECT hash FROM receipts \
             WHERE signing_key_id = $1 \
             ORDER BY created_at DESC \
             LIMIT 1",
        )
        .bind(signing_key_id)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row
            .and_then(|r| r.try_get::<Vec<u8>, _>("hash").ok())
            .unwrap_or_else(|| vec![0u8; 32]))
    }

    /// Pull recent receipts (audit view).
    pub async fn list_recent_receipts(&self, limit: i64) -> Result<Vec<Receipt>> {
        let rows = sqlx::query(
            "SELECT id, event_kind, event_payload, prev_hash, hash, signature, signing_key_id, created_at \
             FROM receipts \
             ORDER BY created_at DESC \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(Receipt {
                id: salesman_core::ReceiptId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil())),
                event_kind: r.try_get("event_kind").unwrap_or_default(),
                event_payload: r.try_get("event_payload").unwrap_or(serde_json::Value::Null),
                prev_hash: r.try_get("prev_hash").unwrap_or_default(),
                hash: r.try_get("hash").unwrap_or_default(),
                signature: r.try_get("signature").unwrap_or_default(),
                signing_key_id: r.try_get("signing_key_id").unwrap_or_default(),
                created_at: r.try_get("created_at").unwrap_or_else(|_| chrono::Utc::now()),
            });
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // llm cost ledger
    // -----------------------------------------------------------------

    /// Record one LLM call in `llm_calls`. Cost is computed by the
    /// caller (uses salesman_llm::compute_cost_micro_usd).
    pub async fn insert_llm_call(&self, c: &LlmCallRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO llm_calls
             (id, backend, model, prompt_tokens, output_tokens,
              cache_hit_tokens, latency_ms, cost_micro_usd, purpose,
              related_id, related_kind)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(&c.backend)
        .bind(&c.model)
        .bind(c.prompt_tokens as i32)
        .bind(c.output_tokens as i32)
        .bind(c.cache_hit_tokens as i32)
        .bind(c.latency_ms as i32)
        .bind(c.cost_micro_usd as i64)
        .bind(&c.purpose)
        .bind(c.related_id)
        .bind(&c.related_kind)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// Set a cost cap on a campaign. NULL clears.
    pub async fn set_campaign_cost_cap(
        &self,
        campaign_id: CampaignId,
        cap_micro_usd: Option<i64>,
    ) -> Result<()> {
        sqlx::query("UPDATE campaigns SET cost_cap_micro_usd = $2 WHERE id = $1")
            .bind(campaign_id.0)
            .bind(cap_micro_usd)
            .execute(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// Total cost of LLM calls attributed to a specific campaign.
    /// Sums llm_calls.cost_micro_usd where related_id = campaign_id
    /// and related_kind = 'campaign'.
    pub async fn campaign_cost_so_far(&self, campaign_id: CampaignId) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(cost_micro_usd), 0)::BIGINT AS total
             FROM llm_calls
             WHERE related_kind = 'campaign' AND related_id = $1",
        )
        .bind(campaign_id.0)
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("total").unwrap_or(0))
    }

    /// Returns Some(cap) if the campaign has a cap set, else None.
    pub async fn campaign_cost_cap(&self, campaign_id: CampaignId) -> Result<Option<i64>> {
        let row = sqlx::query("SELECT cost_cap_micro_usd FROM campaigns WHERE id = $1")
            .bind(campaign_id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.and_then(|r| r.try_get::<Option<i64>, _>("cost_cap_micro_usd").unwrap_or(None)))
    }

    /// True if the campaign is over its cost cap (or no cap is set →
    /// always returns false).
    pub async fn campaign_over_cost_cap(&self, campaign_id: CampaignId) -> Result<bool> {
        let Some(cap) = self.campaign_cost_cap(campaign_id).await? else {
            return Ok(false);
        };
        let so_far = self.campaign_cost_so_far(campaign_id).await?;
        Ok(so_far >= cap)
    }

    /// Pause a campaign with a reason (used when cost cap exceeded).
    pub async fn pause_campaign(&self, campaign_id: CampaignId, reason: &str) -> Result<()> {
        sqlx::query(
            "UPDATE campaigns
             SET status = 'paused', paused_at = NOW(), paused_reason = $2
             WHERE id = $1",
        )
        .bind(campaign_id.0)
        .bind(reason)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// Per-campaign cost breakdown for the cost report. Joins through
    /// llm_calls.related_id where related_kind='campaign'.
    pub async fn campaign_cost_summary(&self, since_hours: i64) -> Result<Vec<CampaignCostRow>> {
        let rows = sqlx::query(
            "SELECT c.id, c.name, c.status, c.cost_cap_micro_usd,
                    COALESCE(SUM(l.cost_micro_usd), 0)::BIGINT AS spent_micro_usd,
                    COUNT(l.id)::BIGINT AS calls
             FROM campaigns c
             LEFT JOIN llm_calls l
               ON l.related_id = c.id
              AND l.related_kind = 'campaign'
              AND l.created_at > NOW() - ($1 || ' hours')::INTERVAL
             GROUP BY c.id, c.name, c.status, c.cost_cap_micro_usd
             ORDER BY spent_micro_usd DESC",
        )
        .bind(since_hours.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| CampaignCostRow {
                id: CampaignId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil())),
                name: r.try_get("name").unwrap_or_default(),
                status: r.try_get("status").unwrap_or_default(),
                cost_cap_micro_usd: r.try_get("cost_cap_micro_usd").unwrap_or(None),
                spent_micro_usd: r.try_get("spent_micro_usd").unwrap_or(0),
                calls: r.try_get("calls").unwrap_or(0),
            })
            .collect())
    }

    /// Roll-up by (backend, model) over a recent window.
    pub async fn cost_summary(&self, since_hours: i64) -> Result<Vec<CostSummaryRow>> {
        let rows = sqlx::query(
            "SELECT backend, model,
                    COUNT(*)::BIGINT          AS n,
                    SUM(prompt_tokens)::BIGINT  AS prompt,
                    SUM(output_tokens)::BIGINT  AS output,
                    SUM(cache_hit_tokens)::BIGINT AS cache_hit,
                    SUM(cost_micro_usd)::BIGINT AS micro_usd,
                    AVG(latency_ms)::BIGINT   AS avg_latency,
                    PERCENTILE_DISC(0.95) WITHIN GROUP (ORDER BY latency_ms)::BIGINT AS p95_latency
             FROM llm_calls
             WHERE created_at > NOW() - ($1 || ' hours')::INTERVAL
             GROUP BY backend, model
             ORDER BY micro_usd DESC",
        )
        .bind(since_hours.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| CostSummaryRow {
                backend: r.try_get("backend").unwrap_or_default(),
                model: r.try_get("model").unwrap_or_default(),
                count: r.try_get("n").unwrap_or(0),
                prompt_tokens: r.try_get("prompt").unwrap_or(0),
                output_tokens: r.try_get("output").unwrap_or(0),
                cache_hit_tokens: r.try_get("cache_hit").unwrap_or(0),
                cost_micro_usd: r.try_get("micro_usd").unwrap_or(0),
                avg_latency_ms: r.try_get("avg_latency").unwrap_or(0),
                p95_latency_ms: r.try_get("p95_latency").unwrap_or(0),
            })
            .collect())
    }

    // -----------------------------------------------------------------
    // rate-cap helpers
    // -----------------------------------------------------------------

    /// Count touches (any outcome) sent to `to_email` in the last
    /// `window_hours` — used to enforce per-recipient rate caps.
    pub async fn count_touches_to_email_since(
        &self,
        to_email: &str,
        window_hours: i64,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS n
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             LEFT JOIN contacts ct ON ct.id = p.primary_contact_id
             WHERE ct.email = $1 AND t.sent_at IS NOT NULL
               AND t.sent_at > NOW() - ($2 || ' hours')::INTERVAL",
        )
        .bind(to_email)
        .bind(window_hours.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    /// Count touches sent to any address in `domain` in the last
    /// `window_hours` — per-domain rate cap.
    pub async fn count_touches_to_domain_since(
        &self,
        domain: &str,
        window_hours: i64,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS n
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             LEFT JOIN contacts ct ON ct.id = p.primary_contact_id
             WHERE ct.email LIKE '%@' || $1 AND t.sent_at IS NOT NULL
               AND t.sent_at > NOW() - ($2 || ' hours')::INTERVAL",
        )
        .bind(domain)
        .bind(window_hours.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    /// Pipeline summary counts for the daily/weekly digest.
    pub async fn pipeline_summary(&self, since_hours: i64) -> Result<PipelineSummary> {
        let row = sqlx::query(
            "SELECT
                (SELECT COUNT(*) FROM companies)                                                                       ::BIGINT AS companies,
                (SELECT COUNT(*) FROM prospects)                                                                       ::BIGINT AS prospects,
                (SELECT COUNT(*) FROM prospects WHERE state = 'new')                                                   ::BIGINT AS new_prospects,
                (SELECT COUNT(*) FROM prospects WHERE state = 'contacted')                                             ::BIGINT AS contacted,
                (SELECT COUNT(*) FROM prospects WHERE state = 'engaged')                                               ::BIGINT AS engaged,
                (SELECT COUNT(*) FROM prospects WHERE state = 'won')                                                   ::BIGINT AS won,
                (SELECT COUNT(*) FROM prospects WHERE state = 'lost')                                                  ::BIGINT AS lost,
                (SELECT COUNT(*) FROM prospects WHERE state = 'suppressed')                                            ::BIGINT AS suppressed_prospects,
                (SELECT COUNT(*) FROM touches WHERE outcome = 'awaiting_approval')                                     ::BIGINT AS awaiting_approval,
                (SELECT COUNT(*) FROM touches WHERE outcome = 'sent' AND sent_at > NOW() - ($1 || ' hours')::INTERVAL) ::BIGINT AS sent_recent,
                (SELECT COUNT(*) FROM replies WHERE received_at > NOW() - ($1 || ' hours')::INTERVAL)                  ::BIGINT AS replies_recent,
                (SELECT COUNT(*) FROM replies WHERE kind = 'optout' AND received_at > NOW() - ($1 || ' hours')::INTERVAL) ::BIGINT AS optout_recent,
                (SELECT COUNT(*) FROM suppressions)                                                                    ::BIGINT AS suppressions,
                (SELECT COUNT(*) FROM receipts WHERE created_at > NOW() - ($1 || ' hours')::INTERVAL)                  ::BIGINT AS receipts_recent",
        )
        .bind(since_hours.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        Ok(PipelineSummary {
            companies: row.try_get("companies").unwrap_or(0),
            prospects: row.try_get("prospects").unwrap_or(0),
            new_prospects: row.try_get("new_prospects").unwrap_or(0),
            contacted: row.try_get("contacted").unwrap_or(0),
            engaged: row.try_get("engaged").unwrap_or(0),
            won: row.try_get("won").unwrap_or(0),
            lost: row.try_get("lost").unwrap_or(0),
            suppressed_prospects: row.try_get("suppressed_prospects").unwrap_or(0),
            awaiting_approval: row.try_get("awaiting_approval").unwrap_or(0),
            sent_recent: row.try_get("sent_recent").unwrap_or(0),
            replies_recent: row.try_get("replies_recent").unwrap_or(0),
            optout_recent: row.try_get("optout_recent").unwrap_or(0),
            suppressions: row.try_get("suppressions").unwrap_or(0),
            receipts_recent: row.try_get("receipts_recent").unwrap_or(0),
            since_hours,
        })
    }

    /// Most recent replies for a campaign — for the inbox view.
    pub async fn list_recent_replies_for_campaign(
        &self,
        campaign_id: CampaignId,
        limit: i64,
    ) -> Result<Vec<ReplyRow>> {
        let rows = sqlx::query(
            "SELECT r.from_address, r.subject, r.body, r.kind, r.received_at
             FROM replies r
             JOIN prospects p ON p.id = r.prospect_id
             WHERE p.campaign_id = $1
             ORDER BY r.received_at DESC
             LIMIT $2",
        )
        .bind(campaign_id.0)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| ReplyRow {
                from_address: r.try_get("from_address").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                kind: r.try_get("kind").unwrap_or_default(),
                received_at: r.try_get("received_at").unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect())
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
