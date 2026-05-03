use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::ModelConfig;
use crate::db::Database;
use crate::llm::{truncate_history, LLMClientPool, LLMMessage};
use crate::skills::{self, CoreSkills};
use crate::tools::ToolRegistry;

/// Format a `## Project Context` system-prompt block from
/// `mcp_akw__group_start`'s `recommended_context` field. Empty input → empty
/// string (caller skips emission).
fn format_project_context(items: &[String]) -> String {
    let pieces: Vec<&str> = items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if pieces.is_empty() {
        return String::new();
    }
    format!("## Project Context\n\n{}", pieces.join("\n\n---\n\n"))
}

/// Format a `## User Preferences` system-prompt block from per-pref bodies.
fn format_user_preferences(items: &[String]) -> String {
    let pieces: Vec<&str> = items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if pieces.is_empty() {
        return String::new();
    }
    format!("## User Preferences\n\n{}", pieces.join("\n\n---\n\n"))
}

/// Format a `## Prior Work` system-prompt block from per-hit content.
fn format_prior_work(items: &[String]) -> String {
    let pieces: Vec<&str> = items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if pieces.is_empty() {
        return String::new();
    }
    format!("## Prior Work\n\n{}", pieces.join("\n\n---\n\n"))
}

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
    // ---------- EP-00015 ----------
    prefs_pool_dir: PathBuf,
    prefs_min_match_hits: u32,
    prefs_token_budget: u32,
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
        prefs_pool_dir: PathBuf,
        prefs_min_match_hits: u32,
        prefs_token_budget: u32,
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
            prefs_pool_dir,
            prefs_min_match_hits,
            prefs_token_budget,
        }
    }

    /// Local preference pool directory (EP-00015 Decision A).
    pub fn prefs_pool_dir(&self) -> &Path {
        &self.prefs_pool_dir
    }

    pub fn prefs_min_match_hits(&self) -> u32 {
        self.prefs_min_match_hits
    }

    pub fn prefs_token_budget(&self) -> u32 {
        self.prefs_token_budget
    }

    /// Borrow the tool registry for harness-side helpers (prior-work search,
    /// reflection writes, etc.). Per-agent registry — DO NOT share across
    /// agents.
    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Borrow the shared SQLite database. Used by harness-side helpers
    /// (manual-save trigger, session-draft producer, reflection retrieval).
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Single-shot LLM call without tool-loop / DB persistence. Used by
    /// harness-owned helpers (research-draft summarization, session-draft
    /// summarization, reflection pattern detection) to get a cheap synchronous
    /// response. Returns the response content on success, or a failure-prefix
    /// string on error so callers can detect failures via the same string-match
    /// convention as `agent_loop.run` (`"I'm sorry, all models failed"` /
    /// `"LLM call failed"`).
    pub async fn cheap_call(&self, system: &str, user: &str) -> String {
        let messages = vec![LLMMessage::user(user)];
        match self
            .pool
            .chat_with_fallback(&self.fallback_chain, &messages, Some(system), None)
            .await
        {
            Ok(resp) => resp.content,
            Err(e) => format!("LLM call failed: {}", e),
        }
    }

    /// Main entry point for the agent reasoning loop.
    ///
    /// `recommended_context` is the project-keyed standing context returned by
    /// `mcp_akw__group_start` (per Decision D of EP-00015). Empty slice → omit
    /// the `## Project Context` block entirely.
    ///
    /// `selected_preferences` holds the per-segment cached preference
    /// selection (EP-00015 Decision A). Each entry is a fully-formatted
    /// preference body (e.g. "### slug (scope: x)\n\n…"). Empty slice → omit
    /// the `## User Preferences` block.
    ///
    /// `prior_work` holds the per-segment cached prior-work hits from
    /// `mcp_akw__memory_search` (EP-00015 Decision B). Each entry is one
    /// AKW page's content prefixed by its path. Empty slice → omit the
    /// `## Prior Work` block.
    ///
    /// `previous_run_result` is for recurring tasks (Decision C) — the
    /// previous completion's `result` field, already formatted as the
    /// `## Previous Run Result` block (or empty string if not applicable).
    pub async fn run(
        &self,
        message: &str,
        conversation_id: &str,
        channel_type: &str,
        parent_id: Option<&str>,
        recommended_context: &[String],
        selected_preferences: &[String],
        prior_work: &[String],
        previous_run_result: &str,
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
        let system = self.build_system_prompt(
            message,
            conversation_id,
            parent_id,
            &dynamic_skills,
            recommended_context,
            selected_preferences,
            prior_work,
            previous_run_result,
        );

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

    /// Build the agent's system prompt.
    ///
    /// Block order is pinned by Decision J2 of EP-00015:
    ///   1. Character sheet
    ///   2. ## User Preferences           (added in EP-00015 Phase 2)
    ///   3. Core skills
    ///   4. Equipped / AKW-fallback skills
    ///   5. ## Project Context            (recommended_context — Decision D)
    ///   6. ## Prior Work                 (added in EP-00015 Phase 3)
    ///   7. ## Previous Run Result        (added in EP-00015 Phase 3, task channel only)
    ///   8. Cross-agent @mention context
    ///   9. Parent conversation context
    ///   10. (User message — appended by the LLM client, not here)
    ///
    /// Empty blocks are omitted entirely (header + content). We never emit an
    /// empty heading — that would just train the LLM to ignore the heading style.
    fn build_system_prompt(
        &self,
        message: &str,
        conversation_id: &str,
        parent_id: Option<&str>,
        dynamic_skills: &str,
        recommended_context: &[String],
        selected_preferences: &[String],
        prior_work: &[String],
        previous_run_result: &str,
    ) -> String {
        let mut prompt = self.character_sheet.clone();

        // 2. User Preferences (EP-00015 Decision A — selected per task/segment).
        let user_prefs = format_user_preferences(selected_preferences);
        if !user_prefs.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&user_prefs);
        }

        // 3. Core skills (from config/skills/*.md) — always injected when non-empty.
        let skills_section = self.core_skills.format_for_prompt();
        if !skills_section.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&skills_section);
        }

        // 4. Dynamic skills: either "## Equipped Skills" (from local pool) or
        //    "## AKW Skills" (AKW fallback). The header is included by the formatter.
        if !dynamic_skills.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(dynamic_skills);
        }

        // 5. Project Context (recommended_context from AKW group_start).
        let project_context = format_project_context(recommended_context);
        if !project_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&project_context);
        }

        // 6. Prior Work (EP-00015 Decision B — message-aware AKW retrieval).
        let prior_work_block = format_prior_work(prior_work);
        if !prior_work_block.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&prior_work_block);
        }

        // 7. Previous Run Result (EP-00015 Decision C — recurring tasks).
        let prev_run = previous_run_result.trim();
        if !prev_run.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(prev_run);
        }

        // 8. Cross-agent @mention context
        let mention_context = self.build_mention_context(message);
        if !mention_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&mention_context);
        }

        // 9. Parent conversation context
        if let Some(pid) = parent_id {
            let parent_context = self.build_parent_context(pid);
            if !parent_context.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&parent_context);
            }
        } else if let Ok(Some(pid)) = self.db.get_parent_id(conversation_id) {
            // Check if the first message in this conv has a parent_id.
            let parent_context = self.build_parent_context(&pid);
            if !parent_context.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&parent_context);
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
            PathBuf::from("/nonexistent/_preferences"),
            2,
            4000,
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
            PathBuf::from("/nonexistent/_preferences"),
            2,
            4000,
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
            PathBuf::from("/nonexistent/_preferences"),
            2,
            4000,
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
            PathBuf::from("/nonexistent/_preferences"),
            2,
            4000,
        );

        let context = loop_.build_parent_context("parent-conv");
        assert!(context.contains("Previous Conversation Context"));
        assert!(context.contains("original question"));
        assert!(context.contains("original answer"));
    }

    // ---------- Phase 1 (EP-00015) tests ----------

    #[test]
    fn test_format_project_context_empty() {
        assert!(format_project_context(&[]).is_empty());
        assert!(format_project_context(&["".into(), "  ".into()]).is_empty());
    }

    #[test]
    fn test_format_project_context_single() {
        let out = format_project_context(&["alpha content".into()]);
        assert!(out.starts_with("## Project Context"));
        assert!(out.contains("alpha content"));
        assert!(!out.contains("---"));
    }

    #[test]
    fn test_format_project_context_multiple() {
        let out = format_project_context(&["alpha".into(), "beta".into()]);
        assert!(out.starts_with("## Project Context"));
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
        assert!(out.contains("---"));
    }

    fn build_loop_for_prompt_test(db: Arc<Database>, core: CoreSkills) -> AgentLoop {
        let pool = Arc::new(LLMClientPool::new(
            &crate::config::ModelRegistry { models: vec![] },
            &std::collections::HashMap::new(),
        ));
        AgentLoop::new(
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
            core,
            false,
            PathBuf::from("/nonexistent/_skills"),
            4000,
            2,
            PathBuf::from("/nonexistent/_preferences"),
            2,
            4000,
        )
    }

    #[test]
    fn test_build_system_prompt_omits_empty_project_context() {
        let db = setup_db();
        let core = CoreSkills {
            content: String::new(),
            count: 0,
            token_estimate: 0,
        };
        let loop_ = build_loop_for_prompt_test(db, core);

        let prompt = loop_.build_system_prompt("hello", "conv-1", None, "", &[], &[], &[], "");
        assert!(prompt.contains("You are ino."));
        assert!(!prompt.contains("## Project Context"));
        assert!(!prompt.contains("## User Preferences"));
    }

    #[test]
    fn test_build_system_prompt_includes_project_context() {
        let db = setup_db();
        let core = CoreSkills {
            content: String::new(),
            count: 0,
            token_estimate: 0,
        };
        let loop_ = build_loop_for_prompt_test(db, core);

        let ctx = vec!["nvda price playbook".to_string()];
        let prompt = loop_.build_system_prompt("hello", "conv-1", None, "", &ctx, &[], &[], "");
        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("nvda price playbook"));
    }

    #[test]
    fn test_build_system_prompt_includes_user_preferences() {
        let db = setup_db();
        let core = CoreSkills {
            content: String::new(),
            count: 0,
            token_estimate: 0,
        };
        let loop_ = build_loop_for_prompt_test(db, core);

        let prefs = vec!["### git_style (scope: git)\n\nUse imperative mood.".to_string()];
        let prompt = loop_.build_system_prompt("hello", "conv-1", None, "", &[], &prefs, &[], "");
        assert!(prompt.contains("## User Preferences"));
        assert!(prompt.contains("Use imperative mood"));
    }

    /// Decision J2 — block order is pinned. Asserts that with all blocks
    /// non-empty, the rendered prompt has them in the right order. Catches
    /// drift in later phases.
    #[test]
    fn test_build_system_prompt_block_order() {
        let db = setup_db();
        // Set up parent context too
        db.save_message(
            "parent-conv", "ino", "user", "earlier question",
            "cli", None, 0, 0, "t1", true, None,
        ).unwrap();
        // Mention agent setup
        db.save_message(
            "robin-conv", "robin", "assistant", "robin earlier",
            "cli", None, 0, 0, "t2", true, None,
        ).unwrap();

        let core = CoreSkills {
            content: "Do good work.".into(),
            count: 1,
            token_estimate: 5,
        };
        let loop_ = build_loop_for_prompt_test(db, core);

        let ctx = vec!["project-level pref".to_string()];
        let prefs = vec!["### my_pref (scope: x)\n\npref body".to_string()];
        let prior = vec!["### a/b/c.md\n\nprior research".to_string()];
        let prev_run = "## Previous Run Result\n\nyesterday's result";
        let dynamic = "## Equipped Skills\n\nEquipped body";
        let prompt = loop_.build_system_prompt(
            "hey @robin do something",
            "conv-x",
            Some("parent-conv"),
            dynamic,
            &ctx,
            &prefs,
            &prior,
            prev_run,
        );

        // Find the index of each section's heading.
        let charsheet_pos = prompt.find("You are ino.").expect("character sheet");
        let user_prefs_pos = prompt.find("## User Preferences").expect("user prefs");
        let core_pos = prompt.find("## Core Skills").expect("core skills");
        let equipped_pos = prompt.find("## Equipped Skills").expect("equipped");
        let project_pos = prompt.find("## Project Context").expect("project");
        let prior_pos = prompt.find("## Prior Work").expect("prior work");
        let prev_pos = prompt.find("## Previous Run Result").expect("previous run");
        let mention_pos = prompt.find("## Context from @robin").expect("mention");
        let parent_pos = prompt.find("## Previous Conversation Context").expect("parent");

        // Decision J2 order: charsheet < user_prefs < core < equipped <
        //                   project < prior < prev_run < mention < parent.
        assert!(charsheet_pos < user_prefs_pos, "charsheet must precede user preferences");
        assert!(user_prefs_pos < core_pos, "user preferences must precede core skills");
        assert!(core_pos < equipped_pos, "core must precede equipped");
        assert!(equipped_pos < project_pos, "equipped must precede project context");
        assert!(project_pos < prior_pos, "project context must precede prior work");
        assert!(prior_pos < prev_pos, "prior work must precede previous run");
        assert!(prev_pos < mention_pos, "previous run must precede mention");
        assert!(mention_pos < parent_pos, "mention must precede parent context");
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
            PathBuf::from("/nonexistent/_preferences"),
            2,
            4000,
        );

        let context = loop_.build_parent_context("nonexistent-conv");
        assert!(context.is_empty());
    }
}
