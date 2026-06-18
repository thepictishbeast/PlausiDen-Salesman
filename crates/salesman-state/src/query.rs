//! Typed queries over the schema. We hand-roll these instead of using
//! sqlx::query_as! macros so we don't require the database to be live
//! at compile time. Trade-off: slightly more boilerplate, no
//! compile-time SQL checking.

use crate::State;
use chrono::Utc;
use salesman_core::model::ReplyKind;
use salesman_core::model::{CampaignStatus, TechSignal};
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

/// A prospect joined with its company's enrichment facts, as consumed
/// by the drafter tools.
#[derive(Debug, Clone)]
pub struct ProspectWithFacts {
    /// The prospect id.
    pub prospect_id: ProspectId,
    /// The prospect's company id.
    pub company_id: CompanyId,
    /// Company display name.
    pub display_name: String,
    /// Company homepage URL, if known.
    pub homepage: Option<String>,
    /// Company industry, if known.
    pub industry: Option<String>,
    /// Company description, if known.
    pub description: Option<String>,
    /// Company region (free-text), if known. Drives local-first ranking
    /// (see salesman_discovery::locality).
    pub region: Option<String>,
    /// Detected tech signals (JSONB array).
    pub tech_signals: serde_json::Value,
    /// Per-prospect tags JSONB (interests, notes, do-not-pitch
    /// list). `{}` for new prospects; accumulates via
    /// `add_prospect_interest` + future LLM extraction (U52).
    /// Drafter prompts pass this through verbatim so personalized
    /// copy can reference it.
    pub tags: serde_json::Value,
}

impl ProspectWithFacts {
    /// Canonical JSON shape passed to drafter tools as the
    /// `prospect` field. Centralized so every draft path (cold,
    /// trigger-anchored, sequence-step, account-fanout) emits the
    /// SAME shape — operator changes and new fact fields land in
    /// one place.
    pub fn to_prompt_json(&self) -> serde_json::Value {
        serde_json::json!({
            "display_name": self.display_name,
            "homepage": self.homepage,
            "industry": self.industry,
            "description": self.description,
            "region": self.region,
            "tech_signals": self.tech_signals,
            "tags": self.tags,
        })
    }
}

/// One step in a campaign sequence definition.
#[derive(Debug, Clone)]
pub struct SequenceStepInput {
    /// Channel for this step (e.g. `email`).
    pub channel: String,
    /// Template key to draft from.
    pub template_key: String,
    /// Delay before this step, in days.
    pub delay_days: u32,
}

/// A prospect whose next sequence step is due to be drafted.
#[derive(Debug, Clone)]
pub struct DueProspect {
    /// The prospect id.
    pub prospect_id: ProspectId,
    /// The sequence the prospect is enrolled in.
    pub sequence_id: uuid::Uuid,
    /// The step index that is now due.
    pub current_step: u32,
    /// Template key for the due step.
    pub template_key: String,
    /// Channel for the due step.
    pub channel: String,
}

/// Per-template performance counters.
#[derive(Debug, Clone)]
pub struct TemplateStat {
    /// The template key these stats are for.
    pub template_key: String,
    /// Number of drafts produced from this template.
    pub drafted: i64,
    /// Number sent.
    pub sent: i64,
    /// Number that received any reply.
    pub replied: i64,
    /// Number that received an engaged (positive) reply.
    pub engaged_replied: i64,
}

impl TemplateStat {
    /// Replies per send, in [0,1] (0 when nothing has been sent).
    pub fn reply_rate(&self) -> f32 {
        if self.sent == 0 {
            0.0
        } else {
            self.replied as f32 / self.sent as f32
        }
    }
    /// Engaged replies per send, in [0,1] (0 when nothing has been sent).
    pub fn engaged_rate(&self) -> f32 {
        if self.sent == 0 {
            0.0
        } else {
            self.engaged_replied as f32 / self.sent as f32
        }
    }
}

/// A campaign with its spend vs. cost cap.
#[derive(Debug, Clone)]
pub struct CampaignCostRow {
    /// Campaign id.
    pub id: CampaignId,
    /// Campaign name.
    pub name: String,
    /// Campaign status (wire string).
    pub status: String,
    /// Configured cost cap in micro-USD, if any.
    pub cost_cap_micro_usd: Option<i64>,
    /// Total spent so far, in micro-USD.
    pub spent_micro_usd: i64,
    /// Number of LLM calls attributed to the campaign.
    pub calls: i64,
}

impl CampaignCostRow {
    /// True once spend has reached/exceeded the campaign cost cap; false
    /// when no cap is set.
    pub fn over_cap(&self) -> bool {
        self.cost_cap_micro_usd
            .map(|cap| self.spent_micro_usd >= cap)
            .unwrap_or(false)
    }
    /// Percent of the cost cap consumed, or `None` when no positive cap is set.
    pub fn pct_used(&self) -> Option<f32> {
        self.cost_cap_micro_usd.and_then(|cap| {
            if cap <= 0 {
                None
            } else {
                Some((self.spent_micro_usd as f32) / (cap as f32) * 100.0)
            }
        })
    }
}

/// One `llm_calls` row to persist (a single inference call).
#[derive(Debug, Clone)]
pub struct LlmCallRecord {
    /// Backend that served the call.
    pub backend: String,
    /// Model identifier.
    pub model: String,
    /// Prompt token count.
    pub prompt_tokens: u32,
    /// Output token count.
    pub output_tokens: u32,
    /// Cache-hit token count.
    pub cache_hit_tokens: u32,
    /// Call latency in milliseconds.
    pub latency_ms: u64,
    /// Estimated cost in micro-USD.
    pub cost_micro_usd: u64,
    /// What the call was for (e.g. `draft_cold`).
    pub purpose: String,
    /// Related entity id (touch/prospect/etc.), if any.
    pub related_id: Option<uuid::Uuid>,
    /// Kind of the related entity, if any.
    pub related_kind: Option<String>,
}

/// Aggregated LLM cost/usage grouped by backend + model.
#[derive(Debug, Clone)]
pub struct CostSummaryRow {
    /// Backend.
    pub backend: String,
    /// Model identifier.
    pub model: String,
    /// Number of calls in the group.
    pub count: i64,
    /// Total prompt tokens.
    pub prompt_tokens: i64,
    /// Total output tokens.
    pub output_tokens: i64,
    /// Total cache-hit tokens.
    pub cache_hit_tokens: i64,
    /// Total cost in micro-USD.
    pub cost_micro_usd: i64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: i64,
    /// 95th-percentile latency in milliseconds.
    pub p95_latency_ms: i64,
}

/// Aggregated LLM cost/usage grouped by call purpose.
#[derive(Debug, Clone)]
pub struct PurposeCostRow {
    /// The call purpose this row aggregates.
    pub purpose: String,
    /// Number of calls in the group.
    pub count: i64,
    /// Total prompt tokens.
    pub prompt_tokens: i64,
    /// Total output tokens.
    pub output_tokens: i64,
    /// Total cache-hit tokens.
    pub cache_hit_tokens: i64,
    /// Total cost in micro-USD.
    pub cost_micro_usd: i64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: i64,
    /// 95th-percentile latency in milliseconds.
    pub p95_latency_ms: i64,
}

/// One entry on the suppression list.
#[derive(Debug, Clone)]
pub struct SuppressionRow {
    /// Row id.
    pub id: uuid::Uuid,
    /// The suppressed target (e.g. an email address or domain).
    pub target: String,
    /// Kind of target (e.g. `email`, `domain`).
    pub target_kind: String,
    /// Why the target was suppressed.
    pub reason: String,
    /// What added it (e.g. `reply_optout`, `manual`).
    pub source: String,
    /// When it was added.
    pub added_at: chrono::DateTime<chrono::Utc>,
}

/// A snapshot of pipeline counts over a recent time window.
#[derive(Debug, Clone)]
pub struct PipelineSummary {
    /// Total companies discovered.
    pub companies: i64,
    /// Total prospects.
    pub prospects: i64,
    /// Prospects in the `new` state.
    pub new_prospects: i64,
    /// Prospects in the `contacted` state.
    pub contacted: i64,
    /// Prospects in the `engaged` state.
    pub engaged: i64,
    /// Prospects in the `won` state.
    pub won: i64,
    /// Prospects in the `lost` state.
    pub lost: i64,
    /// Prospects in the `suppressed` state.
    pub suppressed_prospects: i64,
    /// Drafts awaiting operator approval.
    pub awaiting_approval: i64,
    /// Sends within the window.
    pub sent_recent: i64,
    /// Replies within the window.
    pub replies_recent: i64,
    /// Opt-outs within the window.
    pub optout_recent: i64,
    /// Total suppression-list size.
    pub suppressions: i64,
    /// Receipts written within the window.
    pub receipts_recent: i64,
    /// The window size, in hours.
    pub since_hours: i64,
}

impl PipelineSummary {
    /// Render this pipeline summary as the human-readable text block
    /// shown by the operator CLI.
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
            self.companies,
            self.prospects,
            self.new_prospects,
            self.contacted,
            self.engaged,
            self.won,
            self.lost,
            self.suppressed_prospects,
            self.since_hours,
            self.sent_recent,
            self.replies_recent,
            self.optout_recent,
            self.receipts_recent,
            self.awaiting_approval,
            self.suppressions
        )
    }
}

/// A stored inbound reply, in display form.
#[derive(Debug, Clone)]
pub struct ReplyRow {
    /// Sender address.
    pub from_address: String,
    /// Subject line, if any.
    pub subject: Option<String>,
    /// Message body.
    pub body: String,
    /// Classified reply kind (wire string).
    pub kind: String,
    /// When the reply was received.
    pub received_at: chrono::DateTime<chrono::Utc>,
}

/// One turn in a prospect's conversation thread. Either an
/// outbound touch we sent or an inbound reply they sent. The
/// reply-drafter consumes a chronological list of these so it can
/// reference the prior back-and-forth instead of treating every
/// reply as if it's the first.
#[derive(Debug, Clone)]
pub struct ThreadTurn {
    /// When this turn occurred.
    pub at: chrono::DateTime<chrono::Utc>,
    /// "outbound" for touches we sent; "reply" for inbound replies.
    pub role: String,
    /// Subject line, if any.
    pub subject: Option<String>,
    /// Message body.
    pub body: String,
    /// Only set on inbound replies — the classifier kind
    /// (engaged / question / objection / …).
    pub reply_kind: Option<String>,
}

/// An inbound reply that has not yet been classified.
#[derive(Debug, Clone)]
pub struct UnclassifiedReply {
    /// The reply's id.
    pub reply_id: uuid::Uuid,
    /// The prospect the reply belongs to.
    pub prospect_id: ProspectId,
    /// The campaign the prospect is in.
    pub campaign_id: CampaignId,
    /// Sender address.
    pub from_address: String,
    /// Subject line, if any.
    pub subject: Option<String>,
    /// Message body.
    pub body: String,
    /// Raw header bag persisted at insert_reply_threaded time.
    /// The classifier checks `Authentication-Results` here BEFORE
    /// auto-suppressing on Optout / LegalThreat — defends against
    /// suppression-list poisoning by forged inbounds (RFC 8601).
    pub raw_headers: serde_json::Value,
}

/// A classified reply that needs a response. Carries both the
/// inbound details and (when threading lined up) the original
/// outbound that prompted it. Used by `salesman draft-replies`.
#[derive(Debug, Clone)]
pub struct ReplyNeedingResponse {
    /// The reply's id.
    pub reply_id: uuid::Uuid,
    /// The prospect the reply belongs to.
    pub prospect_id: ProspectId,
    /// Sender address.
    pub from_address: String,
    /// Inbound subject, if any.
    pub inbound_subject: Option<String>,
    /// Inbound body being replied to.
    pub inbound_body: String,
    /// Classified inbound kind (wire string).
    pub inbound_kind: String,
    /// The outbound that this reply is in response to, if threading
    /// matched. Often Some — IMAP threading via In-Reply-To /
    /// References lines up most of the time.
    pub outbound_subject: Option<String>,
    /// Body of the matched outbound, if any.
    pub outbound_body: Option<String>,
    /// Prospect display fields the drafter uses for personalization.
    pub company_name: String,
    /// Prospect company industry, if known.
    pub industry: Option<String>,
    /// Prospect company description, if known.
    pub description: Option<String>,
}

/// A queued/awaiting-approval touch in display form.
#[derive(Debug, Clone)]
pub struct TouchSummary {
    /// The touch id.
    pub touch_id: salesman_core::TouchId,
    /// The prospect the touch targets.
    pub prospect_id: ProspectId,
    /// Company display name.
    pub company: String,
    /// Channel (e.g. `email`).
    pub channel: String,
    /// Subject line, if any.
    pub subject: Option<String>,
    /// Message body.
    pub body: String,
    /// When the touch was queued.
    pub queued_at: chrono::DateTime<chrono::Utc>,
    /// Provenance JSONB { backend, model, via_fallback, purpose }.
    /// None for legacy touches drafted before migration 0005.
    pub produced_by: Option<serde_json::Value>,
}

impl TouchSummary {
    /// True when produced_by.via_fallback is explicitly true. False
    /// for either-not-set or false. Used by the send-pending
    /// `--require-primary` gate.
    pub fn via_fallback(&self) -> bool {
        self.produced_by
            .as_ref()
            .and_then(|v| v.get("via_fallback"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// `backend/model` short string for display.
    pub fn produced_by_short(&self) -> Option<String> {
        let pb = self.produced_by.as_ref()?;
        let backend = pb.get("backend").and_then(|v| v.as_str()).unwrap_or("?");
        let model = pb.get("model").and_then(|v| v.as_str()).unwrap_or("?");
        Some(format!("{backend}/{model}"))
    }
}

/// A single trigger event surfaced by the trigger-scanner. The
/// drafter consumes these as personalization anchors.
#[derive(Debug, Clone)]
pub struct TriggerEventRow {
    /// Trigger event id.
    pub id: uuid::Uuid,
    /// The prospect this event is an anchor for.
    pub prospect_id: ProspectId,
    /// Company display name.
    pub company: String,
    /// Where the event came from (e.g. `recent_news`).
    pub source: String,
    /// The event headline used as the outreach anchor.
    pub headline: String,
    /// Source URL, if any.
    pub url: Option<String>,
    /// Recency score in 0..=1.
    pub recency_score: f32,
    /// Relevance score in 0..=1.
    pub relevance_score: f32,
    /// When the event was recorded.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl TriggerEventRow {
    /// Composite ranking score (recency × relevance) used to order
    /// trigger events for outreach prioritisation.
    pub fn rank(&self) -> f32 {
        self.recency_score * self.relevance_score
    }
}

/// Input shape for `insert_trigger_event`. Groups the per-event
/// fields so the public function stays under the seven-arg lint.
#[derive(Debug, Clone)]
pub struct TriggerEventInsert<'a> {
    /// The prospect the event anchors to.
    pub prospect_id: ProspectId,
    /// Where the event came from.
    pub source: &'a str,
    /// The event headline.
    pub headline: &'a str,
    /// Source URL, if any.
    pub url: Option<&'a str>,
    /// Recency score in 0..=1.
    pub recency_score: f32,
    /// Relevance score in 0..=1.
    pub relevance_score: f32,
    /// Raw source payload (JSONB), retained for audit.
    pub raw: &'a serde_json::Value,
}

/// Input shape for `insert_owner_notification`. Groups the per-contact
/// fields so the public function stays under the seven-arg lint.
#[derive(Debug, Clone)]
pub struct OwnerNotificationInsert<'a> {
    /// The touch this notification is about, if any.
    pub touch_id: Option<salesman_core::TouchId>,
    /// The prospect that was contacted.
    pub prospect_id: ProspectId,
    /// Prospect name/business, captured at send time (subject line).
    pub prospect_label: &'a str,
    /// The recipient address that was contacted.
    pub to_address: &'a str,
    /// Channel used (e.g. `email`).
    pub channel: &'a str,
    /// When the contact was sent.
    pub sent_at: chrono::DateTime<chrono::Utc>,
    /// The subject that was sent, if any.
    pub subject: Option<&'a str>,
    /// The body that was sent.
    pub body: &'a str,
    /// The signed receipt id for the send, if recorded.
    pub receipt_id: Option<salesman_core::ReceiptId>,
    /// The campaign the contact belonged to, if any.
    pub campaign: Option<&'a str>,
}

/// A persisted owner audit-notification row.
#[derive(Debug, Clone)]
pub struct OwnerNotificationRow {
    /// Row id.
    pub id: uuid::Uuid,
    /// The prospect that was contacted.
    pub prospect_id: ProspectId,
    /// Prospect name/business captured at send time.
    pub prospect_label: String,
    /// The recipient address.
    pub to_address: String,
    /// Channel used.
    pub channel: String,
    /// When the contact was sent.
    pub sent_at: chrono::DateTime<chrono::Utc>,
    /// Subject sent, if any.
    pub subject: Option<String>,
    /// Body sent.
    pub body: String,
    /// Signed receipt id, if any.
    pub receipt_id: Option<uuid::Uuid>,
    /// Campaign, if any.
    pub campaign: Option<String>,
    /// When the notification row was queued.
    pub queued_at: chrono::DateTime<chrono::Utc>,
    /// When the operator mailbox received it; `None` while pending.
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
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
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
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
        row.try_get::<i64, _>("n")
            .map_err(|e| Error::Db(e.to_string()))
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

    /// Days since the campaign was created. Used to enforce the
    /// sender-warmup gradient — younger campaigns have lower
    /// per-batch caps to protect the new sender domain's reputation.
    /// Returns 0 for a campaign created today.
    pub async fn campaign_age_days(&self, id: CampaignId) -> Result<i64> {
        let row = sqlx::query(
            "SELECT EXTRACT(EPOCH FROM (NOW() - created_at))::BIGINT AS secs \
             FROM campaigns WHERE id = $1",
        )
        .bind(id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        let secs = row
            .and_then(|r| r.try_get::<i64, _>("secs").ok())
            .unwrap_or(0);
        Ok((secs / 86400).max(0))
    }

    /// Create a contact row + link it as the prospect's primary
    /// contact, atomically. Returns the new contact's id. Idempotent
    /// on (company, email) — re-running with the same email simply
    /// links the existing contact without inserting a duplicate.
    /// `email_verified` defaults to false because the email is a
    /// GUESS from the team-scraper; operator should verify before
    /// sending.
    pub async fn insert_contact_and_link_as_primary(
        &self,
        company_id: CompanyId,
        prospect_id: ProspectId,
        name: &str,
        title: &str,
        email: &str,
        source: &str,
    ) -> Result<uuid::Uuid> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        // Canonicalize email so two find-buyers runs that discover
        // `John@Acme.com` and `john@acme.com` (or
        // `john+sales@gmail.com` and `john@gmail.com`) collapse onto
        // the same row via ON CONFLICT (company_id, email). Without
        // this they'd create duplicate contacts for the same logical
        // mailbox and the prospect's primary_contact_id would point
        // at whichever was inserted last. Same canonicalization rule
        // every other email comparison uses — see
        // salesman-core::email_match.
        let canonical_email = salesman_core::normalize_email_for_match(email);
        // ON CONFLICT (company_id, email) DO UPDATE on the displayable
        // fields so re-runs replace stale title/name with current.
        let row = sqlx::query(
            "INSERT INTO contacts (id, company_id, kind, name, title, email, source) \
             VALUES ($1, $2, 'decision_maker', $3, $4, $5, $6) \
             ON CONFLICT (company_id, email) DO UPDATE SET \
                name = EXCLUDED.name, \
                title = EXCLUDED.title, \
                source = EXCLUDED.source \
             RETURNING id",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(company_id.0)
        .bind(name)
        .bind(title)
        .bind(&canonical_email)
        .bind(source)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        let contact_id: uuid::Uuid = row.try_get("id").map_err(|e| Error::Db(e.to_string()))?;
        sqlx::query("UPDATE prospects SET primary_contact_id = $2 WHERE id = $1")
            .bind(prospect_id.0)
            .bind(contact_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        tx.commit().await.map_err(|e| Error::Db(e.to_string()))?;
        Ok(contact_id)
    }

    /// Look up the prospect-row id for a (campaign, company) pair.
    /// Returns None when the pair has no prospect yet (not seeded).
    pub async fn find_prospect_by_company_in_campaign(
        &self,
        campaign_id: CampaignId,
        company_id: CompanyId,
    ) -> Result<Option<ProspectId>> {
        let row = sqlx::query(
            "SELECT id FROM prospects WHERE campaign_id = $1 AND company_id = $2 LIMIT 1",
        )
        .bind(campaign_id.0)
        .bind(company_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.map(|r| ProspectId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()))))
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
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
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
            "SELECT p.id AS prospect_id, p.tags AS tags, c.id AS company_id,
                    c.display_name, c.homepage, c.industry,
                    c.description, c.region, c.tech_signals
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
            let prospect_id = ProspectId(
                r.try_get("prospect_id")
                    .unwrap_or_else(|_| uuid::Uuid::nil()),
            );
            let company_id = CompanyId(
                r.try_get("company_id")
                    .unwrap_or_else(|_| uuid::Uuid::nil()),
            );
            let display_name: String = r.try_get("display_name").unwrap_or_default();
            let homepage: Option<String> = r.try_get("homepage").unwrap_or(None);
            let industry: Option<String> = r.try_get("industry").unwrap_or(None);
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let region: Option<String> = r.try_get("region").unwrap_or(None);
            let tech_signals: serde_json::Value = r
                .try_get("tech_signals")
                .unwrap_or(serde_json::Value::Array(vec![]));
            let tags: serde_json::Value = r
                .try_get("tags")
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            out.push(ProspectWithFacts {
                prospect_id,
                company_id,
                display_name,
                homepage,
                industry,
                description,
                region,
                tech_signals,
                tags,
            });
        }
        Ok(out)
    }

    /// Append `interest` to `prospects.tags['interests']` (deduped,
    /// case-insensitive on the trimmed value). Used by the operator
    /// `salesman tag` command today; will also be the merge target
    /// for U52's LLM interest extractor. Returns true when a row
    /// was actually updated (i.e. the interest wasn't already there).
    pub async fn add_prospect_interest(
        &self,
        prospect_id: ProspectId,
        interest: &str,
    ) -> Result<bool> {
        let cleaned = interest.trim();
        if cleaned.is_empty() {
            return Ok(false);
        }
        let row = sqlx::query(
            "UPDATE prospects \
             SET tags = jsonb_set( \
                 COALESCE(tags, '{}'::jsonb), \
                 '{interests}', \
                 ( \
                   SELECT to_jsonb(array_agg(DISTINCT i)) \
                   FROM unnest( \
                     COALESCE( \
                       ARRAY( \
                         SELECT jsonb_array_elements_text( \
                           COALESCE(tags->'interests', '[]'::jsonb) \
                         ) \
                       ), \
                       ARRAY[]::TEXT[] \
                     ) || ARRAY[$2::TEXT] \
                   ) AS i \
                 ), \
                 true \
             ) \
             WHERE id = $1 \
               AND ( \
                 NOT (tags ? 'interests') \
                 OR NOT (tags->'interests' @> to_jsonb($2::TEXT)) \
               ) \
             RETURNING id",
        )
        .bind(prospect_id.0)
        .bind(cleaned)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.is_some())
    }

    /// Append a free-text `note` to `prospects.tags['notes']`
    /// (deduped on trimmed value). Symmetric to add_prospect_interest
    /// but for unstructured operator context (e.g. "introduced by
    /// Mike Chen", "no decision until Q3"). Drafter sees them via
    /// to_prompt_json so every subsequent touch can reference them.
    pub async fn add_prospect_note(&self, prospect_id: ProspectId, note: &str) -> Result<bool> {
        let cleaned = note.trim();
        if cleaned.is_empty() {
            return Ok(false);
        }
        let row = sqlx::query(
            "UPDATE prospects \
             SET tags = jsonb_set( \
                 COALESCE(tags, '{}'::jsonb), \
                 '{notes}', \
                 ( \
                   SELECT to_jsonb(array_agg(DISTINCT i)) \
                   FROM unnest( \
                     COALESCE( \
                       ARRAY( \
                         SELECT jsonb_array_elements_text( \
                           COALESCE(tags->'notes', '[]'::jsonb) \
                         ) \
                       ), \
                       ARRAY[]::TEXT[] \
                     ) || ARRAY[$2::TEXT] \
                   ) AS i \
                 ), \
                 true \
             ) \
             WHERE id = $1 \
               AND ( \
                 NOT (tags ? 'notes') \
                 OR NOT (tags->'notes' @> to_jsonb($2::TEXT)) \
               ) \
             RETURNING id",
        )
        .bind(prospect_id.0)
        .bind(cleaned)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.is_some())
    }

    /// Read the per-prospect tags JSONB. Returns `{}` when the
    /// prospect doesn't exist or has no tags set yet.
    pub async fn get_prospect_tags(&self, prospect_id: ProspectId) -> Result<serde_json::Value> {
        let row = sqlx::query("SELECT tags FROM prospects WHERE id = $1")
            .bind(prospect_id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row
            .and_then(|r| r.try_get::<serde_json::Value, _>("tags").ok())
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())))
    }

    /// Single-prospect facts lookup. Used by the trigger-anchored
    /// draft path (one trigger event → one drafted touch) so we
    /// don't have to fetch the whole campaign.
    pub async fn get_prospect_with_facts(
        &self,
        prospect_id: ProspectId,
    ) -> Result<Option<ProspectWithFacts>> {
        let row = sqlx::query(
            "SELECT p.id AS prospect_id, p.tags AS tags, c.id AS company_id,
                    c.display_name, c.homepage, c.industry,
                    c.description, c.region, c.tech_signals
             FROM prospects p
             JOIN companies c ON c.id = p.company_id
             WHERE p.id = $1",
        )
        .bind(prospect_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.map(|r| ProspectWithFacts {
            prospect_id: ProspectId(
                r.try_get("prospect_id")
                    .unwrap_or_else(|_| uuid::Uuid::nil()),
            ),
            company_id: CompanyId(
                r.try_get("company_id")
                    .unwrap_or_else(|_| uuid::Uuid::nil()),
            ),
            display_name: r.try_get("display_name").unwrap_or_default(),
            homepage: r.try_get::<Option<String>, _>("homepage").unwrap_or(None),
            industry: r.try_get::<Option<String>, _>("industry").unwrap_or(None),
            description: r
                .try_get::<Option<String>, _>("description")
                .unwrap_or(None),
            region: r.try_get::<Option<String>, _>("region").unwrap_or(None),
            tech_signals: r
                .try_get::<serde_json::Value, _>("tech_signals")
                .unwrap_or(serde_json::Value::Array(vec![])),
            tags: r
                .try_get::<serde_json::Value, _>("tags")
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        }))
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
        self.insert_touch_draft_full(prospect_id, channel, subject, body, template_key, None)
            .await
    }

    /// Full-form draft insert. `produced_by` is a JSON object recording
    /// which LLM backend + model produced this draft (per the
    /// MODEL_RESILIENCE.md contract). Pass None for non-LLM drafts
    /// (templates / manual inserts) — the column stays NULL and the
    /// audit query treats it as "(unknown provenance)".
    pub async fn insert_touch_draft_full(
        &self,
        prospect_id: ProspectId,
        channel: salesman_core::TouchChannel,
        subject: Option<&str>,
        body: &str,
        template_key: Option<&str>,
        produced_by: Option<serde_json::Value>,
    ) -> Result<salesman_core::TouchId> {
        let id = salesman_core::TouchId::new();
        sqlx::query(
            "INSERT INTO touches
             (id, prospect_id, channel, subject, body, queued_at, sent_at, outcome, receipt_id, template_key, produced_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
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
        .bind(produced_by)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(id)
    }

    /// Persist an owner audit-notification for one outbound contact.
    /// The row is queued undelivered (`delivered_at IS NULL`) — actual
    /// delivery to the operator mailbox is gated behind send approval.
    /// Returns the new row id.
    pub async fn insert_owner_notification(
        &self,
        n: &OwnerNotificationInsert<'_>,
    ) -> Result<uuid::Uuid> {
        let id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO owner_notifications
             (id, touch_id, prospect_id, prospect_label, to_address, channel,
              sent_at, subject, body, receipt_id, campaign)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(id)
        .bind(n.touch_id.map(|t| t.0))
        .bind(n.prospect_id.0)
        .bind(n.prospect_label)
        .bind(n.to_address)
        .bind(n.channel)
        .bind(n.sent_at)
        .bind(n.subject)
        .bind(n.body)
        .bind(n.receipt_id.map(|r| r.0))
        .bind(n.campaign)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(id)
    }

    /// The pending (undelivered) owner-notification queue, oldest first,
    /// capped at `limit`.
    pub async fn list_pending_owner_notifications(
        &self,
        limit: i64,
    ) -> Result<Vec<OwnerNotificationRow>> {
        let rows = sqlx::query(
            "SELECT id, prospect_id, prospect_label, to_address, channel, sent_at,
                    subject, body, receipt_id, campaign, queued_at, delivered_at
             FROM owner_notifications
             WHERE delivered_at IS NULL
             ORDER BY queued_at ASC
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| OwnerNotificationRow {
                id: r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                prospect_label: r.try_get("prospect_label").unwrap_or_default(),
                to_address: r.try_get("to_address").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
                sent_at: r.try_get("sent_at").unwrap_or_else(|_| Utc::now()),
                subject: r.try_get("subject").ok(),
                body: r.try_get("body").unwrap_or_default(),
                receipt_id: r.try_get("receipt_id").ok(),
                campaign: r.try_get("campaign").ok(),
                queued_at: r.try_get("queued_at").unwrap_or_else(|_| Utc::now()),
                delivered_at: r.try_get("delivered_at").ok(),
            })
            .collect())
    }

    /// Mark an owner-notification delivered (operator mailbox received
    /// it). Returns the number of rows updated (0 if the id is unknown
    /// or it was already delivered).
    pub async fn mark_owner_notification_delivered(
        &self,
        id: uuid::Uuid,
        delivered_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE owner_notifications
             SET delivered_at = $2
             WHERE id = $1 AND delivered_at IS NULL",
        )
        .bind(id)
        .bind(delivered_at)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(res.rows_affected())
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

    /// All known contacts at a company. Used by account-based
    /// fanout to surface OTHER stakeholders the operator can pursue
    /// when a prospect engages. Returns (id, name, title, email, source).
    pub async fn list_contacts_for_company(
        &self,
        company_id: CompanyId,
    ) -> Result<
        Vec<(
            uuid::Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )>,
    > {
        let rows = sqlx::query(
            "SELECT id, name, title, email, source \
             FROM contacts \
             WHERE company_id = $1 \
             ORDER BY discovered_at DESC",
        )
        .bind(company_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.try_get::<uuid::Uuid, _>("id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                    r.try_get::<Option<String>, _>("name").unwrap_or(None),
                    r.try_get::<Option<String>, _>("title").unwrap_or(None),
                    r.try_get::<Option<String>, _>("email").unwrap_or(None),
                    r.try_get::<String, _>("source").unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Look up a prospect's company_id + campaign_id + display name.
    /// Used by account-based fanout to find peers at the same company.
    pub async fn prospect_company_and_campaign(
        &self,
        prospect_id: ProspectId,
    ) -> Result<Option<(CompanyId, CampaignId, String)>> {
        let row = sqlx::query(
            "SELECT p.company_id, p.campaign_id, c.display_name \
             FROM prospects p JOIN companies c ON c.id = p.company_id \
             WHERE p.id = $1",
        )
        .bind(prospect_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.map(|r| {
            (
                CompanyId(
                    r.try_get("company_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                CampaignId(
                    r.try_get("campaign_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                r.try_get("display_name").unwrap_or_default(),
            )
        }))
    }

    /// Reply-rate broken down by (day-of-week, hour-of-day) of SEND
    /// time, in the operator's chosen timezone offset (minutes).
    /// Filters to buckets with `sent >= min_sent` so noise is
    /// suppressed. Returns rows sorted by engaged_rate desc.
    pub async fn reply_rate_by_send_window(
        &self,
        tz_offset_minutes: i32,
        min_sent: i64,
    ) -> Result<Vec<(i32, i32, i64, i64, i64)>> {
        // Returns (dow 0=Sunday, hour 0-23, sent, replied, engaged).
        let rows = sqlx::query(
            "SELECT \
               EXTRACT(DOW FROM (t.sent_at + ($1 || ' minutes')::INTERVAL))::INT AS dow, \
               EXTRACT(HOUR FROM (t.sent_at + ($1 || ' minutes')::INTERVAL))::INT AS hour, \
               COUNT(*)::BIGINT AS sent, \
               COUNT(DISTINCT r.id) FILTER (WHERE r.id IS NOT NULL)::BIGINT AS replied, \
               COUNT(DISTINCT r.id) FILTER (WHERE r.kind = 'engaged')::BIGINT AS engaged \
             FROM touches t \
             LEFT JOIN replies r ON r.touch_id = t.id \
             WHERE t.sent_at IS NOT NULL \
               AND t.outcome = 'sent' \
             GROUP BY dow, hour \
             HAVING COUNT(*) >= $2 \
             ORDER BY \
               (COUNT(DISTINCT r.id) FILTER (WHERE r.kind = 'engaged')::FLOAT \
                  / NULLIF(COUNT(*)::FLOAT, 0)) DESC NULLS LAST, \
               COUNT(*) DESC",
        )
        .bind(tz_offset_minutes.to_string())
        .bind(min_sent)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.try_get::<i32, _>("dow").unwrap_or(0),
                    r.try_get::<i32, _>("hour").unwrap_or(0),
                    r.try_get::<i64, _>("sent").unwrap_or(0),
                    r.try_get::<i64, _>("replied").unwrap_or(0),
                    r.try_get::<i64, _>("engaged").unwrap_or(0),
                )
            })
            .collect())
    }

    /// Same shape as template_stats, but joined with companies so
    /// each row is (template_key, segment) where `segment` is the
    /// company's industry. Lets the operator answer "which template
    /// wins for security CISOs vs devops engineers."
    pub async fn template_stats_by_segment(&self) -> Result<Vec<(String, String, TemplateStat)>> {
        let rows = sqlx::query(
            "SELECT
               t.template_key,
               COALESCE(c.industry, '(unknown)') AS segment,
               COUNT(*) FILTER (WHERE t.outcome != 'rejected')::BIGINT AS drafted,
               COUNT(*) FILTER (WHERE t.outcome = 'sent')::BIGINT     AS sent,
               COUNT(DISTINCT r.id) FILTER (WHERE r.id IS NOT NULL)::BIGINT AS replied,
               COUNT(DISTINCT r.id) FILTER (WHERE r.kind = 'engaged')::BIGINT AS engaged_replied
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             JOIN companies c ON c.id = p.company_id
             LEFT JOIN replies r ON r.touch_id = t.id
             WHERE t.template_key IS NOT NULL
             GROUP BY t.template_key, COALESCE(c.industry, '(unknown)')
             ORDER BY drafted DESC",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let template_key: String = r.try_get("template_key").unwrap_or_default();
                let segment: String = r.try_get("segment").unwrap_or_default();
                let stat = TemplateStat {
                    template_key: template_key.clone(),
                    drafted: r.try_get("drafted").unwrap_or(0),
                    sent: r.try_get("sent").unwrap_or(0),
                    replied: r.try_get("replied").unwrap_or(0),
                    engaged_replied: r.try_get("engaged_replied").unwrap_or(0),
                };
                (template_key, segment, stat)
            })
            .collect())
    }

    /// Per-template funnel counts (drafted / sent / replied / engaged),
    /// for A/B comparison of cold-email templates. See [`TemplateStat`].
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
                    t.produced_by, c.display_name AS company
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
                touch_id: salesman_core::TouchId(
                    r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                company: r.try_get("company").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                queued_at: r
                    .try_get("queued_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
                produced_by: r.try_get("produced_by").ok().flatten(),
            });
        }
        Ok(out)
    }

    /// Like [`Self::list_drafts_awaiting_approval`] but across ALL
    /// campaigns. Powers the API `/drafts` operator view. Read-only.
    pub async fn list_all_drafts_awaiting_approval(&self) -> Result<Vec<TouchSummary>> {
        let rows = sqlx::query(
            "SELECT t.id, t.prospect_id, t.subject, t.body, t.channel, t.queued_at,
                    t.produced_by, c.display_name AS company
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             JOIN companies c ON c.id = p.company_id
             WHERE t.outcome = 'awaiting_approval'
             ORDER BY t.queued_at",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(TouchSummary {
                touch_id: salesman_core::TouchId(
                    r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                company: r.try_get("company").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                queued_at: r
                    .try_get("queued_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
                produced_by: r.try_get("produced_by").ok().flatten(),
            });
        }
        Ok(out)
    }

    /// List all campaigns, newest first. Powers the API `/campaigns`
    /// operator view. Read-only.
    pub async fn list_campaigns(&self) -> Result<Vec<Campaign>> {
        let rows = sqlx::query(
            "SELECT id, name, goal, target_segment, status, created_at,
                    paused_at, paused_reason
             FROM campaigns
             ORDER BY created_at DESC",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            // status is stored as a snake_case string; fall back to Draft
            // if it is somehow unparseable rather than failing the whole list.
            let status = r
                .try_get::<String, _>("status")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(CampaignStatus::Draft);
            out.push(Campaign {
                id: CampaignId(r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil())),
                name: r.try_get("name").unwrap_or_default(),
                goal: r.try_get("goal").unwrap_or_default(),
                target_segment: r.try_get("target_segment").unwrap_or_default(),
                status,
                created_at: r.try_get("created_at").unwrap_or_else(|_| Utc::now()),
                paused_at: r.try_get("paused_at").unwrap_or(None),
                paused_reason: r.try_get("paused_reason").unwrap_or(None),
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

    /// Queue an owner audit-notification for a sent touch (call right
    /// after [`Self::mark_touch_sent`]). Populates the row by joining the
    /// touch to its prospect, company, contact, and campaign: the label
    /// prefers the contact name, falling back to the company name; the
    /// recipient address is the prospect's primary contact email (or `""`).
    /// Returns the new row id, or `None` if `touch_id` is unknown.
    ///
    /// This only WRITES the pending audit row — actual delivery of the
    /// notification email to the operator is gated behind the send path.
    pub async fn enqueue_owner_notification_for_touch(
        &self,
        touch_id: TouchId,
    ) -> Result<Option<uuid::Uuid>> {
        let id = uuid::Uuid::now_v7();
        let res = sqlx::query(
            "INSERT INTO owner_notifications
               (id, touch_id, prospect_id, prospect_label, to_address, channel,
                sent_at, subject, body, receipt_id, campaign)
             SELECT $1, t.id, t.prospect_id,
                    COALESCE(ct.name, c.display_name),
                    COALESCE(ct.email, ''),
                    t.channel,
                    COALESCE(t.sent_at, NOW()),
                    t.subject, t.body, t.receipt_id,
                    cmp.name
             FROM touches t
             JOIN prospects p   ON p.id = t.prospect_id
             JOIN companies c   ON c.id = p.company_id
             JOIN campaigns cmp ON cmp.id = p.campaign_id
             LEFT JOIN contacts ct ON ct.id = p.primary_contact_id
             WHERE t.id = $2",
        )
        .bind(id)
        .bind(touch_id.0)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok((res.rows_affected() > 0).then_some(id))
    }

    /// List touches in `approved` state for a campaign — these are
    /// the work queue for `send-pending`.
    pub async fn list_approved_touches(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Vec<TouchSummary>> {
        let rows = sqlx::query(
            "SELECT t.id, t.prospect_id, t.subject, t.body, t.channel, t.queued_at,
                    t.produced_by, c.display_name AS company
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
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                company: r.try_get("company").unwrap_or_default(),
                channel: r.try_get("channel").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                queued_at: r
                    .try_get("queued_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
                produced_by: r.try_get("produced_by").ok().flatten(),
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

    /// Look up the prospect facts (company display, industry,
    /// description, tech_signals) the drafter would have seen for a
    /// given touch. Used by the fact-trace detector gate to verify
    /// that numeric claims in the draft are anchored in real input
    /// data, not hallucinated. Returns a JSON object suitable for
    /// passing to `salesman_detector::score_with_facts`.
    pub async fn touch_facts(&self, touch_id: TouchId) -> Result<Option<serde_json::Value>> {
        let row = sqlx::query(
            "SELECT c.display_name, c.industry, c.description, c.tech_signals
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             JOIN companies c ON c.id = p.company_id
             WHERE t.id = $1",
        )
        .bind(touch_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.map(|r| {
            serde_json::json!({
                "company": r.try_get::<String, _>("display_name").unwrap_or_default(),
                "industry": r.try_get::<Option<String>, _>("industry").unwrap_or(None),
                "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
                "tech_signals": r
                    .try_get::<serde_json::Value, _>("tech_signals")
                    .unwrap_or(serde_json::Value::Null),
            })
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
    ///
    /// SECURITY: matches against ANY canonical form of the
    /// from-address (verbatim / lowercased / +-stripped /
    /// Gmail-dot-stripped). Without this an inbound from
    /// `John+Sales@Gmail.com` would be silently dropped when the
    /// prospect was originally seeded as `john@gmail.com` — the
    /// worst-case false negative because we'd miss opt-outs and
    /// positive replies. Same canonicalization rule everywhere a
    /// recipient address is compared.
    pub async fn insert_reply_threaded(
        &self,
        from_address: &str,
        subject: Option<&str>,
        body: &str,
        raw_headers: &serde_json::Value,
    ) -> Result<Option<uuid::Uuid>> {
        let candidates = salesman_core::email_match_candidates(from_address);
        if candidates.is_empty() {
            tracing::warn!(%from_address, "empty from-address on inbound reply — dropping");
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT p.id AS prospect_id
             FROM prospects p
             JOIN contacts c ON c.id = p.primary_contact_id
             WHERE c.email = ANY($1)
             ORDER BY p.state_changed_at DESC
             LIMIT 1",
        )
        .bind(&candidates)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;

        let Some(row) = row else {
            tracing::warn!(%from_address, "no prospect matches reply from-address — dropping");
            return Ok(None);
        };
        let prospect_id_uuid: uuid::Uuid = row
            .try_get("prospect_id")
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

    /// List classified replies that need a response — engaged /
    /// question / objection — and don't yet have a response_touch_id
    /// linked. The drafter consumes this and produces approval-queue
    /// touches.
    pub async fn list_replies_needing_response(
        &self,
        limit: i64,
    ) -> Result<Vec<ReplyNeedingResponse>> {
        let rows = sqlx::query(
            "SELECT
                r.id            AS reply_id,
                r.prospect_id   AS prospect_id,
                r.from_address  AS from_address,
                r.subject       AS reply_subject,
                r.body          AS reply_body,
                r.kind          AS reply_kind,
                t.subject       AS outbound_subject,
                t.body          AS outbound_body,
                c.display_name  AS company_name,
                c.industry      AS industry,
                c.description   AS description
             FROM replies r
             JOIN prospects p ON p.id = r.prospect_id
             JOIN companies c ON c.id = p.company_id
             LEFT JOIN touches t ON t.id = r.touch_id
             WHERE r.kind IN ('engaged','question','objection')
               AND r.response_touch_id IS NULL
             ORDER BY r.received_at
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| ReplyNeedingResponse {
                reply_id: r.try_get("reply_id").unwrap_or_else(|_| uuid::Uuid::nil()),
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                from_address: r.try_get("from_address").unwrap_or_default(),
                inbound_subject: r.try_get("reply_subject").unwrap_or(None),
                inbound_body: r.try_get("reply_body").unwrap_or_default(),
                inbound_kind: r.try_get("reply_kind").unwrap_or_default(),
                outbound_subject: r.try_get("outbound_subject").unwrap_or(None),
                outbound_body: r.try_get("outbound_body").unwrap_or(None),
                company_name: r.try_get("company_name").unwrap_or_default(),
                industry: r.try_get("industry").unwrap_or(None),
                description: r.try_get("description").unwrap_or(None),
            })
            .collect())
    }

    /// Link a freshly-drafted response touch to the reply it answers.
    /// Idempotent — re-running with the same (reply, touch) is a
    /// no-op via the WHERE response_touch_id IS NULL guard.
    pub async fn link_reply_response(
        &self,
        reply_id: uuid::Uuid,
        response_touch_id: salesman_core::TouchId,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE replies
             SET response_touch_id = $2
             WHERE id = $1 AND response_touch_id IS NULL",
        )
        .bind(reply_id)
        .bind(response_touch_id.0)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(r.rows_affected())
    }

    /// List replies in `unclassified` state (queue for the classifier).
    pub async fn list_unclassified_replies(&self, limit: i64) -> Result<Vec<UnclassifiedReply>> {
        let rows = sqlx::query(
            "SELECT r.id, r.prospect_id, r.from_address, r.subject, r.body, \
                    r.raw_headers, p.campaign_id
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
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                campaign_id: CampaignId(
                    r.try_get("campaign_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                from_address: r.try_get("from_address").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                raw_headers: r.try_get("raw_headers").unwrap_or(serde_json::Value::Null),
            });
        }
        Ok(out)
    }

    /// Chronological conversation thread for a prospect: outbound
    /// touches we sent + inbound replies they sent, oldest first.
    /// Used by the reply-drafter to anchor responses in the prior
    /// back-and-forth instead of treating every reply as if it's
    /// the first turn. `limit` caps the total turns (touches +
    /// replies combined) to bound prompt size — keep around 6.
    pub async fn list_thread_for_prospect(
        &self,
        prospect_id: ProspectId,
        limit: i64,
    ) -> Result<Vec<ThreadTurn>> {
        let rows = sqlx::query(
            "(SELECT 'outbound' AS role, t.queued_at AS at, \
                     t.subject AS subject, t.body AS body, \
                     NULL::TEXT AS reply_kind \
              FROM touches t \
              WHERE t.prospect_id = $1 \
                AND t.outcome IN ('sent', 'approved')) \
             UNION ALL \
             (SELECT 'reply' AS role, r.received_at AS at, \
                     r.subject AS subject, r.body AS body, \
                     r.kind AS reply_kind \
              FROM replies r \
              WHERE r.prospect_id = $1) \
             ORDER BY at \
             LIMIT $2",
        )
        .bind(prospect_id.0)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| ThreadTurn {
                at: r.try_get("at").unwrap_or_else(|_| chrono::Utc::now()),
                role: r.try_get("role").unwrap_or_default(),
                subject: r.try_get("subject").unwrap_or(None),
                body: r.try_get("body").unwrap_or_default(),
                reply_kind: r.try_get("reply_kind").unwrap_or(None),
            })
            .collect())
    }

    /// Overwrite a reply's classified [`ReplyKind`] (e.g. after re-running
    /// the classifier). Does not itself advance the prospect's funnel — see
    /// `apply_reply_to_prospect` for that.
    pub async fn update_reply_kind(&self, reply_id: uuid::Uuid, kind: ReplyKind) -> Result<()> {
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
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        let mut summary = String::new();

        // Always update reply.kind first.
        sqlx::query("UPDATE replies SET kind = $2 WHERE id = $1")
            .bind(reply_id)
            .bind(kind.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;

        // Map ReplyKind → FunnelState transition. The policy lives in
        // salesman-core (ReplyKind::funnel_state_label) so it is unit-tested
        // independently of the DB — see salesman-core reply_kind_props.rs.
        let new_state: Option<&str> = kind.funnel_state_label();
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

        // Optout / LegalThreat: add to suppressions + reject any
        // in-flight touches. LegalThreat carries a distinct source
        // tag so audit + alerts can distinguish a benign opt-out
        // from a legally-charged inbound that needs operator
        // attention RIGHT NOW.
        if kind.is_suppression_trigger() {
            let (reason_text, source_tag) = match kind {
                ReplyKind::LegalThreat => (
                    "reply contained legal threat (cease-and-desist / attorney / regulator)",
                    "reply_legal_threat",
                ),
                _ => ("reply optout", "reply_optout"),
            };
            // SECURITY: store the canonical form so future
            // is_suppressed lookups against `john@gmail.com` also
            // match an opt-out logged from `John+Sales@Gmail.com`.
            // Same canonicalization rule as the public
            // add_suppression — see salesman-core::email_match.
            let canonical_from = salesman_core::normalize_email_for_match(from_address);
            sqlx::query(
                "INSERT INTO suppressions (id, target, target_kind, reason, source) \
                 VALUES ($1, $2, 'email', $3, $4) \
                 ON CONFLICT (target) DO NOTHING",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(&canonical_from)
            .bind(reason_text)
            .bind(source_tag)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            summary.push_str(&format!("added to suppressions ({source_tag}); "));

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

        // Adaptive cadence (U32): when a prospect REPLIES — engaged,
        // question, objection, OOO — pause their static sequence so
        // we don't fire a tone-deaf follow-up while the reply-drafter
        // is composing the actual response. The operator resumes the
        // sequence manually with `salesman cadence resume` if they
        // decide the prospect needs to keep getting the canned cadence.
        // Optout / Bounce / Spam are already terminal upstream;
        // pausing the sequence is cheap insurance.
        let pause_reason: Option<&str> = match kind {
            ReplyKind::Engaged => Some("auto: reply classified engaged"),
            ReplyKind::Question => Some("auto: reply classified question"),
            ReplyKind::Objection => Some("auto: reply classified objection"),
            ReplyKind::OutOfOffice => Some("auto: reply classified out_of_office"),
            ReplyKind::Optout => Some("auto: reply classified optout"),
            ReplyKind::Bounce => Some("auto: reply classified bounce"),
            ReplyKind::LegalThreat => {
                Some("auto: reply classified legal_threat — operator must handle")
            }
            _ => None,
        };
        if let Some(reason) = pause_reason {
            sqlx::query(
                "UPDATE prospect_sequence_state \
                 SET paused = TRUE, paused_reason = $2 \
                 WHERE prospect_id = $1 AND NOT paused",
            )
            .bind(prospect_id.0)
            .bind(reason)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
            summary.push_str("sequence paused; ");
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
            return Err(Error::Validation(
                "sequence must have at least one step".into(),
            ));
        }
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        let sequence_id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO sequences (id, campaign_id, name) VALUES ($1, $2, $3)")
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
    pub async fn advance_prospect_in_sequence(&self, prospect_id: ProspectId) -> Result<bool> {
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

    /// Merge a value into the `tags` JSONB on a reply. Existing keys
    /// are preserved; the new key replaces (or adds). Used by the
    /// competitor-mention detector + future intent / urgency
    /// detectors.
    pub async fn set_reply_tag(
        &self,
        reply_id: uuid::Uuid,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE replies \
             SET tags = COALESCE(tags, '{}'::jsonb) || jsonb_build_object($2::text, $3::jsonb) \
             WHERE id = $1",
        )
        .bind(reply_id)
        .bind(key)
        .bind(value)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(())
    }

    /// Recent replies that mention any competitor — used by
    /// `salesman alerts` to surface "this prospect is comparing us
    /// to X."
    pub async fn list_competitor_mention_replies(
        &self,
        since_hours: i64,
        limit: i64,
    ) -> Result<Vec<(ReplyRow, Vec<String>, ProspectId)>> {
        let rows = sqlx::query(
            "SELECT r.id, r.prospect_id, r.from_address, r.subject, r.body, \
                    r.kind, r.received_at, r.tags->'competitors' AS comps \
             FROM replies r \
             WHERE r.received_at > NOW() - ($1 || ' hours')::INTERVAL \
               AND r.tags ? 'competitors' \
               AND jsonb_array_length(r.tags->'competitors') > 0 \
             ORDER BY r.received_at DESC \
             LIMIT $2",
        )
        .bind(since_hours.to_string())
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let prospect_id = ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                );
                let comps: Vec<String> = r
                    .try_get::<Option<serde_json::Value>, _>("comps")
                    .unwrap_or(None)
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                (
                    ReplyRow {
                        from_address: r.try_get("from_address").unwrap_or_default(),
                        subject: r.try_get("subject").unwrap_or(None),
                        body: r.try_get("body").unwrap_or_default(),
                        kind: r.try_get("kind").unwrap_or_default(),
                        received_at: r
                            .try_get("received_at")
                            .unwrap_or_else(|_| chrono::Utc::now()),
                    },
                    comps,
                    prospect_id,
                )
            })
            .collect())
    }

    /// Prospects in `won` state whose state_changed_at is ≥
    /// `min_days_since_won` ago AND who don't yet have a touch with
    /// `template_key='referral_ask'`. The post-close referral pool.
    /// Returns enough fields to drive the drafter without a second
    /// round-trip.
    pub async fn list_won_prospects_for_referral_ask(
        &self,
        min_days_since_won: i64,
        limit: i64,
    ) -> Result<Vec<ProspectWithFacts>> {
        let rows = sqlx::query(
            "SELECT p.id AS prospect_id, p.tags AS tags, c.id AS company_id, \
                    c.display_name, c.homepage, c.industry, \
                    c.description, c.region, c.tech_signals \
             FROM prospects p \
             JOIN companies c ON c.id = p.company_id \
             WHERE p.state = 'won' \
               AND p.state_changed_at < NOW() - ($1 || ' days')::INTERVAL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM touches t \
                 WHERE t.prospect_id = p.id AND t.template_key = 'referral_ask' \
               ) \
             ORDER BY p.state_changed_at \
             LIMIT $2",
        )
        .bind(min_days_since_won.to_string())
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| ProspectWithFacts {
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                company_id: CompanyId(
                    r.try_get("company_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                display_name: r.try_get("display_name").unwrap_or_default(),
                homepage: r.try_get::<Option<String>, _>("homepage").unwrap_or(None),
                industry: r.try_get::<Option<String>, _>("industry").unwrap_or(None),
                description: r
                    .try_get::<Option<String>, _>("description")
                    .unwrap_or(None),
                region: r.try_get::<Option<String>, _>("region").unwrap_or(None),
                tech_signals: r
                    .try_get::<Option<serde_json::Value>, _>("tech_signals")
                    .unwrap_or(None)
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default(),
                tags: r
                    .try_get::<serde_json::Value, _>("tags")
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            })
            .collect())
    }

    /// Resume a paused prospect-sequence. Idempotent — running on
    /// an already-active prospect is a no-op via the WHERE filter.
    /// Returns rows affected (0 = wasn't paused).
    pub async fn resume_prospect_sequence(&self, prospect_id: ProspectId) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE prospect_sequence_state \
             SET paused = FALSE, paused_reason = NULL \
             WHERE prospect_id = $1 AND paused",
        )
        .bind(prospect_id.0)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(r.rows_affected())
    }

    /// List paused prospect-sequence-states with the company name +
    /// reason. Operator-facing: "who's stalled in their cadence and
    /// why?"
    pub async fn list_paused_prospects(
        &self,
        limit: i64,
    ) -> Result<Vec<(ProspectId, String, String, chrono::DateTime<chrono::Utc>)>> {
        let rows = sqlx::query(
            "SELECT pss.prospect_id, c.display_name AS company, \
                    COALESCE(pss.paused_reason, '(no reason)') AS reason, \
                    pss.last_advanced_at \
             FROM prospect_sequence_state pss \
             JOIN prospects p ON p.id = pss.prospect_id \
             JOIN companies c ON c.id = p.company_id \
             WHERE pss.paused \
             ORDER BY pss.last_advanced_at DESC \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    ProspectId(
                        r.try_get("prospect_id")
                            .unwrap_or_else(|_| uuid::Uuid::nil()),
                    ),
                    r.try_get("company").unwrap_or_default(),
                    r.try_get("reason").unwrap_or_default(),
                    r.try_get("last_advanced_at")
                        .unwrap_or_else(|_| chrono::Utc::now()),
                )
            })
            .collect())
    }

    /// List prospect-sequence-states whose next_due_at has passed and
    /// are not paused — the work queue for the sequence scheduler.
    pub async fn list_due_prospects(&self, limit: i64) -> Result<Vec<DueProspect>> {
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
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                sequence_id: r
                    .try_get("sequence_id")
                    .unwrap_or_else(|_| uuid::Uuid::nil()),
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
    ///
    /// SECURITY: emails are normalized via
    /// `salesman_core::normalize_email_for_match` before insert so
    /// repeat suppressions of the same logical mailbox (e.g.
    /// `John+Sales@Gmail.com` and `john@gmail.com`) collapse onto
    /// the same row. Domains are lowercased.
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
        // Canonicalize so future is_suppressed() lookups against the
        // SAME logical mailbox match this row regardless of casing,
        // plus-suffix, or Gmail dotting in either direction.
        let canonical_target = match target_kind {
            "email" => salesman_core::normalize_email_for_match(target),
            "domain" => target.trim().to_lowercase(),
            _ => unreachable!(),
        };
        let row = sqlx::query(
            "INSERT INTO suppressions (id, target, target_kind, reason, source) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (target) DO NOTHING \
             RETURNING id",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(&canonical_target)
        .bind(target_kind)
        .bind(reason)
        .bind(source)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        // Only fire the cross-repo NOTIFY when a row was actually
        // inserted — re-stating a known suppression should not look
        // like a new event to the CRM listener.
        if row.is_some() {
            notify_event(
                self.pool(),
                "suppression.added",
                serde_json::json!({
                    // Emit the canonical form on the wire — downstream
                    // CRM listeners should dedupe against the same
                    // value future is_suppressed checks will hit.
                    "target": canonical_target,
                    "target_kind": target_kind,
                    "source": source,
                    "reason": reason,
                }),
            )
            .await;
        }
        Ok(())
    }

    /// True if the email matches a `target_kind=email` suppression
    /// in any of its canonical forms (verbatim / lowercased /
    /// plus-stripped / Gmail-dot-stripped) OR if its domain is on
    /// the list.
    ///
    /// Broader than the legacy exact-match — a subsequent send to
    /// `john+sales@gmail.com` is now correctly blocked when an
    /// earlier opt-out was logged for `john@gmail.com` (and vice
    /// versa). See salesman-core/src/email_match.rs for the rules.
    ///
    /// SECURITY: this function is the LAST gate before
    /// `send-pending --for-real` actually transmits. Any new
    /// candidate generator added in salesman-core must continue to
    /// be a SUPERSET of the prior generator's output (false
    /// positives are tolerated; false negatives are not).
    pub async fn is_suppressed(&self, email: &str) -> Result<bool> {
        let domain = email
            .rsplit_once('@')
            .map(|(_, d)| d.trim().to_lowercase())
            .unwrap_or_else(|| email.trim().to_lowercase());
        let candidates = salesman_core::email_match_candidates(email);
        if candidates.is_empty() {
            // Caller passed an empty/whitespace string. Treat as
            // not-suppressed but log so the caller sees this didn't
            // gate a send (it'd be a separate validation bug if a
            // blank email reached the send path).
            tracing::warn!("is_suppressed called with empty email — returning false");
            return Ok(false);
        }
        let row = sqlx::query(
            "SELECT EXISTS (
                SELECT 1 FROM suppressions
                WHERE (target_kind = 'email'  AND target = ANY($1))
                   OR (target_kind = 'domain' AND target = $2)
             ) AS hit",
        )
        .bind(&candidates)
        .bind(&domain)
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
        row.try_get::<i64, _>("n")
            .map_err(|e| Error::Db(e.to_string()))
    }

    /// Page through the suppression list. `source_filter` (when Some)
    /// restricts to that source tag. Newest-first ordering — the
    /// operator typically cares most about what just got added.
    pub async fn list_suppressions(
        &self,
        source_filter: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SuppressionRow>> {
        let rows = match source_filter {
            Some(s) => {
                sqlx::query(
                    "SELECT id, target, target_kind, reason, source, added_at \
                     FROM suppressions \
                     WHERE source = $1 \
                     ORDER BY added_at DESC \
                     LIMIT $2",
                )
                .bind(s)
                .bind(limit)
                .fetch_all(self.pool())
                .await
            }
            None => {
                sqlx::query(
                    "SELECT id, target, target_kind, reason, source, added_at \
                     FROM suppressions \
                     ORDER BY added_at DESC \
                     LIMIT $1",
                )
                .bind(limit)
                .fetch_all(self.pool())
                .await
            }
        }
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| SuppressionRow {
                id: r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                target: r.try_get("target").unwrap_or_default(),
                target_kind: r.try_get("target_kind").unwrap_or_default(),
                reason: r.try_get("reason").unwrap_or_default(),
                source: r.try_get("source").unwrap_or_default(),
                added_at: r.try_get("added_at").unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect())
    }

    /// Counts suppressions grouped by `source`. Returns rows of
    /// (source, n) pairs.
    pub async fn count_suppressions_by_source(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query(
            "SELECT source, COUNT(*)::BIGINT AS n \
             FROM suppressions \
             GROUP BY source \
             ORDER BY n DESC",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.try_get::<String, _>("source").unwrap_or_default(),
                    r.try_get::<i64, _>("n").unwrap_or(0),
                )
            })
            .collect())
    }

    /// Count hard-bounce suppressions for a given domain in the last
    /// `window_hours`. Used by send-pending to soft-quarantine
    /// domains whose recent bounce rate is suspiciously high — an
    /// early signal that the prospect list is junk OR that the
    /// recipient mail provider has put us in tarpit mode.
    pub async fn count_bounces_to_domain_since(
        &self,
        domain: &str,
        window_hours: i64,
    ) -> Result<i64> {
        // Lowercase the domain — callers commonly pass `to
        // .rsplit_once('@').map(...)` which preserves whatever
        // casing the recipient stored. Add_suppression now stores
        // the canonical (lowercased) form on every insert, so the
        // LIKE pattern would miss historical or case-mismatched
        // suppressions if we didn't normalize the lookup side too.
        let needle = domain.trim().to_ascii_lowercase();
        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS n \
             FROM suppressions \
             WHERE source = 'bounce' \
               AND target_kind = 'email' \
               AND target LIKE '%@' || $1 \
               AND added_at > NOW() - ($2 || ' hours')::INTERVAL",
        )
        .bind(&needle)
        .bind(window_hours.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    /// Remove a suppression by target. Idempotent — returns the
    /// number of rows actually deleted (0 or 1). Fires
    /// `suppression.removed` on the salesman_event channel when a
    /// row was actually deleted (so CRM/dashboards see the audit
    /// event).
    pub async fn remove_suppression(&self, target: &str) -> Result<u64> {
        let r = sqlx::query("DELETE FROM suppressions WHERE target = $1")
            .bind(target)
            .execute(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        if r.rows_affected() > 0 {
            notify_event(
                self.pool(),
                "suppression.removed",
                serde_json::json!({ "target": target }),
            )
            .await;
        }
        Ok(r.rows_affected())
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

    /// All receipts oldest-first — required by `verify_chain` because
    /// each prev_hash references the previous record's hash. Use a
    /// large limit (default 100k in CLI) to walk the entire chain.
    pub async fn list_receipts_oldest_first(&self, limit: i64) -> Result<Vec<Receipt>> {
        let rows = sqlx::query(
            "SELECT id, event_kind, event_payload, prev_hash, hash, signature, signing_key_id, created_at \
             FROM receipts \
             ORDER BY created_at ASC, id ASC \
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
                event_payload: r
                    .try_get("event_payload")
                    .unwrap_or(serde_json::Value::Null),
                prev_hash: r.try_get("prev_hash").unwrap_or_default(),
                hash: r.try_get("hash").unwrap_or_default(),
                signature: r.try_get("signature").unwrap_or_default(),
                signing_key_id: r.try_get("signing_key_id").unwrap_or_default(),
                created_at: r
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
            });
        }
        Ok(out)
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
                event_payload: r
                    .try_get("event_payload")
                    .unwrap_or(serde_json::Value::Null),
                prev_hash: r.try_get("prev_hash").unwrap_or_default(),
                hash: r.try_get("hash").unwrap_or_default(),
                signature: r.try_get("signature").unwrap_or_default(),
                signing_key_id: r.try_get("signing_key_id").unwrap_or_default(),
                created_at: r
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
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
        Ok(row.and_then(|r| {
            r.try_get::<Option<i64>, _>("cost_cap_micro_usd")
                .unwrap_or(None)
        }))
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

    /// LLM cost rolled up by `purpose` tag (the chat_for(purpose) the
    /// caller passed). Returned ORDER BY cost DESC so the most
    /// expensive subsystem floats to the top of the operator's
    /// report.
    pub async fn cost_by_purpose(&self, since_hours: i64) -> Result<Vec<PurposeCostRow>> {
        let rows = sqlx::query(
            "SELECT purpose,
                    COUNT(*)::BIGINT          AS n,
                    SUM(prompt_tokens)::BIGINT  AS prompt,
                    SUM(output_tokens)::BIGINT  AS output,
                    SUM(cache_hit_tokens)::BIGINT AS cache_hit,
                    SUM(cost_micro_usd)::BIGINT AS micro_usd,
                    AVG(latency_ms)::BIGINT   AS avg_latency,
                    PERCENTILE_DISC(0.95) WITHIN GROUP (ORDER BY latency_ms)::BIGINT AS p95_latency
             FROM llm_calls
             WHERE created_at > NOW() - ($1 || ' hours')::INTERVAL
             GROUP BY purpose
             ORDER BY micro_usd DESC",
        )
        .bind(since_hours.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| PurposeCostRow {
                purpose: r.try_get("purpose").unwrap_or_default(),
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
    ///
    /// SECURITY: matches against ANY canonical form of the input
    /// (verbatim / lowercased / +-stripped / Gmail-dot-stripped)
    /// via `salesman_core::email_match_candidates`. Prevents the
    /// rate cap from being bypassed by sending to the same logical
    /// mailbox under multiple aliases (e.g. an attacker — or just a
    /// real prospect — using `john@gmail.com` and
    /// `j.o.h.n+work@gmail.com` interchangeably). Same rule the
    /// suppression matcher uses, kept in sync deliberately.
    pub async fn count_touches_to_email_since(
        &self,
        to_email: &str,
        window_hours: i64,
    ) -> Result<i64> {
        let candidates = salesman_core::email_match_candidates(to_email);
        if candidates.is_empty() {
            return Ok(0);
        }
        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS n
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             LEFT JOIN contacts ct ON ct.id = p.primary_contact_id
             WHERE ct.email = ANY($1) AND t.sent_at IS NOT NULL
               AND t.sent_at > NOW() - ($2 || ' hours')::INTERVAL",
        )
        .bind(&candidates)
        .bind(window_hours.to_string())
        .fetch_one(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    /// Count touches sent to any address in `domain` in the last
    /// `window_hours` — per-domain rate cap.
    ///
    /// Same casing-normalization story as
    /// `count_bounces_to_domain_since`: lowercase the needle so the
    /// LIKE match doesn't miss historical or case-mismatched
    /// rows. contacts.email is now stored canonically (lowercased,
    /// plus-stripped, gmail-dot-stripped) per the email-canon
    /// sweep, but legacy rows may have any casing.
    pub async fn count_touches_to_domain_since(
        &self,
        domain: &str,
        window_hours: i64,
    ) -> Result<i64> {
        let needle = domain.trim().to_ascii_lowercase();
        let row = sqlx::query(
            "SELECT COUNT(*)::BIGINT AS n
             FROM touches t
             JOIN prospects p ON p.id = t.prospect_id
             LEFT JOIN contacts ct ON ct.id = p.primary_contact_id
             WHERE ct.email LIKE '%@' || $1 AND t.sent_at IS NOT NULL
               AND t.sent_at > NOW() - ($2 || ' hours')::INTERVAL",
        )
        .bind(&needle)
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

    /// All replies in the last `hours` whose kind is in `kinds`.
    /// Used by `salesman alerts` to triage what just landed.
    /// `kinds` is a small `IN` set (e.g. ["engaged","question"]).
    pub async fn list_replies_since_with_kinds(
        &self,
        hours: i64,
        kinds: &[&str],
    ) -> Result<Vec<ReplyRow>> {
        if kinds.is_empty() {
            return Ok(vec![]);
        }
        // Build a parameterized IN clause: ($2, $3, ...)
        let placeholders: Vec<String> = (0..kinds.len()).map(|i| format!("${}", i + 2)).collect();
        let q = format!(
            "SELECT r.from_address, r.subject, r.body, r.kind, r.received_at
             FROM replies r
             WHERE r.received_at > NOW() - ($1 || ' hours')::INTERVAL
               AND r.kind IN ({})
             ORDER BY r.received_at DESC
             LIMIT 200",
            placeholders.join(",")
        );
        let mut q = sqlx::query(&q).bind(hours.to_string());
        for k in kinds {
            q = q.bind(*k);
        }
        let rows = q
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
                received_at: r
                    .try_get("received_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect())
    }

    /// All suppressions added in the last `hours`. Used by `alerts`
    /// to surface bounce-rate spikes / opt-out clusters.
    pub async fn list_suppressions_since(&self, hours: i64) -> Result<Vec<SuppressionRow>> {
        let rows = sqlx::query(
            "SELECT id, target, target_kind, reason, source, added_at
             FROM suppressions
             WHERE added_at > NOW() - ($1 || ' hours')::INTERVAL
             ORDER BY added_at DESC
             LIMIT 200",
        )
        .bind(hours.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| SuppressionRow {
                id: r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                target: r.try_get("target").unwrap_or_default(),
                target_kind: r.try_get("target_kind").unwrap_or_default(),
                reason: r.try_get("reason").unwrap_or_default(),
                source: r.try_get("source").unwrap_or_default(),
                added_at: r.try_get("added_at").unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect())
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
                received_at: r
                    .try_get("received_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
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

    // -----------------------------------------------------------------
    // trigger events
    // -----------------------------------------------------------------

    /// Insert a trigger event for a prospect. Idempotent on
    /// (prospect, source, url) so re-running the scanner doesn't
    /// produce duplicates. Returns true when a row was actually
    /// inserted, false on conflict.
    pub async fn insert_trigger_event(&self, ev: TriggerEventInsert<'_>) -> Result<bool> {
        let row = sqlx::query(
            "INSERT INTO trigger_events
             (id, prospect_id, source, headline, url, recency_score, relevance_score, raw)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (prospect_id, source, url) DO NOTHING
             RETURNING id",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(ev.prospect_id.0)
        .bind(ev.source)
        .bind(ev.headline)
        .bind(ev.url)
        .bind(ev.recency_score)
        .bind(ev.relevance_score)
        .bind(ev.raw)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(row.is_some())
    }

    /// Top trigger events across (optionally) one campaign in the
    /// last `since_hours`. Sorted by recency × relevance descending.
    /// `unused_only` filters to triggers we haven't already used to
    /// anchor a touch — that's the operator's "what should I send
    /// today" view.
    pub async fn list_trigger_events(
        &self,
        campaign_id: Option<CampaignId>,
        since_hours: i64,
        unused_only: bool,
        limit: i64,
    ) -> Result<Vec<TriggerEventRow>> {
        let mut sql = String::from(
            "SELECT te.id, te.prospect_id, te.source, te.headline, te.url, \
                    te.recency_score, te.relevance_score, te.created_at, \
                    c.display_name AS company_name \
             FROM trigger_events te \
             JOIN prospects p ON p.id = te.prospect_id \
             JOIN companies c ON c.id = p.company_id \
             WHERE te.created_at > NOW() - ($1 || ' hours')::INTERVAL ",
        );
        if unused_only {
            sql.push_str("AND te.used_in_touch IS NULL ");
        }
        if campaign_id.is_some() {
            sql.push_str("AND p.campaign_id = $2 ");
        }
        sql.push_str(
            "ORDER BY (te.recency_score * te.relevance_score) DESC, te.created_at DESC \
             LIMIT ",
        );
        let limit_pos = if campaign_id.is_some() { "$3" } else { "$2" };
        sql.push_str(limit_pos);

        let mut q = sqlx::query(&sql).bind(since_hours.to_string());
        if let Some(cid) = campaign_id {
            q = q.bind(cid.0);
        }
        q = q.bind(limit);
        let rows = q
            .fetch_all(self.pool())
            .await
            .map_err(|e| Error::Db(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| TriggerEventRow {
                id: r.try_get("id").unwrap_or_else(|_| uuid::Uuid::nil()),
                prospect_id: ProspectId(
                    r.try_get("prospect_id")
                        .unwrap_or_else(|_| uuid::Uuid::nil()),
                ),
                company: r.try_get("company_name").unwrap_or_default(),
                source: r.try_get("source").unwrap_or_default(),
                headline: r.try_get("headline").unwrap_or_default(),
                url: r.try_get("url").unwrap_or(None),
                recency_score: r.try_get("recency_score").unwrap_or(0.0),
                relevance_score: r.try_get("relevance_score").unwrap_or(0.0),
                created_at: r
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect())
    }

    /// Mark a trigger event as having been used to anchor a touch.
    /// Idempotent.
    pub async fn mark_trigger_used(
        &self,
        trigger_id: uuid::Uuid,
        touch_id: salesman_core::TouchId,
    ) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE trigger_events SET used_in_touch = $2 \
             WHERE id = $1 AND used_in_touch IS NULL",
        )
        .bind(trigger_id)
        .bind(touch_id.0)
        .execute(self.pool())
        .await
        .map_err(|e| Error::Db(e.to_string()))?;
        Ok(r.rows_affected())
    }
}
