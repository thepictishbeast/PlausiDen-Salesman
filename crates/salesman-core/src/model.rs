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
    /// Stable identifier.
    pub id: CompanyId,
    /// Registered legal name, if known.
    pub legal_name: Option<String>,
    /// Name used in outreach + the UI.
    pub display_name: String,
    /// Primary website URL.
    pub homepage: Option<Url>,
    /// Free-text industry/sector label.
    pub industry: Option<String>,
    /// Coarse headcount band.
    pub size_band: Option<SizeBand>,
    /// Geographic region (free-text; used for local-first targeting).
    pub region: Option<String>,
    /// Short description of what the company does.
    pub description: Option<String>,
    /// Detected technology fingerprints.
    pub tech_signals: Vec<TechSignal>,
    /// When this company first entered the system.
    pub discovered_at: DateTime<Utc>,
    /// When enrichment last refreshed this record, if ever.
    pub last_enriched_at: Option<DateTime<Utc>>,
    /// How the company was discovered.
    pub source: DiscoverySource,
    /// Source-specific raw payload, retained for audit/re-parsing.
    pub raw: BTreeMap<String, serde_json::Value>,
}

/// Coarse company headcount band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SizeBand {
    /// A single person.
    Solo,
    /// Small team / startup.
    Small,
    /// Mid-market.
    Mid,
    /// Large company.
    Large,
    /// Enterprise.
    Enterprise,
    /// Headcount not determined.
    Unknown,
}

/// A detected technology signal (e.g. a framework or vendor) for a company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechSignal {
    /// Signal category (e.g. `framework`, `analytics`).
    pub kind: String,
    /// The detected value (e.g. `nextjs`).
    pub value: String,
    /// Detection confidence in `0.0..=1.0`.
    pub confidence: f32,
}

/// How a company or contact entered the system.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    /// Found via a search query.
    Search,
    /// Found by the web crawler.
    Crawler,
    /// Seeded directly by the operator (e.g. CSV import).
    OwnerSeed,
    /// Sourced from LinkedIn.
    Linkedin,
    /// Any other source.
    Other,
}

/// A person at a company. Role addresses (sales@, info@) are
/// represented as `Contact` with `kind = Role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Stable identifier.
    pub id: ContactId,
    /// The company this contact belongs to.
    pub company_id: CompanyId,
    /// Whether this is a role mailbox or a named person.
    pub kind: ContactKind,
    /// Person's name, if known.
    pub name: Option<String>,
    /// Job title, if known.
    pub title: Option<String>,
    /// Email address, if known.
    pub email: Option<String>,
    /// Whether the email has been verified deliverable.
    pub email_verified: bool,
    /// LinkedIn profile URL, if known.
    pub linkedin_url: Option<Url>,
    /// How this contact was discovered.
    pub source: DiscoverySource,
    /// When this contact first entered the system.
    pub discovered_at: DateTime<Utc>,
}

/// Whether a [`Contact`] is a generic role mailbox or a named person.
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
    /// Stable identifier.
    pub id: CampaignId,
    /// Human-readable, unique campaign name.
    pub name: String,
    /// What this campaign is trying to achieve.
    pub goal: String,
    /// The prospect segment this campaign targets.
    pub target_segment: String,
    /// Lifecycle status.
    pub status: CampaignStatus,
    /// When the campaign was created.
    pub created_at: DateTime<Utc>,
    /// When the campaign was paused, if it is paused.
    pub paused_at: Option<DateTime<Utc>>,
    /// Why the campaign was paused, if it is paused.
    pub paused_reason: Option<String>,
}

/// Lifecycle status of a [`Campaign`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    /// Being set up; not yet sending.
    Draft,
    /// Actively running.
    Active,
    /// Temporarily halted.
    Paused,
    /// Finished normally.
    Completed,
    /// Stopped permanently (e.g. by the operator).
    Killed,
}

/// A specific (campaign, company) pair — the trackable unit of the
/// pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prospect {
    /// Stable identifier.
    pub id: ProspectId,
    /// The campaign this prospect belongs to.
    pub campaign_id: CampaignId,
    /// The company being targeted.
    pub company_id: CompanyId,
    /// The chosen contact at the company, if selected.
    pub primary_contact_id: Option<ContactId>,
    /// Current funnel state.
    pub state: FunnelState,
    /// Why the prospect is in its current state, if recorded.
    pub state_reason: Option<String>,
    /// When the state last changed.
    pub state_changed_at: DateTime<Utc>,
    /// Fit score in `0.0..=1.0`, if computed.
    pub fit_score: Option<f32>,
    /// Free-text operator notes.
    pub notes: Option<String>,
}

/// Where a [`Prospect`] sits in the sales funnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FunnelState {
    /// Just created; not yet qualified.
    New,
    /// Qualified as a fit, not yet contacted.
    Qualified,
    /// At least one outbound touch has been sent.
    Contacted,
    /// The prospect has replied with interest.
    Engaged,
    /// A meeting is scheduled or held.
    Meeting,
    /// A proposal has been sent.
    Proposal,
    /// Closed-won (terminal).
    Won,
    /// Closed-lost — bounce or operator mark (terminal).
    Lost,
    /// Opted out / legally suppressed (terminal).
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
    /// Stable identifier.
    pub id: TouchId,
    /// The prospect this touch targets.
    pub prospect_id: ProspectId,
    /// The channel the touch was sent on.
    pub channel: TouchChannel,
    /// Subject line (email channel), if any.
    pub subject: Option<String>,
    /// Message body.
    pub body: String,
    /// When the touch was queued for sending.
    pub queued_at: DateTime<Utc>,
    /// When the touch was actually sent, if it has been.
    pub sent_at: Option<DateTime<Utc>>,
    /// Current outcome of the touch.
    pub outcome: TouchOutcome,
    /// The signed receipt id once sent, if any.
    pub receipt_id: Option<ReceiptId>,
}

/// The channel an outbound [`Touch`] is delivered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TouchChannel {
    /// Email.
    Email,
    /// LinkedIn message.
    Linkedin,
    /// A web contact form.
    Form,
    /// Twitter / X DM.
    Twitter,
    /// Any other channel.
    Other,
}

/// The lifecycle outcome of a [`Touch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TouchOutcome {
    /// Drafted, not yet routed for approval.
    Drafted,
    /// Waiting on operator approval before send.
    AwaitingApproval,
    /// Approved for sending.
    Approved,
    /// Rejected by the operator; will not send.
    Rejected,
    /// Sent successfully.
    Sent,
    /// The recipient mail server bounced it.
    Bounced,
    /// Sending failed for a non-bounce reason.
    Failed,
    /// Suppressed before send (opt-out / legal).
    Suppressed,
}

/// An inbound message from a contact (reply to a touch, or
/// unsolicited).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    /// Stable identifier.
    pub id: TouchId,
    /// The prospect this reply is associated with.
    pub prospect_id: ProspectId,
    /// The outbound touch this is a reply to, if matched.
    pub touch_id: Option<TouchId>,
    /// The sender's email address.
    pub from_address: String,
    /// Subject line, if any.
    pub subject: Option<String>,
    /// Message body.
    pub body: String,
    /// When the reply was received.
    pub received_at: DateTime<Utc>,
    /// The classified kind of reply.
    pub kind: ReplyKind,
    /// Raw inbound headers, retained for audit.
    pub raw_headers: BTreeMap<String, String>,
}

/// The classified intent of an inbound [`Reply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ReplyKind {
    /// Positive, interested reply.
    Engaged,
    /// A question that needs answering.
    Question,
    /// A pushback or concern (left for operator judgement).
    Objection,
    /// An opt-out / unsubscribe request.
    Optout,
    /// An automated out-of-office bounce-back.
    OutOfOffice,
    /// A delivery-failure bounce.
    Bounce,
    /// Unsolicited spam.
    Spam,
    /// Could not be classified.
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
