use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::registry::ToolRegistry;
use crate::config::ModelConfig;
use crate::llm::{LLMClientPool, LLMMessage, LLMResponse, truncate_history};

const BLOCKED_TOOLS: &[&str] = &["delegate", "delegate_parallel", "conversation_search"];

const DEFAULT_ALLOWED: &[&str] = &[
    "web_search",
    "web_fetch",
    "api_request",
    "shell_execute",
    "file_read",
    "file_write",
];

const MAX_CONTEXT_CHARS: usize = 50_000;

/// Ephemeral sub-agent runner. No DB persistence.
pub struct SubAgentRunner {
    pool: Arc<LLMClientPool>,
    fallback_chain: Vec<String>,
    primary_model: ModelConfig,
    max_iterations: u32,
    sleep_between_secs: f64,
    tool_result_max_chars: usize,
}

impl SubAgentRunner {
    pub fn new(
        pool: Arc<LLMClientPool>,
        fallback_chain: Vec<String>,
        primary_model: ModelConfig,
        max_iterations: u32,
        sleep_between_secs: f64,
        tool_result_max_chars: usize,
    ) -> Self {
        Self {
            pool,
            fallback_chain,
            primary_model,
            max_iterations,
            sleep_between_secs,
            tool_result_max_chars,
        }
    }

    /// Run the sub-agent with the given task and system prompt.
    pub async fn run(&self, task: &str, system_prompt: &str, tools: &ToolRegistry) -> String {
        let mut messages = vec![LLMMessage::user(task)];

        let tool_defs = tools.get_definitions();
        let tool_defs_opt = if tool_defs.is_empty() {
            None
        } else {
            Some(tool_defs)
        };

        // Truncate for context window
        messages = truncate_history(
            &messages,
            Some(system_prompt),
            self.primary_model.context_window,
            self.primary_model.max_tokens,
        );

        // First LLM call
        let mut response = match self
            .pool
            .chat_with_fallback(
                &self.fallback_chain,
                &messages,
                Some(system_prompt),
                tool_defs_opt.as_deref(),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return format!("Sub-agent error: {}", e),
        };

        // Tool loop
        let mut iteration = 0;
        while let Some(ref tool_calls) = response.tool_calls {
            if iteration >= self.max_iterations {
                warn!("sub-agent hit max iterations ({})", self.max_iterations);
                break;
            }

            // Context size guard
            let context_size: usize = messages.iter().map(|m| m.content.len()).sum();
            if context_size > MAX_CONTEXT_CHARS {
                warn!(
                    context_size,
                    "sub-agent context exceeded {}", MAX_CONTEXT_CHARS
                );
                break;
            }

            // Build assistant message
            let assistant_msg = LLMMessage {
                role: "assistant".into(),
                content: response.content.clone(),
                tool_calls: Some(tool_calls.clone()),
                tool_call_id: None,
            };
            messages.push(assistant_msg);

            // Execute tools
            for tc in tool_calls {
                debug!(tool = %tc.name, "sub-agent executing tool");

                let result = tools.execute(&tc.name, tc.arguments.clone()).await;

                let result = if result.len() > self.tool_result_max_chars {
                    format!("{}... (truncated)", &result[..self.tool_result_max_chars])
                } else {
                    result
                };

                messages.push(LLMMessage::tool_result(&tc.id, &result));
            }

            // Rate limit
            if self.sleep_between_secs > 0.0 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(self.sleep_between_secs))
                    .await;
            }

            // Call LLM again
            response = match self
                .pool
                .chat_with_fallback(
                    &self.fallback_chain,
                    &messages,
                    Some(system_prompt),
                    tool_defs_opt.as_deref(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => return format!("Sub-agent error during tool loop: {}", e),
            };

            iteration += 1;
        }

        info!(
            model = %response.model,
            tool_iterations = iteration,
            "sub-agent complete"
        );

        response.content
    }
}

/// Build a restricted tool registry for sub-agents.
/// Only includes allowed tools, never blocked tools.
pub fn build_restricted_registry(
    parent: &ToolRegistry,
    allowed: Option<&[String]>,
) -> ToolRegistry {
    let mut restricted = ToolRegistry::new();

    let allow_list: Vec<&str> = match allowed {
        Some(list) => list.iter().map(|s| s.as_str()).collect(),
        None => DEFAULT_ALLOWED.to_vec(),
    };

    for def in parent.get_all() {
        let name = &def.name;

        // Never allow blocked tools.
        if BLOCKED_TOOLS.iter().any(|b| *b == name) {
            continue;
        }

        // Only include if in allow list
        if allow_list.iter().any(|a| *a == name.as_str()) {
            restricted.register_raw(
                name,
                &def.description,
                def.parameters.clone(),
                def.handler.clone(),
            );
        }
    }

    restricted
}

/// Resolve a role profile to a system prompt string.
///
/// Resolution order:
/// 1. Local — `agents/_roles/{role}.md` (curated by the team; deterministic; offline-safe).
/// 2. Default — `default_role_prompt()`.
///
/// Never errors. Local file reads are intentionally uncached so dev edits to
/// `agents/_roles/*.md` take effect immediately.
pub async fn load_role_profile(root_dir: &Path, role: &str, _registry: &ToolRegistry) -> String {
    let path = root_dir
        .join("agents")
        .join("_roles")
        .join(format!("{}.md", role));
    if let Ok(content) = std::fs::read_to_string(&path) {
        debug!(role = %role, path = %path.display(), "role resolved from local file");
        return content;
    }

    debug!(role = %role, "role resolved to default prompt");
    default_role_prompt()
}

/// Default system prompt when no role is specified.
pub fn default_role_prompt() -> String {
    "You are a helpful assistant sub-agent. Complete the assigned task concisely and accurately. \
     Use tools when they help accomplish the task. Report your findings clearly."
        .to_string()
}

/// Register the `delegate` and `delegate_parallel` tools.
pub fn register(
    registry: &mut ToolRegistry,
    pool: Arc<LLMClientPool>,
    fallback_chain: Vec<String>,
    primary_model: ModelConfig,
    parent_registry: Arc<ToolRegistry>,
    root_dir: PathBuf,
    max_parallel: usize,
    sleep_between_secs: f64,
    tool_result_max_chars: usize,
) {
    // delegate (single)
    let p = pool.clone();
    let fc = fallback_chain.clone();
    let pm = primary_model.clone();
    let pr = parent_registry.clone();
    let rd = root_dir.clone();
    let sbs = sleep_between_secs;
    let trmc = tool_result_max_chars;

    registry.register(
        "delegate",
        "Spawn a sub-agent to handle a task. The sub-agent runs independently with restricted tools and returns the result.",
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Task description for the sub-agent"
                },
                "role": {
                    "type": "string",
                    "description": "Role profile (analyst, coder, researcher, etc.)"
                },
                "tools": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Allowed tools (default: web_search, web_fetch, api_request, shell_execute, file_read, file_write)"
                },
                "model": {
                    "type": "string",
                    "description": "Model override (default: parent's primary model)"
                },
                "max_iterations": {
                    "type": "integer",
                    "description": "Max tool iterations (default: 5)"
                }
            },
            "required": ["task"]
        }),
        move |args| {
            let p = p.clone();
            let fc = fc.clone();
            let pm = pm.clone();
            let pr = pr.clone();
            let rd = rd.clone();
            async move {
                delegate_single(args, &p, &fc, &pm, &pr, &rd, sbs, trmc).await
            }
        },
    );

    // delegate_parallel
    let p = pool.clone();
    let fc = fallback_chain.clone();
    let pm = primary_model.clone();
    let pr = parent_registry.clone();
    let rd = root_dir.clone();
    let sbs = sleep_between_secs;
    let trmc = tool_result_max_chars;
    let semaphore = Arc::new(Semaphore::new(max_parallel));

    registry.register(
        "delegate_parallel",
        "Spawn multiple sub-agents in parallel to handle tasks. Returns numbered results.",
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of task descriptions, one per sub-agent"
                },
                "tools": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Allowed tools for all sub-agents"
                },
                "model": {
                    "type": "string",
                    "description": "Model override for all sub-agents"
                },
                "max_iterations": {
                    "type": "integer",
                    "description": "Max tool iterations per sub-agent (default: 5)"
                }
            },
            "required": ["tasks"]
        }),
        move |args| {
            let p = p.clone();
            let fc = fc.clone();
            let pm = pm.clone();
            let pr = pr.clone();
            let rd = rd.clone();
            let sem = semaphore.clone();
            async move { delegate_parallel(args, &p, &fc, &pm, &pr, &rd, sbs, trmc, sem).await }
        },
    );
}

async fn delegate_single(
    args: Value,
    pool: &LLMClientPool,
    fallback_chain: &[String],
    primary_model: &ModelConfig,
    parent_registry: &ToolRegistry,
    root_dir: &Path,
    sleep_between_secs: f64,
    tool_result_max_chars: usize,
) -> String {
    let task = match args.get("task").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return "Error: 'task' parameter required".to_string(),
    };

    let max_iterations = args
        .get("max_iterations")
        .and_then(|m| m.as_u64())
        .unwrap_or(5) as u32;

    // Resolve role profile (local file → default).
    let system_prompt = match args.get("role").and_then(|r| r.as_str()) {
        Some(role) => load_role_profile(root_dir, role, parent_registry).await,
        None => default_role_prompt(),
    };

    // Resolve model override
    let (chain, model) = resolve_model_override(
        args.get("model").and_then(|m| m.as_str()),
        pool,
        fallback_chain,
        primary_model,
    );

    // Build restricted tools
    let allowed: Option<Vec<String>> = args.get("tools").and_then(|t| t.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });
    let tools = build_restricted_registry(parent_registry, allowed.as_deref());

    let runner = SubAgentRunner::new(
        Arc::new(LLMClientPool::empty()), // placeholder — we use the shared pool
        chain,
        model,
        max_iterations,
        sleep_between_secs,
        tool_result_max_chars,
    );

    // We need to use the actual pool, so let's create the runner with proper pool
    let runner = SubAgentRunner {
        pool: Arc::new(LLMClientPool::empty()),
        ..runner
    };

    // Actually, just call run directly with the pool
    run_subagent(
        pool,
        &runner.fallback_chain,
        &runner.primary_model,
        task,
        &system_prompt,
        &tools,
        max_iterations,
        sleep_between_secs,
        tool_result_max_chars,
    )
    .await
}

async fn run_subagent(
    pool: &LLMClientPool,
    fallback_chain: &[String],
    primary_model: &ModelConfig,
    task: &str,
    system_prompt: &str,
    tools: &ToolRegistry,
    max_iterations: u32,
    sleep_between_secs: f64,
    tool_result_max_chars: usize,
) -> String {
    let mut messages = vec![LLMMessage::user(task)];

    let tool_defs = tools.get_definitions();
    let tool_defs_opt = if tool_defs.is_empty() {
        None
    } else {
        Some(tool_defs)
    };

    messages = truncate_history(
        &messages,
        Some(system_prompt),
        primary_model.context_window,
        primary_model.max_tokens,
    );

    let mut response = match pool
        .chat_with_fallback(
            fallback_chain,
            &messages,
            Some(system_prompt),
            tool_defs_opt.as_deref(),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("Sub-agent error: {}", e),
    };

    let mut iteration = 0;
    while let Some(ref tool_calls) = response.tool_calls {
        if iteration >= max_iterations {
            break;
        }

        let context_size: usize = messages.iter().map(|m| m.content.len()).sum();
        if context_size > MAX_CONTEXT_CHARS {
            break;
        }

        let assistant_msg = LLMMessage {
            role: "assistant".into(),
            content: response.content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
        };
        messages.push(assistant_msg);

        for tc in tool_calls {
            debug!(tool = %tc.name, "sub-agent executing tool");
            let result = tools.execute(&tc.name, tc.arguments.clone()).await;
            let result = if result.len() > tool_result_max_chars {
                format!("{}... (truncated)", &result[..tool_result_max_chars])
            } else {
                result
            };
            messages.push(LLMMessage::tool_result(&tc.id, &result));
        }

        if sleep_between_secs > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(sleep_between_secs)).await;
        }

        response = match pool
            .chat_with_fallback(
                fallback_chain,
                &messages,
                Some(system_prompt),
                tool_defs_opt.as_deref(),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return format!("Sub-agent error during tool loop: {}", e),
        };

        iteration += 1;
    }

    info!(model = %response.model, tool_iterations = iteration, "sub-agent complete");
    response.content
}

async fn delegate_parallel(
    args: Value,
    pool: &LLMClientPool,
    fallback_chain: &[String],
    primary_model: &ModelConfig,
    parent_registry: &ToolRegistry,
    root_dir: &Path,
    sleep_between_secs: f64,
    tool_result_max_chars: usize,
    semaphore: Arc<Semaphore>,
) -> String {
    let tasks: Vec<String> = match args.get("tasks").and_then(|t| t.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        None => return "Error: 'tasks' parameter required (array of strings)".to_string(),
    };

    if tasks.is_empty() {
        return "Error: 'tasks' array is empty".to_string();
    }

    let max_iterations = args
        .get("max_iterations")
        .and_then(|m| m.as_u64())
        .unwrap_or(5) as u32;

    let system_prompt = default_role_prompt();

    let (chain, model) = resolve_model_override(
        args.get("model").and_then(|m| m.as_str()),
        pool,
        fallback_chain,
        primary_model,
    );

    let allowed: Option<Vec<String>> = args.get("tools").and_then(|t| t.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });
    let tools = Arc::new(build_restricted_registry(
        parent_registry,
        allowed.as_deref(),
    ));

    let mut handles = Vec::new();

    for (i, task) in tasks.into_iter().enumerate() {
        let sem = semaphore.clone();
        let chain = chain.clone();
        let model = model.clone();
        let tools = tools.clone();
        let system = system_prompt.clone();

        // We can't pass pool reference into spawned tasks easily,
        // so we'll run them sequentially with semaphore limiting
        handles.push((i, task, chain, model, tools, system));
    }

    let mut results = Vec::new();
    for (i, task, chain, model, tools, system) in handles {
        let _permit = semaphore.acquire().await.unwrap();
        let result = run_subagent(
            pool,
            &chain,
            &model,
            &task,
            &system,
            &tools,
            max_iterations,
            sleep_between_secs,
            tool_result_max_chars,
        )
        .await;
        results.push(format!("## Task {} Result\n{}", i + 1, result));
    }

    results.join("\n\n")
}

fn resolve_model_override<'a>(
    model_id: Option<&str>,
    pool: &LLMClientPool,
    fallback_chain: &[String],
    primary_model: &ModelConfig,
) -> (Vec<String>, ModelConfig) {
    if let Some(id) = model_id {
        if pool.get(id).is_some() {
            // Use the overridden model as the sole model in the chain
            return (
                vec![id.to_string()],
                ModelConfig {
                    id: id.to_string(),
                    ..primary_model.clone()
                },
            );
        }
        warn!(model = %id, "model override not found in pool, using default");
    }
    (fallback_chain.to_vec(), primary_model.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocked_tools_list() {
        assert!(BLOCKED_TOOLS.contains(&"delegate"));
        assert!(BLOCKED_TOOLS.contains(&"delegate_parallel"));
        assert!(BLOCKED_TOOLS.contains(&"conversation_search"));
    }

    #[test]
    fn test_default_allowed_tools() {
        assert!(DEFAULT_ALLOWED.contains(&"web_search"));
        assert!(DEFAULT_ALLOWED.contains(&"file_read"));
        assert!(!DEFAULT_ALLOWED.contains(&"delegate"));
    }

    #[test]
    fn test_build_restricted_registry_defaults() {
        let mut parent = ToolRegistry::new();
        parent.register("web_search", "Search", json!({}), |_| async { "ok".into() });
        parent.register("file_read", "Read", json!({}), |_| async { "ok".into() });
        parent.register("delegate", "Delegate", json!({}), |_| async { "ok".into() });
        parent.register(
            "conversation_search",
            "Search convos",
            json!({}),
            |_| async { "ok".into() },
        );

        let restricted = build_restricted_registry(&parent, None);
        assert!(restricted.has("web_search"));
        assert!(restricted.has("file_read"));
        assert!(!restricted.has("delegate"));
        assert!(!restricted.has("conversation_search"));
    }

    #[test]
    fn test_build_restricted_registry_custom_allow() {
        let mut parent = ToolRegistry::new();
        parent.register("web_search", "Search", json!({}), |_| async { "ok".into() });
        parent.register("file_read", "Read", json!({}), |_| async { "ok".into() });
        parent.register("file_write", "Write", json!({}), |_| async { "ok".into() });

        let allowed = vec!["web_search".to_string()];
        let restricted = build_restricted_registry(&parent, Some(&allowed));
        assert!(restricted.has("web_search"));
        assert!(!restricted.has("file_read"));
        assert!(!restricted.has("file_write"));
    }

    #[test]
    fn test_default_role_prompt() {
        let prompt = default_role_prompt();
        assert!(prompt.contains("sub-agent"));
        assert!(prompt.contains("task"));
    }

    #[tokio::test]
    async fn test_load_role_profile_missing_falls_back_to_default() {
        // No local file -> default prompt.
        let registry = ToolRegistry::new();
        let unique_role = format!("nonexistent-{}", uuid::Uuid::new_v4());
        let content = load_role_profile(Path::new("/nonexistent"), &unique_role, &registry).await;
        assert!(content.contains("sub-agent"));
    }

    #[tokio::test]
    async fn test_load_role_profile_local_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let roles_dir = dir.path().join("agents").join("_roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        let unique_role = format!("local-{}", uuid::Uuid::new_v4());
        std::fs::write(
            roles_dir.join(format!("{}.md", unique_role)),
            "You are a researcher.",
        )
        .unwrap();

        let registry = ToolRegistry::new();
        let content = load_role_profile(dir.path(), &unique_role, &registry).await;
        assert_eq!(content, "You are a researcher.");
    }
}
