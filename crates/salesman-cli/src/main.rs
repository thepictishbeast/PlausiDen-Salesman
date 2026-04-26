//! salesman — operator CLI.
//!
//! Subcommands available in Phase 1.0:
//!   plan      — dry-run: round-trip a fake prospect through the
//!               orchestrator with the EchoTool registered. Proves
//!               the loop wires up end-to-end.
//!   discover  — placeholder; lands in Phase 1.1.
//!   halt      — kill switch (placeholder; persists a halt marker
//!               that workers will respect).
//!   tools     — list registered tools.
//!   backends  — list registered LLM backends.

use anyhow::Result;
use clap::{Parser, Subcommand};
use salesman_llm::{LlmRouter, Message, Role, RouteHint};
use salesman_orchestrator::Orchestrator;
use salesman_tools::{EchoTool, ToolRegistry};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "salesman",
    about = "PlausiDen-Salesman operator CLI",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Dry-run the orchestrator with a fake prospect + echo tool.
    Plan {
        #[arg(long, default_value = "demo: introduce ourselves")]
        goal: String,
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
    /// List the registered LLM backends.
    Backends,
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
    let router = Arc::new(LlmRouter::new()); // no backends registered in dry-run

    match cli.cmd {
        Cmd::Plan { goal } => {
            let orch = Orchestrator::new(router, tools);
            let messages = vec![
                Message {
                    role: Role::System,
                    content:
                        "You are PlausiDen-Salesman. Use tools to make progress on the goal."
                            .into(),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
                Message {
                    role: Role::User,
                    content: format!("Goal: {goal}\nReply with `done.` if you have nothing to do."),
                    tool_calls: vec![],
                    tool_results: vec![],
                },
            ];
            // No backends registered → router returns config error.
            // That's the expected dry-run outcome until 1.0+ wires
            // real backends from env vars.
            match orch.run(RouteHint::Reasoning, messages).await {
                Ok(resp) => println!("ORCH OK: {}", resp.message.content),
                Err(e) => println!("ORCH ERR (expected in scaffold): {e}"),
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
            for kind in router.registered_kinds() {
                println!("- {kind}");
            }
            if router.registered_kinds().is_empty() {
                println!("(none registered — this is the scaffold default)");
            }
        }
    }

    Ok(())
}
