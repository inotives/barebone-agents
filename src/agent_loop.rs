use regex::Regex;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::ModelConfig;
use crate::db::Database;
use crate::llm::{truncate_history, LLMClientPool, LLMMessage};
use crate::tools::ToolRegistry;

pub struct AgentLoop {
    pub agent_name: String,
    character_sheet: String,
    pool: Arc<LLMClientPool>,
    fallback_chain: Vec<String>,
    registry: Arc<ToolRegistry>,
    db: Arc<Database>,
    primary_model: ModelConfig,
    max_tool_iterations: u32,
    tool_result_max_chars: usize,
    history_limit: u32,
}

impl AgentLoop {
    pub fn new(
        agent_name: String,
        character_sheet: String,
        pool: Arc<LLMClientPool>,
        fallback_chain: Vec<String>,
        registry: Arc<ToolRegistry>,
        db: Arc<Database>,
        primary_model: ModelConfig,
        max_tool_iterations: u32,
        tool_result_max_chars: usize,
        history_limit: u32,
    ) -> Self {
        Self {
            agent_name,
            character_sheet,
            pool,
            fallback_chain,
            registry,
            db,
            primary_model,
            max_tool_iterations,
            tool_result_max_chars,
            history_limit,
        }
    }

    /// Main entry point for the agent reasoning loop.
    pub async fn run(
        &self,
        message: &str,
        conversation_id: &str,
        channel_type: &str,
        parent_id: Option<&str>,
    ) -> String {
        let turn_id = format!("turn-{}", &uuid::Uuid::new_v4().to_string()[..8]);

        debug!(
            agent = %self.agent_name,
            conv_id = %conversation_id,
            turn = %turn_id,
            "starting turn"
        );

        // Load conversation history
        let history = self
            .db
            .load_history(conversation_id, self.history_limit)
            .unwrap_or_default();

        // Save user message
        let metadata = parent_id.map(|pid| format!(r#"{{"parent_id":"{}"}}"#, pid));
        if let Err(e) = self.db.save_message(
            conversation_id,
            &self.agent_name,
            "user",
            message,
            channel_type,
            None,
            0,
            0,
            &turn_id,
            true,
            metadata.as_deref(),
        ) {
            warn!(error = %e, "failed to save user message");
        }

        // Build system prompt
        let system = self.build_system_prompt(message, conversation_id, parent_id);

        // Convert history to LLM messages
        let mut messages: Vec<LLMMessage> = history
            .iter()
            .map(|m| LLMMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();
        messages.push(LLMMessage::user(message));

        // Truncate to fit context window
        messages = truncate_history(
            &messages,
            Some(&system),
            self.primary_model.context_window,
            self.primary_model.max_tokens,
        );

        // Get tool definitions
        let tool_defs = self.registry.get_definitions();
        let tools = if tool_defs.is_empty() {
            None
        } else {
            Some(tool_defs)
        };

        // Call LLM
        let mut response = match self
            .pool
            .chat_with_fallback(
                &self.fallback_chain,
                &messages,
                Some(&system),
                tools.as_deref(),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let error_msg = format!("I'm sorry, all models failed: {}", e);
                self.save_final_response(conversation_id, &turn_id, channel_type, &error_msg, 0, 0, "error");
                return error_msg;
            }
        };

        // Tool loop
        let mut iteration = 0;
        while let Some(ref tool_calls) = response.tool_calls {
            if iteration >= self.max_tool_iterations {
                warn!(
                    agent = %self.agent_name,
                    iterations = iteration,
                    "max tool iterations reached"
                );
                break;
            }

            // Save assistant message with tool calls (is_final=0)
            let tc_meta = serde_json::to_string(
                &tool_calls
                    .iter()
                    .map(|tc| {
                        serde_json::json!({
                            "id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .ok();

            let _ = self.db.save_message(
                conversation_id,
                &self.agent_name,
                "assistant",
                &response.content,
                channel_type,
                Some(&response.model),
                response.usage.input_tokens as i64,
                response.usage.output_tokens as i64,
                &turn_id,
                false,
                tc_meta.as_deref(),
            );

            // Build assistant message for LLM context
            let assistant_msg = LLMMessage {
                role: "assistant".into(),
                content: response.content.clone(),
                tool_calls: Some(tool_calls.clone()),
                tool_call_id: None,
            };
            messages.push(assistant_msg);

            // Execute each tool call
            for tc in tool_calls {
                info!(
                    agent = %self.agent_name,
                    tool = %tc.name,
                    "executing tool"
                );

                let result = self.registry.execute(&tc.name, tc.arguments.clone()).await;

                // Truncate result
                let result = if result.len() > self.tool_result_max_chars {
                    format!(
                        "{}... (truncated at {} chars)",
                        &result[..self.tool_result_max_chars],
                        self.tool_result_max_chars
                    )
                } else {
                    result
                };

                // Save tool result (is_final=0)
                let tool_meta = serde_json::json!({
                    "tool_call_id": tc.id,
                    "tool_name": tc.name,
                })
                .to_string();

                let _ = self.db.save_message(
                    conversation_id,
                    &self.agent_name,
                    "tool",
                    &result,
                    channel_type,
                    None,
                    0,
                    0,
                    &turn_id,
                    false,
                    Some(&tool_meta),
                );

                messages.push(LLMMessage::tool_result(&tc.id, &result));
            }

            // Call LLM again with tool results
            response = match self
                .pool
                .chat_with_fallback(
                    &self.fallback_chain,
                    &messages,
                    Some(&system),
                    tools.as_deref(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let error_msg = format!("LLM call failed during tool loop: {}", e);
                    self.save_final_response(
                        conversation_id, &turn_id, channel_type, &error_msg, 0, 0, "error",
                    );
                    return error_msg;
                }
            };

            iteration += 1;
        }

        // Save final response
        self.save_final_response(
            conversation_id,
            &turn_id,
            channel_type,
            &response.content,
            response.usage.input_tokens as i64,
            response.usage.output_tokens as i64,
            &response.model,
        );

        info!(
            agent = %self.agent_name,
            model = %response.model,
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            tool_iterations = iteration,
            "turn complete"
        );

        response.content
    }

    fn save_final_response(
        &self,
        conversation_id: &str,
        turn_id: &str,
        channel_type: &str,
        content: &str,
        input_tokens: i64,
        output_tokens: i64,
        model: &str,
    ) {
        if let Err(e) = self.db.save_message(
            conversation_id,
            &self.agent_name,
            "assistant",
            content,
            channel_type,
            Some(model),
            input_tokens,
            output_tokens,
            turn_id,
            true,
            None,
        ) {
            warn!(error = %e, "failed to save final response");
        }
    }

    fn build_system_prompt(
        &self,
        message: &str,
        conversation_id: &str,
        parent_id: Option<&str>,
    ) -> String {
        let mut prompt = self.character_sheet.clone();

        // Cross-agent @mention context
        let mention_context = self.build_mention_context(message);
        if !mention_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&mention_context);
        }

        // Parent conversation context
        if let Some(pid) = parent_id {
            let parent_context = self.build_parent_context(pid);
            if !parent_context.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&parent_context);
            }
        } else {
            // Check if the first message in this conv has a parent_id
            if let Ok(Some(pid)) = self.db.get_parent_id(conversation_id) {
                let parent_context = self.build_parent_context(&pid);
                if !parent_context.is_empty() {
                    prompt.push_str("\n\n");
                    prompt.push_str(&parent_context);
                }
            }
        }

        prompt
    }

    fn build_mention_context(&self, message: &str) -> String {
        let re = Regex::new(r"@(\w+)").unwrap();
        let agents = self.db.get_registered_agents().unwrap_or_default();

        let mut context_parts = Vec::new();

        for cap in re.captures_iter(message) {
            let name = &cap[1];
            if name == self.agent_name || !agents.contains(&name.to_string()) {
                continue;
            }

            let mut section = format!("## Context from @{}\n", name);

            // Recent completed tasks
            if let Ok(tasks) = self.db.list_tasks(Some(name), Some("done"), None) {
                for task in tasks.iter().take(5) {
                    let result_preview = task
                        .result
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect::<String>();
                    section.push_str(&format!(
                        "- Task {}: {} — {}\n",
                        task.key, task.title, result_preview
                    ));
                }
            }

            // Recent messages
            if let Ok(msgs) = self.db.load_recent_messages(name, 5) {
                for msg in &msgs {
                    let preview: String = msg.content.chars().take(300).collect();
                    section.push_str(&format!("- [{}] {}\n", msg.role, preview));
                }
            }

            if section.lines().count() > 1 {
                context_parts.push(section);
            }
        }

        context_parts.join("\n")
    }

    fn build_parent_context(&self, parent_conv_id: &str) -> String {
        let messages = self
            .db
            .load_history(parent_conv_id, 5)
            .unwrap_or_default();

        if messages.is_empty() {
            return String::new();
        }

        let mut context = "## Previous Conversation Context\n".to_string();
        for msg in &messages {
            let preview: String = msg.content.chars().take(300).collect();
            context.push_str(&format!("- [{}] {}\n", msg.role, preview));
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Arc<Database> {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.register_agent("ino").unwrap();
        db.register_agent("robin").unwrap();
        db
    }

    // Test build_mention_context
    #[test]
    fn test_mention_context_with_known_agent() {
        let db = setup_db();
        // Add some data for robin
        db.save_message("c1", "robin", "assistant", "I finished the task", "cli", None, 0, 0, "t1", true, None).unwrap();
        db.create_task("Robin's task", None, None, Some("robin"), None, None, None).unwrap();
        db.update_task("TSK-00001", Some("done"), Some("Completed successfully"), None, None).unwrap();

        let pool = Arc::new(LLMClientPool::new(
            &crate::config::ModelRegistry { models: vec![] },
            &std::collections::HashMap::new(),
        ));

        let loop_ = AgentLoop::new(
            "ino".into(),
            "You are ino.".into(),
            pool,
            vec![],
            Arc::new(ToolRegistry::new()),
            db,
            ModelConfig {
                id: "test".into(),
                provider: crate::config::Provider::Nvidia,
                model: "test".into(),
                api_key_env: None,
                base_url: None,
                context_window: 128000,
                max_tokens: 8192,
                temperature: None,
            },
            10,
            5000,
            20,
        );

        let context = loop_.build_mention_context("Hey @robin, what's the status?");
        assert!(context.contains("Context from @robin"));
        assert!(context.contains("Robin's task"));
    }

    #[test]
    fn test_mention_context_self_excluded() {
        let db = setup_db();
        let pool = Arc::new(LLMClientPool::new(
            &crate::config::ModelRegistry { models: vec![] },
            &std::collections::HashMap::new(),
        ));

        let loop_ = AgentLoop::new(
            "ino".into(),
            "You are ino.".into(),
            pool,
            vec![],
            Arc::new(ToolRegistry::new()),
            db,
            ModelConfig {
                id: "test".into(),
                provider: crate::config::Provider::Nvidia,
                model: "test".into(),
                api_key_env: None,
                base_url: None,
                context_window: 128000,
                max_tokens: 8192,
                temperature: None,
            },
            10,
            5000,
            20,
        );

        let context = loop_.build_mention_context("@ino do something");
        assert!(context.is_empty());
    }

    #[test]
    fn test_mention_context_unknown_agent() {
        let db = setup_db();
        let pool = Arc::new(LLMClientPool::new(
            &crate::config::ModelRegistry { models: vec![] },
            &std::collections::HashMap::new(),
        ));

        let loop_ = AgentLoop::new(
            "ino".into(),
            "You are ino.".into(),
            pool,
            vec![],
            Arc::new(ToolRegistry::new()),
            db,
            ModelConfig {
                id: "test".into(),
                provider: crate::config::Provider::Nvidia,
                model: "test".into(),
                api_key_env: None,
                base_url: None,
                context_window: 128000,
                max_tokens: 8192,
                temperature: None,
            },
            10,
            5000,
            20,
        );

        let context = loop_.build_mention_context("@unknown_agent hello");
        assert!(context.is_empty());
    }

    #[test]
    fn test_parent_context() {
        let db = setup_db();
        db.save_message("parent-conv", "ino", "user", "original question", "cli", None, 0, 0, "t1", true, None).unwrap();
        db.save_message("parent-conv", "ino", "assistant", "original answer", "cli", None, 0, 0, "t1", true, None).unwrap();

        let pool = Arc::new(LLMClientPool::new(
            &crate::config::ModelRegistry { models: vec![] },
            &std::collections::HashMap::new(),
        ));

        let loop_ = AgentLoop::new(
            "ino".into(),
            "You are ino.".into(),
            pool,
            vec![],
            Arc::new(ToolRegistry::new()),
            db,
            ModelConfig {
                id: "test".into(),
                provider: crate::config::Provider::Nvidia,
                model: "test".into(),
                api_key_env: None,
                base_url: None,
                context_window: 128000,
                max_tokens: 8192,
                temperature: None,
            },
            10,
            5000,
            20,
        );

        let context = loop_.build_parent_context("parent-conv");
        assert!(context.contains("Previous Conversation Context"));
        assert!(context.contains("original question"));
        assert!(context.contains("original answer"));
    }

    #[test]
    fn test_parent_context_empty() {
        let db = setup_db();
        let pool = Arc::new(LLMClientPool::new(
            &crate::config::ModelRegistry { models: vec![] },
            &std::collections::HashMap::new(),
        ));

        let loop_ = AgentLoop::new(
            "ino".into(),
            "You are ino.".into(),
            pool,
            vec![],
            Arc::new(ToolRegistry::new()),
            db,
            ModelConfig {
                id: "test".into(),
                provider: crate::config::Provider::Nvidia,
                model: "test".into(),
                api_key_env: None,
                base_url: None,
                context_window: 128000,
                max_tokens: 8192,
                temperature: None,
            },
            10,
            5000,
            20,
        );

        let context = loop_.build_parent_context("nonexistent-conv");
        assert!(context.is_empty());
    }
}
