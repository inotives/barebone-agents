mod cli;
mod config;

use clap::Parser;
use cli::{Cli, Commands};
use tracing::info;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize tracing with log level from args or default
    let log_level = cli.log_level.as_deref().unwrap_or("info");
    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .init();

    info!("barebone-agent v{}", env!("CARGO_PKG_VERSION"));

    match cli.command {
        Commands::Run { agent } => {
            info!(agent = %agent, "starting agent");
            // TODO: EP-00005 — agent loop + CLI channel
            println!("Agent '{}' not yet implemented", agent);
        }
        Commands::Status { agent, json } => {
            info!("status query");
            // TODO: EP-10 — status subcommand
            println!(
                "Status not yet implemented (agent: {:?}, json: {})",
                agent, json
            );
        }
    }
}
