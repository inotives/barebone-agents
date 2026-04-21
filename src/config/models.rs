use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Google,
    Nvidia,
    Openrouter,
    Openai,
    Groq,
    Ollama,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub provider: Provider,
    pub model: String,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub context_window: u32,
    pub max_tokens: u32,
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelRegistry {
    pub models: Vec<ModelConfig>,
}

impl ModelRegistry {
    /// Load model registry from a YAML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read models config at {}: {}", path.display(), e))?;
        serde_yaml::from_str(&content)
            .map_err(|e| format!("Failed to parse models config: {}", e))
    }

    /// Find a model by its registry ID.
    pub fn get(&self, id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn sample_yaml() -> &'static str {
        r#"
models:
  - id: claude-sonnet-4
    provider: anthropic
    model: claude-sonnet-4-20250514
    api_key_env: ANTHROPIC_API_KEY
    context_window: 200000
    max_tokens: 16384

  - id: gemini-2.5-flash
    provider: google
    model: gemini-2.5-flash-preview-04-17
    api_key_env: GOOGLE_GEMINI_API_KEY
    context_window: 1048576
    max_tokens: 65536

  - id: nvidia-minimax
    provider: nvidia
    model: minimaxai/minimax-m2.7
    api_key_env: NVIDIA_API_KEY
    base_url: https://integrate.api.nvidia.com/v1
    context_window: 192000
    max_tokens: 8192
    temperature: 0.7
"#
    }

    fn write_yaml(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("models.yml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(sample_yaml().as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_load_model_registry() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir);
        let registry = ModelRegistry::load(&path).unwrap();
        assert_eq!(registry.models.len(), 3);
    }

    #[test]
    fn test_provider_parsing() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir);
        let registry = ModelRegistry::load(&path).unwrap();

        assert_eq!(registry.models[0].provider, Provider::Anthropic);
        assert_eq!(registry.models[1].provider, Provider::Google);
        assert_eq!(registry.models[2].provider, Provider::Nvidia);
    }

    #[test]
    fn test_get_model_by_id() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir);
        let registry = ModelRegistry::load(&path).unwrap();

        let model = registry.get("claude-sonnet-4").unwrap();
        assert_eq!(model.model, "claude-sonnet-4-20250514");
        assert_eq!(model.context_window, 200000);
        assert!(model.temperature.is_none());
    }

    #[test]
    fn test_get_model_not_found() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir);
        let registry = ModelRegistry::load(&path).unwrap();

        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_optional_fields() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(&dir);
        let registry = ModelRegistry::load(&path).unwrap();

        let nvidia = registry.get("nvidia-minimax").unwrap();
        assert_eq!(nvidia.base_url.as_deref(), Some("https://integrate.api.nvidia.com/v1"));
        assert_eq!(nvidia.temperature, Some(0.7));

        let claude = registry.get("claude-sonnet-4").unwrap();
        assert!(claude.base_url.is_none());
        assert!(claude.temperature.is_none());
    }

    #[test]
    fn test_load_missing_file() {
        let result = ModelRegistry::load(Path::new("/nonexistent/models.yml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_invalid_yaml() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("models.yml");
        std::fs::write(&path, "not: valid: yaml: [").unwrap();

        let result = ModelRegistry::load(&path);
        assert!(result.is_err());
    }
}
