mod cli;
mod config;
mod git;
mod policy;
mod session;
mod judge;
mod output;
mod tui;
mod audit;
mod hooks;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .with_target(false)
        .init();

    match cli.command {
        Commands::Start { mission, agent, watch } => {
            session::start(mission, agent, watch).await
        }
        Commands::Check { session_id, json, share } => {
            session::check(session_id, json, share).await
        }
        Commands::Judge { provider, model, endpoint, json } => {
            session::judge(provider, model, endpoint, json).await
        }
        Commands::Report { markdown } => {
            session::report(markdown).await
        }
        Commands::Diff { problems } => {
            session::diff(problems).await
        }
        Commands::Hook { action } => {
            match action {
                cli::HookAction::Install => hooks::install().await,
                cli::HookAction::Uninstall => hooks::uninstall().await,
                cli::HookAction::Status => hooks::status().await,
            }
        }
        Commands::Audit { range, session_id } => {
            audit::run(range, session_id).await
        }
        Commands::Use { agent } => {
            config::integrate_agent(agent).await
        }
        Commands::Init { preset } => {
            config::init(preset).await
        }
        Commands::Watch => {
            tui::run_watch().await
        }
        Commands::Status => {
            session::status().await
        }
    }
}
