//! Domain model. These types are the persistent state of the system.

use crate::ids::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use strum::{Display, EnumString};
use url::Url;

/// A company we've discovered. May or may not be in any campaign yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Company {
    pub id: CompanyId,
    pub legal_name: Option<String>,
    pub display_name: String,
    pub homepage: Option<Url>,
    pub industry: Option<String>,
    pub size_band: Option<SizeBand>,
    pub region: Option<String>,
    pub description: Option<String>,
    pub tech_signals: Vec<TechSignal>,
    pub discovered_at: DateTime<Utc>,
    pub last_enriched_at: Option<DateTime<Utc>>,
    pub source: DiscoverySource,
    pub raw: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SizeBand {
    Solo,
    Small,
    Mid,
    Large,
    Enterprise,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechSignal {
    pub kind: String,
    pub value: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    Search,
    Crawler,
    OwnerSeed,
    Linkedin,
    Other,
}

/// A person at a company. Role addresses (sales@, info@) are
/// represented as `Contact` with `kind = Role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: ContactId,
    pub company_id: CompanyId,
    pub kind: ContactKind,
    pub name: Option<String>,
    pub title: Option<String>,
    pub email: Option<String>,
    pub email_verified: bool,
    pub linkedin_url: Option<Url>,
    pub source: DiscoverySource,
    pub discovered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ContactKind {
    /// `info@`, `sales@`, `hello@` — generic mailbox.
    Role,
    /// Named individual.
    Person,
}

/// A reusable, named outreach effort with a goal and templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Campaign {
    pub id: CampaignId,
    pub name: String,
    pub goal: String,
    pub target_segment: String,
    pub status: CampaignStatus,
    pub created_at: DateTime<Utc>,
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Draft,
    Active,
    Paused,
    Completed,
    Killed,
}

/// A specific (campaign, company) pair — the trackable unit of the
/// pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prospect {
    pub id: ProspectId,
    pub campaign_id: CampaignId,
    pub company_id: CompanyId,
    pub primary_contact_id: Option<ContactId>,
    pub state: FunnelState,
    pub state_reason: Option<String>,
    pub state_changed_at: DateTime<Utc>,
    pub fit_score: Option<f32>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FunnelState {
    New,
    Qualified,
    Contacted,
    Engaged,
    Meeting,
    Proposal,
    Won,
    Lost,
    Suppressed,
}

impl FunnelState {
    /// Is the transition `self → to` allowed?
    ///
    /// Forward progression is generally allowed up to Won. Lost +
    /// Suppressed are terminal — once there, no transition (the
    /// system does not "un-suppress" prospects automatically).
    /// Backward transitions are generally disallowed except for
    /// Engaged → Contacted (a re-touch can downgrade if re-classified).
    pub fn can_transition_to(self, to: Self) -> bool {
        use FunnelState::*;
        if self == to {
            return true;
        }
        // Terminals
        if matches!(self, Won | Lost | Suppressed) {
            return false;
        }
        // Suppressed is reachable from any non-terminal state (opt-out
        // can land at any time).
        if to == Suppressed {
            return true;
        }
        // Lost is reachable from any non-terminal (bounce, owner mark).
        if to == Lost {
            return true;
        }
        // Forward chain.
        let order = [New, Qualified, Contacted, Engaged, Meeting, Proposal, Won];
        let pos = |s: Self| order.iter().position(|x| *x == s);
        match (pos(self), pos(to)) {
            (Some(i), Some(j)) => j >= i, // forward only on the chain
            _ => false,
        }
    }
}

/// A single outbound action taken on a prospect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Touch {
    pub id: TouchId,
    pub prospect_id: ProspectId,
    pub channel: TouchChannel,
    pub subject: Option<String>,
    pub body: String,
    pub queued_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub outcome: TouchOutcome,
    pub receipt_id: Option<ReceiptId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TouchChannel {
    Email,
    Linkedin,
    Form,
    Twitter,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TouchOutcome {
    Drafted,
    AwaitingApproval,
    Approved,
    Rejected,
    Sent,
    Bounced,
    Failed,
    Suppressed,
}

/// An inbound message from a contact (reply to a touch, or
/// unsolicited).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: TouchId,
    pub prospect_id: ProspectId,
    pub touch_id: Option<TouchId>,
    pub from_address: String,
    pub subject: Option<String>,
    pub body: String,
    pub received_at: DateTime<Utc>,
    pub kind: ReplyKind,
    pub raw_headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ReplyKind {
    Engaged,
    Question,
    Objection,
    Optout,
    OutOfOffice,
    Bounce,
    Spam,
    Unclassified,
    /// Inbound contains a legal threat — cease-and-desist,
    /// attorney/lawyer language, GDPR Article 17 erasure demand,
    /// CAN-SPAM violation claim, or threat to file with a regulator
    /// (FTC / DPA / state AG). The reply MUST NOT be auto-drafted —
    /// operator handles legally-charged replies personally. Sender
    /// is auto-suppressed with source=`reply_legal_threat`, all
    /// in-flight touches to the prospect are rejected, the prospect's
    /// funnel state moves to `suppressed`, and an alert surfaces in
    /// `salesman alerts` so the operator notices within one cycle.
    /// SECURITY: defense in depth — the keyword pre-check fires
    /// BEFORE the LLM call so a model mis-classification can't
    /// downgrade a legal threat to a benign objection.
    LegalThreat,
}

impl ReplyKind {
    /// The `prospects.state` funnel label a classified reply drives the
    /// prospect to, or `None` if this kind does not itself move the funnel
    /// (Objection / OutOfOffice / Spam / Unclassified are left as-is for
    /// operator judgement). The strings are the canonical `FunnelState`
    /// wire labels written to the DB.
    ///
    /// Compliance-critical: `Optout` and `LegalThreat` MUST map to
    /// `"suppressed"`. The match is exhaustive on purpose — a new
    /// `ReplyKind` variant should force an explicit routing decision here
    /// rather than silently fall through to "no transition".
    pub fn funnel_state_label(self) -> Option<&'static str> {
        match self {
            ReplyKind::Engaged | ReplyKind::Question => Some("engaged"),
            ReplyKind::Optout | ReplyKind::LegalThreat => Some("suppressed"),
            ReplyKind::Bounce => Some("lost"),
            ReplyKind::Objection
            | ReplyKind::OutOfOffice
            | ReplyKind::Spam
            | ReplyKind::Unclassified => None,
        }
    }

    /// `true` iff this reply kind MUST suppress the prospect — an opt-out
    /// or a legal threat. This is the consent / legal gate; never narrow
    /// it. Both kinds also add the sender to the global suppression list.
    pub fn is_suppression_trigger(self) -> bool {
        matches!(self, ReplyKind::Optout | ReplyKind::LegalThreat)
    }
}
