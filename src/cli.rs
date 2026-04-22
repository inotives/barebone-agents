use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "barebone-agent", version, about = "A local-first, LLM-agnostic AI agent harness")]
pub struct Cli {
    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run an agent
    Run {
        /// Agent name (matches agents/{name}/ directory)
        #[arg(short, long)]
        agent: String,
        /// Send a single message and exit (one-shot mode)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Show agent status dashboard
    Status {
        /// Filter to specific agent
        #[arg(short, long)]
        agent: Option<String>,
        /// Token usage period: today (default), week, total
        #[arg(short, long, default_value = "today")]
        tokens: String,
        /// Show only a specific section: agents, tokens, tasks, missions, activity
        #[arg(short, long)]
        section: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
