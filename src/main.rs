mod agent_loop;
mod channels;
mod cli;
mod cmd_agents;
mod cmd_config;
mod cmd_conversations;
mod cmd_missions;
mod cmd_tasks;
mod cmd_tokens;
mod config;
mod db;
mod llm;
mod scheduler;
mod session;
mod skills;
mod status;
mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// A fully initialized, running agent with its background handles.
struct RunningAgent {
    name: String,
    agent_loop: Arc<agent_loop::AgentLoop>,
    session_mgr: Arc<tokio::sync::Mutex<session::SessionManager>>,
    heartbeat_handle: JoinHandle<()>,
    discord_handle: Option<JoinHandle<()>>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let log_level = cli.log_level.as_deref().unwrap_or("info");
    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .init();

    info!("barebone-agent v{}", env!("CARGO_PKG_VERSION"));

    match cli.command {
        Commands::Run { agent, all, message } => {
            if let Err(e) = run_agents(agent.as_deref(), all, message.as_deref()).await {
                error!(error = %e, "fatal error");
                std::process::exit(1);
            }
        }
        Commands::Status {
            agent,
            tokens,
            section,
            json,
        } => {
            if let Err(e) = run_status(&agent, &tokens, section.as_deref(), json) {
                error!(error = %e, "status error");
                std::process::exit(1);
            }
        }
        Commands::Tasks { command } => {
            if let Err(e) = run_management_cmd(|db| cmd_tasks::run(db, command)) {
                error!(error = %e, "tasks error");
                std::process::exit(1);
            }
        }
        Commands::Missions { command } => {
            if let Err(e) = run_management_cmd(|db| cmd_missions::run(db, command)) {
                error!(error = %e, "missions error");
                std::process::exit(1);
            }
        }
        Commands::Conversations { command } => {
            if let Err(e) = run_management_cmd(|db| cmd_conversations::run(db, command)) {
                error!(error = %e, "conversations error");
                std::process::exit(1);
            }
        }
        Commands::Agents { command } => {
            if let Err(e) = run_agents_cmd(command) {
                error!(error = %e, "agents error");
                std::process::exit(1);
            }
        }
        Commands::Config { command } => {
            let root_dir = std::env::current_dir()
                .map_err(|e| format!("Failed to get cwd: {}", e))
                .unwrap();
            if let Err(e) = cmd_config::run(&root_dir, command) {
                error!(error = %e, "config error");
                std::process::exit(1);
            }
        }
        Commands::Tokens {
            agent,
            period,
            by_model,
            by_day,
            json,
        } => {
            if let Err(e) = run_management_cmd(|db| {
                cmd_tokens::run(db, agent.as_deref(), &period, by_model, by_day, json)
            }) {
                error!(error = %e, "tokens error");
                std::process::exit(1);
            }
        }
    }
}

/// Discover agent names from the agents/ directory.
pub(crate) fn discover_agents(root_dir: &Path) -> Result<Vec<String>, String> {
    let agents_dir = root_dir.join("agents");
    let mut names = Vec::new();
    let entries = std::fs::read_dir(&agents_dir)
        .map_err(|e| format!("Failed to read agents/ directory: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Skip hidden dirs and _roles
                if !name.starts_with('.') && !name.starts_with('_') {
                    // Must have agent.yml
                    if path.join("agent.yml").exists() {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Resolve the list of agent names from CLI args.
fn resolve_agent_names(
    agent_arg: Option<&str>,
    all: bool,
    root_dir: &Path,
) -> Result<Vec<String>, String> {
    if all {
        let names = discover_agents(root_dir)?;
        if names.is_empty() {
            return Err("No agents found in agents/ directory".into());
        }
        return Ok(names);
    }

    match agent_arg {
        Some(s) => {
            let names: Vec<String> = s.split(',').map(|n| n.trim().to_string()).collect();
            if names.is_empty() {
                return Err("No agent names provided".into());
            }
            Ok(names)
        }
        None => Err("Specify --agent or --all".into()),
    }
}

/// Initialize a single agent and return its RunningAgent.
async fn init_agent(
    agent_name: &str,
    root_dir: &Path,
    settings: &config::Settings,
    model_registry: &config::ModelRegistry,
    database: Arc<db::Database>,
    core_skills: &skills::CoreSkills,
) -> Result<RunningAgent, String> {
    // Load agent config
    let agent_dir = config::settings::agent_dir(root_dir, agent_name);
    let agent_config = config::AgentConfig::load(&agent_dir)?;
    let character_sheet = config::agent::load_character_sheet(&agent_dir, agent_name)?;
    info!(
        agent = %agent_name,
        role = %agent_config.role,
        model = %agent_config.model,
        "agent config loaded"
    );

    // Per-agent LLM client pool (uses agent-specific env for API keys)
    let merged_env = config::settings::merge_env(&settings.env, &agent_dir);
    let pool = Arc::new(llm::LLMClientPool::new(model_registry, &merged_env));

    let mut fallback_chain = vec![agent_config.model.clone()];
    fallback_chain.extend(agent_config.fallbacks.clone());

    let primary_model = model_registry
        .get(&agent_config.model)
        .ok_or_else(|| format!("[{}] Primary model '{}' not found in registry", agent_name, agent_config.model))?
        .clone();

    database.register_agent(agent_name)?;

    // Per-agent tool registry
    let mut tool_registry = tools::ToolRegistry::new();

    let workspace = Arc::new(PathBuf::from(&settings.workspace_dir));
    tools::file_tools::register(&mut tool_registry, workspace.clone());
    tools::shell_tool::register(&mut tool_registry, workspace);
    tools::web_tools::register(&mut tool_registry, settings.tool_result_max_chars as usize);
    tools::task_tools::register(&mut tool_registry, database.clone(), agent_name.to_string());

    if settings.delegation_enabled {
        let parent_registry = Arc::new(tool_registry);
        tool_registry = tools::ToolRegistry::new();

        for def in parent_registry.get_all() {
            tool_registry.register_raw(
                &def.name,
                &def.description,
                def.parameters.clone(),
                def.handler.clone(),
            );
        }

        tools::delegate::register(
            &mut tool_registry,
            pool.clone(),
            fallback_chain.clone(),
            primary_model.clone(),
            parent_registry,
            root_dir.to_path_buf(),
            settings.subagent_max_parallel as usize,
            settings.subagent_sleep_between_secs,
            settings.tool_result_max_chars as usize,
        );
        info!(agent = %agent_name, "delegation tools enabled");
    }

    info!(agent = %agent_name, tools = tool_registry.len(), "tool registry initialized");

    // Per-agent MCP servers
    let _mcp_connections = tools::mcp::load_mcp_servers(
        &agent_config.mcp_servers,
        &merged_env,
        &mut tool_registry,
    )
    .await;

    let tool_registry = Arc::new(tool_registry);

    // Per-agent session manager
    let session_mgr = Arc::new(tokio::sync::Mutex::new(session::SessionManager::new(
        agent_name,
        None,
        settings.session_ttl_minutes,
        tool_registry.clone(),
    )));

    // Agent loop
    let agent_loop = Arc::new(agent_loop::AgentLoop::new(
        agent_name.to_string(),
        character_sheet,
        pool,
        fallback_chain,
        tool_registry,
        database.clone(),
        primary_model,
        settings.max_tool_iterations,
        settings.tool_result_max_chars as usize,
        settings.history_limit,
        core_skills.clone(),
        agent_config.akw_skills,
        root_dir.join("agents").join("_skills"),
        settings.skills_token_budget,
        settings.skills_min_match_hits,
    ));

    // Per-agent heartbeat
    let heartbeat_handle = {
        let al = agent_loop.clone();
        let db = database.clone();
        let sm = session_mgr.clone();
        let name = agent_name.to_string();
        let interval = settings.heartbeat_interval as u64;
        tokio::spawn(async move {
            scheduler::run_heartbeat(al, db, sm, name, interval).await;
        })
    };

    // Per-agent Discord bot
    let discord_handle = if let Some(ref discord_cfg) = agent_config.channels.discord {
        if discord_cfg.enabled {
            let token = merged_env
                .get("DISCORD_BOT_TOKEN")
                .cloned()
                .unwrap_or_default();
            if token.is_empty() {
                warn!(agent = %agent_name, "Discord enabled but DISCORD_BOT_TOKEN not set — skipping");
                None
            } else {
                match channels::run_discord(
                    agent_loop.clone(),
                    session_mgr.clone(),
                    discord_cfg.clone(),
                    &token,
                )
                .await
                {
                    Ok(handle) => {
                        info!(agent = %agent_name, "Discord bot started");
                        Some(handle)
                    }
                    Err(e) => {
                        error!(agent = %agent_name, error = %e, "Failed to start Discord bot");
                        None
                    }
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(RunningAgent {
        name: agent_name.to_string(),
        agent_loop,
        session_mgr,
        heartbeat_handle,
        discord_handle,
    })
}

async fn run_agents(
    agent_arg: Option<&str>,
    all: bool,
    one_shot: Option<&str>,
) -> Result<(), String> {
    let root_dir = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {}", e))?;

    // 1. Shared resources
    let settings = config::Settings::load(&root_dir);
    info!(log_level = %settings.log_level, "settings loaded");

    let registry_path = root_dir.join("config").join("models.yml");
    let model_registry = config::ModelRegistry::load(&registry_path)?;
    info!(models = model_registry.models.len(), "model registry loaded");

    let db_path = root_dir.join(&settings.sqlite_db_path);
    let database = Arc::new(db::Database::open(&db_path)?);
    info!("database initialized");

    let core_skills = skills::CoreSkills::load(&root_dir.join("config").join("skills"));

    // 2. Resolve agent names
    let agent_names = resolve_agent_names(agent_arg, all, &root_dir)?;
    let default_agent = agent_names[0].clone();

    info!(
        agents = ?agent_names,
        default = %default_agent,
        "starting agents"
    );

    // 3. Initialize each agent
    let mut running_agents: Vec<RunningAgent> = Vec::new();
    for name in &agent_names {
        match init_agent(name, &root_dir, &settings, &model_registry, database.clone(), &core_skills).await {
            Ok(ra) => {
                info!(agent = %name, "agent ready");
                running_agents.push(ra);
            }
            Err(e) => {
                error!(agent = %name, error = %e, "failed to initialize agent");
                // Shut down already-started agents
                shutdown_agents(&mut running_agents).await;
                return Err(e);
            }
        }
    }

    // 4. Build agent loop map for CLI routing
    let agent_loops: HashMap<String, Arc<agent_loop::AgentLoop>> = running_agents
        .iter()
        .map(|ra| (ra.name.clone(), ra.agent_loop.clone()))
        .collect();

    // 5. Run CLI channel (blocks until user exits)
    channels::run_cli(&agent_loops, &default_agent, one_shot).await;

    // 6. Graceful shutdown (reverse order)
    shutdown_agents(&mut running_agents).await;

    Ok(())
}

/// Shut down all running agents in reverse startup order.
async fn shutdown_agents(agents: &mut Vec<RunningAgent>) {
    for ra in agents.iter_mut().rev() {
        ra.heartbeat_handle.abort();
        if let Some(handle) = ra.discord_handle.take() {
            handle.abort();
            info!(agent = %ra.name, "Discord bot stopped");
        }
        {
            let mut mgr = ra.session_mgr.lock().await;
            mgr.end_all().await;
        }
        info!(agent = %ra.name, "agent shut down");
    }
    info!("shutdown complete");
}

/// Run an agents command (needs root_dir + model_registry + db).
fn run_agents_cmd(cmd: cli::AgentsCommand) -> Result<(), String> {
    let root_dir = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {}", e))?;
    let settings = config::Settings::load(&root_dir);
    let registry_path = root_dir.join("config").join("models.yml");
    let model_registry = config::ModelRegistry::load(&registry_path)?;
    let db_path = root_dir.join(&settings.sqlite_db_path);
    let database = db::Database::open(&db_path)?;
    cmd_agents::run(&database, &root_dir, &model_registry, cmd)
}

/// Open the database and run a management command against it.
fn run_management_cmd<F>(f: F) -> Result<(), String>
where
    F: FnOnce(&db::Database) -> Result<(), String>,
{
    let root_dir = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {}", e))?;
    let settings = config::Settings::load(&root_dir);
    let db_path = root_dir.join(&settings.sqlite_db_path);
    let database = db::Database::open(&db_path)?;
    f(&database)
}

fn run_status(
    agent_filter: &Option<String>,
    tokens: &str,
    section: Option<&str>,
    json: bool,
) -> Result<(), String> {
    let root_dir = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {}", e))?;

    let settings = config::Settings::load(&root_dir);

    let registry_path = root_dir.join("config").join("models.yml");
    let model_registry = config::ModelRegistry::load(&registry_path)?;

    let db_path = root_dir.join(&settings.sqlite_db_path);
    let database = db::Database::open(&db_path)?;

    let token_period = status::TokenPeriod::parse(tokens)?;
    let section = match section {
        Some(s) => status::Section::parse(s)?,
        None => status::Section::All,
    };

    let query = status::StatusQuery {
        agent_filter: agent_filter.clone(),
        token_period,
        section,
        json,
    };

    status::run_status(&database, &root_dir, &model_registry, &query);
    Ok(())
}
