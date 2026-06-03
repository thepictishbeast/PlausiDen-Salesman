//! salesman — operator CLI.
//!
//! Subcommands:
//!   plan         agent loop with a goal (calls real LLMs if keys set)
//!   migrate      run database migrations
//!   discover     ingest a CSV of companies into a campaign
//!   enrich       fetch homepages for all companies in a campaign
//!   halt         kill switch (stub)
//!   tools        list registered tools (incl. discovery tools)
//!   backends     list registered LLM backends + models
#![deny(missing_docs)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CostsBy {
    Model,
    Purpose,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum TemplateStatsBy {
    Template,
    Segment,
}

#[derive(Subcommand, Debug)]
enum CadenceCmd {
    /// List currently-paused prospect-sequences with their reason.
    List {
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Resume a paused prospect-sequence. Idempotent.
    Resume {
        #[arg(long)]
        prospect_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum TriggerCmd {
    /// Poll the OSINT sources for each prospect in the campaign and
    /// persist any new trigger events. Idempotent — re-running won't
    /// produce duplicates. Run nightly via systemd timer for best
    /// effect.
    Scan {
        #[arg(long)]
        campaign: String,
        /// How many days back to consider an event "fresh". Older
        /// events get exponentially-decaying recency_score.
        #[arg(long, default_value_t = 14)]
        max_age_days: i64,
        /// Cap per prospect to keep the run bounded.
        #[arg(long, default_value_t = 5)]
        max_per_prospect: usize,
    },
    /// "What should I send today?" — ranked recent triggers across
    /// (optionally) one campaign, top N. Default is unused triggers
    /// only — ones we haven't already used to anchor a touch.
    List {
        #[arg(long)]
        campaign: Option<String>,
        #[arg(long, default_value_t = 168)]
        since_hours: i64,
        #[arg(long, default_value_t = 25)]
        top: i64,
        /// Set to false to include triggers already used in a touch.
        #[arg(long, default_value_t = true)]
        unused_only: bool,
    },
    /// Auto-draft cold touches anchored on the top-N unused trigger
    /// events. Closes the loop between trigger detection and
    /// outreach: instead of leaving the operator with a list and
    /// asking them to draft each one, this pre-anchors the drafts in
    /// the awaiting-approval queue with the trigger headline as the
    /// angle_hint. Operator opens `salesman review` and walks
    /// pre-personalized copy.
    Draft {
        #[arg(long)]
        campaign: String,
        /// Product to pitch (Sentinel, Tidy, Atrium, AppGuard, …).
        #[arg(long)]
        product: String,
        /// How fresh the trigger must be (hours).
        #[arg(long, default_value_t = 168)]
        since_hours: i64,
        /// Max drafts to generate. Bounded so a noisy news day
        /// doesn't drain the LLM budget.
        #[arg(long, default_value_t = 10)]
        top: i64,
    },
}

#[derive(Subcommand, Debug)]
enum SuppCmd {
    /// List recent suppressions (newest first). --source filters to
    /// just one origin tag (manual / bounce / reply_optout / one_click /
    /// compliance).
    List {
        #[arg(long)]
        source: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
    /// Add a manual suppression. Reason is required so the audit log
    /// can answer "who decided to block this and why" later.
    Add {
        #[arg(long)]
        target: String,
        #[arg(long, default_value = "email")]
        kind: String,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "manual")]
        source: String,
    },
    /// Remove a suppression. Idempotent; prints how many rows were
    /// affected. Requires --confirm-typed for safety because the
    /// recipient WILL receive future sends after removal.
    Remove {
        #[arg(long)]
        target: String,
        #[arg(long, default_value_t = false)]
        confirm_typed: bool,
    },
    /// Dump the entire suppression list as CSV. Headers: target,
    /// kind, reason, source, added_at.
    Export {
        /// Output file path. `-` (default) writes to stdout.
        #[arg(long, default_value = "-")]
        out: String,
    },
    /// Bulk import from CSV. The file may be a previous --export
    /// dump or a one-column list of email addresses (in which case
    /// each row gets reason="bulk import" + source="manual").
    /// Duplicates are silently skipped (ON CONFLICT DO NOTHING).
    Import {
        #[arg(long)]
        from_csv: PathBuf,
        /// Override the source tag for every imported row.
        #[arg(long)]
        source: Option<String>,
    },
    /// Print one row per source with its count. Quick health metric.
    Count,
}
use salesman_content::{DraftColdEmailTool, ReplyClassifyTool};
use salesman_discovery::{
    BraveSearch, BraveSearchTool, CsvSeed, CsvSeedTool, EmailPatternTool, HomepageFetchTool,
    HomepageFetcher,
};
use salesman_llm::claude::ClaudeBackend;
use salesman_llm::gemini::GeminiBackend;
use salesman_llm::subscriber_cli::SubscriberCliBackend;
use salesman_llm::{LlmBackend, LlmRouter, Message, Role, RouteHint};
use salesman_orchestrator::Orchestrator;
use salesman_outreach::{SmtpConfig, SmtpSender};
use salesman_receipts::{Signer, default_seed_path};
use salesman_reply::{ImapConfig, ImapPoller};
use salesman_state::{State, TouchSummary};
use salesman_tools::{EchoTool, ToolRegistry};
use sqlx::Row;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

// Owner directive: default to the latest Claude (Opus 4.8). Single
// source of truth — override per-run with --claude-model / env
// SALESMAN_CLAUDE_MODEL. Cost tracking for this model lives in
// salesman-llm::rates::RATES.
const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-4-8";
const DEFAULT_GEMINI_MODEL: &str = "gemini-1.5-flash";

#[derive(Parser, Debug)]
#[command(name = "salesman", about = "PlausiDen-Salesman operator CLI", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    #[arg(long, env = "SALESMAN_CLAUDE_MODEL", default_value = DEFAULT_CLAUDE_MODEL)]
    claude_model: String,

    #[arg(long, env = "SALESMAN_GEMINI_MODEL", default_value = DEFAULT_GEMINI_MODEL)]
    gemini_model: String,

    /// Postgres connection string (for migrate / discover / enrich).
    #[arg(long, env = "SALESMAN_DATABASE_URL")]
    database_url: Option<String>,

    /// Emit machine-readable JSON instead of human-readable output
    /// where supported (summary / costs / doctor / suppressions count).
    /// Hooks into Prometheus / Grafana / monitoring scripts.
    #[arg(long, default_value_t = false, global = true)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the agent loop with a goal. Calls real LLMs when keys are set.
    Plan {
        #[arg(long, default_value = "demo: introduce yourself in one short sentence")]
        goal: String,
        #[arg(long, default_value = "reasoning")]
        hint: String,
    },
    /// Run pending database migrations.
    Migrate,
    /// Ingest a CSV of companies into a campaign.
    Discover {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value = "imported via CLI")]
        goal: String,
        #[arg(long, default_value = "unspecified")]
        segment: String,
        #[arg(long)]
        from_csv: PathBuf,
    },
    /// Autonomous prospect discovery via web search. Runs a Brave
    /// Search query (e.g. "EU SMB cybersecurity consultancies"),
    /// normalizes hits into companies, and (with --persist) inserts
    /// them as prospects in the campaign. Skips obvious non-company
    /// hits (wikipedia / linkedin / job boards / news).
    /// Requires BRAVE_SEARCH_API_KEY in env.
    DiscoverSearch {
        #[arg(long)]
        campaign: String,
        /// Free-form search query describing the prospect profile.
        #[arg(long)]
        query: String,
        /// Max companies to import per run.
        #[arg(long, default_value_t = 25)]
        top: u32,
        /// Insert into DB. Without this the command is read-only —
        /// you see what would be imported and can refine the query.
        #[arg(long, default_value_t = false)]
        persist: bool,
    },
    /// Autonomous prospect discovery via the registered LLM (no
    /// search API needed). Asks the model to enumerate companies
    /// that match the supplied ICP, then validates EVERY candidate
    /// homepage by DNS + HTTP fetch — hallucinated domains are
    /// dropped before they enter the campaign. Use when no
    /// BRAVE_SEARCH_API_KEY is configured and the operator has no
    /// prospect CSV to seed from. Defaults to PRINT-only — pass
    /// `--persist` to import the survivors as Companies + Prospects.
    DiscoverLlm {
        #[arg(long)]
        campaign: String,
        /// Ideal customer profile description, e.g. "EU SMB
        /// cybersecurity teams running self-hosted log
        /// infrastructure (Splunk / Elastic / Loki / Wazuh)
        /// who care about data sovereignty under GDPR".
        /// More specific = better; the LLM treats this as
        /// criteria, not keywords.
        #[arg(long)]
        icp: String,
        /// Max validated companies to keep. The LLM is asked
        /// for ~1.5x this so the homepage-validation drop-rate
        /// (typically 20-40%) still leaves enough survivors.
        #[arg(long, default_value_t = 25)]
        top: u32,
        /// Insert into DB. Without this the command is read-only.
        #[arg(long, default_value_t = false)]
        persist: bool,
    },
    /// Fetch homepages for all companies in a campaign + write
    /// extracted facts back into companies.
    Enrich {
        #[arg(long)]
        campaign: String,
        /// Concurrency cap (don't hammer one host).
        #[arg(long, default_value_t = 4)]
        concurrency: u32,
    },
    /// Approve a draft (move from awaiting_approval → approved).
    /// Default: refuses if AI-detector risk score >= threshold.
    Approve {
        #[arg(long)]
        touch: String,
        /// Detector risk threshold (0.0–1.0). Default 0.6.
        #[arg(long, default_value_t = 0.6_f32)]
        detector_threshold: f32,
        /// Override the detector gate. Logged as override-reason.
        #[arg(long)]
        force_override: Option<String>,
    },
    /// Reject a draft (move from awaiting_approval → rejected).
    Reject {
        #[arg(long)]
        touch: String,
    },
    /// Bulk-approve up to N awaiting-approval drafts in a campaign.
    /// Detector gate runs per-draft (skipped if it fails; --force-override
    /// applies to the whole batch). Operator must --confirm-typed.
    ApproveBatch {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = 25)]
        max: u32,
        #[arg(long, default_value_t = 0.6_f32)]
        detector_threshold: f32,
        /// Apply this override-reason to every draft that fails the
        /// detector. Use sparingly — undermines the per-draft review.
        #[arg(long)]
        force_override: Option<String>,
        /// REQUIRED: type the campaign name to proceed (dialoguer).
        #[arg(long, default_value_t = false)]
        confirm_typed: bool,
    },
    /// Add an email or domain to the suppression list (idempotent).
    Suppress {
        /// Either an email or a bare domain.
        #[arg(long)]
        target: String,
        /// 'email' or 'domain'. Auto-detected by '@' presence if omitted.
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value = "operator-issued")]
        reason: String,
    },
    /// Send approved drafts. DEFAULT IS DRY-RUN. Pass --for-real to send.
    /// Two extra reputation safeguards layered on top of suppression
    /// + rate caps:
    ///   --max-batch N         hard cap on touches sent in one invocation
    ///   --ack-new-domains N   max number of NEW domains touched in
    ///   this batch (refuses send if exceeded;
    ///   operator must confirm by raising N)
    SendPending {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = false)]
        for_real: bool,
        /// Per-recipient rate-cap window (hours).
        #[arg(long, default_value_t = 720)]
        per_recipient_window_hours: i64,
        /// Per-recipient max touches in window.
        #[arg(long, default_value_t = 5)]
        per_recipient_max: i64,
        /// Per-domain rate-cap window (hours).
        #[arg(long, default_value_t = 1)]
        per_domain_window_hours: i64,
        /// Per-domain max touches in window.
        #[arg(long, default_value_t = 10)]
        per_domain_max: i64,
        /// Soft-quarantine threshold: skip a domain whose recent
        /// hard-bounce count meets or exceeds this value within the
        /// last 24h. Set to 0 to disable.
        #[arg(long, default_value_t = 3)]
        domain_quarantine_threshold: i64,
        /// HARD cap on touches sent in this single invocation. A
        /// reputation safeguard against accidental large batches.
        /// Note: by default the sender-warmup curve may further
        /// LOWER this cap; the effective cap is min(max_batch,
        /// warmup_cap_for_age). Pass --no-warmup to skip the curve.
        #[arg(long, default_value_t = 25)]
        max_batch: u32,
        /// Disable the sender-warmup gradient. By default a fresh
        /// campaign caps at 5/day for days 1-3, 10 for days 4-7, 25
        /// for days 8-14, 100 thereafter — a curve that protects the
        /// new sender domain's reputation. Pass --no-warmup ONLY if
        /// you've already warmed this domain via another channel; it
        /// is NOT recommended.
        #[arg(long, default_value_t = false)]
        no_warmup: bool,
        /// Max NEW domains (not previously touched in this campaign)
        /// allowed in one batch. Reputation safeguard against
        /// burning the IP on a fresh list. Operator raises explicitly.
        #[arg(long, default_value_t = 10)]
        ack_new_domains: u32,
        /// Skip the 5-second pre-send pause. Use ONLY in CI / scripts.
        #[arg(long, default_value_t = false)]
        no_pause: bool,
        /// Require the operator to TYPE the campaign name to confirm
        /// REAL send (dialoguer prompt). The strongest reputation
        /// safeguard. Recommended for first real send / new domains.
        #[arg(long, default_value_t = false)]
        confirm_typed: bool,
        /// Reputation-safe smoke test: send EXACTLY ONE message —
        /// the first approved touch — but redirect it to this address
        /// instead of the prospect's. Body + subject + headers are
        /// the real ones (so you see what the prospect would see),
        /// but the actual prospect is NOT contacted. Touch is NOT
        /// marked as sent (it stays `approved` for the real run).
        /// No receipt is logged.
        #[arg(long)]
        test_send_to: Option<String>,
        /// Refuse to send any touch whose `produced_by.via_fallback`
        /// is true — i.e. the draft was generated by the secondary
        /// LLM after a primary failure. Use this when you want a
        /// human to re-draft fallback-generated copy before it ships.
        /// Backend-health gate: keeps "good copy" the primary made
        /// from being mixed with "okay copy" the fallback made.
        #[arg(long, default_value_t = false)]
        require_primary: bool,
    },
    /// Verify the most-recent N receipts as individual signed
    /// records. Does NOT verify chain linkage — see `audit-chain`
    /// for that.
    Audit {
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
    /// Verify the FULL hash chain end-to-end. Pulls receipts
    /// oldest-first and walks `prev_hash` against the previous
    /// receipt's `hash`. Surfaces the first break point + summary.
    /// Stronger guarantee than `audit` — proves nothing was inserted
    /// or altered between any two events.
    AuditChain {
        /// Maximum number of receipts to walk. Increase if your audit
        /// trail is longer than this. (Default 100000 covers years
        /// of typical use.)
        #[arg(long, default_value_t = 100_000)]
        limit: i64,
    },
    /// Poll the IMAP inbox once and persist new replies.
    InboxPoll {
        /// Run forever, polling every N seconds. Default = once.
        #[arg(long)]
        every_seconds: Option<u64>,
    },
    /// Classify all unclassified replies + apply transitions.
    /// When --competitors is supplied, every classified reply is
    /// also scanned for competitor mentions; matches land in
    /// replies.tags->competitors and surface in `salesman alerts`.
    ClassifyReplies {
        #[arg(long, default_value_t = 50)]
        batch: i64,
        /// Optional competitor catalog TOML (e.g.
        /// samples/competitors.toml). Mentions get tagged on the
        /// reply for downstream pivot in the reply-drafter.
        #[arg(long)]
        competitors: Option<PathBuf>,
    },
    /// Auto-draft response touches for replies classified as
    /// engaged / question / objection. Each draft lands in the
    /// awaiting-approval queue with the same detector + signed-receipt
    /// gates as cold drafts. Closes the inbox loop: operator reviews
    /// + approves rather than composing from scratch.
    ///
    /// When inbound looks pricing-shaped and --pricing-catalog is
    /// supplied, the drafter quotes SPECIFIC tier numbers. When
    /// inbound looks meeting-shaped and --meeting-slots is supplied,
    /// the drafter proposes 3 concrete slots. When --objections is
    /// supplied, the drafter weaves operator talking points into
    /// matched objection replies.
    DraftReplies {
        #[arg(long, default_value_t = 25)]
        batch: i64,
        /// Optional pricing catalog TOML (e.g. samples/pricing.toml).
        #[arg(long)]
        pricing_catalog: Option<PathBuf>,
        /// Optional meeting-slots TOML (e.g.
        /// samples/meeting-slots.toml). Past slots filtered; drafter
        /// sees the next 3 upcoming.
        #[arg(long)]
        meeting_slots: Option<PathBuf>,
        /// Optional objection library TOML (e.g.
        /// samples/objections.toml). Matched entries get their
        /// talking_points + posture threaded into the drafter.
        #[arg(long)]
        objections: Option<PathBuf>,
    },
    /// Trigger-event scanner — find people to email TODAY based on
    /// real signals (recent news, GitHub activity, HN mentions).
    /// `scan` polls the OSINT sources for each prospect; `list`
    /// shows the operator's "what should I send today" ranked view.
    Triggers {
        #[command(subcommand)]
        action: TriggerCmd,
    },
    /// Adaptive-cadence controls — list paused prospects (auto-paused
    /// on reply or operator-paused) and resume them. By design any
    /// reply (engaged/question/objection/OOO/optout/bounce) pauses
    /// the static sequence; the reply-drafter handles that thread.
    /// Operator resumes manually after deciding the prospect needs
    /// to keep getting the canned cadence.
    Cadence {
        #[command(subcommand)]
        action: CadenceCmd,
    },
    /// Post-close pipeline expansion: draft a referral-ask touch for
    /// each `won` prospect whose deal closed at least --min-days ago
    /// and who hasn't yet been asked. Drafts land in the awaiting-
    /// approval queue with template_key=`referral_ask` so re-runs
    /// idempotently skip already-asked prospects.
    ReferralAsk {
        #[arg(long, default_value_t = 30)]
        min_days: i64,
        #[arg(long, default_value_t = 10)]
        batch: i64,
        /// What product to reference as the one they bought.
        #[arg(long, default_value = "Sentinel")]
        product: String,
    },
    /// Decision-maker finder — for each company in a campaign,
    /// scrape its public team / about / leadership pages and
    /// surface ranked buyer candidates with email guesses + role
    /// rationale + confidence. Defaults to PRINT only — pass
    /// `--persist` to also create contact rows + link as primary.
    /// Email addresses are GUESSES; verify before using.
    FindBuyers {
        #[arg(long)]
        campaign: String,
        /// Top N candidates per company.
        #[arg(long, default_value_t = 3)]
        top: usize,
        /// Persist the top candidate per company as a contact +
        /// link as primary on the prospect. Without this, the
        /// command is read-only — operator reviews the output and
        /// decides.
        #[arg(long, default_value_t = false)]
        persist: bool,
    },
    /// AI-search visibility check (Generative Engine Optimization).
    /// Sends a "who is the best X in Y" query to a registered LLM,
    /// detects whether the operator's brand appears, extracts
    /// competitors mentioned, and (with --recommend) generates
    /// concrete content + schema-markup actions to start showing up.
    Geo {
        /// The query a prospect would ask AI (e.g.
        /// "who is the best realtor in southern Utah").
        #[arg(long)]
        query: String,
        /// The brand to look for (e.g. "Jane Doe Realty").
        #[arg(long)]
        brand: String,
        /// Comma-separated alternate spellings / shorthand.
        #[arg(long)]
        aliases: Option<String>,
        /// Make a second LLM call to generate 5 concrete actions.
        #[arg(long, default_value_t = false)]
        recommend: bool,
    },
    /// Auto-angle picker — for each prospect in a campaign, pick
    /// the best (product, angle) match from a catalog TOML file.
    /// Diagnostic / preview mode. Operator can run this before
    /// `salesman draft --product auto` to see what the system
    /// would pick.
    PickAngle {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value = "samples/products.toml")]
        catalog: PathBuf,
        /// Cap how many prospects to score (the rest are skipped).
        #[arg(long, default_value_t = 10)]
        max: usize,
    },
    /// Show recent classified replies for a campaign.
    Inbox {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Print a pipeline summary (counts + N-hour activity).
    Summary {
        #[arg(long, default_value_t = 24)]
        since_hours: i64,
    },
    /// Triaged digest of the IMPORTANT recent activity — positive
    /// replies, opt-outs, bounces, auto-suppressions. Run this
    /// regularly (manually or via cron) to know what just landed
    /// without scrolling the full inbox.
    Alerts {
        #[arg(long, default_value_t = 24)]
        since_hours: i64,
        /// Post the digest to a Slack/Discord/Mattermost incoming
        /// webhook. URL via env $SALESMAN_ALERT_WEBHOOK_URL or this
        /// flag. Detects Slack-vs-Discord-vs-generic by hostname.
        /// Only posts when there's something interesting (positive
        /// reply, opt-out, bounce spike, or competitor mention).
        #[arg(long)]
        webhook: Option<String>,
        /// Always post the digest, even when nothing interesting
        /// happened. Useful for "I want a daily 'all clear' ping."
        #[arg(long, default_value_t = false)]
        webhook_always: bool,
    },
    /// Print LLM cost report over a time window. Default is by
    /// (backend, model); pass `--by purpose` to roll up by the
    /// chat_for(purpose) tag instead — useful for answering
    /// "which subsystem is eating budget".
    Costs {
        #[arg(long, default_value_t = 24)]
        since_hours: i64,
        /// Group rows by this dimension. `model` is the default
        /// (backend + model), `purpose` rolls up across models by the
        /// purpose tag the caller passed to chat_for.
        #[arg(long, default_value = "model")]
        by: CostsBy,
    },
    /// Per-template performance stats (drafted / sent / replied /
    /// engaged). Pass `--by segment` to break down each template
    /// across the prospect's industry — answers "which template
    /// wins for security CISOs vs devops engineers vs data leaders."
    /// Templates with sent ≥ 10 and engaged_rate < half the
    /// best-performer in the same segment are flagged with ⚠ for
    /// the operator to consider pausing.
    TemplateStats {
        /// Group rows by `template` (default) or
        /// `segment` (per-template per-industry breakdown).
        #[arg(long, default_value = "template")]
        by: TemplateStatsBy,
    },
    /// Send-time analytics — reply rate broken down by
    /// day-of-week + hour-of-day in the operator's local timezone.
    /// Answers "when should I time the next batch?"
    SendTimes {
        /// Operator's timezone offset from UTC, in minutes
        /// (NY=-300, LA=-480, UTC=0, Berlin=60 or 120).
        #[arg(long, default_value_t = 0)]
        tz_offset_minutes: i32,
        /// Minimum sends per bucket before it's reported.
        #[arg(long, default_value_t = 5)]
        min_sent: i64,
        /// Top-N windows to highlight as recommendations.
        #[arg(long, default_value_t = 5)]
        top: usize,
    },
    /// Account-based fanout — given an engaged prospect's id,
    /// surface OTHER known contacts at the same company so the
    /// operator can pursue multi-stakeholder outreach. Read-only
    /// today; future pass adds optional --seed to create prospect
    /// rows for those contacts in the same campaign.
    AccountFanout {
        #[arg(long)]
        prospect_id: String,
    },
    /// Score a body of text through the AI detector.
    Score {
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        templates_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 0.6_f32)]
        threshold: f32,
    },
    /// Set or clear a per-campaign LLM-cost cap (in USD).
    SetCostCap {
        #[arg(long)]
        campaign: String,
        /// Cap in USD. Use 0 (or omit) to clear.
        #[arg(long, default_value_t = 0.0)]
        max_usd: f64,
    },
    /// Per-campaign cost breakdown (with cap utilisation).
    CampaignCosts {
        #[arg(long, default_value_t = 168)]
        since_hours: i64,
    },
    /// Health probe — JSON output. Exit 1 if any required component
    /// is missing.
    Status,
    /// Comprehensive diagnostic. Probes every external dependency
    /// (DB / LLM / SMTP / IMAP / signing key / suppressions /
    /// awaiting-approval queue / disk) and prints a per-check verdict.
    /// Exit 1 if anything required is broken.
    Doctor {
        /// Probe SMTP (will attempt a connection, no email sent).
        #[arg(long, default_value_t = false)]
        probe_smtp: bool,
        /// Probe IMAP (will attempt a connection, no mailbox modify).
        #[arg(long, default_value_t = false)]
        probe_imap: bool,
    },
    /// Print the current sender identity (resolved from env). Exits 1
    /// if any required field is missing.
    Whoami,
    /// Pre-flight check on a CSV before `discover`. Reports parsable
    /// rows + which would be skipped + why. No DB writes.
    ValidateCsv {
        #[arg(long)]
        from_csv: PathBuf,
    },
    /// Bulk-reject every awaiting-approval draft in a campaign.
    /// Useful when starting fresh after a failed draft batch.
    /// Requires --confirm-typed.
    QueueClear {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = false)]
        confirm_typed: bool,
    },
    /// Per-campaign pre-flight check before `send-pending --for-real`.
    /// Verifies signing key, unsubscribe minter, SMTP, LLM backends,
    /// campaign + prospects + drafts ready to go, no obvious test
    /// addresses in the queue, and prints 3 sample drafts for the
    /// operator to eyeball. Exits 1 if anything is blocking.
    Preflight {
        #[arg(long)]
        campaign: String,
        /// Skip the SMTP / IMAP TCP probe (faster; only checks env
        /// vars are populated).
        #[arg(long, default_value_t = false)]
        no_probe: bool,
        /// Number of draft samples to print at the end. 0 disables.
        #[arg(long, default_value_t = 3)]
        sample_drafts: usize,
    },
    /// Resolve SPF / DKIM / DMARC / PTR for the sender domain and
    /// report per-record OK / WARN / FAIL with remediation.
    /// Owner runs this DURING DNS setup (B3 in OWNER_BLOCKERS.md)
    /// instead of hand-running dig + parsing output.
    DnsCheck {
        /// Sender domain (e.g. `outreach.plausiden.com`).
        #[arg(long)]
        domain: String,
        /// DKIM selector to query, e.g. `s1` →
        /// `s1._domainkey.<domain>`.
        #[arg(long, default_value = "s1")]
        dkim_selector: String,
        /// Sender IP (for the PTR check). If omitted, the PTR check
        /// is skipped — DNS-only.
        #[arg(long)]
        sender_ip: Option<String>,
        /// Sending hostname expected in the PTR (e.g.
        /// `mail.plausiden.com`). Required when --sender-ip is set.
        #[arg(long)]
        expected_ptr: Option<String>,
    },
    /// List the pending owner audit-notifications (one per outbound
    /// contact, still undelivered). Each shows who / how / what so you
    /// can find a contact a prospect phoned you about. Read-only.
    OwnerNotifications {
        /// Maximum number of pending notifications to show (oldest first).
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Manage the global do-not-contact list. Subcommands: list,
    /// add, remove, export, import, count. Required for GDPR
    /// right-to-be-forgotten + audit + backup.
    Suppressions {
        #[command(subcommand)]
        action: SuppCmd,
    },
    /// Render a directory of markdown to a static HTML site.
    RenderSite {
        #[arg(long)]
        src: PathBuf,
        #[arg(long)]
        dst: PathBuf,
        #[arg(long, default_value = "https://plausiden.com")]
        origin: String,
        #[arg(long, default_value = "PlausiDen")]
        site_name: String,
    },
    /// Define a multi-touch sequence from a TOML file.
    DefineSequence {
        #[arg(long)]
        campaign: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        from_toml: PathBuf,
    },
    /// Assign a sequence to every prospect in a campaign.
    AssignSequence {
        #[arg(long)]
        campaign: String,
        #[arg(long)]
        sequence: String,
    },
    /// Tick all due prospect sequences — emits draft Touches for
    /// each prospect whose next_due_at has passed.
    TickSequences {
        #[arg(long, default_value_t = 100)]
        batch: i64,
        /// PlausiDen product to anchor templates against.
        #[arg(long, default_value = "Sentinel")]
        product: String,
    },
    /// Generate cold-email drafts for every prospect in a campaign.
    /// Drafts land in `awaiting_approval` — never auto-sent.
    Draft {
        #[arg(long)]
        campaign: String,
        /// PlausiDen product to pitch (Sentinel, Tidy, Atrium, AppGuard, ...).
        #[arg(long)]
        product: String,
        /// Optional steering for the angle.
        #[arg(long)]
        angle_hint: Option<String>,
        /// Skip prospects that already have an awaiting-approval touch.
        #[arg(long, default_value_t = true)]
        skip_existing: bool,
    },
    /// Show drafts awaiting operator approval.
    Review {
        #[arg(long)]
        campaign: String,
    },
    /// Insert N synthetic-but-plausible prospects into the campaign
    /// for end-to-end smoke testing. Each stub gets a realistic
    /// example.com-class homepage, a sensible industry / size_band,
    /// and a small tech_signals array — enough to exercise the
    /// drafter (which reads ProspectWithFacts) without requiring a
    /// real CSV or external discovery API.
    /// USE: validate the openclaw deployment end-to-end (draft →
    /// fact-check → approve-all → send-pending --dry-run) BEFORE
    /// owner has imported real prospects.
    QuickStub {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = 5)]
        count: usize,
    },
    /// Bulk fact-check every awaiting-approval draft in a campaign.
    /// Runs the U44 fact-trace gate (numeric claims must trace back
    /// to prospect facts JSONB) across the whole queue and reports
    /// only the drafts with fabricated claims. Prints touch ids so
    /// the operator can pipe straight into `salesman reject`.
    FactCheck {
        #[arg(long)]
        campaign: String,
        /// Detector threshold above which a draft is flagged.
        /// Default 0.50 matches the standard approve-gate threshold.
        #[arg(long, default_value_t = 0.50)]
        threshold: f32,
        /// Print every draft's score, not only the ones that fail.
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },
    /// Bulk-approve every awaiting-approval draft that passes the
    /// detector ensemble (U44 fact-trace + U46 personalization +
    /// existing AI-tells signals). Failures stay in the queue with
    /// reasons surfaced; operator manually reviews / rejects /
    /// force-overrides those. `--dry-run` previews without state
    /// changes.
    ApproveAll {
        #[arg(long)]
        campaign: String,
        /// Detector threshold — drafts at or above this score stay
        /// in the queue. Matches the standard approve-gate default.
        #[arg(long, default_value_t = 0.50)]
        threshold: f32,
        /// Print what would be approved without changing state.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// "Today's 10" — a daily prioritized to-do list combining
    /// (a) replies needing response, (b) trigger events fired in the
    /// last N hours, (c) paused/stalled prospects, (d) referral-ask
    /// candidates. Goal: focus the operator on the highest-conversion
    /// targets instead of chewing through the prospect list linearly.
    NextBestActions {
        /// How far back to scan for trigger events.
        #[arg(long, default_value_t = 24)]
        trigger_window_hours: i64,
        /// Cap on rows returned per category.
        #[arg(long, default_value_t = 5)]
        per_category: i64,
        /// Optional campaign filter — restricts trigger events; the
        /// other categories are global (replies span campaigns).
        #[arg(long)]
        campaign: Option<String>,
    },
    /// Kill switch — pauses every active campaign.
    Halt {
        #[arg(long, default_value = "operator-issued")]
        reason: String,
    },
    /// Import prospects from a CSV (samples/prospects-warmup-template.csv
    /// shows the expected header). Validates rows (display_name
    /// required, homepage parses, size_band mapped, CSV-injection
    /// rejected), then upserts companies + links them as prospects
    /// to the named campaign. Idempotent on (campaign, company).
    /// `--dry-run` validates without writing.
    ImportCsv {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Tag a prospect with an interest the drafter should remember.
    /// Appended to prospects.tags['interests'] (deduped). Drafter
    /// prompts pick this up automatically via ProspectWithFacts —
    /// the next touch can cite the interest directly. Operator-driven
    /// today; LLM extraction lands as U52.
    Tag {
        #[arg(long)]
        prospect_id: String,
        #[arg(long)]
        interest: String,
    },
    /// Append a free-text note to a prospect (e.g. "introduced by
    /// Mike Chen", "no decision until Q3"). Lands in
    /// prospects.tags['notes'] (deduped); drafter sees it on every
    /// subsequent touch via to_prompt_json. Symmetric to `salesman tag`
    /// but for unstructured operator context rather than topical
    /// interests.
    Note {
        #[arg(long)]
        prospect_id: String,
        #[arg(long)]
        text: String,
    },
    /// Dump the full conversation thread for one prospect — outbound
    /// touches we sent + inbound replies they sent, oldest first.
    /// Useful for "what have we said to this person?" right before
    /// approving a reply.
    Thread {
        #[arg(long)]
        prospect_id: String,
        /// Cap on turns returned. Default 20 covers most sequences.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// List the registered tools.
    Tools,
    /// List the registered LLM backends + models.
    Backends,
}

fn build_router(
    claude_model: &str,
    gemini_model: &str,
    sink: Option<Arc<dyn salesman_llm::LlmCallSink>>,
) -> LlmRouter {
    let mut router = LlmRouter::new();
    // Transport selection — owner directive 2026-04-30: on the
    // openclaw deployment we drive subscriber-login CLIs (claude
    // / gemini) instead of API keys so we use the paid Pro/Max
    // and Gemini Advanced seats. Set SALESMAN_LLM_TRANSPORT=cli
    // to pick that path; default `api` keeps legacy behavior.
    let transport = std::env::var("SALESMAN_LLM_TRANSPORT")
        .unwrap_or_else(|_| "api".to_string())
        .to_ascii_lowercase();
    match transport.as_str() {
        "cli" => {
            match SubscriberCliBackend::claude_from_env(claude_model) {
                Ok(b) => {
                    let kind = b.kind();
                    router.register(Arc::new(b));
                    tracing::info!(%kind, model = %claude_model,
                        "registered Claude (subscriber-cli) backend");
                }
                Err(e) => {
                    tracing::warn!("Claude subscriber-cli backend not registered: {e}");
                }
            }
            match SubscriberCliBackend::gemini_from_env(gemini_model) {
                Ok(b) => {
                    let kind = b.kind();
                    router.register(Arc::new(b));
                    tracing::info!(%kind, model = %gemini_model,
                        "registered Gemini (subscriber-cli) backend");
                }
                Err(e) => {
                    tracing::warn!("Gemini subscriber-cli backend not registered: {e}");
                }
            }
        }
        _ => {
            if let Ok(b) = ClaudeBackend::from_env(claude_model) {
                let kind = b.kind();
                router.register(Arc::new(b));
                tracing::info!(%kind, model = %claude_model, "registered Claude backend");
            } else {
                tracing::info!("ANTHROPIC_API_KEY not set — Claude backend not registered");
            }
            if let Ok(b) = GeminiBackend::from_env(gemini_model) {
                let kind = b.kind();
                router.register(Arc::new(b));
                tracing::info!(%kind, model = %gemini_model, "registered Gemini backend");
            } else {
                tracing::info!("GEMINI_API_KEY not set — Gemini backend not registered");
            }
        }
    }
    if let Some(sink) = sink {
        router = router.with_sink(sink);
        tracing::info!("LLM cost ledger sink attached");
    }
    // Operator brief: optional file path in SALESMAN_OPERATOR_BRIEF.
    // No-op if unset; logs at INFO when loaded.
    router = router.with_operator_brief_from_env();
    router
}

fn build_tools(router: Arc<LlmRouter>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    tools.register(Arc::new(CsvSeedTool::new()));
    tools.register(Arc::new(HomepageFetchTool::new()));
    tools.register(Arc::new(EmailPatternTool::new()));
    if let Ok(brave) = BraveSearch::from_env() {
        tools.register(Arc::new(BraveSearchTool::new(Arc::new(brave))));
        tracing::info!("registered Brave Search tool");
    }
    // OSINT — all free / no-key tools always registered
    tools.register(Arc::new(salesman_osint::GdeltTool::default()));
    tools.register(Arc::new(salesman_osint::GithubOrgTool::default()));
    tools.register(Arc::new(salesman_osint::HnTool::default()));
    tools.register(Arc::new(salesman_osint::WikipediaTool::default()));
    tools.register(Arc::new(salesman_osint::WaybackTool::default()));
    tools.register(Arc::new(salesman_osint::DnsInfoTool::default()));
    tools.register(Arc::new(DraftColdEmailTool::new(
        router.clone(),
        "the PlausiDen team",
        "PlausiDen",
        "Plausible deniability + sovereign data tools for SMB security teams.",
    )));
    tools.register(Arc::new(salesman_content::DraftReplyTool::new(
        router.clone(),
        "William",
        "PlausiDen",
        "Plausible deniability + sovereign data tools for SMB security teams.",
    )));
    tools.register(Arc::new(salesman_content::ReplyClassifyTool::new(router)));
    tools
}

fn parse_hint(s: &str) -> RouteHint {
    match s.to_ascii_lowercase().as_str() {
        "deep" | "deep_reasoning" | "opus" => RouteHint::DeepReasoning,
        "bulk" | "flash" => RouteHint::Bulk,
        "grounded" | "search" => RouteHint::Grounded,
        "sovereign" | "lfi" => RouteHint::Sovereign,
        _ => RouteHint::Reasoning,
    }
}

use std::str::FromStr;

fn truncate_name(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}

#[derive(Debug, serde::Deserialize)]
struct SequenceFile {
    steps: Vec<SequenceStepFile>,
}

#[derive(Debug, serde::Deserialize)]
struct SequenceStepFile {
    template_key: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    delay_days: Option<u32>,
}

async fn sqlx_lookup_sequence(
    state: &State,
    campaign_id: salesman_core::CampaignId,
    name: &str,
) -> Result<uuid::Uuid> {
    let row = sqlx::query("SELECT id FROM sequences WHERE campaign_id = $1 AND name = $2")
        .bind(campaign_id.0)
        .bind(name)
        .fetch_optional(state.pool())
        .await?;
    let row = row.ok_or_else(|| anyhow::anyhow!("sequence `{name}` not found in campaign"))?;
    let id: uuid::Uuid = row.try_get("id")?;
    Ok(id)
}

/// POST a text payload to a Slack / Discord / Mattermost incoming
/// webhook. Detects the platform by hostname so the JSON shape
/// matches: Slack expects `{ text }`, Discord expects `{ content }`,
/// generic falls back to Slack-shape (most platforms accept it).
async fn post_alert_webhook(url: &str, text: &str) -> anyhow::Result<()> {
    // SAFETY: rustls + 10s timeout — Client::build() cannot fail.
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest construction infallible with these settings");
    let lc = url.to_ascii_lowercase();
    let body = if lc.contains("discord") {
        serde_json::json!({ "content": text })
    } else if lc.contains("mattermost") {
        serde_json::json!({ "text": text })
    } else {
        // Slack + most everything else.
        serde_json::json!({ "text": text })
    };
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("webhook returned {status}: {body}");
    }
    Ok(())
}

/// Map a GDELT-format `seen_at` timestamp ("YYYYMMDDhhmmss") + a
/// horizon (max-age in days) to a recency score in [0,1]. Now → 1.0,
/// exactly `max_age_days` old → ~0.0. Half-life ≈ max_age_days/3.
fn recency_score_from_seen_at(seen_at: &str, max_age_days: i64) -> f32 {
    let now = chrono::Utc::now();
    let parsed = if seen_at.len() >= 14 {
        chrono::NaiveDateTime::parse_from_str(&seen_at[..14], "%Y%m%d%H%M%S").ok()
    } else if seen_at.len() >= 8 {
        chrono::NaiveDate::parse_from_str(&seen_at[..8], "%Y%m%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    } else {
        None
    };
    let dt = match parsed {
        Some(d) => d.and_utc(),
        None => return 0.5,
    };
    let age_secs = (now - dt).num_seconds().max(0) as f32;
    let max_secs = (max_age_days as f32) * 86400.0;
    if age_secs >= max_secs {
        return 0.0;
    }
    let half_life = max_secs / 3.0;
    (0.5f32).powf(age_secs / half_life).clamp(0.0, 1.0)
}

fn pct(n: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        (n as f32) / (total as f32) * 100.0
    }
}

/// Run `dig +short TXT <name>` and return one entry per line. Each
/// entry has its surrounding quotes stripped + adjacent quoted runs
/// concatenated (some long records come back as `"part1" "part2"`).
async fn dig_txt(name: &str) -> anyhow::Result<Vec<String>> {
    let out = tokio::process::Command::new("dig")
        .args(["+short", "TXT", name])
        .output()
        .await
        .with_context(|| format!("running dig +short TXT {name}"))?;
    if !out.status.success() {
        anyhow::bail!("dig exit {}", out.status);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(unquote_txt_record)
        .collect())
}

/// Strip the `"…"` quoting from a dig +short TXT line and concatenate
/// adjacent quoted spans (chunked records over 255 chars).
fn unquote_txt_record(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_quotes = false;
    for c in line.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if in_quotes {
            out.push(c);
        }
    }
    if out.is_empty() {
        // Not actually quoted (malformed dig output) — return raw.
        return line.to_string();
    }
    out
}

/// Run `dig +short -x <ip>` and return one PTR per line.
async fn dig_ptr(ip: &str) -> anyhow::Result<Vec<String>> {
    let out = tokio::process::Command::new("dig")
        .args(["+short", "-x", ip])
        .output()
        .await
        .with_context(|| format!("running dig +short -x {ip}"))?;
    if !out.status.success() {
        anyhow::bail!("dig exit {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Quote a CSV field per RFC 4180: wrap in `"` if the value contains
/// a comma, double-quote, CR, or LF; double-up internal `"`. Always
/// quote, regardless — a permanent quote is cheap and bullet-proofs
/// downstream re-import even when fields are empty.
fn csv_quote(s: &str) -> String {
    let escaped = s.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Parse one CSV row tolerantly. Handles RFC 4180 quoted fields with
/// embedded commas and doubled quotes; falls back to plain comma-split
/// when nothing is quoted (common for one-column email lists).
fn parse_csv_row(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            in_quotes = true;
        } else if c == ',' {
            out.push(std::mem::take(&mut field));
        } else {
            field.push(c);
        }
    }
    out.push(field);
    out
}

fn parse_touch_id(s: &str) -> Result<salesman_core::TouchId> {
    let u: uuid::Uuid = s
        .parse()
        .with_context(|| format!("not a valid uuid: {s}"))?;
    Ok(salesman_core::TouchId(u))
}

async fn require_state(database_url: Option<&str>) -> Result<State> {
    let url = database_url.context(
        "SALESMAN_DATABASE_URL not set (or pass --database-url) — required for db operations",
    )?;
    let state = State::connect(url).await?;
    Ok(state)
}

/// If the caller already pre-connected (state_arc is Some), reuse it —
/// avoids a second Postgres connect + skips the migrations check.
/// Allowed-dead because not every command path uses it yet; the
/// pre-connect at startup ALSO wires the LlmCallSink onto the
/// router, which is the primary reason for the up-front connect.
#[allow(dead_code)]
async fn state_or_connect(
    state_arc: &Option<Arc<State>>,
    database_url: Option<&str>,
) -> Result<State> {
    if let Some(s) = state_arc {
        return Ok((**s).clone());
    }
    require_state(database_url).await
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Connect to state up front IF a URL is provided. State doubles as
    // the LlmCallSink so router automatically records cost on every
    // chat() / chat_for() call.
    let state_arc: Option<Arc<State>> = if let Some(url) = &cli.database_url {
        match State::connect(url).await {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::warn!("%e" = %e, "DB pre-connect failed; commands needing state will retry");
                None
            }
        }
    } else {
        None
    };
    let sink: Option<Arc<dyn salesman_llm::LlmCallSink>> = state_arc
        .as_ref()
        .map(|s| Arc::clone(s) as Arc<dyn salesman_llm::LlmCallSink>);

    let router = Arc::new(build_router(&cli.claude_model, &cli.gemini_model, sink));
    let tools = Arc::new(build_tools(router.clone()));

    match cli.cmd {
        Cmd::Plan { goal, hint } => {
            let orch = Orchestrator::new(router, tools);
            let messages = vec![
                Message {
                    role: Role::System,
                    content: "You are PlausiDen-Salesman. \
                              Use tools to make progress on the goal. \
                              When the goal is satisfied, reply with one short summary \
                              line and stop calling tools."
                        .into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: format!("Goal: {goal}"),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ];
            match orch.run(parse_hint(&hint), messages).await {
                Ok(resp) => {
                    println!(
                        "ORCH OK ({:?}, {}ms): {}",
                        resp.finish_reason, resp.usage.latency_ms, resp.message.content
                    );
                    if !resp.message.tool_calls.is_empty() {
                        println!(
                            "(model also requested {} tool call(s))",
                            resp.message.tool_calls.len()
                        );
                    }
                }
                Err(e) => println!("ORCH ERR: {e}"),
            }
        }

        Cmd::Migrate => {
            let _state = require_state(cli.database_url.as_deref()).await?;
            println!("migrations applied (or already current)");
        }

        Cmd::Discover {
            campaign,
            goal,
            segment,
            from_csv,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let seed = CsvSeed::new();
            let companies = seed.read_path(&from_csv)?;
            let company_ids: Vec<_> = companies.iter().map(|c| c.id).collect();
            let inserted_companies = state.insert_companies(&companies).await?;
            let campaign_id = state.ensure_campaign(&campaign, &goal, &segment).await?;
            let inserted_prospects = state
                .upsert_prospects_for_campaign(campaign_id, &company_ids)
                .await?;
            println!(
                "campaign `{campaign}` (id={campaign_id}): \
                 parsed {} CSV row(s), inserted {} new companies, \
                 added {} new prospects",
                companies.len(),
                inserted_companies,
                inserted_prospects,
            );
        }

        Cmd::DiscoverSearch {
            campaign,
            query,
            top,
            persist,
        } => {
            // Hosts that aren't actual prospect company sites — strip
            // them so we don't import job boards / news mentions / wiki
            // entries as if they were companies. Conservative list;
            // operator can refine the query if a target host slips in.
            const NOISE_HOSTS: &[&str] = &[
                "wikipedia.org",
                "linkedin.com",
                "indeed.com",
                "glassdoor.com",
                "crunchbase.com",
                "youtube.com",
                "twitter.com",
                "x.com",
                "facebook.com",
                "github.com",
                "medium.com",
                "reddit.com",
                "ycombinator.com",
                "stackoverflow.com",
                "techcrunch.com",
                "forbes.com",
                "bloomberg.com",
                "g2.com",
                "capterra.com",
                "trustpilot.com",
            ];

            let brave = match salesman_discovery::BraveSearch::from_env() {
                Ok(b) => b,
                Err(e) => anyhow::bail!("DiscoverSearch needs BRAVE_SEARCH_API_KEY in env: {e}"),
            };

            // Brave caps at 20 per call; loop in pages for higher tops.
            let mut hits: Vec<salesman_discovery::SearchHit> = Vec::new();
            let mut remaining = top;
            while remaining > 0 {
                let n = remaining.min(20);
                let page = match brave.search(&query, n).await {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!("%e" = %e, "brave search page failed");
                        break;
                    }
                };
                if page.is_empty() {
                    break;
                }
                hits.extend(page);
                remaining = remaining.saturating_sub(n);
                if remaining > 0 {
                    // Brave self-throttles to 1 QPS internally; this
                    // is just a polite buffer.
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            }

            // Normalize each hit into a Company. Dedup by host.
            let mut seen_hosts: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            let mut companies: Vec<salesman_core::Company> = Vec::new();
            for h in &hits {
                let url = match url::Url::parse(&h.url) {
                    Ok(u) => u,
                    Err(_) => continue,
                };
                let host = url
                    .host_str()
                    .unwrap_or_default()
                    .trim_start_matches("www.")
                    .to_ascii_lowercase();
                if host.is_empty() {
                    continue;
                }
                if NOISE_HOSTS.iter().any(|n| host.ends_with(n)) {
                    continue;
                }
                if !seen_hosts.insert(host.clone()) {
                    continue;
                }
                // Display name: take the first segment of the title
                // before " | " or " - " — that's where most sites put
                // the company name vs page title. Fallback: host root.
                let display_name = h
                    .title
                    .split([':', '|', '-', '·'])
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| host.split('.').next().unwrap_or(&host).to_string());
                let homepage = url::Url::parse(&format!("https://{host}/")).ok();
                companies.push(salesman_core::Company {
                    id: salesman_core::CompanyId::new(),
                    legal_name: None,
                    display_name,
                    homepage,
                    industry: None,
                    size_band: None,
                    region: None,
                    description: Some(h.description.clone()).filter(|s| !s.is_empty()),
                    tech_signals: vec![],
                    discovered_at: chrono::Utc::now(),
                    last_enriched_at: None,
                    source: salesman_core::model::DiscoverySource::Search,
                    raw: std::collections::BTreeMap::new(),
                });
            }

            println!(
                "discover-search query=\"{query}\" — {} hit(s), \
                 {} candidate company(ies) after de-noise + dedup{}",
                hits.len(),
                companies.len(),
                if persist { "" } else { ", DRY-RUN" },
            );
            for c in &companies {
                println!(
                    "  - {:<40} {}",
                    c.display_name.chars().take(40).collect::<String>(),
                    c.homepage
                        .as_ref()
                        .map(|u| u.as_str())
                        .unwrap_or("(no homepage)"),
                );
            }
            if persist {
                let state = require_state(cli.database_url.as_deref()).await?;
                let cids: Vec<_> = companies.iter().map(|c| c.id).collect();
                let n_companies = state.insert_companies(&companies).await?;
                let campaign_id = state
                    .ensure_campaign(
                        &campaign,
                        &format!("autonomous discovery: {query}"),
                        "auto-discovered",
                    )
                    .await?;
                let n_prospects = state
                    .upsert_prospects_for_campaign(campaign_id, &cids)
                    .await?;
                println!(
                    "\npersisted into `{campaign}`: {n_companies} new \
                     companies, {n_prospects} new prospect rows.",
                );
            } else {
                println!(
                    "\n(dry-run) re-run with --persist to import. \
                     Refine the --query if these don't match the profile."
                );
            }
        }

        Cmd::DiscoverLlm {
            campaign,
            icp,
            top,
            persist,
        } => {
            // Ask the LLM for ~1.5x the requested top, since
            // homepage validation typically drops 20-40% to
            // hallucinated / dead / parking-page domains.
            let llm_target = ((top as f64 * 1.5).ceil() as u32).max(top);

            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "discover-llm needs at least one LLM backend registered \
                     (set ANTHROPIC_API_KEY / GEMINI_API_KEY for the API \
                     path, or SALESMAN_LLM_TRANSPORT=cli + login the \
                     subscriber CLIs for the CLI path)"
                );
            }

            let system = "You are a B2B prospect-discovery assistant. The operator \
                          gives you an Ideal Customer Profile (ICP) description. \
                          You return a JSON array of REAL companies that match — \
                          ones you have high confidence actually exist with the \
                          stated homepage. Do NOT invent companies. Do NOT include \
                          companies whose websites you are not certain are correct. \
                          When unsure, return fewer rather than fabricating.\n\n\
                          Output STRICT JSON only, no prose, no code fences:\n\
                          {\"companies\": [\
                          {\"display_name\": string, \
                            \"homepage\": string (full https://... URL), \
                            \"industry\": string, \
                            \"region\": string, \
                            \"description\": string (one short sentence), \
                            \"why_match\": string (one short sentence)\
                          }, ...]}\n\n\
                          Skip social media (linkedin.com, x.com, etc.), \
                          news sites (techcrunch.com, etc.), job boards, and \
                          aggregators (g2.com, capterra.com). Only company \
                          homepages.";

            let user = format!(
                "Ideal customer profile:\n{icp}\n\n\
                 Return up to {llm_target} companies that match. \
                 Quality over quantity — fewer real ones beat more guesses."
            );

            let req = salesman_llm::ChatRequest {
                messages: vec![
                    salesman_llm::Message {
                        role: salesman_llm::Role::System,
                        content: system.to_string(),
                        tool_calls: vec![],
                        tool_results: vec![],
                    },
                    salesman_llm::Message {
                        role: salesman_llm::Role::User,
                        content: user,
                        tool_calls: vec![],
                        tool_results: vec![],
                    },
                ],
                tools: vec![],
                max_tokens: 4096,
                temperature: 0.3,
            };

            println!("discover-llm: asking LLM for ~{llm_target} candidate companies...");
            let resp = router
                .chat_for(salesman_llm::RouteHint::Reasoning, "discover_llm", req)
                .await?;

            #[derive(serde::Deserialize)]
            struct LlmCompany {
                display_name: String,
                homepage: String,
                #[serde(default)]
                industry: String,
                #[serde(default)]
                region: String,
                #[serde(default)]
                description: String,
                #[serde(default)]
                why_match: String,
            }
            #[derive(serde::Deserialize)]
            struct LlmResponse {
                companies: Vec<LlmCompany>,
            }

            let raw = resp.message.content.trim().to_string();
            // Strip code fences if the model added them anyway.
            let stripped = raw
                .trim_start_matches("```json")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim();
            let candidates: Vec<LlmCompany> =
                match serde_json::from_str::<LlmResponse>(stripped) {
                    Ok(r) => r.companies,
                    Err(e) => {
                        // Try to find a JSON object inside the response.
                        match (stripped.find('{'), stripped.rfind('}')) {
                            (Some(s), Some(e2)) if e2 > s => {
                                match serde_json::from_str::<LlmResponse>(&stripped[s..=e2]) {
                                    Ok(r) => r.companies,
                                    Err(e3) => anyhow::bail!(
                                        "LLM output not parseable as JSON: {e3}. \
                                         First 200 chars: {}",
                                        stripped.chars().take(200).collect::<String>()
                                    ),
                                }
                            }
                            _ => anyhow::bail!(
                                "LLM output not parseable as JSON: {e}. \
                                 First 200 chars: {}",
                                stripped.chars().take(200).collect::<String>()
                            ),
                        }
                    }
                };

            println!("  LLM proposed {} candidate(s); validating homepages...", candidates.len());

            // Validate each homepage by HTTP fetch. Drop hallucinated
            // / dead / parked. Concurrency-capped at 8 — we're being
            // polite to small sites.
            let fetcher = std::sync::Arc::new(salesman_discovery::HomepageFetcher::new());
            let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
            let mut handles = Vec::with_capacity(candidates.len());
            for c in candidates {
                let fetcher = fetcher.clone();
                let sem = sem.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await.ok()?;
                    let url = url::Url::parse(&c.homepage).ok()?;
                    // Only http(s).
                    if !matches!(url.scheme(), "http" | "https") {
                        return None;
                    }
                    let host = url.host_str()?.trim_start_matches("www.").to_ascii_lowercase();
                    if host.is_empty() {
                        return None;
                    }
                    // Validate by fetching the homepage. 2xx with a
                    // non-empty title = real page.
                    let facts = fetcher.fetch(&url).await.ok()?;
                    if !(200..400).contains(&facts.status) {
                        return None;
                    }
                    if facts.title.as_deref().unwrap_or("").trim().is_empty() {
                        return None;
                    }
                    Some((c, facts, host))
                }));
            }

            let mut survivors: Vec<(LlmCompany, salesman_discovery::HomepageFacts, String)> =
                Vec::new();
            for h in handles {
                if let Ok(Some(s)) = h.await {
                    survivors.push(s);
                }
            }

            // Dedup by host — LLM sometimes lists subsidiary + parent.
            let mut seen: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            survivors.retain(|(_, _, h)| seen.insert(h.clone()));

            // Truncate to the requested top after validation.
            survivors.truncate(top as usize);

            println!(
                "  {} survivor(s) after homepage validation + dedup{}",
                survivors.len(),
                if persist { "" } else { ", DRY-RUN" },
            );
            for (c, facts, host) in &survivors {
                println!(
                    "  + {:<35} {:<35} status={} {}",
                    c.display_name.chars().take(34).collect::<String>(),
                    host.chars().take(34).collect::<String>(),
                    facts.status,
                    if !c.industry.is_empty() {
                        format!("[{}]", c.industry)
                    } else {
                        String::new()
                    },
                );
                if !c.why_match.is_empty() {
                    println!(
                        "      why: {}",
                        c.why_match.chars().take(120).collect::<String>()
                    );
                }
            }

            if !persist {
                println!(
                    "\n(dry-run) re-run with --persist to import {} prospect(s).",
                    survivors.len()
                );
                return Ok(());
            }

            if survivors.is_empty() {
                println!("\nNothing to persist; refine the --icp.");
                return Ok(());
            }

            let companies: Vec<salesman_core::Company> = survivors
                .iter()
                .map(|(c, facts, _)| salesman_core::Company {
                    id: salesman_core::CompanyId::new(),
                    legal_name: None,
                    display_name: c.display_name.clone(),
                    homepage: Some(facts.final_url.clone()),
                    industry: Some(c.industry.clone()).filter(|s| !s.is_empty()),
                    size_band: None,
                    region: Some(c.region.clone()).filter(|s| !s.is_empty()),
                    description: facts
                        .meta_description
                        .clone()
                        .or_else(|| Some(c.description.clone()).filter(|s| !s.is_empty())),
                    tech_signals: facts.tech_signals.clone(),
                    discovered_at: chrono::Utc::now(),
                    last_enriched_at: Some(chrono::Utc::now()),
                    source: salesman_core::model::DiscoverySource::Other,
                    raw: std::collections::BTreeMap::new(),
                })
                .collect();

            let state = require_state(cli.database_url.as_deref()).await?;
            let cids: Vec<_> = companies.iter().map(|c| c.id).collect();
            let n_companies = state.insert_companies(&companies).await?;
            let campaign_id = state
                .ensure_campaign(
                    &campaign,
                    &format!("autonomous LLM discovery: {icp}"),
                    "discover-llm",
                )
                .await?;
            let n_prospects = state
                .upsert_prospects_for_campaign(campaign_id, &cids)
                .await?;
            println!(
                "\npersisted into `{campaign}`: {n_companies} new \
                 companies, {n_prospects} new prospect rows.",
            );
        }

        Cmd::Enrich {
            campaign,
            concurrency,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(enrich-only)", "(unspecified)")
                .await?;
            let listings = state.list_companies_for_campaign(campaign_id).await?;
            let total = listings.len();
            let fetcher = Arc::new(HomepageFetcher::new());
            let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1) as usize));
            let mut tasks = Vec::new();
            for (id, name, homepage) in listings {
                let Some(homepage) = homepage else { continue };
                let url = match Url::parse(&homepage) {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::warn!(%name, err = %e, "skipping unparseable homepage");
                        continue;
                    }
                };
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let fetcher = fetcher.clone();
                tasks.push(tokio::spawn(async move {
                    let _permit = permit;
                    let result = fetcher.fetch(&url).await;
                    (id, name, result)
                }));
            }
            let mut ok = 0u32;
            let mut err = 0u32;
            for t in tasks {
                let (id, name, res) = t.await.unwrap();
                match res {
                    Ok(facts) => {
                        ok += 1;
                        tracing::info!(
                            %name,
                            status = facts.status,
                            title = ?facts.title,
                            signals = facts.tech_signals.len(),
                            "enriched"
                        );
                        if let Err(e) = state
                            .update_company_enrichment(
                                id,
                                facts.title.as_deref(),
                                facts.meta_description.as_deref(),
                                &facts.tech_signals,
                            )
                            .await
                        {
                            tracing::warn!(%name, "%e" = %e, "enrich write-back failed");
                            err += 1;
                            ok -= 1;
                        }
                    }
                    Err(e) => {
                        err += 1;
                        tracing::warn!(%name, "%e" = %e, "enrich failed");
                    }
                }
            }
            println!("enrich `{campaign}`: total={total} ok={ok} err={err}");
        }

        Cmd::Draft {
            campaign,
            product,
            angle_hint,
            skip_existing,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let campaign_id = state
                .ensure_campaign(&campaign, "(draft-only)", "(unspecified)")
                .await?;
            let prospects = state
                .list_prospects_with_facts_for_campaign(campaign_id)
                .await?;
            // Local-first: draft for prospects matching SALESMAN_TARGET_LOCALITY
            // first (no-op when unset).
            let loc_terms = target_locality_terms();
            let loc_refs: Vec<&str> = loc_terms.iter().map(|s| s.as_str()).collect();
            let prospects = order_local_first_with_terms(prospects, &loc_refs);
            let existing = state.list_drafts_awaiting_approval(campaign_id).await?;
            let existing_ids: std::collections::HashSet<_> =
                existing.iter().map(|t| t.prospect_id).collect();

            let draft_tool = DraftColdEmailTool::new(
                router.clone(),
                "the PlausiDen team",
                "PlausiDen",
                "Plausible deniability + sovereign data tools for SMB security teams.",
            );

            let mut ok = 0u32;
            let mut skipped = 0u32;
            let mut err = 0u32;
            for p in &prospects {
                if skip_existing && existing_ids.contains(&p.prospect_id) {
                    skipped += 1;
                    continue;
                }
                let prospect_json = p.to_prompt_json();
                let mut tool_args = serde_json::json!({
                    "prospect": prospect_json,
                    "product": product,
                });
                if let Some(h) = &angle_hint {
                    tool_args["angle_hint"] = serde_json::Value::String(h.clone());
                }
                let result =
                    salesman_tools::Tool::invoke(&draft_tool, salesman_core::ToolArgs(tool_args))
                        .await;
                match result {
                    Ok(v) => {
                        let subject = v
                            .get("subject")
                            .and_then(|x| x.as_str())
                            .unwrap_or("(no subject)");
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                        let produced_by = v.get("produced_by").cloned();
                        match state
                            .insert_touch_draft_full(
                                p.prospect_id,
                                salesman_core::TouchChannel::Email,
                                Some(subject),
                                body,
                                None,
                                produced_by,
                            )
                            .await
                        {
                            Ok(touch_id) => {
                                ok += 1;
                                tracing::info!(company = %p.display_name, %touch_id, "drafted");
                            }
                            Err(e) => {
                                err += 1;
                                tracing::warn!(company = %p.display_name, "%e" = %e, "draft persist failed");
                            }
                        }
                    }
                    Err(e) => {
                        err += 1;
                        tracing::warn!(company = %p.display_name, "%e" = %e, "draft generation failed");
                    }
                }
            }
            println!(
                "draft `{campaign}`: prospects={} ok={ok} skipped={skipped} err={err}",
                prospects.len()
            );
        }

        Cmd::Review { campaign } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(review-only)", "(unspecified)")
                .await?;
            let drafts = state.list_drafts_awaiting_approval(campaign_id).await?;
            if drafts.is_empty() {
                println!("no drafts awaiting approval in `{campaign}`");
            } else {
                println!("=== {} drafts awaiting approval ===\n", drafts.len());
                for (i, t) in drafts.iter().enumerate() {
                    let pb_tag = match (t.via_fallback(), t.produced_by_short()) {
                        (true, Some(s)) => format!(", produced_by={s} **FALLBACK**"),
                        (false, Some(s)) => format!(", produced_by={s}"),
                        _ => String::new(),
                    };
                    println!(
                        "--- [{}] {} (touch {}, {}{pb_tag})",
                        i + 1,
                        t.company,
                        t.touch_id,
                        t.channel
                    );
                    if let Some(s) = &t.subject {
                        println!("Subject: {s}");
                    }
                    println!();
                    println!("{}", t.body);
                    println!();
                }
            }
        }

        Cmd::QuickStub { campaign, count } => {
            // Synthetic-but-plausible prospect templates. Cycled
            // for `count`; each rotation gets a unique slug appended
            // so the (campaign, company) UNIQUE constraint never
            // collides on repeat runs.
            const STUBS: &[(&str, &str, &str, salesman_core::model::SizeBand, &str)] = &[
                ("Acme Logging Co",     "https://acme-logging.example",     "B2B SaaS",     salesman_core::model::SizeBand::Small,      "Self-hosted log aggregation for security teams"),
                ("Beta Devops",         "https://beta-devops.example",      "Devtools",     salesman_core::model::SizeBand::Mid,        "CI/CD platform for monorepos"),
                ("Gamma Industrial",    "https://gamma-industrial.example", "Manufacturing",salesman_core::model::SizeBand::Enterprise, "IoT telemetry + edge compute"),
                ("Delta Cyber",         "https://delta-cyber.example",      "Security",     salesman_core::model::SizeBand::Small,      "Threat-intel platform for MSSPs"),
                ("Epsilon Health",      "https://epsilonhealth.example",    "Healthtech",   salesman_core::model::SizeBand::Mid,        "GDPR-first patient-data analytics"),
                ("Zeta Mobility",       "https://zeta-mobility.example",    "Logistics",    salesman_core::model::SizeBand::Mid,        "Fleet routing + driver-app platform"),
                ("Eta Climate",         "https://etaclimate.example",       "ClimateTech",  salesman_core::model::SizeBand::Small,      "Carbon-accounting for mid-market"),
                ("Theta Finserv",       "https://thetafinserv.example",     "FinTech",      salesman_core::model::SizeBand::Mid,        "Compliance-first payments rails"),
            ];

            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(quick-stub)", "synthetic-smoke")
                .await?;

            let suffix = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
            let mut companies: Vec<salesman_core::Company> = Vec::with_capacity(count);
            for i in 0..count {
                let (name, homepage, industry, size_band, description) =
                    STUBS[i % STUBS.len()];
                // Disambiguator so re-running quick-stub doesn't
                // collide on the (campaign, company) UNIQUE.
                let display_name = format!("{name} #{suffix}-{i}");
                companies.push(salesman_core::Company {
                    id: salesman_core::CompanyId::new(),
                    legal_name: Some(display_name.clone()),
                    display_name,
                    homepage: url::Url::parse(homepage).ok(),
                    industry: Some(industry.to_string()),
                    size_band: Some(size_band),
                    region: Some("US".to_string()),
                    description: Some(description.to_string()),
                    tech_signals: vec![],
                    discovered_at: chrono::Utc::now(),
                    last_enriched_at: None,
                    source: salesman_core::model::DiscoverySource::OwnerSeed,
                    raw: std::collections::BTreeMap::new(),
                });
            }
            let cids: Vec<_> = companies.iter().map(|c| c.id).collect();
            let n_companies = state.insert_companies(&companies).await?;
            let n_prospects = state
                .upsert_prospects_for_campaign(campaign_id, &cids)
                .await?;
            println!(
                "quick-stub `{campaign}`: {n_companies} new companies, \
                 {n_prospects} new prospects (suffix=`{suffix}`).\n\n\
                 Smoke-test the full pipeline:\n  \
                 salesman draft --campaign {campaign} --product Sentinel\n  \
                 salesman fact-check --campaign {campaign}\n  \
                 salesman approve-all --campaign {campaign} --dry-run\n  \
                 salesman send-pending --campaign {campaign}    # default DRY-RUN",
            );
        }

        Cmd::FactCheck {
            campaign,
            threshold,
            verbose,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(fact-check-only)", "(unspecified)")
                .await?;
            let drafts = state.list_drafts_awaiting_approval(campaign_id).await?;
            if drafts.is_empty() {
                println!("no drafts awaiting approval in `{campaign}`");
            } else {
                let mut bad = 0u32;
                let mut clean = 0u32;
                let mut no_facts = 0u32;
                println!(
                    "scanning {} draft(s) in `{campaign}` (threshold {:.2}) …\n",
                    drafts.len(),
                    threshold
                );
                for t in &drafts {
                    let facts = state.touch_facts(t.touch_id).await.unwrap_or(None);
                    if facts.is_none() {
                        no_facts += 1;
                    }
                    let risk = salesman_detector::score_with_facts(
                        &t.body,
                        t.subject.as_deref(),
                        facts.as_ref(),
                    );
                    let fails = !risk.passes(threshold);
                    if fails {
                        bad += 1;
                        println!(
                            "FAIL  touch={} company={:?} score={:.2}",
                            t.touch_id, t.company, risk.score
                        );
                        for r in risk.reasons() {
                            println!("        {r}");
                        }
                        println!("        reject: salesman reject --touch {}", t.touch_id);
                    } else {
                        clean += 1;
                        if verbose {
                            println!(
                                "ok    touch={} company={:?} score={:.2}",
                                t.touch_id, t.company, risk.score
                            );
                        }
                    }
                }
                println!(
                    "\nfact-check complete: {clean} clean, {bad} flagged, \
                     {no_facts} draft(s) had no facts available (gate downgraded \
                     to soft heuristic for those)"
                );
                if bad > 0 {
                    anyhow::bail!("{bad} draft(s) failed the fact-trace gate");
                }
            }
        }

        Cmd::ApproveAll {
            campaign,
            threshold,
            dry_run,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(approve-all)", "(unspecified)")
                .await?;
            let drafts = state.list_drafts_awaiting_approval(campaign_id).await?;
            if drafts.is_empty() {
                println!("no drafts awaiting approval in `{campaign}`");
            } else {
                let mut approved = 0u32;
                let mut held = 0u32;
                println!(
                    "evaluating {} draft(s) in `{campaign}` (threshold {:.2}{}) …\n",
                    drafts.len(),
                    threshold,
                    if dry_run { ", DRY-RUN" } else { "" },
                );
                for t in &drafts {
                    let facts = state.touch_facts(t.touch_id).await.unwrap_or(None);
                    let risk = salesman_detector::score_with_facts(
                        &t.body,
                        t.subject.as_deref(),
                        facts.as_ref(),
                    );
                    if risk.passes(threshold) {
                        if dry_run {
                            println!(
                                "[DRY-RUN] would approve  touch={} company={:?} score={:.2}",
                                t.touch_id, t.company, risk.score
                            );
                            approved += 1;
                        } else {
                            match state.approve_touch(t.touch_id).await {
                                Ok(n) if n > 0 => {
                                    approved += 1;
                                    println!(
                                        "approved  touch={} company={:?} score={:.2}",
                                        t.touch_id, t.company, risk.score
                                    );
                                }
                                Ok(_) => {
                                    held += 1;
                                    tracing::warn!(
                                        touch=%t.touch_id,
                                        "approve_touch affected 0 rows — state changed under us",
                                    );
                                }
                                Err(e) => {
                                    held += 1;
                                    tracing::warn!(
                                        touch=%t.touch_id, "%e" = %e,
                                        "approve_touch failed — leaving in queue",
                                    );
                                }
                            }
                        }
                    } else {
                        held += 1;
                        println!(
                            "HELD      touch={} company={:?} score={:.2}",
                            t.touch_id, t.company, risk.score
                        );
                        for r in risk.reasons() {
                            println!("            {r}");
                        }
                    }
                }
                println!(
                    "\napprove-all complete: {approved} approved, {held} held \
                     for manual review.{}",
                    if dry_run { " (no state changes)" } else { "" },
                );
            }
        }

        Cmd::NextBestActions {
            trigger_window_hours,
            per_category,
            campaign,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;

            // (a) replies waiting for a response — highest priority.
            // Fast responses close deals; slow responses lose them.
            let replies = state
                .list_replies_needing_response(per_category)
                .await
                .unwrap_or_default();

            // (b) recent trigger events. Optional campaign scope.
            let trigger_campaign = if let Some(c) = campaign.as_ref() {
                Some(
                    state
                        .ensure_campaign(c, "(nba-only)", "(unspecified)")
                        .await?,
                )
            } else {
                None
            };
            let triggers = state
                .list_trigger_events(trigger_campaign, trigger_window_hours, true, per_category)
                .await
                .unwrap_or_default();

            // (c) paused / stalled prospects in their cadence — these
            // need a manual nudge or a `cadence resume`.
            let paused = state
                .list_paused_prospects(per_category)
                .await
                .unwrap_or_default();

            // (d) referral-ask candidates: won prospects 30+ days old
            // we haven't yet asked for a referral.
            let referrals = state
                .list_won_prospects_for_referral_ask(30, per_category)
                .await
                .unwrap_or_default();

            println!(
                "\n=== next-best-actions ({}) ===\n",
                chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
            );

            let mut total = 0usize;
            let mut idx = 1usize;

            if !replies.is_empty() {
                println!("[1] reply needing response — RESPOND TODAY");
                for r in &replies {
                    println!(
                        "  {idx:>2}. {company} ({kind})  →  salesman draft-replies (then `review`)",
                        company = r.company_name,
                        kind = r.inbound_kind,
                    );
                    idx += 1;
                    total += 1;
                }
                println!();
            }

            if !triggers.is_empty() {
                println!(
                    "[2] trigger event in last {trigger_window_hours}h — REACH OUT WHILE FRESH"
                );
                for t in &triggers {
                    println!(
                        "  {idx:>2}. {company}: {headline}  →  salesman draft --campaign <c> (cite the trigger)",
                        company = t.company,
                        headline = t.headline.chars().take(70).collect::<String>(),
                    );
                    idx += 1;
                    total += 1;
                }
                println!();
            }

            if !paused.is_empty() {
                println!("[3] cadence paused / stalled — DECIDE: revive or close");
                for (pid, company, reason, last) in &paused {
                    println!(
                        "  {idx:>2}. {company}: paused since {last} ({reason})  →  salesman cadence resume --prospect-id {pid}",
                        last = last.format("%Y-%m-%d"),
                    );
                    idx += 1;
                    total += 1;
                }
                println!();
            }

            if !referrals.is_empty() {
                println!("[4] won prospects past 30d — ASK FOR REFERRAL");
                for r in &referrals {
                    println!(
                        "  {idx:>2}. {company}  →  salesman referral-ask --prospect-id {pid}",
                        company = r.display_name,
                        pid = r.prospect_id.0,
                    );
                    idx += 1;
                    total += 1;
                }
                println!();
            }

            if total == 0 {
                println!(
                    "no actions queued. either you're caught up, or no \
                     pipeline data is in yet. run `salesman triggers scan`, \
                     `salesman inbox-poll`, and `salesman classify-replies` \
                     to populate the categories."
                );
            } else {
                println!(
                    "{total} action(s) ranked by closing-deals leverage. \
                     replies > triggers > stalled > referrals.",
                );
            }
        }

        Cmd::Approve {
            touch,
            detector_threshold,
            force_override,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let touch_id = parse_touch_id(&touch)?;

            let (subject, body, outcome) = state
                .get_touch_for_review(touch_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("touch {touch} not found"))?;
            if outcome != "awaiting_approval" {
                anyhow::bail!("touch {touch} is in `{outcome}`, not awaiting_approval");
            }

            // Fact-trace gate (U44): pull the prospect facts the
            // drafter saw, so the detector can verify that any
            // numeric claim in the body traces back to real input
            // data — not a fabricated stat.
            let facts = state.touch_facts(touch_id).await.unwrap_or(None);
            let risk =
                salesman_detector::score_with_facts(&body, subject.as_deref(), facts.as_ref());
            if !risk.passes(detector_threshold) {
                if let Some(ref reason) = force_override {
                    tracing::warn!(
                        score = risk.score,
                        threshold = detector_threshold,
                        %reason,
                        "OPERATOR OVERRIDE — approving despite detector failure"
                    );
                    println!(
                        "WARN: approving despite detector score {:.2} >= {:.2} (override: {})",
                        risk.score, detector_threshold, reason
                    );
                    for r in risk.reasons() {
                        println!("  detector: {r}");
                    }
                } else {
                    println!(
                        "REFUSED: detector score {:.2} >= threshold {:.2}. Reasons:",
                        risk.score, detector_threshold
                    );
                    for r in risk.reasons() {
                        println!("  {r}");
                    }
                    println!(
                        "\nIf you've reviewed and want to send anyway, pass \
                         --force-override \"<your reason>\""
                    );
                    anyhow::bail!("approval refused by detector gate");
                }
            } else if !risk.hits.is_empty() {
                tracing::info!(score = risk.score, "detector found minor hits but passed");
            }

            let n = state.approve_touch(touch_id).await?;
            if n == 0 {
                anyhow::bail!("touch {touch} state changed under us — re-check");
            }
            println!("approved touch {touch} (detector score {:.2})", risk.score);
        }

        Cmd::Reject { touch } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let touch_id = parse_touch_id(&touch)?;
            let n = state.reject_touch(touch_id).await?;
            if n == 0 {
                anyhow::bail!("touch {touch} not found OR not in awaiting_approval state");
            }
            println!("rejected touch {touch}");
        }

        Cmd::ApproveBatch {
            campaign,
            max,
            detector_threshold,
            force_override,
            confirm_typed,
        } => {
            if !confirm_typed {
                anyhow::bail!(
                    "approve-batch requires --confirm-typed (type the campaign name to proceed)"
                );
            }
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(approve-batch)", "(unspecified)")
                .await?;
            let pending = state.list_drafts_awaiting_approval(campaign_id).await?;
            let target_n = (max as usize).min(pending.len());
            println!(
                "approve-batch `{campaign}`: {} pending, will attempt up to {target_n}",
                pending.len()
            );

            // Typed confirmation
            {
                use dialoguer::Input;
                let typed: String = Input::new()
                    .with_prompt(format!(
                        "Type the campaign name (`{campaign}`) to confirm bulk approve of up to {target_n} touches"
                    ))
                    .interact_text()
                    .map_err(|e| anyhow::anyhow!("dialoguer: {e}"))?;
                if typed.trim() != campaign {
                    anyhow::bail!("typed campaign name did not match — aborting");
                }
            }

            let mut approved = 0u32;
            let mut blocked_detector = 0u32;
            let mut overrode = 0u32;
            let mut errored = 0u32;
            for t in pending.into_iter().take(target_n) {
                let risk = salesman_detector::score(&t.body, t.subject.as_deref());
                if !risk.passes(detector_threshold) {
                    if let Some(reason) = force_override.as_deref() {
                        tracing::warn!(
                            touch=%t.touch_id,
                            score=risk.score,
                            threshold=detector_threshold,
                            %reason,
                            "OPERATOR OVERRIDE — bulk-approving despite detector failure"
                        );
                        overrode += 1;
                    } else {
                        blocked_detector += 1;
                        tracing::warn!(
                            touch=%t.touch_id,
                            score=risk.score,
                            "blocked by detector; pass --force-override to apply to whole batch"
                        );
                        continue;
                    }
                }
                match state.approve_touch(t.touch_id).await {
                    Ok(1) => approved += 1,
                    Ok(_) => {
                        // already changed under us (race) — count as errored for visibility
                        errored += 1;
                        tracing::warn!(touch=%t.touch_id, "approve returned 0 rows (race)");
                    }
                    Err(e) => {
                        errored += 1;
                        tracing::warn!(touch=%t.touch_id, "%e" = %e, "approve failed");
                    }
                }
            }
            println!(
                "approve-batch result: approved={approved} blocked_detector={blocked_detector} \
                 overridden={overrode} errored={errored}"
            );
        }

        Cmd::Suppress {
            target,
            kind,
            reason,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let kind = kind.unwrap_or_else(|| {
                if target.contains('@') {
                    "email".into()
                } else {
                    "domain".into()
                }
            });
            state
                .add_suppression(&target, &kind, &reason, "manual")
                .await?;
            let n = state.count_suppressions().await?;
            println!("suppressed {kind}={target} ({reason}); total suppressions: {n}");
        }

        Cmd::Audit { limit } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let receipts = state.list_recent_receipts(limit).await?;
            println!("=== {} most recent receipts ===", receipts.len());
            let signer = Signer::load_or_generate(&default_seed_path(), "salesman-default-1")?;
            let vk = signer.verifying_key();
            let mut bad = 0u32;
            for r in &receipts {
                let ok = salesman_receipts::verify_receipt(r, &vk).is_ok();
                if !ok {
                    bad += 1;
                }
                println!(
                    "{} | {} | {} | {} | sig={}",
                    r.created_at.to_rfc3339(),
                    r.event_kind,
                    salesman_receipts::hash_to_hex(&r.hash[..8.min(r.hash.len())]),
                    if ok { "OK" } else { "BAD" },
                    salesman_receipts::hash_to_hex(&r.signature[..8.min(r.signature.len())])
                );
            }
            if bad > 0 {
                println!("\n!! {bad} receipts FAILED verification — investigate immediately");
            }
        }

        Cmd::AuditChain { limit } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let receipts = state.list_receipts_oldest_first(limit).await?;
            println!("=== verifying chain over {} receipts ===", receipts.len());
            if receipts.is_empty() {
                println!("(no receipts to verify)");
                return Ok(());
            }
            let signer = Signer::load_or_generate(&default_seed_path(), "salesman-default-1")?;
            let vk = signer.verifying_key();
            let genesis = vec![0u8; salesman_receipts::HASH_LEN];

            // Walk the chain manually so we can pinpoint the first
            // break + still get a per-receipt sig verdict instead of
            // bailing on the whole call.
            let mut expected_prev = genesis.clone();
            let mut sig_failures = 0u32;
            let mut chain_break_at: Option<usize> = None;
            for (i, r) in receipts.iter().enumerate() {
                let prev_ok = r.prev_hash == expected_prev;
                if !prev_ok && chain_break_at.is_none() {
                    chain_break_at = Some(i);
                }
                let sig_ok = salesman_receipts::verify_receipt(r, &vk).is_ok();
                if !sig_ok {
                    sig_failures += 1;
                }
                if !prev_ok || !sig_ok {
                    println!(
                        "[{i:>5}] {ts} | {kind:<24} | prev_hash {prev} | sig {sig}",
                        ts = r.created_at.to_rfc3339(),
                        kind = r.event_kind,
                        prev = if prev_ok { "OK " } else { "BREAK" },
                        sig = if sig_ok { "OK " } else { "BAD" },
                    );
                }
                // Always advance expected_prev to this row's hash so
                // we don't cascade-flag every subsequent row after a
                // break — only report the FIRST break.
                expected_prev = r.hash.clone();
            }

            println!();
            println!("==========================================");
            match (chain_break_at, sig_failures) {
                (None, 0) => {
                    println!(
                        "VERDICT: GREEN — chain intact across {} receipts; no sig failures",
                        receipts.len()
                    );
                }
                (Some(idx), n) => {
                    println!(
                        "VERDICT: RED — first chain break at index {idx} \
                         ({}); {n} signature failure(s) total",
                        receipts[idx].created_at.to_rfc3339()
                    );
                    anyhow::bail!("audit-chain: chain broken");
                }
                (None, n) => {
                    println!("VERDICT: RED — chain links intact but {n} signature failure(s)");
                    anyhow::bail!("audit-chain: signature failures");
                }
            }
        }

        Cmd::SendPending {
            campaign,
            for_real,
            per_recipient_window_hours,
            per_recipient_max,
            per_domain_window_hours,
            per_domain_max,
            domain_quarantine_threshold,
            max_batch,
            no_warmup,
            ack_new_domains,
            no_pause,
            confirm_typed,
            test_send_to,
            require_primary,
        } => {
            let warmup = !no_warmup;
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(send-only)", "(unspecified)")
                .await?;

            // Sender-warmup gradient: a young campaign on a fresh
            // sender domain MUST start small and ramp. Reputation
            // damage from a cold-flood is permanent and the cost of
            // recovery is months. The curve below is conservative and
            // matches what mailbox providers (especially Gmail) expect
            // — see Postmaster Tools docs.
            let age_days = state.campaign_age_days(campaign_id).await.unwrap_or(0);
            let warmup_cap: u32 = if warmup {
                match age_days {
                    0..=2 => 5,
                    3..=6 => 10,
                    7..=13 => 25,
                    _ => 100,
                }
            } else {
                u32::MAX
            };
            let effective_max_batch = max_batch.min(warmup_cap);
            if warmup && effective_max_batch < max_batch {
                println!(
                    "warmup: campaign age {age_days}d → cap {effective_max_batch}/batch \
                     (operator passed --max-batch={max_batch}; gradient takes precedence). \
                     Pass --no-warmup to override (NOT recommended)."
                );
            }
            // Re-bind so the rest of the function uses the warmup-
            // adjusted cap without further changes.
            let max_batch = effective_max_batch;

            let approved_all = state.list_approved_touches(campaign_id).await?;

            // ----- backend-health gate (U22) -----
            // produced_by tags drafts with the LLM backend that wrote
            // them. When the primary backend fails, the router falls
            // back to a secondary — usually a less capable model.
            // --require-primary refuses to send touches whose
            // via_fallback is true, so an operator can re-draft them
            // before they ship. Surfaced inline + as a counter.
            let fallback_count = approved_all.iter().filter(|t| t.via_fallback()).count();
            let approved: Vec<_> = if require_primary {
                approved_all
                    .iter()
                    .filter(|t| !t.via_fallback())
                    .cloned()
                    .collect()
            } else {
                approved_all.clone()
            };
            if require_primary && fallback_count > 0 {
                println!(
                    "backend-health: --require-primary set; {fallback_count} \
                     fallback-generated touch(es) WILL BE SKIPPED. Re-draft \
                     them with the primary model or unset the flag."
                );
            }

            // ----- reputation pre-flight (BEFORE any SMTP work) ---
            // Pre-resolve every to-address so we can count distinct
            // domains and compare against the previously-touched set.
            let mut to_addresses: Vec<(salesman_core::TouchId, String)> =
                Vec::with_capacity(approved.len());
            for t in &approved {
                if let Some(addr) = state.touch_to_address(t.touch_id).await? {
                    to_addresses.push((t.touch_id, addr));
                }
            }
            let distinct_domains: std::collections::BTreeSet<String> = to_addresses
                .iter()
                .filter_map(|(_, a)| a.rsplit_once('@').map(|(_, d)| d.to_lowercase()))
                .collect();

            // Count NEW domains — domains we've never sent to before
            // in this campaign. Best-effort; failures don't block
            // (we'd rather under-block than over-block; suppression +
            // rate caps still apply).
            let mut new_domain_count = 0u32;
            for d in &distinct_domains {
                let n = state
                    .count_touches_to_domain_since(d, 24 * 365 * 10)
                    .await
                    .unwrap_or(0);
                if n == 0 {
                    new_domain_count += 1;
                }
            }

            println!(
                "\n=== send-pending pre-flight ===\n\
                 campaign:           {campaign}\n\
                 mode:               {}\n\
                 approved touches:   {} (in queue: {}; fallback-generated: {})\n\
                 backend-health:     {}\n\
                 with to-address:    {}\n\
                 distinct domains:   {}\n\
                 NEW domains:        {} (limit --ack-new-domains={})\n\
                 max-batch:          {}\n\
                 per-recipient cap:  {} per {}h\n\
                 per-domain cap:     {} per {}h\n",
                if for_real { "REAL" } else { "DRY-RUN" },
                approved.len(),
                approved_all.len(),
                fallback_count,
                if require_primary {
                    "--require-primary (fallback drafts skipped)"
                } else if fallback_count > 0 {
                    "fallback drafts INCLUDED (pass --require-primary to skip)"
                } else {
                    "all primary"
                },
                to_addresses.len(),
                distinct_domains.len(),
                new_domain_count,
                ack_new_domains,
                max_batch,
                per_recipient_max,
                per_recipient_window_hours,
                per_domain_max,
                per_domain_window_hours,
            );

            if new_domain_count > ack_new_domains {
                anyhow::bail!(
                    "REFUSED: {} new domains in this batch exceeds --ack-new-domains={}.\n\
                     Reputation safeguard. Either approve fewer drafts to new \
                     domains, or raise --ack-new-domains explicitly after \
                     reviewing the list.",
                    new_domain_count,
                    ack_new_domains
                );
            }

            // --- test-send-to: ONE message to the test inbox, then exit
            if let Some(test_addr) = test_send_to.as_ref() {
                if !for_real {
                    anyhow::bail!(
                        "--test-send-to requires --for-real (it actually sends one message)"
                    );
                }
                let Some(first) = approved.first() else {
                    anyhow::bail!("no approved touches to test-send");
                };
                let cfg = SmtpConfig::from_env()?;
                let sender = SmtpSender::new(cfg)?;
                let subject = format!(
                    "[salesman test-send to {test_addr}] {}",
                    first.subject.as_deref().unwrap_or("(no subject)")
                );
                let mut body = format!(
                    "TEST-SEND PROOF\n\
                     ---------------\n\
                     This is a redirected copy of touch {} from campaign `{campaign}`.\n\
                     Real recipient would be: {}\n\
                     Touch is NOT marked sent. No receipt logged.\n\
                     The body that would land in the real recipient's inbox follows below.\n\
                     ===========================================\n\n",
                    first.touch_id,
                    state
                        .touch_to_address(first.touch_id)
                        .await?
                        .unwrap_or_else(|| "(no contact)".into())
                );
                body.push_str(&first.body);
                let outcome = sender.send_email(test_addr, &subject, &body).await?;
                println!(
                    "test-send OK: to={test_addr} touch={} smtp_response={}",
                    first.touch_id, outcome.smtp_response_code
                );
                return Ok(());
            }

            // Strongest gate: typed confirmation (requires TTY).
            if for_real && confirm_typed {
                use dialoguer::Input;
                let typed: String = Input::new()
                    .with_prompt(format!(
                        "Type the campaign name (`{campaign}`) to confirm REAL send"
                    ))
                    .interact_text()
                    .map_err(|e| anyhow::anyhow!("dialoguer: {e}"))?;
                if typed.trim() != campaign {
                    anyhow::bail!("typed campaign name did not match — aborting");
                }
            }

            if for_real && !no_pause {
                println!("Starting REAL send in 5s — Ctrl-C to abort.\n");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }

            // Real-send mode requires SMTP env. Dry-run is fine without it.
            let sender = if for_real {
                let cfg = SmtpConfig::from_env()?;
                Some(SmtpSender::new(cfg)?)
            } else {
                None
            };

            let signer = if for_real {
                Some(Signer::load_or_generate(
                    &default_seed_path(),
                    "salesman-default-1",
                )?)
            } else {
                None
            };

            let mut sent = 0u32;
            let mut blocked_supp = 0u32;
            let mut blocked_rate = 0u32;
            let mut blocked_domain_quarantine = 0u32;
            let mut blocked_no_to = 0u32;
            let mut errored = 0u32;
            let mut bounced = 0u32;
            let mut attempted = 0u32;
            let mut hit_max_batch = false;

            for t in &approved {
                if attempted >= max_batch {
                    hit_max_batch = true;
                    break;
                }
                attempted += 1;
                let to = match state.touch_to_address(t.touch_id).await? {
                    Some(addr) => addr,
                    None => {
                        blocked_no_to += 1;
                        tracing::warn!(touch=%t.touch_id, company=%t.company, "no to-address (no primary contact email) — skipping");
                        continue;
                    }
                };
                if state.is_suppressed(&to).await? {
                    blocked_supp += 1;
                    tracing::warn!(to=%to, "suppressed — skipping");
                    continue;
                }
                let n_recipient = state
                    .count_touches_to_email_since(&to, per_recipient_window_hours)
                    .await?;
                if n_recipient >= per_recipient_max {
                    blocked_rate += 1;
                    tracing::warn!(to=%to, n=%n_recipient, "per-recipient cap hit — skipping");
                    continue;
                }
                let domain = to
                    .rsplit_once('@')
                    .map(|(_, d)| d.to_string())
                    .unwrap_or_default();
                let n_domain = state
                    .count_touches_to_domain_since(&domain, per_domain_window_hours)
                    .await?;
                if n_domain >= per_domain_max {
                    blocked_rate += 1;
                    tracing::warn!(domain=%domain, n=%n_domain, "per-domain cap hit — skipping");
                    continue;
                }
                // Soft-quarantine on rolling 24h hard-bounce count.
                // 0 disables; otherwise compare against the operator-
                // chosen threshold. Not counted as blocked_rate
                // because the cause is signal quality (junk list /
                // tarpit), not volume.
                if domain_quarantine_threshold > 0 {
                    let n_bounces = state.count_bounces_to_domain_since(&domain, 24).await?;
                    if n_bounces >= domain_quarantine_threshold {
                        blocked_domain_quarantine += 1;
                        tracing::warn!(
                            domain=%domain, n_bounces=%n_bounces,
                            threshold=%domain_quarantine_threshold,
                            "domain quarantined (recent hard-bounce rate) — skipping",
                        );
                        continue;
                    }
                }

                if !for_real {
                    let pb_tag = match (t.via_fallback(), t.produced_by_short()) {
                        (true, Some(s)) => format!(" produced_by={s} (FALLBACK)"),
                        (false, Some(s)) => format!(" produced_by={s}"),
                        _ => String::new(),
                    };
                    println!(
                        "[DRY-RUN] would send: to={to} subject={:?} touch={}{pb_tag}",
                        t.subject, t.touch_id
                    );
                    continue;
                }

                let sender = sender.as_ref().expect("for_real implies sender");
                let signer = signer.as_ref().expect("for_real implies signer");

                let subject = t.subject.clone().unwrap_or_default();
                let outcome = match sender.send_email(&to, &subject, &t.body).await {
                    Ok(o) => o,
                    Err(e) => {
                        let err_text = format!("{e}");
                        let failure = salesman_outreach::classify_smtp_failure(&err_text);
                        if failure.should_auto_suppress() {
                            // Hard bounce: the recipient mailbox / domain is gone.
                            // Add to global suppression so we never retry, and log
                            // a structured event the operator can audit later.
                            match state
                                .add_suppression(
                                    &to,
                                    "email",
                                    &format!("auto-suppress on hard bounce: {failure}"),
                                    failure.suppression_source(),
                                )
                                .await
                            {
                                Ok(()) => {
                                    bounced += 1;
                                    tracing::warn!(
                                        to=%to,
                                        failure=%failure,
                                        "hard bounce — auto-suppressed",
                                    );
                                    println!(
                                        "bounced+suppressed: to={to} touch={} reason={failure}",
                                        t.touch_id
                                    );
                                }
                                Err(supp_err) => {
                                    errored += 1;
                                    tracing::error!(
                                        to=%to, failure=%failure, "%e" = %supp_err,
                                        "could not record auto-suppression on bounce",
                                    );
                                }
                            }
                        } else {
                            errored += 1;
                            tracing::warn!(
                                to=%to, classification=%failure, "%e" = %err_text,
                                "smtp send failed",
                            );
                        }
                        continue;
                    }
                };

                // Build + persist receipt + mark sent.
                let prev_hash = state.get_last_hash(signer.key_id()).await?;
                let payload = serde_json::json!({
                    "kind": "send.email",
                    "touch_id": t.touch_id,
                    "to": outcome.to,
                    "from": outcome.from,
                    "subject": outcome.subject,
                    "smtp_response_code": outcome.smtp_response_code,
                    "smtp_message_id": outcome.smtp_message_id,
                });
                let receipt = signer.sign_event("send.email", payload, &prev_hash)?;
                let receipt_id = receipt.id;
                state.insert_receipt(&receipt).await?;
                let n = state
                    .mark_touch_sent(t.touch_id, receipt_id, outcome.sent_at)
                    .await?;
                if n == 1 {
                    sent += 1;
                    // Queue the owner audit-notification (who/how/what was
                    // sent) so the operator has a durable record. Best-effort:
                    // the send already happened, so an enqueue failure must
                    // not fail the run — just warn.
                    if let Err(e) = state.enqueue_owner_notification_for_touch(t.touch_id).await {
                        tracing::warn!(touch = %t.touch_id, error = %e, "owner-notification enqueue failed");
                    }
                    let pb_tag = match (t.via_fallback(), t.produced_by_short()) {
                        (true, Some(s)) => format!(" produced_by={s} (FALLBACK)"),
                        (false, Some(s)) => format!(" produced_by={s}"),
                        _ => String::new(),
                    };
                    println!(
                        "sent: to={to} touch={} receipt={receipt_id}{pb_tag}",
                        t.touch_id
                    );
                } else {
                    tracing::warn!(touch=%t.touch_id, "mark_touch_sent affected 0 rows — race?");
                }
            }

            println!(
                "send-pending `{campaign}` ({}): approved={} attempted={attempted} sent={sent} \
                 blocked_supp={blocked_supp} blocked_rate={blocked_rate} \
                 blocked_quarantine={blocked_domain_quarantine} \
                 blocked_no_to={blocked_no_to} bounced={bounced} errored={errored}{}",
                if for_real { "REAL" } else { "DRY-RUN" },
                approved.len(),
                if hit_max_batch {
                    format!(" (hit --max-batch={max_batch}; rerun to continue)")
                } else {
                    String::new()
                }
            );
        }

        Cmd::InboxPoll { every_seconds } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let cfg = ImapConfig::from_env()?;
            let poller = ImapPoller::new(cfg);
            loop {
                let started = std::time::Instant::now();
                let n = poller
                    .poll_once(|reply| {
                        let state = state.clone();
                        async move {
                            // DSN short-circuit: if this is a Mailer-Daemon
                            // bounce report, classify the embedded status
                            // code and (on hard bounce) auto-suppress the
                            // failed recipient. We DO NOT insert the DSN
                            // into `replies` — it would just clutter the
                            // classifier's queue. We log it so the operator
                            // can audit.
                            if let Some(dsn) = reply.detect_dsn() {
                                let synth = match dsn.status.as_deref() {
                                    Some(s) => format!("{s} {}", dsn.summary),
                                    None => dsn.summary.clone(),
                                };
                                let failure =
                                    salesman_outreach::classify_smtp_failure(&synth);
                                if failure.should_auto_suppress() {
                                    if let Err(e) = state
                                        .add_suppression(
                                            &dsn.recipient,
                                            "email",
                                            &format!("DSN bounce: {failure}"),
                                            failure.suppression_source(),
                                        )
                                        .await
                                    {
                                        tracing::error!(
                                            recipient = %dsn.recipient,
                                            "%e" = %e,
                                            "could not record DSN auto-suppression",
                                        );
                                    } else {
                                        tracing::warn!(
                                            recipient = %dsn.recipient,
                                            failure = %failure,
                                            "DSN hard-bounce → auto-suppressed",
                                        );
                                    }
                                } else {
                                    tracing::info!(
                                        recipient = %dsn.recipient,
                                        failure = %failure,
                                        "DSN observed but not auto-suppressing",
                                    );
                                }
                                return Ok(());
                            }
                            let raw = serde_json::to_value(&reply.raw_headers)?;
                            match state
                                .insert_reply_threaded(
                                    &reply.from_address,
                                    reply.subject.as_deref(),
                                    &reply.body_plain,
                                    &raw,
                                )
                                .await
                            {
                                Ok(Some(id)) => {
                                    tracing::info!(reply_id = %id, from = %reply.from_address, "persisted");
                                }
                                Ok(None) => {} // already warned
                                Err(e) => {
                                    tracing::error!("%e" = %e, "insert_reply failed");
                                    return Err(e);
                                }
                            }
                            Ok(())
                        }
                    })
                    .await?;
                println!(
                    "inbox-poll: handled {n} message(s) in {}ms",
                    started.elapsed().as_millis()
                );
                let Some(secs) = every_seconds else { break };
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            }
        }

        Cmd::ClassifyReplies { batch, competitors } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let competitor_catalog = match competitors.as_ref() {
                Some(path) => {
                    let text = std::fs::read_to_string(path)
                        .with_context(|| format!("reading competitors {}", path.display()))?;
                    Some(
                        salesman_content::load_competitors_toml(&text)
                            .map_err(|e| anyhow::anyhow!(e))?,
                    )
                }
                None => None,
            };
            let classifier = ReplyClassifyTool::new(router.clone());
            let interest_tool = salesman_content::InterestExtractTool::new(router.clone());
            let unclassified = state.list_unclassified_replies(batch).await?;
            if unclassified.is_empty() {
                println!("no unclassified replies");
            }
            let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
            for r in &unclassified {
                let args = serde_json::json!({
                    "subject": r.subject,
                    "body": r.body,
                });
                let result =
                    salesman_tools::Tool::invoke(&classifier, salesman_core::ToolArgs(args)).await;
                let kind_str = match result {
                    Ok(v) => v
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unclassified")
                        .to_string(),
                    Err(e) => {
                        tracing::warn!(reply = %r.reply_id, "%e" = %e, "classify failed");
                        continue;
                    }
                };
                let kind = match salesman_core::model::ReplyKind::from_str(&kind_str) {
                    Ok(k) => k,
                    Err(_) => {
                        tracing::warn!(reply = %r.reply_id, %kind_str, "unknown kind");
                        continue;
                    }
                };

                // SECURITY: anti-spoof gate for auto-suppression.
                // The MIME From: header has no integrity guarantee —
                // anyone with our IMAP inbox address can forge an
                // optout/legal-threat from a real prospect to poison
                // our suppression list (DOS against pipeline).
                //
                // When SALESMAN_TRUSTED_AUTHSERV_ID is set we read
                // Authentication-Results headers stamped by that
                // server and refuse to auto-suppress when SPF/DKIM/
                // DMARC alignment FAILED for the From: domain.
                // Unset env or no AR header → fail OPEN (legacy
                // behavior); operator should set the env on
                // production deployments.
                if matches!(
                    kind,
                    salesman_core::model::ReplyKind::Optout
                        | salesman_core::model::ReplyKind::LegalThreat
                ) && let Ok(trusted) = std::env::var("SALESMAN_TRUSTED_AUTHSERV_ID")
                    && !trusted.trim().is_empty()
                {
                    let ar_headers: Vec<String> = r
                        .raw_headers
                        .as_object()
                        .and_then(|m| m.get("Authentication-Results"))
                        .and_then(|v| v.as_str())
                        .map(|s| vec![s.to_string()])
                        .unwrap_or_default();
                    let from_domain = r
                        .from_address
                        .rsplit_once('@')
                        .map(|(_, d)| d.trim().to_ascii_lowercase())
                        .unwrap_or_default();
                    let trusted_lc = trusted.trim().to_ascii_lowercase();
                    let mut saw_trusted = false;
                    let mut passed = false;
                    for raw in &ar_headers {
                        if let Some(parsed) = salesman_reply::AuthResults::parse(raw)
                            && parsed.authserv_id == trusted_lc
                        {
                            saw_trusted = true;
                            if parsed.is_from_authenticated(&from_domain) {
                                passed = true;
                                break;
                            }
                        }
                    }
                    if saw_trusted && !passed {
                        tracing::warn!(
                            reply = %r.reply_id,
                            from = %r.from_address,
                            kind = %kind_str,
                            "REFUSING to auto-suppress: trusted Authentication-Results \
                             header reports SPF/DKIM/DMARC alignment FAILED — likely \
                             forged inbound. Tagging reply as auth_failed for operator \
                             review; suppression-list NOT modified."
                        );
                        if let Err(e) = state
                            .set_reply_tag(
                                r.reply_id,
                                "auth_failed",
                                &serde_json::json!(true),
                            )
                            .await
                        {
                            tracing::warn!(reply = %r.reply_id, "%e" = %e, "set_reply_tag failed");
                        }
                        // Update kind to a dedicated marker so the
                        // reply doesn't keep getting re-classified
                        // and re-attempted on the next sweep.
                        // We use Spam (existing terminal kind) as a
                        // conservative bucket — operator reviews the
                        // auth_failed tag in `salesman inbox`.
                        if let Err(e) = state
                            .update_reply_kind(
                                r.reply_id,
                                salesman_core::model::ReplyKind::Spam,
                            )
                            .await
                        {
                            tracing::warn!(reply = %r.reply_id, "%e" = %e, "update_reply_kind failed");
                        }
                        *counts.entry("auth_failed_skipped".into()).or_default() += 1;
                        continue;
                    }
                }

                let summary = state
                    .apply_reply_to_prospect(r.reply_id, r.prospect_id, &r.from_address, kind)
                    .await?;
                *counts.entry(kind_str.clone()).or_default() += 1;

                // Competitor-mention detection: tag the reply if any
                // competitor name / alias appears in the body.
                let competitor_note = if let Some(cat) = competitor_catalog.as_ref() {
                    let hits = cat.detect(&r.body);
                    if !hits.is_empty() {
                        let v = serde_json::to_value(&hits).unwrap_or_default();
                        if let Err(e) = state.set_reply_tag(r.reply_id, "competitors", &v).await {
                            tracing::warn!(reply = %r.reply_id, "%e" = %e, "set_reply_tag failed");
                        }
                        format!(" [competitors: {}]", hits.join(", "))
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // U52 interest extraction: only on positive intent
                // signals (engaged / question). Auto-merges into
                // prospects.tags['interests'] so the next touch in the
                // sequence can cite what the prospect actually cared
                // about. Best-effort; failures don't block classify.
                let interest_note = if matches!(kind_str.as_str(), "engaged" | "question") {
                    let args = serde_json::json!({ "body": r.body });
                    match salesman_tools::Tool::invoke(
                        &interest_tool,
                        salesman_core::ToolArgs(args),
                    )
                    .await
                    {
                        Ok(v) => {
                            let tags: Vec<String> = v
                                .get("interests")
                                .and_then(|x| x.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|t| t.as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let mut added = 0u32;
                            for t in &tags {
                                match state.add_prospect_interest(r.prospect_id, t).await {
                                    Ok(true) => added += 1,
                                    Ok(false) => {}
                                    Err(e) => tracing::warn!(
                                        prospect = %r.prospect_id.0, "%e" = %e,
                                        "add_prospect_interest failed",
                                    ),
                                }
                            }
                            if !tags.is_empty() {
                                format!(" [interests +{added}/{}: {}]", tags.len(), tags.join(", "))
                            } else {
                                String::new()
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                reply = %r.reply_id, "%e" = %e,
                                "interest extraction failed (best-effort)",
                            );
                            String::new()
                        }
                    }
                } else {
                    String::new()
                };

                // U55: inline webhook ping when a positive intent
                // signal lands. Speed of response is a closing-deals
                // multiplier — the operator should see "engaged" reply
                // alerts the moment they classify, not when the next
                // alerts cron runs an hour later. SALESMAN_ALERT_WEBHOOK_URL
                // controls; missing env = no-op.
                if matches!(kind_str.as_str(), "engaged" | "question")
                    && let Ok(url) = std::env::var("SALESMAN_ALERT_WEBHOOK_URL")
                    && !url.trim().is_empty()
                {
                    let preview: String = r
                        .body
                        .chars()
                        .take(180)
                        .collect::<String>()
                        .replace('\n', " ");
                    let text = format!(
                        "[{}] reply from {}: \"{}…\" — review: salesman draft-replies",
                        kind_str, r.from_address, preview
                    );
                    if let Err(e) = post_alert_webhook(&url, &text).await {
                        tracing::warn!(
                            reply = %r.reply_id, "%e" = %e,
                            "inline alert webhook failed (best-effort)",
                        );
                    }
                }

                println!(
                    "[{}] {} → {}: {}{competitor_note}{interest_note}",
                    r.from_address, kind_str, r.reply_id, summary
                );
            }
            println!(
                "\nclassified {} replies. counts: {:?}",
                unclassified.len(),
                counts
            );
        }

        Cmd::DraftReplies {
            batch,
            pricing_catalog,
            meeting_slots,
            objections,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let pricing_text = match pricing_catalog.as_ref() {
                Some(path) => Some(
                    std::fs::read_to_string(path)
                        .with_context(|| format!("reading pricing catalog {}", path.display()))?,
                ),
                None => None,
            };
            let calendar_value = match meeting_slots.as_ref() {
                Some(path) => {
                    let text = std::fs::read_to_string(path)
                        .with_context(|| format!("reading meeting slots {}", path.display()))?;
                    let cal = salesman_content::draft_reply::load_calendar_toml(&text)?;
                    let now = chrono::Utc::now();
                    Some(cal.to_drafter_value(now, 3))
                }
                None => None,
            };
            let objection_lib = match objections.as_ref() {
                Some(path) => {
                    let text = std::fs::read_to_string(path)
                        .with_context(|| format!("reading objections {}", path.display()))?;
                    Some(salesman_content::draft_reply::load_objections_toml(&text)?)
                }
                None => None,
            };
            let drafter = salesman_content::DraftReplyTool::new(
                router.clone(),
                std::env::var("SALESMAN_FROM_NAME").unwrap_or_else(|_| "William".into()),
                "PlausiDen",
                "Plausible deniability + sovereign data tools for SMB security teams.",
            );
            let needing = state.list_replies_needing_response(batch).await?;
            if needing.is_empty() {
                println!("no replies awaiting a draft response.");
                return Ok(());
            }
            println!(
                "drafting responses for {} reply(ies){}...\n",
                needing.len(),
                if pricing_text.is_some() {
                    " (pricing catalog loaded)"
                } else {
                    ""
                },
            );
            let mut ok = 0u32;
            let mut err = 0u32;
            let mut flagged = 0u32;
            for r in &needing {
                let prospect_json = serde_json::json!({
                    "display_name": r.company_name,
                    "industry":     r.industry,
                    "description":  r.description,
                });
                // U54: pull the prior conversation thread for this
                // prospect (oldest first, capped) and pass it to the
                // drafter so multi-turn threads anchor in past context
                // instead of re-introducing the prospect every reply.
                let prior_thread_value =
                    match state.list_thread_for_prospect(r.prospect_id, 6).await {
                        Ok(turns) => {
                            let arr: Vec<serde_json::Value> = turns
                                .into_iter()
                                .map(|t| {
                                    serde_json::json!({
                                        "role": t.role,
                                        "at": t.at.to_rfc3339(),
                                        "subject": t.subject,
                                        "body": t.body,
                                        "reply_kind": t.reply_kind,
                                    })
                                })
                                .collect();
                            Some(serde_json::Value::Array(arr))
                        }
                        Err(e) => {
                            tracing::warn!(
                                prospect = %r.prospect_id.0, "%e" = %e,
                                "thread fetch failed; drafting without prior context",
                            );
                            None
                        }
                    };

                let mut args = serde_json::json!({
                    "prospect": prospect_json,
                    "outbound_subject": r.outbound_subject,
                    "outbound_body":    r.outbound_body,
                    "inbound_subject":  r.inbound_subject,
                    "inbound_body":     r.inbound_body,
                    "inbound_kind":     r.inbound_kind,
                });
                if let Some(thread_v) = prior_thread_value {
                    args["prior_thread"] = thread_v;
                }
                // Pass the pricing catalog through to the drafter
                // ONLY when this inbound looks pricing-shaped; the
                // drafter already has the keyword check internally
                // but threading it here saves the no-op LLM tokens
                // when the catalog isn't relevant.
                if let Some(cat) = pricing_text.as_deref()
                    && salesman_content::draft_reply::looks_like_pricing_question(&r.inbound_body)
                {
                    args["pricing_catalog"] = serde_json::Value::String(cat.to_string());
                }
                if let Some(cal_v) = calendar_value.as_ref()
                    && salesman_content::draft_reply::looks_like_meeting_question(&r.inbound_body)
                {
                    args["meeting_calendar"] = cal_v.clone();
                }
                if let Some(lib) = objection_lib.as_ref()
                    && let Some(obj_v) = lib.to_drafter_value(&r.inbound_body)
                {
                    args["objection_match"] = obj_v;
                }
                let result =
                    salesman_tools::Tool::invoke(&drafter, salesman_core::ToolArgs(args)).await;
                match result {
                    Ok(v) => {
                        let subject = v
                            .get("subject")
                            .and_then(|x| x.as_str())
                            .unwrap_or("(no subject)");
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                        let intent = v
                            .get("intent")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unspecified");
                        let det = v
                            .get("detector_score")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(0.0);
                        let passed = v
                            .get("passed_detector")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(true);
                        let produced_by = v.get("produced_by").cloned();
                        match state
                            .insert_touch_draft_full(
                                r.prospect_id,
                                salesman_core::TouchChannel::Email,
                                Some(subject),
                                body,
                                None,
                                produced_by,
                            )
                            .await
                        {
                            Ok(touch_id) => {
                                let _linked = state
                                    .link_reply_response(r.reply_id, touch_id)
                                    .await
                                    .unwrap_or(0);
                                ok += 1;
                                if !passed {
                                    flagged += 1;
                                }
                                println!(
                                    "[{}] {} ({:.2} det{}): {} → {}",
                                    r.company_name,
                                    intent,
                                    det,
                                    if passed { "" } else { " ⚠" },
                                    r.reply_id,
                                    touch_id,
                                );
                            }
                            Err(e) => {
                                err += 1;
                                tracing::warn!(reply = %r.reply_id, "%e" = %e, "draft persist failed");
                            }
                        }
                    }
                    Err(e) => {
                        err += 1;
                        tracing::warn!(reply = %r.reply_id, "%e" = %e, "draft_reply tool failed");
                    }
                }
            }
            println!(
                "\ndraft-replies: drafted {ok} response(s); {flagged} flagged on detector; {err} error(s). \
                 Review with `salesman review` then send."
            );
        }

        Cmd::Inbox { campaign, limit } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(inbox-only)", "(unspecified)")
                .await?;
            let rows = state
                .list_recent_replies_for_campaign(campaign_id, limit)
                .await?;
            if rows.is_empty() {
                println!("no replies for `{campaign}`");
            } else {
                println!("=== {} replies for `{campaign}` ===\n", rows.len());
                for r in rows {
                    println!(
                        "[{}] {} | {} | {}",
                        r.received_at.to_rfc3339(),
                        r.kind,
                        r.from_address,
                        r.subject.as_deref().unwrap_or("")
                    );
                    let snippet: String = r.body.chars().take(160).collect();
                    println!("  {snippet}...\n");
                }
            }
        }

        Cmd::Summary { since_hours } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let s = state.pipeline_summary(since_hours).await?;
            if cli.json {
                let v = serde_json::json!({
                    "since_hours": s.since_hours,
                    "companies": s.companies,
                    "prospects": s.prospects,
                    "by_state": {
                        "new": s.new_prospects,
                        "contacted": s.contacted,
                        "engaged": s.engaged,
                        "won": s.won,
                        "lost": s.lost,
                        "suppressed": s.suppressed_prospects,
                    },
                    "awaiting_approval": s.awaiting_approval,
                    "recent": {
                        "sends": s.sent_recent,
                        "replies": s.replies_recent,
                        "optouts": s.optout_recent,
                        "receipts": s.receipts_recent,
                    },
                    "suppressions_total": s.suppressions,
                });
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{}", s.render_text());
            }
        }

        Cmd::Alerts {
            since_hours,
            webhook,
            webhook_always,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            // Resolve webhook URL: --webhook flag takes precedence over
            // env. Empty string after env-default counts as unset.
            let webhook_url: Option<String> = webhook
                .or_else(|| std::env::var("SALESMAN_ALERT_WEBHOOK_URL").ok())
                .filter(|s| !s.trim().is_empty());
            let positive = state
                .list_replies_since_with_kinds(since_hours, &["engaged", "question"])
                .await
                .unwrap_or_default();
            let legal_threats = state
                .list_replies_since_with_kinds(since_hours, &["legal_threat"])
                .await
                .unwrap_or_default();
            let optouts = state
                .list_replies_since_with_kinds(since_hours, &["optout"])
                .await
                .unwrap_or_default();
            let supp_recent = state
                .list_suppressions_since(since_hours)
                .await
                .unwrap_or_default();
            let bounces: Vec<_> = supp_recent
                .iter()
                .filter(|s| s.source == "bounce")
                .collect();
            let competitor_replies = state
                .list_competitor_mention_replies(since_hours, 50)
                .await
                .unwrap_or_default();

            if cli.json {
                let v = serde_json::json!({
                    "since_hours": since_hours,
                    "legal_threats": legal_threats.iter().map(|r| serde_json::json!({
                        "received_at": r.received_at.to_rfc3339(),
                        "from": r.from_address,
                        "subject": r.subject,
                    })).collect::<Vec<_>>(),
                    "positive_replies": positive.iter().map(|r| serde_json::json!({
                        "received_at": r.received_at.to_rfc3339(),
                        "from": r.from_address,
                        "kind": r.kind,
                        "subject": r.subject,
                    })).collect::<Vec<_>>(),
                    "optout_replies": optouts.iter().map(|r| serde_json::json!({
                        "received_at": r.received_at.to_rfc3339(),
                        "from": r.from_address,
                        "subject": r.subject,
                    })).collect::<Vec<_>>(),
                    "bounces": bounces.iter().map(|s| serde_json::json!({
                        "added_at": s.added_at.to_rfc3339(),
                        "target": s.target,
                        "reason": s.reason,
                    })).collect::<Vec<_>>(),
                    "competitor_mentions": competitor_replies.iter().map(|(reply, comps, pid)| serde_json::json!({
                        "received_at": reply.received_at.to_rfc3339(),
                        "from": reply.from_address,
                        "kind": reply.kind,
                        "subject": reply.subject,
                        "competitors": comps,
                        "prospect_id": pid.0.to_string(),
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }

            println!(
                "salesman alerts — last {since_hours}h ({})\n",
                chrono::Utc::now().to_rfc3339()
            );
            // Legal-threat section first — operator must see this
            // BEFORE any other triage. Only printed when non-empty
            // so the noise-floor stays low on healthy days; absence
            // is implicitly "no legal threats received."
            if !legal_threats.is_empty() {
                println!(
                    "=== ⚠⚠⚠ LEGAL THREATS ({}) — operator must respond personally ===",
                    legal_threats.len()
                );
                for r in &legal_threats {
                    println!(
                        "  {} | {} | {}",
                        r.received_at.format("%Y-%m-%d %H:%M:%SZ"),
                        r.from_address,
                        r.subject.as_deref().unwrap_or("(no subject)"),
                    );
                }
                println!("  → senders auto-suppressed; in-flight touches rejected; \
                          drafter REFUSED to compose. Run `salesman thread <prospect>` \
                          to read context, then respond manually.\n");
            }
            println!(
                "=== positive replies ({}): engaged + question ===",
                positive.len()
            );
            if positive.is_empty() {
                println!("  (none — quiet)");
            } else {
                for r in &positive {
                    println!(
                        "  {} | {} | {} | {}",
                        r.received_at.format("%Y-%m-%d %H:%M:%SZ"),
                        r.kind,
                        r.from_address,
                        r.subject.as_deref().unwrap_or("(no subject)"),
                    );
                }
            }
            println!();
            println!("=== opt-outs ({}) ===", optouts.len());
            if optouts.is_empty() {
                println!("  (none — clean)");
            } else {
                for r in &optouts {
                    println!(
                        "  {} | {} | {}",
                        r.received_at.format("%Y-%m-%d %H:%M:%SZ"),
                        r.from_address,
                        r.subject.as_deref().unwrap_or("(no subject)"),
                    );
                }
            }
            println!();
            println!("=== auto-suppressed bounces ({}) ===", bounces.len());
            if bounces.is_empty() {
                println!("  (none — list quality OK)");
            } else {
                for s in &bounces {
                    let r = if s.reason.chars().count() > 80 {
                        format!("{}…", s.reason.chars().take(79).collect::<String>())
                    } else {
                        s.reason.clone()
                    };
                    println!(
                        "  {} | {} | {}",
                        s.added_at.format("%Y-%m-%d %H:%M:%SZ"),
                        s.target,
                        r,
                    );
                }
            }
            println!();
            println!(
                "=== competitor mentions in replies ({}) ===",
                competitor_replies.len()
            );
            if competitor_replies.is_empty() {
                println!("  (none — no comparison shopping detected)");
            } else {
                for (reply, comps, _) in &competitor_replies {
                    println!(
                        "  {} | {} | {} | comparing to: {}",
                        reply.received_at.format("%Y-%m-%d %H:%M:%SZ"),
                        reply.from_address,
                        reply.subject.as_deref().unwrap_or("(no subject)"),
                        comps.join(", "),
                    );
                }
            }
            println!();
            // Summary banner — fast triage line for ops at a glance.
            // Legal threats trump everything else — operator must
            // know within one alert cycle.
            let banner = if !legal_threats.is_empty() {
                format!(
                    "🛑 {} LEGAL THREAT(S) — handle personally before anything else",
                    legal_threats.len()
                )
            } else if !positive.is_empty() {
                format!("⤴ {} positive reply(ies) — review!", positive.len())
            } else if !competitor_replies.is_empty() {
                format!(
                    "🥊 {} competitor-mention(s) — pivot to comparison",
                    competitor_replies.len()
                )
            } else if !optouts.is_empty() || bounces.len() > 3 {
                format!(
                    "⚠ {} opt-out(s) + {} bounce(s) — investigate list quality",
                    optouts.len(),
                    bounces.len()
                )
            } else {
                "→ nothing important; carry on".to_string()
            };
            println!("{banner}");

            // Webhook fan-out: fire-and-forget. Failure logs +
            // surfaces inline but doesn't error the command.
            if let Some(url) = webhook_url {
                let any_action = !positive.is_empty()
                    || !legal_threats.is_empty()
                    || !optouts.is_empty()
                    || !bounces.is_empty()
                    || !competitor_replies.is_empty();
                if any_action || webhook_always {
                    let mut text = format!("*Salesman alerts — last {since_hours}h*\n");
                    text.push_str(&format!("{banner}\n"));
                    // Legal threats first in the webhook body too —
                    // pages oncall before they scroll through the
                    // positive-replies section.
                    if !legal_threats.is_empty() {
                        text.push_str(&format!(
                            "\n*🛑 LEGAL THREATS ({})* — handle personally\n",
                            legal_threats.len()
                        ));
                        for r in legal_threats.iter().take(5) {
                            text.push_str(&format!(
                                "• {} — {}\n",
                                r.from_address,
                                r.subject.as_deref().unwrap_or("(no subject)"),
                            ));
                        }
                    }
                    if !positive.is_empty() {
                        text.push_str(&format!("\n*Positive replies ({})*\n", positive.len()));
                        for r in positive.iter().take(5) {
                            text.push_str(&format!(
                                "• {} — `{}` ({})\n",
                                r.from_address,
                                r.kind,
                                r.subject.as_deref().unwrap_or("(no subject)"),
                            ));
                        }
                    }
                    if !competitor_replies.is_empty() {
                        text.push_str(&format!(
                            "\n*Competitor mentions ({})*\n",
                            competitor_replies.len()
                        ));
                        for (reply, comps, _) in competitor_replies.iter().take(5) {
                            text.push_str(&format!(
                                "• {} — comparing to {}\n",
                                reply.from_address,
                                comps.join(", "),
                            ));
                        }
                    }
                    if !bounces.is_empty() {
                        text.push_str(&format!(
                            "\n*Auto-suppressed bounces ({})*\n",
                            bounces.len()
                        ));
                    }
                    if !optouts.is_empty() {
                        text.push_str(&format!("\n*Opt-outs ({})*\n", optouts.len()));
                    }
                    match post_alert_webhook(&url, &text).await {
                        Ok(()) => {
                            println!("(posted to webhook)");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "webhook post failed");
                            println!("(webhook post failed: {e})");
                        }
                    }
                }
            }
        }

        Cmd::Score {
            stdin,
            body,
            templates_dir,
            threshold,
        } => {
            let print_score = |body: &str| {
                let s = salesman_detector::score(body, None);
                let reasons = s.reasons().join(";").replace('\n', " ");
                println!(
                    "{:.3}\t{}\t{}",
                    s.score,
                    if s.passes(threshold) { "pass" } else { "fail" },
                    reasons
                );
            };
            if let Some(dir) = templates_dir {
                println!("template\tsegment\tscore\tpass\treasons");
                for entry in std::fs::read_dir(&dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                        continue;
                    }
                    let key = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                    let text = std::fs::read_to_string(&path)?;
                    let parsed: toml::Value = toml::from_str(&text)?;
                    let segment = parsed
                        .get("segment")
                        .and_then(|v| v.as_str())
                        .unwrap_or("any")
                        .to_string();
                    let body = parsed
                        .get("body_seed")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let s = salesman_detector::score(body, None);
                    let reasons = s.reasons().join(";").replace('\n', " ");
                    println!(
                        "{}\t{}\t{:.3}\t{}\t{}",
                        key,
                        segment,
                        s.score,
                        if s.passes(threshold) { "pass" } else { "fail" },
                        reasons
                    );
                }
            } else if let Some(b) = body {
                print_score(&b);
            } else if stdin {
                use std::io::Read;
                let mut b = String::new();
                std::io::stdin().read_to_string(&mut b)?;
                print_score(&b);
            } else {
                anyhow::bail!("score: pass --stdin, --body, or --templates-dir");
            }
        }

        Cmd::SetCostCap { campaign, max_usd } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let cid = state
                .ensure_campaign(&campaign, "(set-cost-cap)", "(unspecified)")
                .await?;
            let cap = if max_usd > 0.0 {
                Some((max_usd * 1_000_000.0) as i64)
            } else {
                None
            };
            state.set_campaign_cost_cap(cid, cap).await?;
            match cap {
                Some(c) => println!(
                    "set cost cap on `{campaign}` to ${:.2} ({} micro USD)",
                    max_usd, c
                ),
                None => println!("cleared cost cap on `{campaign}`"),
            }
        }

        Cmd::CampaignCosts { since_hours } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let rows = state.campaign_cost_summary(since_hours).await?;
            if rows.is_empty() {
                println!("no campaigns / no LLM calls in last {since_hours}h");
            } else {
                println!(
                    "{:<32} {:<10} {:>10} {:>10} {:>10} {:>10}",
                    "campaign", "status", "calls", "spent USD", "cap USD", "% used"
                );
                println!("{}", "-".repeat(90));
                for r in &rows {
                    let cap_str = r
                        .cost_cap_micro_usd
                        .map(|c| format!("{:.2}", c as f64 / 1_000_000.0))
                        .unwrap_or_else(|| "-".into());
                    let pct_str = r
                        .pct_used()
                        .map(|p| format!("{:.1}%{}", p, if r.over_cap() { " !" } else { "" }))
                        .unwrap_or_else(|| "-".into());
                    println!(
                        "{:<32} {:<10} {:>10} {:>10.4} {:>10} {:>10}",
                        truncate_name(&r.name, 32),
                        r.status,
                        r.calls,
                        (r.spent_micro_usd as f64) / 1_000_000.0,
                        cap_str,
                        pct_str,
                    );
                }
            }
        }

        Cmd::AccountFanout { prospect_id } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let pid: uuid::Uuid = prospect_id
                .parse()
                .with_context(|| format!("invalid prospect-id `{prospect_id}`"))?;
            let prospect_id = salesman_core::ProspectId(pid);
            let info = state.prospect_company_and_campaign(prospect_id).await?;
            let Some((company_id, _campaign_id, company_name)) = info else {
                anyhow::bail!("no prospect with id {pid}");
            };
            let contacts = state.list_contacts_for_company(company_id).await?;
            if cli.json {
                let v = serde_json::json!({
                    "company_name": company_name,
                    "company_id": company_id.0.to_string(),
                    "contact_count": contacts.len(),
                    "contacts": contacts.iter().map(|(id, name, title, email, source)| serde_json::json!({
                        "id": id.to_string(),
                        "name": name,
                        "title": title,
                        "email": email,
                        "source": source,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            println!(
                "=== {} — {} known contact(s) ===\n",
                company_name,
                contacts.len()
            );
            if contacts.is_empty() {
                println!(
                    "  (no other contacts on file. Run `salesman find-buyers \
                     --campaign <name> --persist` to discover stakeholders.)"
                );
            } else {
                println!(
                    "{:<38} {:<28} {:<22} {:<28} source",
                    "id", "name", "title", "email"
                );
                println!("{}", "-".repeat(140));
                for (id, name, title, email, source) in &contacts {
                    println!(
                        "{:<38} {:<28} {:<22} {:<28} {}",
                        id,
                        truncate_name(name.as_deref().unwrap_or("(unknown)"), 28),
                        truncate_name(title.as_deref().unwrap_or("(unknown)"), 22),
                        truncate_name(email.as_deref().unwrap_or("(unknown)"), 28),
                        source,
                    );
                }
                println!();
                println!(
                    "Multi-stakeholder play: the engaged contact is one buyer; \
                     these are peers / superiors / direct reports. Consider:"
                );
                println!(
                    "  1. CC the engaged contact + a superior on the next response — anchors the deal."
                );
                println!(
                    "  2. Send a separate first-touch to a peer in a different role (CISO + CTO)."
                );
                println!(
                    "  3. If multiple contacts ALREADY engaged independently, you have the right account; tighten."
                );
            }
        }

        Cmd::SendTimes {
            tz_offset_minutes,
            min_sent,
            top,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let rows = state
                .reply_rate_by_send_window(tz_offset_minutes, min_sent)
                .await?;
            if rows.is_empty() {
                println!(
                    "no send-time data with sent ≥ {min_sent} in any (day, hour) bucket. \
                     Lower --min-sent or send more before this is useful."
                );
                return Ok(());
            }
            const DOW: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            if cli.json {
                let v = serde_json::json!({
                    "tz_offset_minutes": tz_offset_minutes,
                    "min_sent": min_sent,
                    "rows": rows.iter().map(|(dow, hour, sent, replied, engaged)| {
                        let rate = if *sent == 0 { 0.0 } else { *replied as f32 / *sent as f32 };
                        let eng = if *sent == 0 { 0.0 } else { *engaged as f32 / *sent as f32 };
                        serde_json::json!({
                            "dow":   dow,
                            "dow_name": DOW.get(*dow as usize).copied().unwrap_or("?"),
                            "hour":  hour,
                            "sent":  sent,
                            "replied": replied,
                            "engaged": engaged,
                            "reply_rate":   rate,
                            "engaged_rate": eng,
                        })
                    }).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            println!(
                "send-time analytics — tz_offset={tz_offset_minutes}min, min-sent={min_sent}\n"
            );
            println!(
                "{:<5} {:<6} {:>6} {:>8} {:>8} {:>8} {:>9}",
                "day", "hour", "sent", "replied", "engaged", "reply%", "engaged%"
            );
            println!("{}", "-".repeat(70));
            for (i, (dow, hour, sent, replied, engaged)) in rows.iter().enumerate() {
                let dow_name = DOW.get(*dow as usize).copied().unwrap_or("?");
                let reply_rate = if *sent == 0 {
                    0.0
                } else {
                    *replied as f32 / *sent as f32
                };
                let engaged_rate = if *sent == 0 {
                    0.0
                } else {
                    *engaged as f32 / *sent as f32
                };
                let marker = if i < top { " ★" } else { "" };
                println!(
                    "{:<5} {:>4}  {:>6} {:>8} {:>8} {:>7.1}% {:>8.1}%{marker}",
                    dow_name,
                    format!("{hour:02}:00"),
                    sent,
                    replied,
                    engaged,
                    reply_rate * 100.0,
                    engaged_rate * 100.0,
                );
            }
            if !rows.is_empty() {
                println!();
                println!(
                    "★ = top {top} window(s) by engaged_rate. Time your next batch with --no-pause for one of these."
                );
            }
        }

        Cmd::TemplateStats { by } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            match by {
                TemplateStatsBy::Template => {
                    let stats = state.template_stats().await?;
                    if stats.is_empty() {
                        println!("no template-tagged touches yet");
                    } else {
                        println!(
                            "{:<24} {:>8} {:>6} {:>8} {:>8} {:>8}",
                            "template", "drafted", "sent", "replied", "engaged", "reply%"
                        );
                        println!("{}", "-".repeat(70));
                        for s in &stats {
                            println!(
                                "{:<24} {:>8} {:>6} {:>8} {:>8} {:>7.1}%",
                                s.template_key,
                                s.drafted,
                                s.sent,
                                s.replied,
                                s.engaged_replied,
                                s.reply_rate() * 100.0
                            );
                        }
                    }
                }
                TemplateStatsBy::Segment => {
                    let rows = state.template_stats_by_segment().await?;
                    if rows.is_empty() {
                        println!("no template-tagged touches yet");
                        return Ok(());
                    }
                    // Compute, per template, the BEST engaged_rate
                    // across segments. Then flag any segment whose
                    // engaged_rate < half the best (with sent ≥10
                    // floor — small samples flagged are noise).
                    let mut best_per_template: std::collections::BTreeMap<String, f32> =
                        std::collections::BTreeMap::new();
                    for (tk, _seg, s) in &rows {
                        if s.sent >= 10 {
                            let cur = best_per_template.entry(tk.clone()).or_insert(0.0);
                            if s.engaged_rate() > *cur {
                                *cur = s.engaged_rate();
                            }
                        }
                    }
                    println!(
                        "{:<24} {:<24} {:>8} {:>6} {:>8} {:>8} {:>8}",
                        "template", "segment", "drafted", "sent", "replied", "engaged", "reply%"
                    );
                    println!("{}", "-".repeat(100));
                    let mut underperformers: Vec<(String, String, f32)> = Vec::new();
                    for (tk, seg, s) in &rows {
                        let best = best_per_template.get(tk).copied().unwrap_or(0.0);
                        let weak = s.sent >= 10 && best > 0.0 && s.engaged_rate() < best * 0.5;
                        let marker = if weak { " ⚠" } else { "" };
                        println!(
                            "{:<24} {:<24} {:>8} {:>6} {:>8} {:>8} {:>6.1}%{marker}",
                            truncate_name(tk, 24),
                            truncate_name(seg, 24),
                            s.drafted,
                            s.sent,
                            s.replied,
                            s.engaged_replied,
                            s.reply_rate() * 100.0,
                        );
                        if weak {
                            underperformers.push((tk.clone(), seg.clone(), s.engaged_rate()));
                        }
                    }
                    if !underperformers.is_empty() {
                        println!();
                        println!(
                            "⚠ {} (template, segment) pair(s) are underperforming. \
                             Consider pausing or re-tuning.",
                            underperformers.len()
                        );
                        for (tk, seg, rate) in &underperformers {
                            println!(
                                "    {} in `{}` — engaged_rate {:.1}%",
                                tk,
                                seg,
                                rate * 100.0
                            );
                        }
                    }
                }
            }
        }

        Cmd::Costs { since_hours, by } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if cli.json {
                let v = match by {
                    CostsBy::Model => {
                        let rows = state.cost_summary(since_hours).await?;
                        let total: i64 = rows.iter().map(|r| r.cost_micro_usd).sum();
                        serde_json::json!({
                            "since_hours": since_hours,
                            "by": "model",
                            "total_usd": (total as f64) / 1_000_000.0,
                            "rows": rows.iter().map(|r| serde_json::json!({
                                "backend": r.backend,
                                "model": r.model,
                                "calls": r.count,
                                "prompt_tokens": r.prompt_tokens,
                                "output_tokens": r.output_tokens,
                                "cache_hit_tokens": r.cache_hit_tokens,
                                "cost_usd": (r.cost_micro_usd as f64) / 1_000_000.0,
                                "avg_latency_ms": r.avg_latency_ms,
                                "p95_latency_ms": r.p95_latency_ms,
                            })).collect::<Vec<_>>(),
                        })
                    }
                    CostsBy::Purpose => {
                        let rows = state.cost_by_purpose(since_hours).await?;
                        let total: i64 = rows.iter().map(|r| r.cost_micro_usd).sum();
                        serde_json::json!({
                            "since_hours": since_hours,
                            "by": "purpose",
                            "total_usd": (total as f64) / 1_000_000.0,
                            "rows": rows.iter().map(|r| serde_json::json!({
                                "purpose": r.purpose,
                                "calls": r.count,
                                "prompt_tokens": r.prompt_tokens,
                                "output_tokens": r.output_tokens,
                                "cache_hit_tokens": r.cache_hit_tokens,
                                "cost_usd": (r.cost_micro_usd as f64) / 1_000_000.0,
                                "avg_latency_ms": r.avg_latency_ms,
                                "p95_latency_ms": r.p95_latency_ms,
                            })).collect::<Vec<_>>(),
                        })
                    }
                };
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            match by {
                CostsBy::Model => {
                    let rows = state.cost_summary(since_hours).await?;
                    if rows.is_empty() {
                        println!("No LLM calls in the last {since_hours}h.");
                    } else {
                        println!("LLM cost report — last {since_hours}h, by model\n");
                        println!(
                            "{:<10} {:<28} {:>6} {:>10} {:>10} {:>10} {:>10} {:>8} {:>8}",
                            "backend",
                            "model",
                            "calls",
                            "prompt",
                            "output",
                            "cache",
                            "cost USD",
                            "avg ms",
                            "p95 ms"
                        );
                        println!("{}", "-".repeat(110));
                        let mut total_micro_usd: i64 = 0;
                        for r in &rows {
                            println!(
                                "{:<10} {:<28} {:>6} {:>10} {:>10} {:>10} {:>10.4} {:>8} {:>8}",
                                r.backend,
                                r.model,
                                r.count,
                                r.prompt_tokens,
                                r.output_tokens,
                                r.cache_hit_tokens,
                                (r.cost_micro_usd as f64) / 1_000_000.0,
                                r.avg_latency_ms,
                                r.p95_latency_ms,
                            );
                            total_micro_usd += r.cost_micro_usd;
                        }
                        println!("{}", "-".repeat(110));
                        println!(
                            "TOTAL: ${:.4} USD across {} models",
                            (total_micro_usd as f64) / 1_000_000.0,
                            rows.len()
                        );
                    }
                }
                CostsBy::Purpose => {
                    let rows = state.cost_by_purpose(since_hours).await?;
                    if rows.is_empty() {
                        println!("No LLM calls in the last {since_hours}h.");
                    } else {
                        println!("LLM cost report — last {since_hours}h, by purpose\n");
                        println!(
                            "{:<28} {:>6} {:>10} {:>10} {:>10} {:>10} {:>8} {:>8}",
                            "purpose",
                            "calls",
                            "prompt",
                            "output",
                            "cache",
                            "cost USD",
                            "avg ms",
                            "p95 ms"
                        );
                        println!("{}", "-".repeat(100));
                        let mut total_micro_usd: i64 = 0;
                        for r in &rows {
                            println!(
                                "{:<28} {:>6} {:>10} {:>10} {:>10} {:>10.4} {:>8} {:>8}",
                                r.purpose,
                                r.count,
                                r.prompt_tokens,
                                r.output_tokens,
                                r.cache_hit_tokens,
                                (r.cost_micro_usd as f64) / 1_000_000.0,
                                r.avg_latency_ms,
                                r.p95_latency_ms,
                            );
                            total_micro_usd += r.cost_micro_usd;
                        }
                        println!("{}", "-".repeat(100));
                        println!(
                            "TOTAL: ${:.4} USD across {} purpose tag(s)",
                            (total_micro_usd as f64) / 1_000_000.0,
                            rows.len()
                        );
                    }
                }
            }
        }

        Cmd::Whoami => {
            let mut report = serde_json::Map::new();
            let mut missing = Vec::new();
            for k in [
                "SALESMAN_FROM_NAME",
                "SALESMAN_FROM_EMAIL",
                "SALESMAN_REPLY_TO",
                "SALESMAN_LIST_UNSUBSCRIBE",
                "SALESMAN_UNSUBSCRIBE_BASE_URL",
                "SALESMAN_COMPLIANCE_FOOTER",
                "SALESMAN_SMTP_HOST",
                "SALESMAN_SMTP_PORT",
                "SALESMAN_SMTP_USERNAME",
            ] {
                match std::env::var(k) {
                    Ok(v) if !v.is_empty() => {
                        report.insert(k.into(), serde_json::Value::String(v));
                    }
                    _ => missing.push(k),
                }
            }
            // Don't echo passwords or HMAC secrets — only their presence.
            for secret_key in ["SALESMAN_SMTP_PASSWORD", "SALESMAN_UNSUBSCRIBE_HMAC_SECRET"] {
                let set = std::env::var(secret_key)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                report.insert(secret_key.into(), serde_json::Value::Bool(set));
            }
            // Surface whether the per-recipient unsubscribe minter is fully wired.
            let unsub_ready = std::env::var("SALESMAN_UNSUBSCRIBE_BASE_URL")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
                && std::env::var("SALESMAN_UNSUBSCRIBE_HMAC_SECRET")
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
            report.insert(
                "unsubscribe_minter_ready".into(),
                serde_json::Value::Bool(unsub_ready),
            );
            // Operator brief presence — quality signal for the prompt
            // freshness contract (MODEL_RESILIENCE.md §5).
            let brief_path = std::env::var("SALESMAN_OPERATOR_BRIEF")
                .ok()
                .filter(|p| !p.is_empty());
            let brief_loaded = router.operator_brief().is_some();
            report.insert(
                "SALESMAN_OPERATOR_BRIEF".into(),
                serde_json::Value::String(brief_path.unwrap_or_else(|| "(unset)".into())),
            );
            report.insert(
                "operator_brief_loaded".into(),
                serde_json::Value::Bool(brief_loaded),
            );
            report.insert("missing_required".into(), serde_json::json!(missing));
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !missing.is_empty() {
                anyhow::bail!(
                    "sender identity incomplete; {} required fields missing",
                    missing.len()
                );
            }
        }

        Cmd::ValidateCsv { from_csv } => {
            let seed = CsvSeed::new();
            let companies = seed.read_path(&from_csv)?;
            let mut have_homepage = 0usize;
            let mut have_industry = 0usize;
            let mut have_description = 0usize;
            for c in &companies {
                if c.homepage.is_some() {
                    have_homepage += 1;
                }
                if c.industry.is_some() {
                    have_industry += 1;
                }
                if c.description.is_some() {
                    have_description += 1;
                }
            }
            println!(
                "validate-csv {}\n\
                 ---------------------------\n\
                 parsable rows:        {}\n\
                 with homepage:        {} ({:.0}%)\n\
                 with industry:        {} ({:.0}%)\n\
                 with description:     {} ({:.0}%)\n",
                from_csv.display(),
                companies.len(),
                have_homepage,
                pct(have_homepage, companies.len()),
                have_industry,
                pct(have_industry, companies.len()),
                have_description,
                pct(have_description, companies.len()),
            );
            if companies.is_empty() {
                anyhow::bail!("no parsable rows in CSV");
            }
            // Sample preview
            println!("Sample (first 3):");
            for c in companies.iter().take(3) {
                println!(
                    "  - {} | homepage={:?} | industry={:?}",
                    c.display_name,
                    c.homepage.as_ref().map(|u| u.as_str()),
                    c.industry
                );
            }
        }

        Cmd::QueueClear {
            campaign,
            confirm_typed,
        } => {
            if !confirm_typed {
                anyhow::bail!(
                    "queue-clear requires --confirm-typed (type the campaign name to proceed)"
                );
            }
            let state = require_state(cli.database_url.as_deref()).await?;
            let cid = state
                .ensure_campaign(&campaign, "(queue-clear)", "(unspecified)")
                .await?;
            let pending = state.list_drafts_awaiting_approval(cid).await?;
            println!(
                "queue-clear `{campaign}`: {} awaiting-approval touches will be REJECTED",
                pending.len()
            );
            {
                use dialoguer::Input;
                let typed: String = Input::new()
                    .with_prompt(format!("Type the campaign name (`{campaign}`) to confirm"))
                    .interact_text()
                    .map_err(|e| anyhow::anyhow!("dialoguer: {e}"))?;
                if typed.trim() != campaign {
                    anyhow::bail!("typed campaign name did not match — aborting");
                }
            }
            let mut rejected = 0u32;
            for t in &pending {
                if state.reject_touch(t.touch_id).await? == 1 {
                    rejected += 1;
                }
            }
            println!("queue-clear: rejected {rejected} touches");
        }

        Cmd::Preflight {
            campaign,
            no_probe,
            sample_drafts,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            println!(
                "salesman preflight `{campaign}` — {}",
                chrono::Utc::now().to_rfc3339()
            );
            println!("==========================================\n");

            let mut blockers: Vec<String> = Vec::new();
            let mut warnings: Vec<String> = Vec::new();

            macro_rules! check {
                ($label:expr, $body:expr) => {{
                    print!("[ {:<22} ]  ", $label);
                    let r: Result<Result<(), String>, anyhow::Error> = $body;
                    match r {
                        Ok(Ok(())) => println!("OK"),
                        Ok(Err(e)) => {
                            println!("BLOCK  {e}");
                            blockers.push(format!("{}: {e}", $label));
                        }
                        Err(e) => {
                            println!("WARN   {e}");
                            warnings.push(format!("{}: {e}", $label));
                        }
                    }
                }};
            }

            // --- signing key
            check!(
                "signing key",
                Ok::<_, anyhow::Error>({
                    let seed = salesman_receipts::default_seed_path();
                    if seed.exists() {
                        Ok(())
                    } else {
                        Err(format!("seed file not present at {}", seed.display()))
                    }
                })
            );

            // --- unsubscribe minter
            check!(
                "unsubscribe minter",
                Ok::<_, anyhow::Error>({
                    match salesman_outreach::UnsubscribeTokens::from_env() {
                        Ok(t) => {
                            if t.base_url().starts_with("https://") {
                                Ok(())
                            } else if t.base_url().starts_with("http://localhost")
                                || t.base_url().starts_with("http://127.0.0.1")
                            {
                                Err(
                                "base URL is http://localhost — fine for dev, NOT for production"
                                    .into(),
                            )
                            } else {
                                Err("base URL must be HTTPS for Gmail/Yahoo to honor it".into())
                            }
                        }
                        Err(e) => Err(format!("not configured: {e}")),
                    }
                })
            );

            // --- SMTP env
            check!(
                "smtp env",
                Ok::<_, anyhow::Error>({
                    match SmtpConfig::from_env() {
                        Ok(_) => Ok(()),
                        Err(e) => Err(format!("{e}")),
                    }
                })
            );

            // --- SMTP probe (TCP only; no auth, no SEND)
            if !no_probe {
                check!(
                    "smtp connect",
                    Ok::<_, anyhow::Error>({
                        match SmtpConfig::from_env() {
                            Ok(cfg) => {
                                let connect =
                                    tokio::net::TcpStream::connect((cfg.host.as_str(), cfg.port));
                                let r = tokio::time::timeout(
                                    std::time::Duration::from_secs(5),
                                    connect,
                                )
                                .await;
                                match r {
                                    Ok(Ok(_)) => Ok(()),
                                    Ok(Err(e)) => Err(format!("tcp: {e}")),
                                    Err(_) => Err("timeout (5s)".into()),
                                }
                            }
                            Err(e) => Err(format!("{e}")),
                        }
                    })
                );
            }

            // --- LLM backends
            check!(
                "llm backends",
                Ok::<_, anyhow::Error>({
                    let kinds = router.registered_kinds();
                    if kinds.is_empty() {
                        Err(
                            "no backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                                .into(),
                        )
                    } else {
                        Ok(())
                    }
                })
            );

            // --- campaign + prospects
            let cid = state
                .ensure_campaign(&campaign, "(preflight)", "(unspecified)")
                .await?;
            let pending_drafts = state.list_drafts_awaiting_approval(cid).await?;
            check!(
                "campaign + drafts",
                Ok::<_, anyhow::Error>({
                    if pending_drafts.is_empty() {
                        Err(format!(
                            "no awaiting-approval drafts in `{campaign}` — \
                         run `salesman draft --campaign {campaign}` first"
                        ))
                    } else {
                        Ok(())
                    }
                })
            );

            // --- test/demo prospects in queue
            check!(
                "queue hygiene",
                Ok::<_, anyhow::Error>({
                    let bad: Vec<&TouchSummary> = pending_drafts
                        .iter()
                        .filter(|t| {
                            let c = t.company.to_ascii_lowercase();
                            c.contains("test")
                                || c.contains("example")
                                || c.contains("demo")
                                || c == "(testing)"
                                || c.starts_with("acme")
                        })
                        .collect();
                    if bad.is_empty() {
                        Ok(())
                    } else {
                        Err(format!(
                            "{} draft(s) target obvious test companies (acme/test/demo/example) — \
                         queue-clear and re-discover from a real CSV",
                            bad.len()
                        ))
                    }
                })
            );

            // --- AI-detector pass on drafts
            if !pending_drafts.is_empty() {
                let mut high_score = 0u32;
                let mut max_seen = 0.0f32;
                for t in &pending_drafts {
                    let s = salesman_detector::score(&t.body, t.subject.as_deref());
                    if s.score > max_seen {
                        max_seen = s.score;
                    }
                    if s.score >= 0.6 {
                        high_score += 1;
                    }
                }
                check!(
                    "detector ensemble",
                    Ok::<_, anyhow::Error>({
                        if high_score == 0 {
                            Ok(())
                        } else {
                            Err(format!(
                                "{}/{} draft(s) score ≥0.6 on the AI-detector ensemble (max {:.2}) \
                             — review and regenerate before sending",
                                high_score,
                                pending_drafts.len(),
                                max_seen
                            ))
                        }
                    })
                );
            }

            // --- sample drafts
            if sample_drafts > 0 && !pending_drafts.is_empty() {
                println!(
                    "\nSample drafts (first {} of {}):",
                    sample_drafts.min(pending_drafts.len()),
                    pending_drafts.len()
                );
                println!("{}", "-".repeat(60));
                for t in pending_drafts.iter().take(sample_drafts) {
                    println!(
                        "\n[{}] subject: {:?}",
                        t.company,
                        t.subject.as_deref().unwrap_or("")
                    );
                    let snippet: String = t.body.chars().take(280).collect();
                    println!(
                        "{snippet}{}",
                        if t.body.chars().count() > 280 {
                            "..."
                        } else {
                            ""
                        }
                    );
                }
                println!("{}", "-".repeat(60));
            }

            println!();
            println!("==========================================");
            if blockers.is_empty() && warnings.is_empty() {
                println!(
                    "VERDICT: READY — safe to `salesman send-pending --campaign {campaign} --for-real --confirm-typed`"
                );
            } else if blockers.is_empty() {
                println!("VERDICT: READY-WITH-WARNINGS ({})", warnings.len());
                for w in &warnings {
                    println!("  - {w}");
                }
            } else {
                println!(
                    "VERDICT: BLOCKED — {} blocker(s), {} warning(s)",
                    blockers.len(),
                    warnings.len()
                );
                for b in &blockers {
                    println!("  - {b}");
                }
                anyhow::bail!("preflight blocked");
            }
        }

        Cmd::Doctor {
            probe_smtp,
            probe_imap,
        } => {
            // Header
            println!("salesman doctor — {}", chrono::Utc::now().to_rfc3339());
            println!("==========================================\n");

            let mut required_failures = 0u32;
            let mut warnings = 0u32;

            // --- DB
            print!("[ db          ]  ");
            match require_state(cli.database_url.as_deref()).await {
                Ok(s) => match s.count_companies().await {
                    Ok(n) => println!("OK  ({n} companies)"),
                    Err(e) => {
                        println!("FAIL  {e}");
                        required_failures += 1;
                    }
                },
                Err(e) => {
                    println!("FAIL  {e}");
                    required_failures += 1;
                }
            }

            // --- LLM backends
            print!("[ llm         ]  ");
            let kinds = router.registered_kinds();
            if kinds.is_empty() {
                println!(
                    "FAIL  no backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
                required_failures += 1;
            } else {
                let names: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
                println!("OK  {} registered ({})", names.len(), names.join(", "));
            }

            // --- signing key
            print!("[ signing key ]  ");
            let seed = salesman_receipts::default_seed_path();
            if seed.exists() {
                println!("OK  {}", seed.display());
            } else {
                println!(
                    "WARN  not present (will be generated on first send)  {}",
                    seed.display()
                );
                warnings += 1;
            }

            // --- SMTP env
            print!("[ smtp env    ]  ");
            match SmtpConfig::from_env() {
                Ok(_) => println!("OK  SALESMAN_SMTP_* set"),
                Err(e) => {
                    println!("WARN  {e}  (required for send-pending --for-real)");
                    warnings += 1;
                }
            }

            // --- per-recipient unsubscribe minter (RFC 8058)
            print!("[ unsub minter]  ");
            match salesman_outreach::UnsubscribeTokens::from_env() {
                Ok(t) => {
                    let scheme_ok = t.base_url().starts_with("https://")
                        || t.base_url().starts_with("http://localhost")
                        || t.base_url().starts_with("http://127.0.0.1");
                    if scheme_ok {
                        println!("OK  base={}", t.base_url());
                    } else {
                        println!(
                            "WARN  base_url is plain http on a non-localhost host — \
                             Gmail / Yahoo will not honor List-Unsubscribe over plaintext"
                        );
                        warnings += 1;
                    }
                }
                Err(e) => {
                    println!(
                        "WARN  {e}  (Gmail + Yahoo bulk-sender rules require RFC 8058 one-click; \
                         set SALESMAN_UNSUBSCRIBE_BASE_URL + SALESMAN_UNSUBSCRIBE_HMAC_SECRET)"
                    );
                    warnings += 1;
                }
            }

            // --- IMAP env
            print!("[ imap env    ]  ");
            match ImapConfig::from_env() {
                Ok(_) => println!("OK  SALESMAN_IMAP_* set"),
                Err(e) => {
                    println!("WARN  {e}  (required for inbox-poll)");
                    warnings += 1;
                }
            }

            // --- anti-spoof gate (Authentication-Results trust)
            //
            // Without SALESMAN_TRUSTED_AUTHSERV_ID set, classify-replies
            // CANNOT defend against forged inbound replies that try to
            // poison our suppression list. Operator should set this to
            // their MX hostname (the value Postfix etc. stamps in the
            // `Authentication-Results:` header).
            print!("[ auth gate   ]  ");
            match std::env::var("SALESMAN_TRUSTED_AUTHSERV_ID") {
                Ok(v) if !v.trim().is_empty() => {
                    println!("OK  trusted authserv-id = `{}`", v.trim());
                }
                _ => {
                    println!(
                        "WARN  SALESMAN_TRUSTED_AUTHSERV_ID unset — \
                         classify-replies will NOT verify SPF/DKIM/DMARC \
                         on inbound optouts/legal-threats. Set it to your \
                         MX hostname (e.g. mail.plausiden.com) to defend \
                         against suppression-list poisoning."
                    );
                    warnings += 1;
                }
            }

            // --- SMTP probe (optional)
            if probe_smtp {
                print!("[ smtp connect]  ");
                match SmtpConfig::from_env() {
                    Ok(cfg) => match SmtpSender::new(cfg) {
                        Ok(_) => println!("OK  transport built (no email sent)"),
                        Err(e) => {
                            println!("FAIL  {e}");
                            required_failures += 1;
                        }
                    },
                    Err(_) => println!("SKIP  (no SMTP env)"),
                }
            }

            // --- IMAP probe (optional)
            if probe_imap {
                print!("[ imap connect]  ");
                match ImapConfig::from_env() {
                    Ok(cfg) => {
                        let _poller = ImapPoller::new(cfg);
                        println!("OK  poller built (no mailbox modify)");
                    }
                    Err(_) => println!("SKIP  (no IMAP env)"),
                }
            }

            // --- pipeline + quality signal
            if let Some(state) = state_arc.as_ref() {
                let summary = state.pipeline_summary(24).await.ok();
                if let Some(s) = summary {
                    print!("[ pipeline    ]  ");
                    println!(
                        "OK  prospects={} awaiting={} suppressions={}",
                        s.prospects, s.awaiting_approval, s.suppressions
                    );
                }
                let cost = state.cost_summary(24).await.ok();
                if let Some(c) = cost {
                    let total: i64 = c.iter().map(|r| r.cost_micro_usd).sum();
                    print!("[ llm cost 24h]  ");
                    println!(
                        "OK  ${:.4} across {} models",
                        (total as f64) / 1_000_000.0,
                        c.len()
                    );
                }
            }

            // --- web-01 mail relay reachable (5s timeout)
            print!("[ mail relay  ]  ");
            let connect_fut = tokio::net::TcpStream::connect("mail.plausiden.com:25");
            let conn_result =
                tokio::time::timeout(std::time::Duration::from_secs(5), connect_fut).await;
            match conn_result {
                Ok(Ok(_)) => println!("OK  mail.plausiden.com:25 reachable"),
                Ok(Err(e)) => {
                    println!("WARN  {e}");
                    warnings += 1;
                }
                Err(_) => {
                    println!("WARN  timeout (5s)");
                    warnings += 1;
                }
            }
            // --- disk
            print!("[ disk        ]  ");
            match tokio::fs::metadata("/").await {
                Ok(_) => {
                    // Just report — we can't reasonably know the threshold
                    // without statvfs. Skip the percent for now.
                    println!("OK  / mounted");
                }
                Err(e) => {
                    println!("FAIL  {e}");
                    required_failures += 1;
                }
            }

            println!();
            println!("==========================================");
            if required_failures == 0 && warnings == 0 {
                println!("VERDICT: GREEN — all systems go");
            } else if required_failures == 0 {
                println!("VERDICT: YELLOW — {warnings} warning(s); send path may not work yet");
            } else {
                println!(
                    "VERDICT: RED — {required_failures} required failure(s) + {warnings} warning(s)"
                );
                anyhow::bail!("doctor: required components missing");
            }
        }

        Cmd::Status => {
            // Probe each subsystem; emit JSON; exit 1 if anything required is down.
            let mut report = serde_json::Map::new();
            let mut required_ok = true;

            // db
            let db_status = match require_state(cli.database_url.as_deref()).await {
                Ok(s) => match s.count_companies().await {
                    Ok(n) => {
                        report.insert("companies".into(), serde_json::Value::from(n));
                        serde_json::json!({"ok": true})
                    }
                    Err(e) => {
                        required_ok = false;
                        serde_json::json!({"ok": false, "err": e.to_string()})
                    }
                },
                Err(e) => {
                    required_ok = false;
                    serde_json::json!({"ok": false, "err": e.to_string()})
                }
            };
            report.insert("db".into(), db_status);

            // llm backends
            let kinds = router.registered_kinds();
            report.insert(
                "llm_backends".into(),
                serde_json::json!({
                    "registered": kinds.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
                    "ok": !kinds.is_empty()
                }),
            );
            if kinds.is_empty() {
                required_ok = false;
            }

            // signing key
            let signing_present =
                std::path::Path::new("/opt/salesman/config/signing.seed").exists();
            report.insert(
                "signing_key".into(),
                serde_json::json!({
                    "path": "/opt/salesman/config/signing.seed",
                    "ok": signing_present,
                }),
            );

            // smtp + imap env presence
            report.insert(
                "smtp_env_set".into(),
                serde_json::Value::from(std::env::var("SALESMAN_SMTP_HOST").is_ok()),
            );
            report.insert(
                "imap_env_set".into(),
                serde_json::Value::from(std::env::var("SALESMAN_IMAP_HOST").is_ok()),
            );

            // unsubscribe minter (Gmail / Yahoo bulk-sender requirement)
            // Not strictly REQUIRED for the binary to run, but
            // required-by-policy for any --for-real send. Mirrored
            // from doctor's [ unsub minter ] check.
            let unsub_set = std::env::var("SALESMAN_UNSUBSCRIBE_BASE_URL").is_ok()
                && std::env::var("SALESMAN_UNSUBSCRIBE_HMAC_SECRET").is_ok();
            report.insert(
                "unsubscribe_minter_env_set".into(),
                serde_json::Value::from(unsub_set),
            );

            // anti-spoof gate — without this, classify-replies fails
            // open. Surfaced as a warning, not a required failure,
            // because the gate is opt-in by design (see B5.5).
            let auth_gate = std::env::var("SALESMAN_TRUSTED_AUTHSERV_ID")
                .ok()
                .filter(|v| !v.trim().is_empty());
            report.insert(
                "auth_gate".into(),
                serde_json::json!({
                    "trusted_authserv_id": auth_gate,
                    "engaged": auth_gate.is_some(),
                }),
            );

            // sender identity — humans see these in From:; absence
            // means send-pending --for-real will fail at the SMTP
            // sender stage.
            report.insert(
                "sender_identity".into(),
                serde_json::json!({
                    "from_name_set":  std::env::var("SALESMAN_FROM_NAME").is_ok(),
                    "from_email_set": std::env::var("SALESMAN_FROM_EMAIL").is_ok(),
                    "reply_to_set":   std::env::var("SALESMAN_REPLY_TO").is_ok(),
                }),
            );

            // llm transport — useful for monitoring to know whether
            // the box is on the API path or the subscriber-CLI path.
            let transport = std::env::var("SALESMAN_LLM_TRANSPORT")
                .unwrap_or_else(|_| "api".to_string())
                .to_ascii_lowercase();
            report.insert("llm_transport".into(), serde_json::Value::from(transport));

            report.insert("required_ok".into(), serde_json::Value::from(required_ok));
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !required_ok {
                anyhow::bail!("status: required components missing");
            }
        }

        Cmd::DefineSequence {
            campaign,
            name,
            from_toml,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let toml_text = std::fs::read_to_string(&from_toml)
                .with_context(|| format!("read {}", from_toml.display()))?;
            let parsed: SequenceFile = toml::from_str(&toml_text)
                .with_context(|| format!("parse {}", from_toml.display()))?;
            if parsed.steps.is_empty() {
                anyhow::bail!("sequence file has no steps");
            }
            let campaign_id = state
                .ensure_campaign(&campaign, "(sequence-only)", "(unspecified)")
                .await?;
            let inputs: Vec<salesman_state::SequenceStepInput> = parsed
                .steps
                .into_iter()
                .map(|s| salesman_state::SequenceStepInput {
                    channel: s.channel.unwrap_or_else(|| "email".into()),
                    template_key: s.template_key,
                    delay_days: s.delay_days.unwrap_or(0),
                })
                .collect();
            let sid = state.create_sequence(campaign_id, &name, &inputs).await?;
            println!(
                "created sequence `{name}` (id={sid}) with {} step(s)",
                inputs.len()
            );
        }

        Cmd::AssignSequence { campaign, sequence } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(assign-only)", "(unspecified)")
                .await?;
            // Look up sequence by (campaign, name).
            let sid = sqlx_lookup_sequence(&state, campaign_id, &sequence).await?;
            let n = state.assign_sequence_to_campaign(campaign_id, sid).await?;
            println!("assigned sequence `{sequence}` to {n} new prospects (idempotent)");
        }

        Cmd::DnsCheck {
            domain,
            dkim_selector,
            sender_ip,
            expected_ptr,
        } => {
            println!(
                "salesman dns-check `{domain}` — {}",
                chrono::Utc::now().to_rfc3339()
            );
            println!("==========================================\n");

            let mut blockers = 0u32;
            let mut warnings = 0u32;

            // --- SPF
            print!("[ SPF              ]  ");
            match dig_txt(&domain).await {
                Ok(records) => {
                    let spf_records: Vec<&String> =
                        records.iter().filter(|r| r.starts_with("v=spf1")).collect();
                    match spf_records.len() {
                        0 => {
                            println!(
                                "BLOCK  no SPF record on {domain} \
                                 (publish: `v=spf1 ip4:<sender_ip> -all`)"
                            );
                            blockers += 1;
                        }
                        1 => {
                            let r = spf_records[0];
                            if r.ends_with("-all") {
                                println!("OK    {r}");
                            } else if r.ends_with("~all") {
                                println!(
                                    "WARN  uses `~all` (softfail) — fine for warmup \
                                     but escalate to `-all` after 48h: {r}"
                                );
                                warnings += 1;
                            } else if r.ends_with("?all") {
                                println!(
                                    "WARN  uses `?all` (neutral) — Gmail will not honor it: {r}"
                                );
                                warnings += 1;
                            } else if r.ends_with("+all") {
                                println!(
                                    "BLOCK SPF allows ANY sender (+all) — strip and \
                                     republish with -all: {r}"
                                );
                                blockers += 1;
                            } else {
                                println!("WARN  no qualifier on `all` (defaulting to +all): {r}");
                                warnings += 1;
                            }
                        }
                        n => {
                            println!(
                                "BLOCK {n} SPF records published; RFC 7208 forbids more than one. \
                                 Merge into a single record."
                            );
                            blockers += 1;
                        }
                    }
                }
                Err(e) => {
                    println!("WARN  cannot resolve TXT for {domain}: {e}");
                    warnings += 1;
                }
            }

            // --- DKIM
            let dkim_name = format!("{dkim_selector}._domainkey.{domain}");
            print!("[ DKIM             ]  ");
            match dig_txt(&dkim_name).await {
                Ok(records) if !records.is_empty() => {
                    let r = records.join(" ");
                    if r.contains("v=DKIM1") && r.contains("p=") && !r.contains("p=;") {
                        println!("OK    {dkim_name} ({} chars)", r.len());
                    } else if r.contains("p=;") {
                        println!(
                            "BLOCK DKIM record published but public key is empty (`p=;`) — \
                             selector revoked. Re-run opendkim-genkey + re-publish."
                        );
                        blockers += 1;
                    } else {
                        println!("WARN  unexpected DKIM record shape: {r}");
                        warnings += 1;
                    }
                }
                Ok(_) => {
                    println!(
                        "BLOCK no DKIM record at {dkim_name} \
                         (run `opendkim-genkey -d {domain} -s {dkim_selector}` and \
                         publish the .txt as TXT)"
                    );
                    blockers += 1;
                }
                Err(e) => {
                    println!("WARN  cannot resolve {dkim_name}: {e}");
                    warnings += 1;
                }
            }

            // --- DMARC
            let dmarc_name = format!("_dmarc.{domain}");
            print!("[ DMARC            ]  ");
            match dig_txt(&dmarc_name).await {
                Ok(records) => {
                    let dmarc: Vec<&String> = records
                        .iter()
                        .filter(|r| r.starts_with("v=DMARC1"))
                        .collect();
                    match dmarc.len() {
                        0 => {
                            println!(
                                "BLOCK no DMARC record at {dmarc_name} \
                                 (publish `v=DMARC1; p=none; rua=mailto:dmarc@<root-domain>`)"
                            );
                            blockers += 1;
                        }
                        1 => {
                            let r = dmarc[0];
                            let policy = r
                                .split(';')
                                .find_map(|f| f.trim().strip_prefix("p="))
                                .unwrap_or("?");
                            let level = match policy {
                                "none" => {
                                    "WARN  policy=none — fine for first-week monitoring; escalate to quarantine then reject after 7+ days clean"
                                }
                                "quarantine" => "OK    policy=quarantine",
                                "reject" => "OK    policy=reject (hardest)",
                                _ => "WARN  unrecognized policy",
                            };
                            if level.starts_with("WARN") {
                                warnings += 1;
                            }
                            println!("{level}: {r}");
                        }
                        n => {
                            println!("BLOCK {n} DMARC records published; only one allowed.");
                            blockers += 1;
                        }
                    }
                }
                Err(e) => {
                    println!("WARN  cannot resolve {dmarc_name}: {e}");
                    warnings += 1;
                }
            }

            // --- PTR (optional; only if sender_ip provided)
            if let (Some(ip), Some(expected)) = (sender_ip.as_ref(), expected_ptr.as_ref()) {
                print!("[ PTR              ]  ");
                match dig_ptr(ip).await {
                    Ok(records) if !records.is_empty() => {
                        let normalized: Vec<String> = records
                            .iter()
                            .map(|s| s.trim_end_matches('.').to_string())
                            .collect();
                        let exp = expected.trim_end_matches('.');
                        if normalized.iter().any(|r| r == exp) {
                            println!("OK    {ip} → {exp}");
                        } else {
                            println!(
                                "BLOCK PTR for {ip} resolves to {} (expected {exp}). \
                                 Update Vultr reverse-DNS in the IP settings panel.",
                                normalized.join(", ")
                            );
                            blockers += 1;
                        }
                    }
                    Ok(_) => {
                        println!("BLOCK no PTR for {ip}. Set Vultr reverse-DNS to {expected}.");
                        blockers += 1;
                    }
                    Err(e) => {
                        println!("WARN  cannot resolve PTR for {ip}: {e}");
                        warnings += 1;
                    }
                }
            } else if sender_ip.is_some() != expected_ptr.is_some() {
                println!(
                    "[ PTR              ]  WARN  --sender-ip and --expected-ptr must be \
                     used together; PTR check skipped"
                );
                warnings += 1;
            }

            println!();
            println!("==========================================");
            if blockers == 0 && warnings == 0 {
                println!("VERDICT: GREEN — DNS is fully configured");
            } else if blockers == 0 {
                println!(
                    "VERDICT: YELLOW — {warnings} warning(s); send may work but reputation may suffer"
                );
            } else {
                println!(
                    "VERDICT: RED — {blockers} blocker(s) + {warnings} warning(s); fix before send"
                );
                anyhow::bail!("dns-check: {blockers} blocker(s)");
            }
        }

        Cmd::Geo {
            query,
            brand,
            aliases,
            recommend,
        } => {
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let aliases_vec: Vec<String> = aliases
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let tool = salesman_content::GeoTool::new(router.clone());
            let args = serde_json::json!({
                "query": query,
                "brand": brand,
                "aliases": aliases_vec,
                "recommend": recommend,
            });
            let result = salesman_tools::Tool::invoke(&tool, salesman_core::ToolArgs(args)).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }

            let report: salesman_content::GeoReport = serde_json::from_value(result)?;
            println!("=== AI-search visibility — `{}` ===\n", report.query);
            println!("Backend: {} ({})", report.backend, report.model);
            println!("Brand sought: {}", report.brand);
            if report.brand_mentioned {
                let pos = report
                    .mention_position
                    .map(|p| format!(" at position #{}", p + 1))
                    .unwrap_or_default();
                println!("✅ MENTIONED{pos}\n");
            } else {
                println!("❌ NOT MENTIONED\n");
            }
            if !report.competitors_mentioned.is_empty() {
                println!("Competitors mentioned:");
                for c in &report.competitors_mentioned {
                    println!("  - {c}");
                }
                println!();
            }
            println!("--- raw LLM response ---");
            for line in report.raw_response.lines().take(40) {
                println!("  {line}");
            }
            if report.raw_response.lines().count() > 40 {
                println!("  ... (truncated)");
            }
            if !report.recommendations.is_empty() {
                println!("\n--- 5 concrete actions to improve visibility ---");
                for (i, r) in report.recommendations.iter().enumerate() {
                    println!("  {}. {r}", i + 1);
                }
            } else if !recommend {
                println!(
                    "\nRun again with --recommend to generate 5 concrete content + markup actions."
                );
            }
        }

        Cmd::PickAngle {
            campaign,
            catalog,
            max,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let catalog_text = std::fs::read_to_string(&catalog)
                .with_context(|| format!("reading catalog {}", catalog.display()))?;
            let products = salesman_content::load_catalog_toml(&catalog_text)?;
            let products_value = serde_json::to_value(&products)?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(pick-angle)", "(unspecified)")
                .await?;
            let prospects = state
                .list_prospects_with_facts_for_campaign(campaign_id)
                .await?;
            // Local-first BEFORE truncate, so a limited batch targets local
            // prospects first (no-op when SALESMAN_TARGET_LOCALITY unset).
            let loc_terms = target_locality_terms();
            let loc_refs: Vec<&str> = loc_terms.iter().map(|s| s.as_str()).collect();
            let mut prospects = order_local_first_with_terms(prospects, &loc_refs);
            prospects.truncate(max);
            if prospects.is_empty() {
                println!("(no prospects in `{campaign}` to score)");
                return Ok(());
            }
            let picker = salesman_content::AnglePickerTool::new(router.clone(), "PlausiDen");
            println!(
                "pick-angle `{campaign}`: matching {} prospect(s) against {} product(s)\n",
                prospects.len(),
                products.len()
            );
            let mut by_product: std::collections::BTreeMap<String, u32> =
                std::collections::BTreeMap::new();
            let mut total_conf: f32 = 0.0;
            let mut count: u32 = 0;
            for p in &prospects {
                let prospect_json = p.to_prompt_json();
                let args = serde_json::json!({
                    "prospect": prospect_json,
                    "catalog":  products_value,
                });
                match salesman_tools::Tool::invoke(&picker, salesman_core::ToolArgs(args)).await {
                    Ok(v) => {
                        let product = v
                            .get("picked_product")
                            .and_then(|x| x.as_str())
                            .unwrap_or("?");
                        let angle = v
                            .get("picked_angle")
                            .and_then(|x| x.as_str())
                            .unwrap_or("?");
                        let rationale = v.get("rationale").and_then(|x| x.as_str()).unwrap_or("");
                        let conf =
                            v.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                        let valid = v
                            .get("valid_pick")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(true);
                        *by_product.entry(product.to_string()).or_default() += 1;
                        total_conf += conf;
                        count += 1;
                        println!(
                            "[{:.2}] {} → {} ({}){}\n         {}",
                            conf,
                            p.display_name,
                            product,
                            angle,
                            if valid { "" } else { " ⚠ FALLBACK" },
                            rationale.chars().take(180).collect::<String>(),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(prospect = %p.display_name, "%e" = %e, "pick failed");
                    }
                }
            }
            if count > 0 {
                println!(
                    "\npick-angle complete: scored {count}, mean confidence {:.2}. \
                     Distribution: {:?}",
                    total_conf / count as f32,
                    by_product
                );
            }
        }

        Cmd::FindBuyers {
            campaign,
            top,
            persist,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(find-buyers)", "(unspecified)")
                .await?;
            let companies = state.list_companies_for_campaign(campaign_id).await?;
            println!(
                "find-buyers `{campaign}`: scraping team pages for {} companies (persist={persist})\n",
                companies.len()
            );
            let scraper = salesman_discovery::TeamScraper::new();
            let mut hit_count = 0u32;
            let mut miss_count = 0u32;
            let mut persisted = 0u32;
            for (company_id, name, homepage) in &companies {
                let homepage_url = match homepage.as_deref().and_then(|s| url::Url::parse(s).ok()) {
                    Some(u) => u,
                    None => {
                        println!("  [skip] {name}: no parseable homepage");
                        miss_count += 1;
                        continue;
                    }
                };
                let candidates = match scraper.find_for_company(name, &homepage_url, top).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(company = %name, "%e" = %e, "team scrape failed");
                        miss_count += 1;
                        continue;
                    }
                };
                if candidates.is_empty() {
                    println!("  [miss] {name}: no decision-maker candidates found");
                    miss_count += 1;
                    continue;
                }
                hit_count += 1;
                println!("  [hit]  {name}");
                for (i, c) in candidates.iter().enumerate() {
                    println!(
                        "         {i}. {:>5.2} | {:<28} | {:<22} | {} ({})",
                        c.confidence, c.name, c.role, c.email, c.email_pattern,
                    );
                }
                if persist
                    && let Some(top_c) = candidates.first()
                    && let Some(prospect_id) = state
                        .find_prospect_by_company_in_campaign(campaign_id, *company_id)
                        .await?
                {
                    match state
                        .insert_contact_and_link_as_primary(
                            *company_id,
                            prospect_id,
                            &top_c.name,
                            &top_c.role,
                            &top_c.email,
                            &format!("team_scraper:{}", top_c.source_url),
                        )
                        .await
                    {
                        Ok(contact_id) => {
                            persisted += 1;
                            println!("         → persisted contact {contact_id} as primary");
                        }
                        Err(e) => {
                            tracing::warn!(company = %name, "%e" = %e, "persist failed");
                        }
                    }
                }
            }
            println!(
                "\nfind-buyers complete: {hit_count} hit(s), {miss_count} miss(es), {persisted} persisted. \
                 Email addresses are GUESSES — verify before sending."
            );
        }

        Cmd::ReferralAsk {
            min_days,
            batch,
            product,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let pool = state
                .list_won_prospects_for_referral_ask(min_days, batch)
                .await?;
            if pool.is_empty() {
                println!("no won prospects eligible for referral-ask (min-days={min_days}).");
                return Ok(());
            }
            println!(
                "drafting referral-ask for {} won prospect(s) (min-days={min_days}, product={product})\n",
                pool.len()
            );
            let drafter = salesman_content::DraftColdEmailTool::new(
                router.clone(),
                "the PlausiDen team",
                "PlausiDen",
                "Plausible deniability + sovereign data tools for SMB security teams.",
            );
            // Force the referral_ask template via env so the existing
            // template-loading path in DraftColdEmailTool picks it up.
            // (The tool reads SALESMAN_TEMPLATES_DIR + a `template_key`
            // arg — we pass referral_ask explicitly.)
            let mut ok = 0u32;
            let mut err = 0u32;
            for p in &pool {
                let prospect_json = p.to_prompt_json();
                let args = serde_json::json!({
                    "prospect": prospect_json,
                    "product":  product,
                    "template_key": "referral_ask",
                    "angle_hint": "ask for two specific-shape intros to similar-pain companies; warm + grateful; no hard sell",
                });
                match salesman_tools::Tool::invoke(&drafter, salesman_core::ToolArgs(args)).await {
                    Ok(v) => {
                        let subject = v.get("subject").and_then(|x| x.as_str()).unwrap_or("");
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                        let produced_by = v.get("produced_by").cloned();
                        match state
                            .insert_touch_draft_full(
                                p.prospect_id,
                                salesman_core::TouchChannel::Email,
                                Some(subject),
                                body,
                                Some("referral_ask"),
                                produced_by,
                            )
                            .await
                        {
                            Ok(touch_id) => {
                                ok += 1;
                                println!("  [drafted] {} → touch {touch_id}", p.display_name);
                            }
                            Err(e) => {
                                err += 1;
                                tracing::warn!(prospect = %p.display_name, "%e" = %e, "persist failed");
                            }
                        }
                    }
                    Err(e) => {
                        err += 1;
                        tracing::warn!(prospect = %p.display_name, "%e" = %e, "draft failed");
                    }
                }
            }
            println!(
                "\nreferral-ask complete: {ok} drafted, {err} error(s). \
                 Review with `salesman review`, then send like any other batch."
            );
        }

        Cmd::Cadence { action } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            match action {
                CadenceCmd::List { limit } => {
                    let rows = state.list_paused_prospects(limit).await?;
                    if cli.json {
                        let v = serde_json::json!({
                            "count": rows.len(),
                            "paused": rows.iter().map(|(pid, company, reason, last)| serde_json::json!({
                                "prospect_id": pid.0.to_string(),
                                "company": company,
                                "reason": reason,
                                "last_advanced_at": last.to_rfc3339(),
                            })).collect::<Vec<_>>(),
                        });
                        println!("{}", serde_json::to_string_pretty(&v)?);
                        return Ok(());
                    }
                    if rows.is_empty() {
                        println!("(no paused prospects — every sequence is active)");
                    } else {
                        println!("=== {} paused prospect(s) ===\n", rows.len());
                        println!(
                            "{:<38} {:<28} {:<22} reason",
                            "prospect_id", "company", "last_advanced_at"
                        );
                        println!("{}", "-".repeat(120));
                        for (pid, company, reason, last) in &rows {
                            let comp = if company.chars().count() > 26 {
                                format!("{}…", company.chars().take(25).collect::<String>())
                            } else {
                                company.clone()
                            };
                            println!(
                                "{:<38} {:<28} {:<22} {}",
                                pid.0,
                                comp,
                                last.format("%Y-%m-%d %H:%M:%SZ"),
                                reason,
                            );
                        }
                        println!();
                        println!("Resume one with: salesman cadence resume --prospect-id <UUID>");
                    }
                }
                CadenceCmd::Resume { prospect_id } => {
                    let pid: uuid::Uuid = prospect_id
                        .parse()
                        .with_context(|| format!("invalid prospect-id `{prospect_id}`"))?;
                    let n = state
                        .resume_prospect_sequence(salesman_core::ProspectId(pid))
                        .await?;
                    if n == 0 {
                        println!(
                            "no paused prospect-sequence with id {pid} (already active or unknown)"
                        );
                    } else {
                        println!("resumed: {pid}");
                    }
                }
            }
        }

        Cmd::Triggers { action } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            match action {
                TriggerCmd::Scan {
                    campaign,
                    max_age_days,
                    max_per_prospect,
                } => {
                    let campaign_id = state
                        .ensure_campaign(&campaign, "(triggers-scan)", "(unspecified)")
                        .await?;
                    let companies = state.list_companies_for_campaign(campaign_id).await?;
                    println!(
                        "triggers scan `{campaign}`: probing GDELT + HN for {} companies (max-age {max_age_days}d)\n",
                        companies.len()
                    );
                    let gdelt = salesman_osint::GdeltClient::new();
                    let hn = salesman_osint::HnClient::new();
                    let mut new_inserts = 0u32;
                    let mut probed = 0u32;
                    for (_company_id, name, _homepage) in &companies {
                        let prospect_id = match state
                            .find_prospect_by_company_in_campaign(campaign_id, *_company_id)
                            .await?
                        {
                            Some(p) => p,
                            None => continue,
                        };
                        probed += 1;

                        // ---- GDELT (news) ----
                        let hits = match gdelt.search_news(name, max_per_prospect as u32).await {
                            Ok(h) => h,
                            Err(e) => {
                                tracing::warn!(company = %name, "%e" = %e, "gdelt search failed");
                                vec![]
                            }
                        };
                        for hit in hits {
                            let recency = recency_score_from_seen_at(&hit.seen_at, max_age_days);
                            let relevance = if hit
                                .title
                                .to_ascii_lowercase()
                                .contains(&name.to_ascii_lowercase())
                            {
                                0.85
                            } else {
                                0.5
                            };
                            let raw = serde_json::json!({
                                "seen_at": hit.seen_at,
                                "source_country": hit.source_country,
                            });
                            match state
                                .insert_trigger_event(salesman_state::TriggerEventInsert {
                                    prospect_id,
                                    source: "gdelt",
                                    headline: &hit.title,
                                    url: Some(&hit.url),
                                    recency_score: recency,
                                    relevance_score: relevance,
                                    raw: &raw,
                                })
                                .await
                            {
                                Ok(true) => new_inserts += 1,
                                Ok(false) => {}
                                Err(e) => {
                                    tracing::warn!(company = %name, "%e" = %e, "insert (gdelt) failed");
                                }
                            }
                        }

                        // ---- HN (community discussion) ----
                        let hn_hits = match hn.search(name, max_per_prospect as u32).await {
                            Ok(h) => h,
                            Err(e) => {
                                tracing::warn!(company = %name, "%e" = %e, "hn search failed");
                                vec![]
                            }
                        };
                        for h in hn_hits {
                            let title = h.title.clone().unwrap_or_else(|| {
                                h.story_text
                                    .clone()
                                    .unwrap_or_else(|| h.comment_text.clone().unwrap_or_default())
                            });
                            if title.is_empty() {
                                continue;
                            }
                            // ISO 8601 date — recency_score helper expects YYYYMMDD;
                            // convert by stripping non-digits and taking first 14.
                            let condensed: String = h
                                .created_at
                                .chars()
                                .filter(|c| c.is_ascii_digit())
                                .collect();
                            let recency = recency_score_from_seen_at(&condensed, max_age_days);
                            // Title match → high relevance; otherwise body-only mention.
                            let lc_title = title.to_ascii_lowercase();
                            let relevance = if lc_title.contains(&name.to_ascii_lowercase()) {
                                0.85
                            } else {
                                0.55
                            };
                            let raw = serde_json::json!({
                                "object_id": h.object_id,
                                "points": h.points,
                                "author": h.author,
                                "created_at": h.created_at,
                            });
                            // Prefer the post URL if present; fall back to the
                            // canonical HN story URL.
                            let url = h
                                .url
                                .as_deref()
                                .filter(|u| !u.is_empty())
                                .unwrap_or(&h.story_url);
                            // Trim title for column-friendliness (tags + hashes
                            // can balloon to ≥200 chars).
                            let headline: String = title.chars().take(200).collect();
                            match state
                                .insert_trigger_event(salesman_state::TriggerEventInsert {
                                    prospect_id,
                                    source: "hn",
                                    headline: &headline,
                                    url: Some(url),
                                    recency_score: recency,
                                    relevance_score: relevance,
                                    raw: &raw,
                                })
                                .await
                            {
                                Ok(true) => new_inserts += 1,
                                Ok(false) => {}
                                Err(e) => {
                                    tracing::warn!(company = %name, "%e" = %e, "insert (hn) failed");
                                }
                            }
                        }
                    }
                    println!(
                        "triggers scan complete: probed {probed} prospect(s); inserted {new_inserts} new trigger(s) across GDELT + HN. \
                         Run `salesman triggers list --campaign {campaign}` to review."
                    );
                }
                TriggerCmd::List {
                    campaign,
                    since_hours,
                    top,
                    unused_only,
                } => {
                    let campaign_id = match campaign.as_deref() {
                        Some(name) => Some(
                            state
                                .ensure_campaign(name, "(triggers-list)", "(unspecified)")
                                .await?,
                        ),
                        None => None,
                    };
                    let rows = state
                        .list_trigger_events(campaign_id, since_hours, unused_only, top)
                        .await?;
                    if cli.json {
                        let v = serde_json::json!({
                            "since_hours": since_hours,
                            "unused_only": unused_only,
                            "count": rows.len(),
                            "triggers": rows.iter().map(|r| serde_json::json!({
                                "id": r.id.to_string(),
                                "prospect_id": r.prospect_id.0.to_string(),
                                "company": r.company,
                                "source": r.source,
                                "headline": r.headline,
                                "url": r.url,
                                "rank": r.rank(),
                                "recency_score": r.recency_score,
                                "relevance_score": r.relevance_score,
                                "created_at": r.created_at.to_rfc3339(),
                            })).collect::<Vec<_>>(),
                        });
                        println!("{}", serde_json::to_string_pretty(&v)?);
                        return Ok(());
                    }
                    if rows.is_empty() {
                        println!(
                            "(no trigger events match — run `triggers scan` first or widen the window)"
                        );
                    } else {
                        println!(
                            "=== top {} trigger event(s){} — last {since_hours}h ===\n",
                            rows.len(),
                            campaign
                                .as_deref()
                                .map(|c| format!(" in `{c}`"))
                                .unwrap_or_default(),
                        );
                        for (i, r) in rows.iter().enumerate() {
                            let head = if r.headline.chars().count() > 80 {
                                format!("{}…", r.headline.chars().take(79).collect::<String>())
                            } else {
                                r.headline.clone()
                            };
                            println!(
                                "{:>2}. [{:.2}] {} ({}) — {}",
                                i + 1,
                                r.rank(),
                                r.company,
                                r.source,
                                head,
                            );
                            if let Some(u) = &r.url {
                                println!("     {u}");
                            }
                        }
                    }
                }
                TriggerCmd::Draft {
                    campaign,
                    product,
                    since_hours,
                    top,
                } => {
                    if router.registered_kinds().is_empty() {
                        anyhow::bail!(
                            "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                        );
                    }
                    let campaign_id = state
                        .ensure_campaign(&campaign, "(triggers-draft)", "(unspecified)")
                        .await?;
                    let triggers = state
                        .list_trigger_events(Some(campaign_id), since_hours, true, top)
                        .await?;
                    if triggers.is_empty() {
                        println!(
                            "(no unused trigger events match — run `triggers scan` first or widen --since-hours)"
                        );
                    } else {
                        let draft_tool = DraftColdEmailTool::new(
                            router.clone(),
                            "the PlausiDen team",
                            "PlausiDen",
                            "Plausible deniability + sovereign data tools for SMB security teams.",
                        );
                        let mut ok = 0u32;
                        let mut err = 0u32;
                        let mut skipped_no_facts = 0u32;
                        for t in &triggers {
                            let p = match state.get_prospect_with_facts(t.prospect_id).await? {
                                Some(p) => p,
                                None => {
                                    skipped_no_facts += 1;
                                    tracing::warn!(
                                        prospect = %t.prospect_id.0,
                                        "trigger references unknown prospect — skipping",
                                    );
                                    continue;
                                }
                            };
                            let prospect_json = p.to_prompt_json();
                            let angle_hint = format!(
                                "anchor on this trigger event ({}): {}",
                                t.source, t.headline,
                            );
                            let tool_args = serde_json::json!({
                                "prospect": prospect_json,
                                "product":  product,
                                "angle_hint": angle_hint,
                            });
                            match salesman_tools::Tool::invoke(
                                &draft_tool,
                                salesman_core::ToolArgs(tool_args),
                            )
                            .await
                            {
                                Ok(v) => {
                                    let subject = v
                                        .get("subject")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("(no subject)");
                                    let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                                    let produced_by = v.get("produced_by").cloned();
                                    match state
                                        .insert_touch_draft_full(
                                            t.prospect_id,
                                            salesman_core::TouchChannel::Email,
                                            Some(subject),
                                            body,
                                            None,
                                            produced_by,
                                        )
                                        .await
                                    {
                                        Ok(touch_id) => {
                                            // Tag the trigger as used so the next
                                            // run doesn't re-draft the same anchor.
                                            let _ = state.mark_trigger_used(t.id, touch_id).await;
                                            ok += 1;
                                            println!(
                                                "drafted touch={touch_id} \
                                                 company={} trigger={} \
                                                 (anchored on: {})",
                                                p.display_name,
                                                t.source,
                                                t.headline.chars().take(60).collect::<String>(),
                                            );
                                        }
                                        Err(e) => {
                                            err += 1;
                                            tracing::warn!(
                                                prospect = %t.prospect_id.0,
                                                "%e" = %e,
                                                "draft persist failed",
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    err += 1;
                                    tracing::warn!(
                                        prospect = %t.prospect_id.0,
                                        "%e" = %e,
                                        "draft generation failed",
                                    );
                                }
                            }
                        }
                        println!(
                            "\ntriggers draft complete: {ok} draft(s) queued, \
                             {err} error(s), {skipped_no_facts} skipped (no \
                             prospect record). Review with `salesman review` \
                             or `salesman fact-check`.",
                        );
                    }
                }
            }
        }

        Cmd::OwnerNotifications { limit } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let pending = state
                .list_pending_owner_notifications(limit)
                .await
                .unwrap_or_default();
            if cli.json {
                let v = serde_json::json!({
                    "count": pending.len(),
                    "pending": pending.iter().map(|n| serde_json::json!({
                        "id": n.id.to_string(),
                        "prospect_label": n.prospect_label,
                        "to_address": n.to_address,
                        "channel": n.channel,
                        "sent_at": n.sent_at.to_rfc3339(),
                        "subject": n.subject,
                        "campaign": n.campaign,
                        "receipt_id": n.receipt_id.map(|r| r.to_string()),
                        "queued_at": n.queued_at.to_rfc3339(),
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            println!(
                "salesman owner-notifications — {} pending (undelivered)\n",
                pending.len()
            );
            for n in &pending {
                println!(
                    "  {} | {} <{}> | {} | {}",
                    n.sent_at.format("%Y-%m-%d %H:%M:%SZ"),
                    n.prospect_label,
                    n.to_address,
                    n.channel,
                    n.subject.as_deref().unwrap_or("(no subject)"),
                );
            }
            if pending.is_empty() {
                println!("  (nothing pending — notifications appear here as contacts are made)");
            }
        }

        Cmd::Suppressions { action } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            match action {
                SuppCmd::List { source, limit } => {
                    let rows = state.list_suppressions(source.as_deref(), limit).await?;
                    if rows.is_empty() {
                        println!("(no suppressions match the filter)");
                    } else {
                        println!(
                            "{:<32} {:<8} {:<14} {:<22} reason",
                            "target", "kind", "source", "added_at"
                        );
                        println!("{}", "-".repeat(110));
                        for r in &rows {
                            let target = if r.target.chars().count() > 30 {
                                format!("{}…", r.target.chars().take(29).collect::<String>())
                            } else {
                                r.target.clone()
                            };
                            let reason = if r.reason.chars().count() > 60 {
                                format!("{}…", r.reason.chars().take(59).collect::<String>())
                            } else {
                                r.reason.clone()
                            };
                            println!(
                                "{:<32} {:<8} {:<14} {:<22} {}",
                                target,
                                r.target_kind,
                                r.source,
                                r.added_at.format("%Y-%m-%d %H:%M:%SZ"),
                                reason
                            );
                        }
                        println!("{}", "-".repeat(110));
                        println!("{} row(s)", rows.len());
                    }
                }
                SuppCmd::Add {
                    target,
                    kind,
                    reason,
                    source,
                } => {
                    if reason.trim().is_empty() {
                        anyhow::bail!("--reason cannot be empty (audit trail requires it)");
                    }
                    state
                        .add_suppression(&target, &kind, &reason, &source)
                        .await?;
                    println!("added: {target} (kind={kind} source={source})");
                }
                SuppCmd::Remove {
                    target,
                    confirm_typed,
                } => {
                    if !confirm_typed {
                        anyhow::bail!(
                            "remove requires --confirm-typed (the recipient will receive future sends after removal)"
                        );
                    }
                    {
                        use dialoguer::Input;
                        let typed: String = Input::new()
                            .with_prompt(format!("Type the target ({target}) to confirm removal"))
                            .interact_text()
                            .map_err(|e| anyhow::anyhow!("dialoguer: {e}"))?;
                        if typed.trim() != target {
                            anyhow::bail!("typed target did not match — aborting");
                        }
                    }
                    let n = state.remove_suppression(&target).await?;
                    if n == 0 {
                        println!("no suppression found for `{target}` (already absent)");
                    } else {
                        println!("removed: {target} ({n} row)");
                    }
                }
                SuppCmd::Export { out } => {
                    // Pull the whole table — by design suppression
                    // count is bounded (~k of rows even at scale).
                    let rows = state.list_suppressions(None, i64::MAX).await?;
                    let mut sink: Box<dyn std::io::Write> = if out == "-" {
                        Box::new(std::io::stdout().lock())
                    } else {
                        Box::new(std::fs::File::create(&out)?)
                    };
                    writeln!(sink, "target,kind,reason,source,added_at")?;
                    for r in &rows {
                        writeln!(
                            sink,
                            "{},{},{},{},{}",
                            csv_quote(&r.target),
                            csv_quote(&r.target_kind),
                            csv_quote(&r.reason),
                            csv_quote(&r.source),
                            r.added_at.to_rfc3339()
                        )?;
                    }
                    if out != "-" {
                        eprintln!("exported {} row(s) to {out}", rows.len());
                    }
                }
                SuppCmd::Import { from_csv, source } => {
                    let text = std::fs::read_to_string(&from_csv)?;
                    let mut imported = 0u32;
                    let mut skipped = 0u32;
                    let lines: Vec<&str> = text.lines().collect();
                    let has_header = lines
                        .first()
                        .map(|l| l.starts_with("target,") || l.starts_with("\"target\","))
                        .unwrap_or(false);
                    let body = if has_header { &lines[1..] } else { &lines[..] };
                    for line in body {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let cols: Vec<String> = parse_csv_row(line);
                        let target = match cols.first() {
                            Some(t) if !t.is_empty() => t.clone(),
                            _ => {
                                skipped += 1;
                                continue;
                            }
                        };
                        let kind = cols.get(1).cloned().unwrap_or_else(|| {
                            // Single-column file: assume email.
                            "email".to_string()
                        });
                        let kind = if kind.is_empty() {
                            "email".to_string()
                        } else {
                            kind
                        };
                        let reason = cols
                            .get(2)
                            .cloned()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "bulk import".to_string());
                        let row_source = source
                            .clone()
                            .or_else(|| cols.get(3).cloned().filter(|s| !s.is_empty()))
                            .unwrap_or_else(|| "manual".to_string());
                        state
                            .add_suppression(&target, &kind, &reason, &row_source)
                            .await?;
                        imported += 1;
                    }
                    println!(
                        "import: {imported} added, {skipped} skipped (duplicates ignored at DB level)"
                    );
                }
                SuppCmd::Count => {
                    let rows = state.count_suppressions_by_source().await?;
                    let total: i64 = rows.iter().map(|(_, n)| n).sum();
                    if cli.json {
                        let v = serde_json::json!({
                            "total": total,
                            "by_source": rows.iter().map(|(s, n)| serde_json::json!({
                                "source": s, "count": n
                            })).collect::<Vec<_>>(),
                        });
                        println!("{}", serde_json::to_string_pretty(&v)?);
                    } else if rows.is_empty() {
                        println!("(suppression list empty)");
                    } else {
                        println!("{:<20} {:>10}", "source", "count");
                        println!("{}", "-".repeat(32));
                        for (s, n) in &rows {
                            println!("{:<20} {:>10}", s, n);
                        }
                        println!("{}", "-".repeat(32));
                        println!("{:<20} {:>10}", "TOTAL", total);
                    }
                }
            }
        }

        Cmd::RenderSite {
            src,
            dst,
            origin,
            site_name,
        } => {
            let cfg = salesman_content::SiteConfig::new(&origin, &site_name);
            let pages = salesman_content::render_site(&src, &dst, &cfg)?;
            println!("rendered {} pages to {}", pages.len(), dst.display());
            for p in &pages {
                println!("  {} → {}", p.slug, p.output_path.display());
            }
        }

        Cmd::TickSequences { batch, product } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!(
                    "no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)"
                );
            }
            let due = state.list_due_prospects(batch).await?;
            if due.is_empty() {
                println!("no prospects due");
                return Ok(());
            }
            let draft_tool = DraftColdEmailTool::new(
                router.clone(),
                "the PlausiDen team",
                "PlausiDen",
                "Plausible deniability + sovereign data tools for SMB security teams.",
            );
            let mut ok = 0u32;
            let mut err = 0u32;
            for d in &due {
                let p = match state.get_prospect_with_facts(d.prospect_id).await? {
                    Some(p) => p,
                    None => {
                        tracing::warn!(prospect = %d.prospect_id, "no facts; skipping");
                        err += 1;
                        continue;
                    }
                };
                let prospect_json = p.to_prompt_json();

                // U56: build a follow-up-aware angle hint. If this
                // prospect has prior outbound touches, summarize the
                // most recent one inline so the LLM knows step N
                // is a CONTINUATION, not a fresh intro. Without this
                // every step reads "Hi, I'm Will from PlausiDen…"
                // which is jarring on touch 3+.
                let prior = state
                    .list_thread_for_prospect(d.prospect_id, 6)
                    .await
                    .unwrap_or_default();
                let prior_outbound: Vec<&salesman_state::ThreadTurn> =
                    prior.iter().filter(|t| t.role == "outbound").collect();
                let angle_hint = if let Some(last) = prior_outbound.last() {
                    let prev_subject = last.subject.as_deref().unwrap_or("(no subject)");
                    format!(
                        "step {} of sequence (template: {}). THIS IS A \
                         FOLLOW-UP. Earlier outbound subject was: \
                         \"{}\" sent {}. Acknowledge briefly that you've \
                         already been in touch — do NOT re-introduce \
                         yourself or repeat the full value prop. \
                         Reference one specific thing from the prior \
                         outbound or the prospect facts and propose a \
                         lower-friction next step than the first email \
                         did.",
                        d.current_step,
                        d.template_key,
                        prev_subject,
                        last.at.format("%Y-%m-%d"),
                    )
                } else {
                    format!(
                        "step {} of sequence (template: {})",
                        d.current_step, d.template_key,
                    )
                };

                let tool_args = serde_json::json!({
                    "prospect": prospect_json,
                    "product":  product,
                    "angle_hint": angle_hint,
                });
                match salesman_tools::Tool::invoke(&draft_tool, salesman_core::ToolArgs(tool_args))
                    .await
                {
                    Ok(v) => {
                        let subject = v.get("subject").and_then(|x| x.as_str()).unwrap_or("");
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                        let produced_by = v.get("produced_by").cloned();
                        if let Err(e) = state
                            .insert_touch_draft_full(
                                d.prospect_id,
                                salesman_core::TouchChannel::Email,
                                Some(subject),
                                body,
                                Some(&d.template_key),
                                produced_by,
                            )
                            .await
                        {
                            tracing::warn!(prospect = %d.prospect_id, "%e" = %e, "draft persist failed");
                            err += 1;
                            continue;
                        }
                        // advance the sequence — schedules next_due_at for the *next* step
                        let _advanced = state.advance_prospect_in_sequence(d.prospect_id).await?;
                        ok += 1;
                    }
                    Err(e) => {
                        tracing::warn!(prospect = %d.prospect_id, "%e" = %e, "draft generation failed");
                        err += 1;
                    }
                }
            }
            println!(
                "tick-sequences: due={} drafted={ok} errored={err}",
                due.len()
            );
        }

        Cmd::Halt { reason } => {
            println!("(stub) halt requested: {reason} — lands in Phase 1.4");
        }

        Cmd::ImportCsv {
            path,
            campaign,
            dry_run,
        } => {
            #[derive(serde::Deserialize)]
            struct Row {
                display_name: String,
                #[serde(default)]
                homepage: String,
                #[serde(default)]
                legal_name: String,
                #[serde(default)]
                industry: String,
                #[serde(default)]
                region: String,
                #[serde(default)]
                description: String,
                #[serde(default)]
                size_band: String,
            }

            // CSV-injection sentinel: spreadsheet apps interpret values
            // starting with these characters as formulas. Refuse them
            // at import time so we don't propagate them downstream into
            // exports, dashboards, or per-row drafter prompts.
            fn safe_field(s: &str) -> bool {
                let s = s.trim_start();
                if s.is_empty() {
                    return true;
                }
                let first = s.chars().next().unwrap_or(' ');
                !matches!(first, '=' | '+' | '-' | '@' | '\t' | '\r')
            }

            fn map_size_band(s: &str) -> Option<salesman_core::model::SizeBand> {
                use salesman_core::model::SizeBand::{
                    Enterprise, Large, Mid, Small, Solo, Unknown,
                };
                match s.trim().to_ascii_lowercase().as_str() {
                    "" | "unknown" | "(unknown)" => Some(Unknown),
                    "solo" => Some(Solo),
                    "small" | "smb" => Some(Small),
                    "mid" | "mid-market" | "midmarket" => Some(Mid),
                    "large" => Some(Large),
                    "enterprise" => Some(Enterprise),
                    _ => None,
                }
            }

            let state = require_state(cli.database_url.as_deref()).await?;
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(true)
                .trim(csv::Trim::All)
                .from_path(&path)
                .map_err(|e| anyhow::anyhow!("open CSV {}: {e}", path.display()))?;

            let mut companies: Vec<salesman_core::Company> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            let mut row_idx = 1u32; // 1 = header; data rows start at 2.
            for rec in rdr.deserialize::<Row>() {
                row_idx += 1;
                let row = match rec {
                    Ok(r) => r,
                    Err(e) => {
                        errors.push(format!("row {row_idx}: parse — {e}"));
                        continue;
                    }
                };
                if row.display_name.trim().is_empty() {
                    errors.push(format!("row {row_idx}: display_name is required"));
                    continue;
                }
                for (label, val) in [
                    ("display_name", &row.display_name),
                    ("homepage", &row.homepage),
                    ("legal_name", &row.legal_name),
                    ("industry", &row.industry),
                    ("region", &row.region),
                    ("description", &row.description),
                    ("size_band", &row.size_band),
                ] {
                    if !safe_field(val) {
                        errors.push(format!(
                            "row {row_idx}: {label} starts with formula char — refusing"
                        ));
                    }
                }
                let homepage = if row.homepage.trim().is_empty() {
                    None
                } else {
                    match url::Url::parse(row.homepage.trim()) {
                        Ok(u) => Some(u),
                        Err(e) => {
                            errors.push(format!(
                                "row {row_idx}: homepage `{}` does not parse — {e}",
                                row.homepage
                            ));
                            continue;
                        }
                    }
                };
                let size_band = match map_size_band(&row.size_band) {
                    Some(sb) => Some(sb),
                    None => {
                        errors.push(format!(
                            "row {row_idx}: size_band `{}` is not one of \
                             solo|small|smb|mid|mid-market|large|enterprise",
                            row.size_band,
                        ));
                        continue;
                    }
                };
                companies.push(salesman_core::Company {
                    id: salesman_core::CompanyId::new(),
                    legal_name: Some(row.legal_name.trim().to_string()).filter(|s| !s.is_empty()),
                    display_name: row.display_name.trim().to_string(),
                    homepage,
                    industry: Some(row.industry.trim().to_string()).filter(|s| !s.is_empty()),
                    size_band,
                    region: Some(row.region.trim().to_string()).filter(|s| !s.is_empty()),
                    description: Some(row.description.trim().to_string()).filter(|s| !s.is_empty()),
                    tech_signals: vec![],
                    discovered_at: chrono::Utc::now(),
                    last_enriched_at: None,
                    source: salesman_core::model::DiscoverySource::OwnerSeed,
                    raw: std::collections::BTreeMap::new(),
                });
            }

            println!(
                "import-csv {}: {} valid row(s), {} error(s){}",
                path.display(),
                companies.len(),
                errors.len(),
                if dry_run { ", DRY-RUN" } else { "" },
            );
            for e in &errors {
                println!("  ERROR  {e}");
            }
            if errors.iter().any(|e| !e.is_empty()) && !dry_run {
                anyhow::bail!(
                    "{} validation error(s); refusing to import. Re-run with \
                     --dry-run to see all errors at once, or fix and retry.",
                    errors.len()
                );
            }
            if dry_run {
                println!("\n(dry-run; no DB changes)");
            } else {
                let campaign_id = state
                    .ensure_campaign(&campaign, "(import-csv)", "(unspecified)")
                    .await?;
                let cids: Vec<_> = companies.iter().map(|c| c.id).collect();
                let n_companies = state.insert_companies(&companies).await?;
                let n_prospects = state
                    .upsert_prospects_for_campaign(campaign_id, &cids)
                    .await?;
                println!(
                    "imported into `{campaign}`: {n_companies} new \
                     company row(s), {n_prospects} new prospect row(s) \
                     (idempotent — re-runs skip existing pairs).",
                );
            }
        }

        Cmd::Tag {
            prospect_id,
            interest,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let pid = salesman_core::ProspectId(
                uuid::Uuid::parse_str(&prospect_id)
                    .map_err(|e| anyhow::anyhow!("invalid prospect-id: {e}"))?,
            );
            let added = state.add_prospect_interest(pid, &interest).await?;
            if added {
                println!("tagged prospect {prospect_id}: interest=\"{interest}\"");
            } else {
                println!(
                    "no change — prospect {prospect_id} already had interest=\"{interest}\" \
                     (or the trimmed value was empty)"
                );
            }
            let tags = state.get_prospect_tags(pid).await?;
            println!(
                "tags now: {}",
                serde_json::to_string_pretty(&tags).unwrap_or_default()
            );
        }

        Cmd::Note { prospect_id, text } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let pid = salesman_core::ProspectId(
                uuid::Uuid::parse_str(&prospect_id)
                    .map_err(|e| anyhow::anyhow!("invalid prospect-id: {e}"))?,
            );
            let added = state.add_prospect_note(pid, &text).await?;
            if added {
                println!("noted prospect {prospect_id}: \"{text}\"");
            } else {
                println!(
                    "no change — prospect {prospect_id} already had that note \
                     (or the trimmed value was empty)"
                );
            }
            let tags = state.get_prospect_tags(pid).await?;
            println!(
                "tags now: {}",
                serde_json::to_string_pretty(&tags).unwrap_or_default()
            );
        }

        Cmd::Thread { prospect_id, limit } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let pid = salesman_core::ProspectId(
                uuid::Uuid::parse_str(&prospect_id)
                    .map_err(|e| anyhow::anyhow!("invalid prospect-id: {e}"))?,
            );
            let turns = state.list_thread_for_prospect(pid, limit).await?;
            if turns.is_empty() {
                println!("(no thread history for {prospect_id})");
            } else {
                println!(
                    "=== thread for prospect {prospect_id} — {} turn(s) ===\n",
                    turns.len(),
                );
                for (i, t) in turns.iter().enumerate() {
                    let kind_tag = t
                        .reply_kind
                        .as_deref()
                        .map(|k| format!(" ({k})"))
                        .unwrap_or_default();
                    let subject = t.subject.as_deref().unwrap_or("(no subject)");
                    println!(
                        "[{:>2}] {} {}{kind_tag}\n     {} | {subject}\n",
                        i + 1,
                        t.at.format("%Y-%m-%d %H:%M UTC"),
                        t.role.to_uppercase(),
                        t.role,
                    );
                    for line in t.body.lines().take(20) {
                        println!("     | {line}");
                    }
                    let extra_lines = t.body.lines().count().saturating_sub(20);
                    if extra_lines > 0 {
                        println!("     | … ({extra_lines} more line(s))");
                    }
                    println!();
                }
            }
        }

        Cmd::Tools => {
            let mut names = tools.names();
            names.sort();
            for name in names {
                println!("- {name}");
            }
        }

        Cmd::Backends => {
            let kinds = router.registered_kinds();
            if kinds.is_empty() {
                println!("(none registered — set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)");
            } else {
                for kind in kinds {
                    println!("- {kind}");
                }
            }
        }
    }

    Ok(())
}

/// Operator-configured target localities from `SALESMAN_TARGET_LOCALITY`
/// (comma-separated, e.g. `"Edinburgh, Scotland, UK"`). Empty/unset means
/// no local-first preference.
fn target_locality_terms() -> Vec<String> {
    std::env::var("SALESMAN_TARGET_LOCALITY")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Reorder a campaign's prospects local-first (highest locality match to
/// `terms` first; stable otherwise). A no-op when `terms` is empty, so
/// default behavior is unchanged unless the operator opts in. Pure
/// reordering of already-fetched rows — does not change what is
/// discovered.
fn order_local_first_with_terms(
    mut prospects: Vec<salesman_state::query::ProspectWithFacts>,
    terms: &[&str],
) -> Vec<salesman_state::query::ProspectWithFacts> {
    if terms.is_empty() {
        return prospects;
    }
    let regions: Vec<Option<&str>> = prospects.iter().map(|p| p.region.as_deref()).collect();
    let order = salesman_discovery::locality::rank_local_first(&regions, terms);
    let mut slots: Vec<Option<salesman_state::query::ProspectWithFacts>> =
        prospects.drain(..).map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("rank_local_first yields each index once"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prospect_in(region: Option<&str>) -> salesman_state::query::ProspectWithFacts {
        salesman_state::query::ProspectWithFacts {
            prospect_id: salesman_core::ProspectId::new(),
            company_id: salesman_core::CompanyId::new(),
            display_name: region.unwrap_or("nowhere").to_string(),
            homepage: None,
            industry: None,
            description: None,
            region: region.map(str::to_string),
            tech_signals: serde_json::Value::Array(vec![]),
            tags: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    #[test]
    fn order_local_first_puts_matching_regions_first() {
        let ps = vec![
            prospect_in(Some("Tokyo, JP")),
            prospect_in(Some("Edinburgh, Scotland")),
            prospect_in(None),
            prospect_in(Some("Glasgow, Scotland")),
        ];
        let out = order_local_first_with_terms(ps, &["scotland"]);
        // Both Scotland rows come first (stable: Edinburgh before Glasgow),
        // then the non-matching rows in original order.
        assert_eq!(out[0].region.as_deref(), Some("Edinburgh, Scotland"));
        assert_eq!(out[1].region.as_deref(), Some("Glasgow, Scotland"));
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn order_local_first_empty_terms_is_noop() {
        let ps = vec![prospect_in(Some("Tokyo")), prospect_in(Some("Edinburgh"))];
        let out = order_local_first_with_terms(ps, &[]);
        assert_eq!(out[0].region.as_deref(), Some("Tokyo"));
        assert_eq!(out[1].region.as_deref(), Some("Edinburgh"));
    }

    #[test]
    fn csv_quote_wraps_and_escapes() {
        assert_eq!(csv_quote("hi"), r#""hi""#);
        assert_eq!(csv_quote(r#"a "quote""#), r#""a ""quote""""#);
        assert_eq!(csv_quote("with,comma"), r#""with,comma""#);
        assert_eq!(csv_quote(""), r#""""#);
        assert_eq!(csv_quote("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn parse_csv_row_handles_quoted_and_plain() {
        assert_eq!(parse_csv_row("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(parse_csv_row(""), vec![""]);
        assert_eq!(
            parse_csv_row(r#""a,b","c","d""e""#),
            vec!["a,b", "c", r#"d"e"#]
        );
        assert_eq!(
            parse_csv_row("alice@example.com"),
            vec!["alice@example.com"]
        );
        assert_eq!(parse_csv_row("a,,c"), vec!["a", "", "c"]);
    }

    #[test]
    fn csv_quote_round_trips_through_parse() {
        let original = vec![
            "alice@example.com".to_string(),
            r#"reason with "quotes" and , comma"#.to_string(),
            "manual".to_string(),
        ];
        let line = original
            .iter()
            .map(|s| csv_quote(s))
            .collect::<Vec<_>>()
            .join(",");
        let parsed = parse_csv_row(&line);
        assert_eq!(parsed, original);
    }
}
