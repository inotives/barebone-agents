use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub role: String,
    pub model: String,
    #[serde(default)]
    pub fallbacks: Vec<String>,
    #[serde(default)]
    pub channels: ChannelConfig,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChannelConfig {
    #[serde(default)]
    pub discord: Option<DiscordConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub guilds: HashMap<String, GuildConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuildConfig {
    #[serde(default, rename = "requireMention")]
    pub require_mention: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub tools: Vec<String>,
}

impl McpServerConfig {
    /// Resolve `${VAR}` placeholders in env values from the merged env.
    pub fn resolve_env(&self, merged_env: &HashMap<String, String>) -> HashMap<String, String> {
        self.env
            .iter()
            .map(|(k, v)| {
                let resolved = if v.starts_with("${") && v.ends_with('}') {
                    let var_name = &v[2..v.len() - 1];
                    merged_env
                        .get(var_name)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    v.clone()
                };
                (k.clone(), resolved)
            })
            .collect()
    }
}

impl AgentConfig {
    /// Load agent config from agent.yml in the agent's directory.
    pub fn load(agent_dir: &Path) -> Result<Self, String> {
        let path = agent_dir.join("agent.yml");
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read agent config at {}: {}", path.display(), e))?;
        serde_yaml::from_str(&content)
            .map_err(|e| format!("Failed to parse agent config: {}", e))
    }
}

/// Load the agent's character sheet (AGENT.md) with template variable substitution.
pub fn load_character_sheet(agent_dir: &Path, agent_name: &str) -> Result<String, String> {
    let path = agent_dir.join("AGENT.md");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read character sheet at {}: {}", path.display(), e))?;
    Ok(content.replace("{{AGENT_NAME}}", agent_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn sample_agent_yaml() -> &'static str {
        r#"
role: leader
model: nvidia-minimax-2.7
fallbacks:
  - openrouter-deepseek-v3
  - gemini-2.5-flash

channels:
  discord:
    enabled: true
    allow_from: ["user-id-1"]
    guilds:
      "guild-id":
        requireMention: true

skills:
  - sprint_planning
  - code_review

mcp_servers:
  - name: github
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: "${GITHUB_TOKEN}"
    tools:
      - create_issue
      - search_repositories
"#
    }

    fn minimal_agent_yaml() -> &'static str {
        r#"
role: coder
model: claude-sonnet-4
"#
    }

    fn write_agent_yml(dir: &Path, content: &str) {
        let mut f = std::fs::File::create(dir.join("agent.yml")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_load_full_agent_config() {
        let dir = TempDir::new().unwrap();
        write_agent_yml(dir.path(), sample_agent_yaml());

        let config = AgentConfig::load(dir.path()).unwrap();
        assert_eq!(config.role, "leader");
        assert_eq!(config.model, "nvidia-minimax-2.7");
        assert_eq!(config.fallbacks.len(), 2);
        assert_eq!(config.skills.len(), 2);
        assert_eq!(config.mcp_servers.len(), 1);
    }

    #[test]
    fn test_load_minimal_agent_config() {
        let dir = TempDir::new().unwrap();
        write_agent_yml(dir.path(), minimal_agent_yaml());

        let config = AgentConfig::load(dir.path()).unwrap();
        assert_eq!(config.role, "coder");
        assert_eq!(config.model, "claude-sonnet-4");
        assert!(config.fallbacks.is_empty());
        assert!(config.skills.is_empty());
        assert!(config.mcp_servers.is_empty());
        assert!(config.channels.discord.is_none());
    }

    #[test]
    fn test_discord_config() {
        let dir = TempDir::new().unwrap();
        write_agent_yml(dir.path(), sample_agent_yaml());

        let config = AgentConfig::load(dir.path()).unwrap();
        let discord = config.channels.discord.unwrap();
        assert!(discord.enabled);
        assert_eq!(discord.allow_from, vec!["user-id-1"]);
        assert!(discord.guilds.get("guild-id").unwrap().require_mention);
    }

    #[test]
    fn test_mcp_server_config() {
        let dir = TempDir::new().unwrap();
        write_agent_yml(dir.path(), sample_agent_yaml());

        let config = AgentConfig::load(dir.path()).unwrap();
        let mcp = &config.mcp_servers[0];
        assert_eq!(mcp.name, "github");
        assert_eq!(mcp.command, "npx");
        assert_eq!(mcp.args, vec!["-y", "@modelcontextprotocol/server-github"]);
        assert_eq!(mcp.tools, vec!["create_issue", "search_repositories"]);
    }

    #[test]
    fn test_mcp_env_var_substitution() {
        let dir = TempDir::new().unwrap();
        write_agent_yml(dir.path(), sample_agent_yaml());

        let config = AgentConfig::load(dir.path()).unwrap();
        let mcp = &config.mcp_servers[0];

        let mut merged_env = HashMap::new();
        merged_env.insert("GITHUB_TOKEN".into(), "ghp_secret123".into());

        let resolved = mcp.resolve_env(&merged_env);
        assert_eq!(resolved.get("GITHUB_TOKEN").unwrap(), "ghp_secret123");
    }

    #[test]
    fn test_mcp_env_var_missing() {
        let dir = TempDir::new().unwrap();
        write_agent_yml(dir.path(), sample_agent_yaml());

        let config = AgentConfig::load(dir.path()).unwrap();
        let mcp = &config.mcp_servers[0];

        let merged_env = HashMap::new(); // empty — var not found
        let resolved = mcp.resolve_env(&merged_env);
        assert_eq!(resolved.get("GITHUB_TOKEN").unwrap(), ""); // empty string fallback
    }

    #[test]
    fn test_load_character_sheet() {
        let dir = TempDir::new().unwrap();
        let mut f = std::fs::File::create(dir.path().join("AGENT.md")).unwrap();
        writeln!(f, "# {{{{AGENT_NAME}}}}").unwrap();
        writeln!(f, "I am {{{{AGENT_NAME}}}}, your assistant.").unwrap();

        let content = load_character_sheet(dir.path(), "robin").unwrap();
        assert!(content.contains("# robin"));
        assert!(content.contains("I am robin, your assistant."));
        assert!(!content.contains("{{AGENT_NAME}}"));
    }

    #[test]
    fn test_load_character_sheet_missing() {
        let dir = TempDir::new().unwrap();
        let result = load_character_sheet(dir.path(), "robin");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_agent_config_missing() {
        let dir = TempDir::new().unwrap();
        let result = AgentConfig::load(dir.path());
        assert!(result.is_err());
    }
}
