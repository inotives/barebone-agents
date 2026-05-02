use regex::Regex;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::ModelConfig;
use crate::db::Database;
use crate::llm::{truncate_history, LLMClientPool, LLMMessage};
use crate::skills::{self, CoreSkills};
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
    core_skills: CoreSkills,
    akw_skills_enabled: bool,
    equipped_skills_pool_dir: PathBuf,
    equipped_skills_token_budget: u32,
    equipped_skills_min_match_hits: u32,
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
        core_skills: CoreSkills,
        akw_skills_enabled: bool,
        equipped_skills_pool_dir: PathBuf,
        equipped_skills_token_budget: u32,
        equipped_skills_min_match_hits: u32,
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
            core_skills,
            akw_skills_enabled,
            equipped_skills_pool_dir,
            equipped_skills_token_budget,
            equipped_skills_min_match_hits,
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

        // Pick task-relevant skills: local pool first, AKW only if local returns nothing.
        let dynamic_skills = self.fetch_dynamic_skills(message).await;

        // Build system prompt
        let system = self.build_system_prompt(message, conversation_id, parent_id, &dynamic_skills);

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
        dynamic_skills: &str,
    ) -> String {
        let mut prompt = self.character_sheet.clone();

        // Core skills (from config/skills/*.md) — always injected.
        let skills_section = self.core_skills.format_for_prompt();
        if !skills_section.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&skills_section);
        }

        // Dynamic skills: either "## Equipped Skills" (from local pool) or
        // "## AKW Skills" (AKW fallback). The header is included by the formatter.
        if !dynamic_skills.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(dynamic_skills);
        }

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

    /// Pick task-relevant skills for system-prompt injection.
    ///
    /// Resolution order:
    /// 1. Local `agents/_skills/*.md` pool — keyword + body match against the message,
    ///    filtered by `min_match_hits`, packed into `token_budget`. Returns formatted
    ///    `## Equipped Skills` block on success.
    /// 2. AKW `skill_search` fallback — only if the local pool returned no matches.
    ///    Returns formatted `## AKW Skills` block on success.
    ///
    /// Returns empty string when neither resolves.
    async fn fetch_dynamic_skills(&self, message: &str) -> String {
        // 1. Local pool first
        let pool = skills::load_equipped_pool(&self.equipped_skills_pool_dir);
        if !pool.is_empty() {
            let chosen = skills::select_equipped_skills(
                &pool,
                message,
                self.equipped_skills_min_match_hits,
                self.equipped_skills_token_budget,
            );
            if !chosen.is_empty() {
                debug!(
                    agent = %self.agent_name,
                    count = chosen.len(),
                    slugs = ?chosen.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>(),
                    "equipped skills picked from local pool"
                );
                return skills::format_equipped_skills(&chosen);
            }
        }

        // 2. AKW fallback
        self.fetch_akw_skills(message).await
    }

    async fn fetch_akw_skills(&self, message: &str) -> String {
        if !self.akw_skills_enabled || !self.registry.has("mcp_akw__skill_search") {
            return String::new();
        }

        // Search for skills matching the task/message. skill_search is tier-scoped
        // by definition (only walks 3_intelligences/skills/**/SKILL.md).
        let search_result = self
            .registry
            .execute(
                "mcp_akw__skill_search",
                serde_json::json!({
                    "query": message,
                }),
            )
            .await;

        debug!(
            agent = %self.agent_name,
            result_len = search_result.len(),
            "AKW skill_search response"
        );

        let paths: Vec<String> = match serde_json::from_str::<Value>(&search_result) {
            Ok(json) => {
                // Handle multiple response formats:
                // 1. {"result": [{...}, ...]} — wrapped array
                // 2. [{...}, ...] — bare array
                // 3. {"path": ...} — single object
                let items = if let Some(arr) = json.get("result").and_then(|r| r.as_array()) {
                    arr.clone()
                } else if let Some(arr) = json.as_array() {
                    arr.clone()
                } else if json.get("path").is_some() {
                    vec![json]
                } else {
                    Vec::new()
                };

                items
                    .iter()
                    .take(3)
                    .filter_map(|item| {
                        item.get("path").and_then(|p| p.as_str()).map(String::from)
                    })
                    .collect()
            }
            Err(e) => {
                debug!(agent = %self.agent_name, error = %e, "failed to parse AKW search result");
                return String::new();
            }
        };

        debug!(agent = %self.agent_name, paths = ?paths, "AKW skill paths found");

        if paths.is_empty() {
            return String::new();
        }

        // Read each skill's full content
        let mut skills = Vec::new();
        for path in &paths {
            let read_result = self
                .registry
                .execute(
                    "mcp_akw__memory_read",
                    serde_json::json!({"path": path}),
                )
                .await;

            if let Ok(json) = serde_json::from_str::<Value>(&read_result) {
                if let Some(content) = json.get("content").and_then(|c| c.as_str()) {
                    skills.push(content.to_string());
                }
            } else if !read_result.is_empty() {
                // Fallback: the result itself might be the content as plain text
                skills.push(read_result);
            }
        }

        debug!(
            agent = %self.agent_name,
            count = skills.len(),
            "AKW skills loaded for task"
        );

        if skills.is_empty() {
            String::new()
        } else {
            format!("## AKW Skills\n\n{}", skills.join("\n\n---\n\n"))
        }
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
            CoreSkills { content: String::new(), count: 0, token_estimate: 0 },
            false,
            PathBuf::from("/nonexistent/_skills"),
            4000,
            2,
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
            CoreSkills { content: String::new(), count: 0, token_estimate: 0 },
            false,
            PathBuf::from("/nonexistent/_skills"),
            4000,
            2,
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
            CoreSkills { content: String::new(), count: 0, token_estimate: 0 },
            false,
            PathBuf::from("/nonexistent/_skills"),
            4000,
            2,
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
            CoreSkills { content: String::new(), count: 0, token_estimate: 0 },
            false,
            PathBuf::from("/nonexistent/_skills"),
            4000,
            2,
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
            CoreSkills { content: String::new(), count: 0, token_estimate: 0 },
            false,
            PathBuf::from("/nonexistent/_skills"),
            4000,
            2,
        );

        let context = loop_.build_parent_context("nonexistent-conv");
        assert!(context.is_empty());
    }
}
