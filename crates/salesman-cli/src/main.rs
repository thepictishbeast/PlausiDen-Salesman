//! salesman — operator CLI.
//!
//! Subcommands:
//!   plan      — round-trip a fake prospect through the orchestrator
//!               with the EchoTool registered. If `ANTHROPIC_API_KEY`
//!               or `GEMINI_API_KEY` are set, those backends register
//!               automatically and the model is actually called.
//!   discover  — placeholder; lands in Phase 1.1.
//!   halt      — kill switch (placeholder; persists a halt marker
//!               that workers will respect).
//!   tools     — list registered tools.
//!   backends  — list registered LLM backends + which models.

use anyhow::Result;
use clap::{Parser, Subcommand};
use salesman_llm::{LlmBackend, LlmRouter, Message, Role, RouteHint};
use salesman_llm::claude::ClaudeBackend;
use salesman_llm::gemini::GeminiBackend;
use salesman_orchestrator::Orchestrator;
use salesman_tools::{EchoTool, ToolRegistry};
use std::sync::Arc;

const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_GEMINI_MODEL: &str = "gemini-1.5-flash";

#[derive(Parser, Debug)]
#[command(
    name = "salesman",
    about = "PlausiDen-Salesman operator CLI",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Override the Claude model (env: SALESMAN_CLAUDE_MODEL).
    #[arg(long, env = "SALESMAN_CLAUDE_MODEL", default_value = DEFAULT_CLAUDE_MODEL)]
    claude_model: String,

    /// Override the Gemini model (env: SALESMAN_GEMINI_MODEL).
    #[arg(long, env = "SALESMAN_GEMINI_MODEL", default_value = DEFAULT_GEMINI_MODEL)]
    gemini_model: String,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the agent loop with a goal. Calls real LLMs when keys are set.
    Plan {
        #[arg(long, default_value = "demo: introduce yourself in one short sentence")]
        goal: String,
        /// Routing hint: reasoning | deep | bulk | grounded
        #[arg(long, default_value = "reasoning")]
        hint: String,
    },
    /// Discovery stub (lands in Phase 1.1).
    Discover {
        #[arg(long)]
        query: String,
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

fn parse_hint(s: &str) -> RouteHint {
    match s.to_ascii_lowercase().as_str() {
        "deep" | "deep_reasoning" | "opus" => RouteHint::DeepReasoning,
        "bulk" | "flash" => RouteHint::Bulk,
        "grounded" | "search" => RouteHint::Grounded,
        "sovereign" | "lfi" => RouteHint::Sovereign,
        _ => RouteHint::Reasoning,
    }
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

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let tools = Arc::new(tools);
    let router = Arc::new(build_router(&cli.claude_model, &cli.gemini_model));

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
                    println!("ORCH OK ({:?}, {}ms): {}",
                        resp.finish_reason, resp.usage.latency_ms, resp.message.content);
                    if !resp.message.tool_calls.is_empty() {
                        println!("(model also requested {} tool call(s))",
                            resp.message.tool_calls.len());
                    }
                }
                Err(e) => println!("ORCH ERR: {e}"),
            }
        }
        Cmd::Discover { query } => {
            println!("(stub) discover query={query} — lands in Phase 1.1");
        }
        Cmd::Halt { reason } => {
            println!("(stub) halt requested: {reason} — lands in Phase 1.4");
        }
        Cmd::Tools => {
            for name in tools.names() {
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
