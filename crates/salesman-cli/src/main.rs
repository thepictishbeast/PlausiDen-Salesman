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

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use salesman_content::{DraftColdEmailTool, ReplyClassifyTool};
use salesman_discovery::{CsvSeed, CsvSeedTool, HomepageFetchTool, HomepageFetcher};
use salesman_outreach::{SmtpConfig, SmtpSender};
use salesman_reply::{ImapConfig, ImapPoller};
use salesman_receipts::{Signer, default_seed_path};
use salesman_llm::claude::ClaudeBackend;
use salesman_llm::gemini::GeminiBackend;
use salesman_llm::{LlmBackend, LlmRouter, Message, Role, RouteHint};
use salesman_orchestrator::Orchestrator;
use salesman_state::State;
use salesman_tools::{EchoTool, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6";
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
    },
    /// Verify the receipt chain (audit).
    Audit {
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
    /// Poll the IMAP inbox once and persist new replies.
    InboxPoll {
        /// Run forever, polling every N seconds. Default = once.
        #[arg(long)]
        every_seconds: Option<u64>,
    },
    /// Classify all unclassified replies + apply transitions.
    ClassifyReplies {
        #[arg(long, default_value_t = 50)]
        batch: i64,
    },
    /// Show recent classified replies for a campaign.
    Inbox {
        #[arg(long)]
        campaign: String,
        #[arg(long, default_value_t = 50)]
        limit: i64,
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
    /// Kill switch — pauses every active campaign.
    Halt {
        #[arg(long, default_value = "operator-issued")]
        reason: String,
    },
    /// List the registered tools.
    Tools,
    /// List the registered LLM backends + models.
    Backends,
}

fn build_router(claude_model: &str, gemini_model: &str) -> LlmRouter {
    let mut router = LlmRouter::new();
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
    router
}

fn build_tools(router: Arc<LlmRouter>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    tools.register(Arc::new(CsvSeedTool::new()));
    tools.register(Arc::new(HomepageFetchTool::new()));
    tools.register(Arc::new(DraftColdEmailTool::new(
        router,
        "the PlausiDen team",
        "PlausiDen",
        "Plausible deniability + sovereign data tools for SMB security teams.",
    )));
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let router = Arc::new(build_router(&cli.claude_model, &cli.gemini_model));
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
                let prospect_json = serde_json::json!({
                    "display_name": p.display_name,
                    "homepage": p.homepage,
                    "industry": p.industry,
                    "description": p.description,
                    "tech_signals": p.tech_signals,
                });
                let mut tool_args = serde_json::json!({
                    "prospect": prospect_json,
                    "product": product,
                });
                if let Some(h) = &angle_hint {
                    tool_args["angle_hint"] = serde_json::Value::String(h.clone());
                }
                let result = salesman_tools::Tool::invoke(
                    &draft_tool,
                    salesman_core::ToolArgs(tool_args),
                )
                .await;
                match result {
                    Ok(v) => {
                        let subject = v.get("subject").and_then(|x| x.as_str()).unwrap_or("(no subject)");
                        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                        match state
                            .insert_touch_draft(
                                p.prospect_id,
                                salesman_core::TouchChannel::Email,
                                Some(subject),
                                body,
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
                    println!("--- [{}] {} (touch {}, {})", i + 1, t.company, t.touch_id, t.channel);
                    if let Some(s) = &t.subject {
                        println!("Subject: {s}");
                    }
                    println!();
                    println!("{}", t.body);
                    println!();
                }
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

            let risk = salesman_detector::score(&body, subject.as_deref());
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

        Cmd::Suppress { target, kind, reason } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let kind = kind.unwrap_or_else(|| {
                if target.contains('@') { "email".into() } else { "domain".into() }
            });
            state.add_suppression(&target, &kind, &reason, "manual").await?;
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
                if !ok { bad += 1; }
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

        Cmd::SendPending {
            campaign,
            for_real,
            per_recipient_window_hours,
            per_recipient_max,
            per_domain_window_hours,
            per_domain_max,
        } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(send-only)", "(unspecified)")
                .await?;
            let approved = state.list_approved_touches(campaign_id).await?;

            // Real-send mode requires SMTP env. Dry-run is fine without it.
            let sender = if for_real {
                let cfg = SmtpConfig::from_env()?;
                Some(SmtpSender::new(cfg)?)
            } else { None };

            let signer = if for_real {
                Some(Signer::load_or_generate(&default_seed_path(), "salesman-default-1")?)
            } else { None };

            let mut sent = 0u32;
            let mut blocked_supp = 0u32;
            let mut blocked_rate = 0u32;
            let mut blocked_no_to = 0u32;
            let mut errored = 0u32;

            for t in &approved {
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
                let domain = to.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_default();
                let n_domain = state
                    .count_touches_to_domain_since(&domain, per_domain_window_hours)
                    .await?;
                if n_domain >= per_domain_max {
                    blocked_rate += 1;
                    tracing::warn!(domain=%domain, n=%n_domain, "per-domain cap hit — skipping");
                    continue;
                }

                if !for_real {
                    println!(
                        "[DRY-RUN] would send: to={to} subject={:?} touch={}",
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
                        errored += 1;
                        tracing::warn!(to=%to, "%e" = %e, "smtp send failed");
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
                let n = state.mark_touch_sent(t.touch_id, receipt_id, outcome.sent_at).await?;
                if n == 1 {
                    sent += 1;
                    println!("sent: to={to} touch={} receipt={receipt_id}", t.touch_id);
                } else {
                    tracing::warn!(touch=%t.touch_id, "mark_touch_sent affected 0 rows — race?");
                }
            }

            println!(
                "send-pending `{campaign}` ({}): approved={} sent={sent} \
                 blocked_supp={blocked_supp} blocked_rate={blocked_rate} \
                 blocked_no_to={blocked_no_to} errored={errored}",
                if for_real { "REAL" } else { "DRY-RUN" },
                approved.len()
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

        Cmd::ClassifyReplies { batch } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            if router.registered_kinds().is_empty() {
                anyhow::bail!("no LLM backends registered (set ANTHROPIC_API_KEY and/or GEMINI_API_KEY)");
            }
            let classifier = ReplyClassifyTool::new(router.clone());
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
                let result = salesman_tools::Tool::invoke(
                    &classifier,
                    salesman_core::ToolArgs(args),
                ).await;
                let kind_str = match result {
                    Ok(v) => v.get("kind").and_then(|x| x.as_str()).unwrap_or("unclassified").to_string(),
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
                let summary = state
                    .apply_reply_to_prospect(r.reply_id, r.prospect_id, &r.from_address, kind)
                    .await?;
                *counts.entry(kind_str.clone()).or_default() += 1;
                println!("[{}] {} → {}: {}", r.from_address, kind_str, r.reply_id, summary);
            }
            println!("\nclassified {} replies. counts: {:?}", unclassified.len(), counts);
        }

        Cmd::Inbox { campaign, limit } => {
            let state = require_state(cli.database_url.as_deref()).await?;
            let campaign_id = state
                .ensure_campaign(&campaign, "(inbox-only)", "(unspecified)")
                .await?;
            let rows = state.list_recent_replies_for_campaign(campaign_id, limit).await?;
            if rows.is_empty() {
                println!("no replies for `{campaign}`");
            } else {
                println!("=== {} replies for `{campaign}` ===\n", rows.len());
                for r in rows {
                    println!("[{}] {} | {} | {}", r.received_at.to_rfc3339(), r.kind, r.from_address, r.subject.as_deref().unwrap_or(""));
                    let snippet: String = r.body.chars().take(160).collect();
                    println!("  {snippet}...\n");
                }
            }
        }

        Cmd::Halt { reason } => {
            println!("(stub) halt requested: {reason} — lands in Phase 1.4");
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
