use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Platform-wide settings parsed from root .env
#[derive(Debug, Clone)]
pub struct Settings {
    pub sqlite_db_path: String,
    pub log_level: String,
    pub session_ttl_minutes: u32,
    pub history_limit: u32,
    pub max_tool_iterations: u32,
    pub tool_result_max_chars: u32,
    pub workspace_dir: String,
    pub session_fallback_dir: String,
    pub subagent_max_parallel: u32,
    pub subagent_sleep_between_secs: f64,
    pub skills_dir: String,
    pub skills_token_budget: u32,
    pub skills_min_match_hits: u32,
    pub heartbeat_interval: u32,
    pub platform_name: String,
    /// All key-value pairs from root .env (for API keys, etc.)
    pub env: HashMap<String, String>,
}

impl Settings {
    /// Load settings from root .env file.
    /// Falls back to defaults for missing keys.
    pub fn load(root_dir: &Path) -> Self {
        let env = load_env_file(&root_dir.join(".env"));
        Self::from_env(env)
    }

    /// Build Settings from a pre-loaded env map (useful for testing).
    pub fn from_env(env: HashMap<String, String>) -> Self {
        Self {
            sqlite_db_path: env_or(&env, "SQLITE_DB_PATH", "./data/barebone-agent.db"),
            log_level: env_or(&env, "LOG_LEVEL", "info"),
            session_ttl_minutes: env_or_parse(&env, "SESSION_TTL_MINUTES", 30),
            history_limit: env_or_parse(&env, "HISTORY_LIMIT", 20),
            max_tool_iterations: env_or_parse(&env, "MAX_TOOL_ITERATIONS", 10),
            tool_result_max_chars: env_or_parse(&env, "TOOL_RESULT_MAX_CHARS", 5000),
            workspace_dir: env_or(&env, "WORKSPACE_DIR", "./workspace"),
            session_fallback_dir: env_or(&env, "SESSION_FALLBACK_DIR", "./data/sessions"),
            subagent_max_parallel: env_or_parse(&env, "SUBAGENT_MAX_PARALLEL", 3),
            subagent_sleep_between_secs: env_or_parse(&env, "SUBAGENT_SLEEP_BETWEEN_SECS", 5.0),
            skills_dir: env_or(&env, "SKILLS_DIR", "./skills"),
            skills_token_budget: env_or_parse(&env, "SKILLS_TOKEN_BUDGET", 4000),
            skills_min_match_hits: env_or_parse(&env, "SKILLS_MIN_MATCH_HITS", 2),
            heartbeat_interval: env_or_parse(&env, "HEARTBEAT_INTERVAL", 60),
            platform_name: env_or(&env, "PLATFORM_NAME", "barebone-agent"),
            env,
        }
    }
}

/// Load a .env file into a HashMap without mutating process env.
pub fn load_env_file(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(iter) = dotenvy::from_path_iter(path) {
        for item in iter {
            if let Ok((key, value)) = item {
                map.insert(key, value);
            }
        }
    }
    map
}

/// Merge root env with agent-specific env (agent overrides root).
pub fn merge_env(root: &HashMap<String, String>, agent_dir: &Path) -> HashMap<String, String> {
    let mut merged = root.clone();
    let agent_env = load_env_file(&agent_dir.join(".env"));
    merged.extend(agent_env);
    merged
}

/// Resolve the absolute path for an agent directory.
pub fn agent_dir(root_dir: &Path, agent_name: &str) -> PathBuf {
    root_dir.join("agents").join(agent_name)
}

fn env_or(env: &HashMap<String, String>, key: &str, default: &str) -> String {
    env.get(key).cloned().unwrap_or_else(|| default.to_string())
}

fn env_or_parse<T: std::str::FromStr>(env: &HashMap<String, String>, key: &str, default: T) -> T {
    env.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_settings_defaults() {
        let settings = Settings::from_env(HashMap::new());
        assert_eq!(settings.sqlite_db_path, "./data/barebone-agent.db");
        assert_eq!(settings.log_level, "info");
        assert_eq!(settings.session_ttl_minutes, 30);
        assert_eq!(settings.history_limit, 20);
        assert_eq!(settings.max_tool_iterations, 10);
        assert_eq!(settings.heartbeat_interval, 60);
        assert_eq!(settings.platform_name, "barebone-agent");
    }

    #[test]
    fn test_settings_from_env() {
        let mut env = HashMap::new();
        env.insert("LOG_LEVEL".into(), "debug".into());
        env.insert("HISTORY_LIMIT".into(), "50".into());
        env.insert("HEARTBEAT_INTERVAL".into(), "120".into());

        let settings = Settings::from_env(env);
        assert_eq!(settings.log_level, "debug");
        assert_eq!(settings.history_limit, 50);
        assert_eq!(settings.heartbeat_interval, 120);
    }

    #[test]
    fn test_settings_invalid_parse_uses_default() {
        let mut env = HashMap::new();
        env.insert("HISTORY_LIMIT".into(), "not_a_number".into());

        let settings = Settings::from_env(env);
        assert_eq!(settings.history_limit, 20); // falls back to default
    }

    #[test]
    fn test_load_env_file() {
        let dir = TempDir::new().unwrap();
        let env_path = dir.path().join(".env");
        let mut file = std::fs::File::create(&env_path).unwrap();
        writeln!(file, "API_KEY=secret123").unwrap();
        writeln!(file, "LOG_LEVEL=debug").unwrap();

        let env = load_env_file(&env_path);
        assert_eq!(env.get("API_KEY").unwrap(), "secret123");
        assert_eq!(env.get("LOG_LEVEL").unwrap(), "debug");
    }

    #[test]
    fn test_load_env_file_missing() {
        let env = load_env_file(Path::new("/nonexistent/.env"));
        assert!(env.is_empty());
    }

    #[test]
    fn test_merge_env_agent_overrides_root() {
        let dir = TempDir::new().unwrap();
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let mut agent_env = std::fs::File::create(agent_dir.join(".env")).unwrap();
        writeln!(agent_env, "SHARED_KEY=agent_value").unwrap();
        writeln!(agent_env, "AGENT_ONLY=true").unwrap();

        let mut root = HashMap::new();
        root.insert("SHARED_KEY".into(), "root_value".into());
        root.insert("ROOT_ONLY".into(), "yes".into());

        let merged = merge_env(&root, &agent_dir);
        assert_eq!(merged.get("SHARED_KEY").unwrap(), "agent_value"); // agent wins
        assert_eq!(merged.get("ROOT_ONLY").unwrap(), "yes"); // root preserved
        assert_eq!(merged.get("AGENT_ONLY").unwrap(), "true"); // agent-only added
    }

    #[test]
    fn test_settings_load_from_file() {
        let dir = TempDir::new().unwrap();
        let mut env_file = std::fs::File::create(dir.path().join(".env")).unwrap();
        writeln!(env_file, "LOG_LEVEL=warn").unwrap();
        writeln!(env_file, "HEARTBEAT_INTERVAL=300").unwrap();

        let settings = Settings::load(dir.path());
        assert_eq!(settings.log_level, "warn");
        assert_eq!(settings.heartbeat_interval, 300);
    }
}
