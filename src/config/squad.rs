use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct SquadConfig {
    pub name: String,
    #[serde(default)]
    pub teams: HashMap<String, TeamConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TeamConfig {
    pub leader: String,
    #[serde(default)]
    pub members: Vec<String>,
}

impl SquadConfig {
    /// Load squad config from a YAML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read squad config at {}: {}", path.display(), e))?;
        serde_yaml::from_str(&content)
            .map_err(|e| format!("Failed to parse squad config: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn sample_yaml() -> &'static str {
        r#"
name: "Test Squad"
teams:
  alpha:
    leader: ino
    members: [robin, aria]
  bravo:
    leader: kai
    members: []
"#
    }

    #[test]
    fn test_load_squad_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("squad.yml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(sample_yaml().as_bytes()).unwrap();

        let config = SquadConfig::load(&path).unwrap();
        assert_eq!(config.name, "Test Squad");
        assert_eq!(config.teams.len(), 2);
    }

    #[test]
    fn test_team_structure() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("squad.yml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(sample_yaml().as_bytes()).unwrap();

        let config = SquadConfig::load(&path).unwrap();
        let alpha = config.teams.get("alpha").unwrap();
        assert_eq!(alpha.leader, "ino");
        assert_eq!(alpha.members, vec!["robin", "aria"]);

        let bravo = config.teams.get("bravo").unwrap();
        assert_eq!(bravo.leader, "kai");
        assert!(bravo.members.is_empty());
    }

    #[test]
    fn test_load_missing_file() {
        let result = SquadConfig::load(Path::new("/nonexistent/squad.yml"));
        assert!(result.is_err());
    }
}
