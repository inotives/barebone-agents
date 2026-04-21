use std::collections::HashMap;
use serde_json::Value;
use tracing::{info, warn};

use crate::config::{ModelConfig, ModelRegistry, Provider};
use super::anthropic_client::AnthropicClient;
use super::gemini_client::GeminiClient;
use super::openai_client::OpenAIClient;
use super::types::{LLMClient, LLMMessage, LLMResponse};

pub struct LLMClientPool {
    clients: HashMap<String, Box<dyn LLMClient>>,
}

impl LLMClientPool {
    /// Build a client pool from the model registry, resolving API keys from the merged env.
    /// Skips models with missing API keys (logs warning).
    pub fn new(registry: &ModelRegistry, env: &HashMap<String, String>) -> Self {
        let mut clients: HashMap<String, Box<dyn LLMClient>> = HashMap::new();

        for model in &registry.models {
            match build_client(model, env) {
                Some(client) => {
                    info!(model_id = %model.id, provider = ?model.provider, "registered LLM client");
                    clients.insert(model.id.clone(), client);
                }
                None => {
                    let key_name = model.api_key_env.as_deref().unwrap_or("(none)");
                    warn!(
                        model_id = %model.id,
                        api_key_env = %key_name,
                        "skipping model — API key not found in env"
                    );
                }
            }
        }

        Self { clients }
    }

    /// Get a client by model ID.
    pub fn get(&self, model_id: &str) -> Option<&dyn LLMClient> {
        self.clients.get(model_id).map(|c| c.as_ref())
    }

    /// Try models in order until one succeeds. Sets `response.model` to the ID that worked.
    pub async fn chat_with_fallback(
        &self,
        chain: &[String],
        messages: &[LLMMessage],
        system: Option<&str>,
        tools: Option<&[Value]>,
    ) -> Result<LLMResponse, String> {
        let mut last_error = String::from("No models in fallback chain");

        for model_id in chain {
            let client = match self.get(model_id) {
                Some(c) => c,
                None => {
                    warn!(model_id = %model_id, "model not in pool, skipping");
                    last_error = format!("Model {} not available in pool", model_id);
                    continue;
                }
            };

            match client.chat(messages, system, tools).await {
                Ok(mut resp) => {
                    resp.model = model_id.clone();
                    info!(model_id = %model_id, "LLM call succeeded");
                    return Ok(resp);
                }
                Err(e) => {
                    warn!(model_id = %model_id, error = %e, "LLM call failed, trying next");
                    last_error = e;
                }
            }
        }

        Err(format!("All models failed. Last error: {}", last_error))
    }

    /// Number of registered clients.
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

fn build_client(model: &ModelConfig, env: &HashMap<String, String>) -> Option<Box<dyn LLMClient>> {
    // Resolve API key — Ollama doesn't require one
    let api_key = match &model.api_key_env {
        Some(key_env) => match env.get(key_env) {
            Some(key) if !key.is_empty() => key.clone(),
            _ => return None,
        },
        None => {
            // No API key required (e.g., Ollama)
            String::new()
        }
    };

    let base_url = model
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1");

    match model.provider {
        Provider::Anthropic => Some(Box::new(AnthropicClient::new(
            &api_key,
            &model.model,
            model.max_tokens,
        ))),
        Provider::Google => Some(Box::new(GeminiClient::new(
            &api_key,
            &model.model,
            model.max_tokens,
        ))),
        Provider::Nvidia
        | Provider::Openrouter
        | Provider::Openai
        | Provider::Groq
        | Provider::Ollama => Some(Box::new(OpenAIClient::new(
            base_url,
            &api_key,
            &model.model,
            model.max_tokens,
            model.temperature,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelRegistry;

    fn test_registry() -> ModelRegistry {
        let yaml = r#"
models:
  - id: test-nvidia
    provider: nvidia
    model: test-model
    api_key_env: NVIDIA_API_KEY
    base_url: https://example.com/v1
    context_window: 128000
    max_tokens: 8192
  - id: test-anthropic
    provider: anthropic
    model: claude-test
    api_key_env: ANTHROPIC_API_KEY
    context_window: 200000
    max_tokens: 16384
  - id: test-gemini
    provider: google
    model: gemini-test
    api_key_env: GOOGLE_API_KEY
    context_window: 1000000
    max_tokens: 65536
  - id: test-ollama
    provider: ollama
    model: local-model
    base_url: http://localhost:11434/v1
    context_window: 32000
    max_tokens: 4096
  - id: test-missing-key
    provider: nvidia
    model: no-key-model
    api_key_env: MISSING_KEY
    base_url: https://example.com/v1
    context_window: 128000
    max_tokens: 8192
"#;
        serde_yaml::from_str(yaml).unwrap()
    }

    fn test_env() -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("NVIDIA_API_KEY".into(), "nvda-key".into());
        env.insert("ANTHROPIC_API_KEY".into(), "ant-key".into());
        env.insert("GOOGLE_API_KEY".into(), "goog-key".into());
        env
    }

    #[test]
    fn test_pool_registers_available_clients() {
        let registry = test_registry();
        let env = test_env();
        let pool = LLMClientPool::new(&registry, &env);

        // Should register: nvidia, anthropic, gemini, ollama (no key needed)
        // Should skip: missing-key
        assert_eq!(pool.len(), 4);
        assert!(pool.get("test-nvidia").is_some());
        assert!(pool.get("test-anthropic").is_some());
        assert!(pool.get("test-gemini").is_some());
        assert!(pool.get("test-ollama").is_some());
        assert!(pool.get("test-missing-key").is_none());
    }

    #[test]
    fn test_pool_skips_missing_keys() {
        let registry = test_registry();
        let env = HashMap::new(); // no keys at all
        let pool = LLMClientPool::new(&registry, &env);

        // Only Ollama should be registered (no key required)
        assert_eq!(pool.len(), 1);
        assert!(pool.get("test-ollama").is_some());
    }

    #[test]
    fn test_pool_empty_key_value() {
        let registry = test_registry();
        let mut env = HashMap::new();
        env.insert("NVIDIA_API_KEY".into(), "".into()); // empty
        let pool = LLMClientPool::new(&registry, &env);

        assert!(pool.get("test-nvidia").is_none());
    }

    #[tokio::test]
    async fn test_fallback_no_models() {
        let registry: ModelRegistry = serde_yaml::from_str("models: []").unwrap();
        let pool = LLMClientPool::new(&registry, &HashMap::new());

        let result = pool
            .chat_with_fallback(&[], &[LLMMessage::user("hi")], None, None)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No models"));
    }

    #[tokio::test]
    async fn test_fallback_skips_unavailable() {
        let registry = test_registry();
        let pool = LLMClientPool::new(&registry, &HashMap::new()); // only ollama

        let chain = vec![
            "test-nvidia".to_string(),    // not in pool
            "test-anthropic".to_string(), // not in pool
        ];
        let result = pool
            .chat_with_fallback(&chain, &[LLMMessage::user("hi")], None, None)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available in pool"));
    }
}
