use std::path::Path;

use serde_json::json;

use crate::cli::AgentsCommand;
use crate::config::{AgentConfig, ModelRegistry};
use crate::config::settings::agent_dir;
use crate::db::Database;
use crate::discover_agents;

pub fn run(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    cmd: AgentsCommand,
) -> Result<(), String> {
    match cmd {
        AgentsCommand::List { json } => run_list(db, root_dir, model_registry, json),
        AgentsCommand::Show { name, json } => run_show(db, root_dir, model_registry, &name, json),
    }
}

fn run_list(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    as_json: bool,
) -> Result<(), String> {
    let names = discover_agents(root_dir)?;

    if names.is_empty() {
        if as_json {
            println!("[]");
        } else {
            println!("(no agents found in agents/ directory)");
        }
        return Ok(());
    }

    if as_json {
        let arr: Vec<_> = names
            .iter()
            .map(|name| {
                let dir = agent_dir(root_dir, name);
                let config = AgentConfig::load(&dir).ok();
                let last_active = db.get_agent_last_active(name).ok().flatten();
                let model_name = config
                    .as_ref()
                    .and_then(|c| model_registry.get(&c.model))
                    .map(|m| m.model.clone());
                let mcp_count = config.as_ref().map(|c| c.mcp_servers.len()).unwrap_or(0);

                json!({
                    "name": name,
                    "role": config.as_ref().map(|c| c.role.as_str()),
                    "model": config.as_ref().map(|c| c.model.as_str()),
                    "model_name": model_name,
                    "mcp_servers": mcp_count,
                    "last_active": last_active,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else {
        println!(
            "{:<12} {:<12} {:<24} {:<6} {}",
            "NAME", "ROLE", "MODEL", "MCP", "LAST ACTIVE"
        );
        println!("{}", "-".repeat(72));
        for name in &names {
            let dir = agent_dir(root_dir, name);
            let config = AgentConfig::load(&dir).ok();
            let last_active = db
                .get_agent_last_active(name)
                .ok()
                .flatten()
                .unwrap_or_else(|| "-".to_string());
            let role = config
                .as_ref()
                .map(|c| c.role.as_str())
                .unwrap_or("?");
            let model = config
                .as_ref()
                .map(|c| c.model.as_str())
                .unwrap_or("?");
            let mcp_count = config.as_ref().map(|c| c.mcp_servers.len()).unwrap_or(0);

            println!(
                "{:<12} {:<12} {:<24} {:<6} {}",
                name, role, model, mcp_count, last_active
            );
        }
    }
    Ok(())
}

fn run_show(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    name: &str,
    as_json: bool,
) -> Result<(), String> {
    let dir = agent_dir(root_dir, name);
    let config = AgentConfig::load(&dir)
        .map_err(|e| format!("Agent '{}' not found or invalid config: {}", name, e))?;

    let last_active = db.get_agent_last_active(name).ok().flatten();
    let model_info = model_registry.get(&config.model);

    if as_json {
        let fallbacks: Vec<_> = config.fallbacks.iter().map(|f| json!(f)).collect();
        let mcp_servers: Vec<_> = config
            .mcp_servers
            .iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "command": m.command,
                    "args": m.args,
                    "tools": m.tools,
                })
            })
            .collect();
        let discord = config.channels.discord.as_ref().map(|d| {
            json!({
                "enabled": d.enabled,
                "allow_from": d.allow_from,
                "guilds": d.guilds.keys().collect::<Vec<_>>(),
            })
        });

        let val = json!({
            "name": name,
            "role": config.role,
            "model": config.model,
            "model_name": model_info.map(|m| m.model.as_str()),
            "provider": model_info.map(|m| format!("{:?}", m.provider)),
            "fallbacks": fallbacks,
            "discord": discord,
            "mcp_servers": mcp_servers,
            "skills": config.skills,
            "akw_skills": config.akw_skills,
            "last_active": last_active,
        });
        println!("{}", serde_json::to_string_pretty(&val).unwrap());
    } else {
        println!("Name:        {}", name);
        println!("Role:        {}", config.role);
        println!("Model:       {}", config.model);
        if let Some(mi) = model_info {
            println!("Model name:  {}", mi.model);
            println!("Provider:    {:?}", mi.provider);
            println!("Context:     {}", mi.context_window);
            println!("Max tokens:  {}", mi.max_tokens);
        }
        if !config.fallbacks.is_empty() {
            println!("Fallbacks:   {}", config.fallbacks.join(", "));
        }
        println!("AKW skills:  {}", if config.akw_skills { "enabled" } else { "disabled" });
        println!(
            "Last active: {}",
            last_active.as_deref().unwrap_or("-")
        );

        // Discord
        if let Some(discord) = &config.channels.discord {
            println!();
            println!("Discord:");
            println!("  Enabled:    {}", discord.enabled);
            if !discord.allow_from.is_empty() {
                println!("  Allow from: {}", discord.allow_from.join(", "));
            }
            for (gid, gcfg) in &discord.guilds {
                println!(
                    "  Guild {}:  requireMention={}",
                    gid, gcfg.require_mention
                );
            }
        }

        // MCP servers
        if !config.mcp_servers.is_empty() {
            println!();
            println!("MCP Servers:");
            for m in &config.mcp_servers {
                let tools_str = if m.tools.is_empty() {
                    "(all)".to_string()
                } else {
                    m.tools.join(", ")
                };
                println!("  {} — {} {} [{}]", m.name, m.command, m.args.join(" "), tools_str);
            }
        }

        // Skills
        if !config.skills.is_empty() {
            println!();
            println!("Skills:      {}", config.skills.join(", "));
        }
    }
    Ok(())
}
