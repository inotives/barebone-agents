mod agent_loop;
mod channels;
mod cli;
mod config;
mod db;
mod llm;
mod scheduler;
mod session;
mod skills;
mod status;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use tracing::{error, info, warn};

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
        Commands::Run { agent, message } => {
            if let Err(e) = run_agent(&agent, message.as_deref()).await {
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
    }
}

async fn run_agent(agent_name: &str, one_shot: Option<&str>) -> Result<(), String> {
    let root_dir = std::env::current_dir().map_err(|e| format!("Failed to get cwd: {}", e))?;

    // 1. Load settings from root .env
    let settings = config::Settings::load(&root_dir);
    info!(log_level = %settings.log_level, "settings loaded");

    // 2. Load model registry
    let registry_path = root_dir.join("config").join("models.yml");
    let model_registry = config::ModelRegistry::load(&registry_path)?;
    info!(models = model_registry.models.len(), "model registry loaded");

    // 3. Load agent config
    let agent_dir = config::settings::agent_dir(&root_dir, agent_name);
    let agent_config = config::AgentConfig::load(&agent_dir)?;
    let character_sheet = config::agent::load_character_sheet(&agent_dir, agent_name)?;
    info!(
        agent = %agent_name,
        role = %agent_config.role,
        model = %agent_config.model,
        "agent config loaded"
    );

    // 4. Merge env and create LLM client pool
    let merged_env = config::settings::merge_env(&settings.env, &agent_dir);
    let pool = Arc::new(llm::LLMClientPool::new(&model_registry, &merged_env));
    info!(clients = pool.len(), "LLM client pool initialized");

    // Build fallback chain
    let mut fallback_chain = vec![agent_config.model.clone()];
    fallback_chain.extend(agent_config.fallbacks.clone());

    // Get primary model config for context window
    let primary_model = model_registry
        .get(&agent_config.model)
        .ok_or_else(|| format!("Primary model '{}' not found in registry", agent_config.model))?
        .clone();

    // 5. Init database
    let db_path = root_dir.join(&settings.sqlite_db_path);
    let database = Arc::new(db::Database::open(&db_path)?);
    database.register_agent(agent_name)?;
    info!("database initialized");

    // 6. Create tool registry
    let mut tool_registry = tools::ToolRegistry::new();

    // Register file I/O + shell tools
    let workspace = Arc::new(PathBuf::from(&settings.workspace_dir));
    tools::file_tools::register(&mut tool_registry, workspace.clone());
    tools::shell_tool::register(&mut tool_registry, workspace);

    // Register web tools
    tools::web_tools::register(&mut tool_registry, settings.tool_result_max_chars as usize);

    // Register task/mission/conversation tools
    tools::task_tools::register(&mut tool_registry, database.clone(), agent_name.to_string());

    // Register delegation tools (gated by DELEGATION_ENABLED)
    if settings.delegation_enabled {
        let parent_registry = Arc::new(tool_registry);
        tool_registry = tools::ToolRegistry::new();

        // Re-register all parent tools into the new registry
        for def in parent_registry.get_all() {
            tool_registry.register_raw(
                &def.name,
                &def.description,
                def.parameters.clone(),
                def.handler.clone(),
            );
        }

        // Register delegate + delegate_parallel
        tools::delegate::register(
            &mut tool_registry,
            pool.clone(),
            fallback_chain.clone(),
            primary_model.clone(),
            parent_registry,
            root_dir.clone(),
            settings.subagent_max_parallel as usize,
            settings.subagent_sleep_between_secs,
            settings.tool_result_max_chars as usize,
        );
        info!("delegation tools enabled");
    }

    info!(tools = tool_registry.len(), "tool registry initialized");

    // 7. Load MCP servers (if configured)
    let _mcp_connections = tools::mcp::load_mcp_servers(
        &agent_config.mcp_servers,
        &merged_env,
        &mut tool_registry,
    )
    .await;

    // 8. Load core skills
    let core_skills = skills::CoreSkills::load(&root_dir.join("config").join("skills"));

    let tool_registry = Arc::new(tool_registry);

    // 9. Create session manager
    let session_mgr = Arc::new(tokio::sync::Mutex::new(session::SessionManager::new(
        agent_name,
        None, // project_id — set via AKW MCP if connected
        settings.session_ttl_minutes,
        tool_registry.clone(),
    )));

    // 10. Create agent loop
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
        core_skills,
    ));

    // 11. Start heartbeat background task
    let heartbeat_handle = {
        let agent_loop = agent_loop.clone();
        let db = database.clone();
        let session_mgr = session_mgr.clone();
        let name = agent_name.to_string();
        let interval = settings.heartbeat_interval as u64;

        tokio::spawn(async move {
            scheduler::run_heartbeat(agent_loop, db, session_mgr, name, interval).await;
        })
    };

    // 12. Start Discord bot (if enabled)
    let discord_handle = if let Some(ref discord_cfg) = agent_config.channels.discord {
        if discord_cfg.enabled {
            let token = merged_env
                .get("DISCORD_BOT_TOKEN")
                .cloned()
                .unwrap_or_default();
            if token.is_empty() {
                warn!("Discord enabled but DISCORD_BOT_TOKEN not set — skipping");
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
                        error!(error = %e, "Failed to start Discord bot");
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

    // 13. Run CLI channel (blocks until user exits)
    channels::run_cli(&agent_loop, one_shot).await;

    // 14. Graceful shutdown
    heartbeat_handle.abort();
    if let Some(handle) = discord_handle {
        handle.abort();
        info!("Discord bot stopped");
    }
    {
        let mut mgr = session_mgr.lock().await;
        mgr.end_all().await;
    }
    info!("shutdown complete");

    Ok(())
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
