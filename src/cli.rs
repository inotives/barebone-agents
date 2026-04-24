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
    /// Run one or more agents
    Run {
        /// Agent name(s), comma-separated (e.g. ino,robin)
        #[arg(short, long, required_unless_present = "all")]
        agent: Option<String>,
        /// Start all agents found in agents/ directory
        #[arg(long, default_value_t = false)]
        all: bool,
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
    /// Manage tasks
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
    /// Manage missions
    Missions {
        #[command(subcommand)]
        command: MissionsCommand,
    },
    /// View conversations
    Conversations {
        #[command(subcommand)]
        command: ConversationsCommand,
    },
    /// View agent configurations
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
    /// Validate configuration files
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// View token usage breakdown
    Tokens {
        /// Filter by agent name
        #[arg(long)]
        agent: Option<String>,
        /// Period: today (default), week, total
        #[arg(long, default_value = "today")]
        period: String,
        /// Break down by model
        #[arg(long, default_value_t = false)]
        by_model: bool,
        /// Break down by day
        #[arg(long, default_value_t = false)]
        by_day: bool,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum TasksCommand {
    /// List tasks
    List {
        /// Filter by status (backlog, todo, in_progress, done, blocked)
        #[arg(long)]
        status: Option<String>,
        /// Filter by agent name
        #[arg(long)]
        agent: Option<String>,
        /// Filter by mission key
        #[arg(long)]
        mission: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show task details
    Show {
        /// Task key (e.g. TSK-00001)
        key: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Create a new task
    Create {
        /// Task title
        #[arg(long)]
        title: String,
        /// Task description
        #[arg(long)]
        description: Option<String>,
        /// Parent mission key
        #[arg(long)]
        mission: Option<String>,
        /// Assign to agent
        #[arg(long)]
        agent: Option<String>,
        /// Priority: critical, high, medium, low
        #[arg(long, default_value = "medium")]
        priority: String,
        /// Schedule: hourly, daily@HH:MM, weekly@DAY@HH:MM, every:Nh/Nm
        #[arg(long)]
        schedule: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Update a task
    Update {
        /// Task key (e.g. TSK-00001)
        key: String,
        /// New status (backlog, todo, in_progress, done, blocked)
        #[arg(long)]
        status: Option<String>,
        /// New priority (critical, high, medium, low)
        #[arg(long)]
        priority: Option<String>,
        /// Assign to agent
        #[arg(long)]
        agent: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Delete a task
    Delete {
        /// Task key (e.g. TSK-00001)
        key: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum MissionsCommand {
    /// List missions
    List {
        /// Filter by status (active, paused, completed)
        #[arg(long)]
        status: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show mission details and its tasks
    Show {
        /// Mission key (e.g. MIS-00001)
        key: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Create a new mission
    Create {
        /// Mission title
        #[arg(long)]
        title: String,
        /// Mission description
        #[arg(long)]
        description: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Update a mission
    Update {
        /// Mission key (e.g. MIS-00001)
        key: String,
        /// New status (active, paused, completed)
        #[arg(long)]
        status: Option<String>,
        /// New title
        #[arg(long)]
        title: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Delete a mission (fails if mission has tasks)
    Delete {
        /// Mission key (e.g. MIS-00001)
        key: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ConversationsCommand {
    /// List conversations
    List {
        /// Filter by agent name
        #[arg(long)]
        agent: Option<String>,
        /// Max number of conversations to show
        #[arg(long, default_value = "20")]
        limit: u32,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show conversation messages
    Show {
        /// Conversation ID
        id: String,
        /// Show full turn details (tool calls, intermediate steps)
        #[arg(long, default_value_t = false)]
        full: bool,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum AgentsCommand {
    /// List all configured agents
    List {
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show agent configuration details
    Show {
        /// Agent name
        name: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Validate all configuration files
    Validate {
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
