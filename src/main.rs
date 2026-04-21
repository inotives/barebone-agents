mod agent_loop;
mod channels;
mod cli;
mod config;
mod db;
mod llm;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use tracing::{error, info};

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

    // Register delegation tools (needs Arc of parent registry, so defer until after other tools)
    let parent_registry = Arc::new(tool_registry);
    let mut tool_registry = tools::ToolRegistry::new();

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

    info!(tools = tool_registry.len(), "tool registry initialized");

    // 7. Load MCP servers (if configured)
    let _mcp_connections = tools::mcp::load_mcp_servers(
        &agent_config.mcp_servers,
        &merged_env,
        &mut tool_registry,
    )
    .await;

    // 8. Create agent loop
    let agent_loop = agent_loop::AgentLoop::new(
        agent_name.to_string(),
        character_sheet,
        pool,
        fallback_chain,
        Arc::new(tool_registry),
        database,
        primary_model,
        settings.max_tool_iterations,
        settings.tool_result_max_chars as usize,
        settings.history_limit,
    );

    // 9. Run CLI channel
    channels::run_cli(&agent_loop, one_shot).await;

    Ok(())
}
