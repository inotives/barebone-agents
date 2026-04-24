use std::path::Path;

use serde_json::json;

use crate::cli::ConfigCommand;
use crate::config::{AgentConfig, ModelRegistry};
use crate::config::settings::agent_dir;
use crate::discover_agents;

#[derive(Debug)]
struct Check {
    component: String,
    status: Status,
    message: String,
}

#[derive(Debug)]
enum Status {
    Ok,
    Warn,
    Error,
}

impl Status {
    fn label(&self) -> &str {
        match self {
            Status::Ok => "OK",
            Status::Warn => "WARN",
            Status::Error => "ERROR",
        }
    }
}

pub fn run(root_dir: &Path, cmd: ConfigCommand) -> Result<(), String> {
    match cmd {
        ConfigCommand::Validate { json } => run_validate(root_dir, json),
    }
}

fn run_validate(root_dir: &Path, as_json: bool) -> Result<(), String> {
    let mut checks: Vec<Check> = Vec::new();

    // 1. Check models.yml
    let registry_path = root_dir.join("config").join("models.yml");
    let model_registry = match ModelRegistry::load(&registry_path) {
        Ok(r) => {
            checks.push(Check {
                component: "models.yml".into(),
                status: Status::Ok,
                message: format!("{} models loaded", r.models.len()),
            });
            Some(r)
        }
        Err(e) => {
            checks.push(Check {
                component: "models.yml".into(),
                status: Status::Error,
                message: e,
            });
            None
        }
    };

    // 2. Check agents
    let agents = match discover_agents(root_dir) {
        Ok(names) => {
            if names.is_empty() {
                checks.push(Check {
                    component: "agents/".into(),
                    status: Status::Warn,
                    message: "No agents found".into(),
                });
            }
            names
        }
        Err(e) => {
            checks.push(Check {
                component: "agents/".into(),
                status: Status::Error,
                message: e,
            });
            Vec::new()
        }
    };

    // 3. Check each agent
    for name in &agents {
        let dir = agent_dir(root_dir, name);

        // agent.yml
        match AgentConfig::load(&dir) {
            Ok(config) => {
                checks.push(Check {
                    component: format!("agents/{}/agent.yml", name),
                    status: Status::Ok,
                    message: format!("role={}, model={}", config.role, config.model),
                });

                // Check model exists in registry
                if let Some(ref registry) = model_registry {
                    if registry.get(&config.model).is_none() {
                        checks.push(Check {
                            component: format!("agents/{} model", name),
                            status: Status::Error,
                            message: format!(
                                "Model '{}' not found in models.yml",
                                config.model
                            ),
                        });
                    }

                    // Check fallbacks
                    for fb in &config.fallbacks {
                        if registry.get(fb).is_none() {
                            checks.push(Check {
                                component: format!("agents/{} fallback", name),
                                status: Status::Warn,
                                message: format!(
                                    "Fallback model '{}' not found in models.yml",
                                    fb
                                ),
                            });
                        }
                    }
                }
            }
            Err(e) => {
                checks.push(Check {
                    component: format!("agents/{}/agent.yml", name),
                    status: Status::Error,
                    message: e,
                });
            }
        }

        // AGENT.md
        let agent_md = dir.join("AGENT.md");
        if !agent_md.exists() {
            checks.push(Check {
                component: format!("agents/{}/AGENT.md", name),
                status: Status::Warn,
                message: "Character sheet not found".into(),
            });
        }
    }

    // Output
    let has_errors = checks.iter().any(|c| matches!(c.status, Status::Error));

    if as_json {
        let arr: Vec<_> = checks
            .iter()
            .map(|c| {
                json!({
                    "component": c.component,
                    "status": c.status.label(),
                    "message": c.message,
                })
            })
            .collect();
        let result = json!({
            "valid": !has_errors,
            "checks": arr,
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        for c in &checks {
            let marker = match c.status {
                Status::Ok => " ",
                Status::Warn => "!",
                Status::Error => "X",
            };
            println!(
                "[{}] {:<6} {:<32} {}",
                marker,
                c.status.label(),
                c.component,
                c.message
            );
        }
        println!();
        if has_errors {
            println!("Validation FAILED — fix errors above before running agents.");
        } else {
            println!("Validation passed.");
        }
    }

    Ok(())
}
