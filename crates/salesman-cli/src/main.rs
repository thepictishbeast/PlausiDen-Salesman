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
use salesman_content::DraftColdEmailTool;
use salesman_discovery::{CsvSeed, CsvSeedTool, HomepageFetchTool, HomepageFetcher};
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
