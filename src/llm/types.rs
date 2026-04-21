use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct LLMMessage {
    pub role: String,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

impl LLMMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LLMResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub model: String,
    pub stop_reason: String,
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[async_trait]
pub trait LLMClient: Send + Sync {
    async fn chat(
        &self,
        messages: &[LLMMessage],
        system: Option<&str>,
        tools: Option<&[Value]>,
    ) -> Result<LLMResponse, String>;
}

/// Estimate token count using len/4 heuristic.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() / 4) as u32
}

/// Truncate message history to fit within context window.
/// Always keeps the most recent user message.
pub fn truncate_history(
    messages: &[LLMMessage],
    system: Option<&str>,
    context_window: u32,
    max_tokens: u32,
) -> Vec<LLMMessage> {
    let budget = context_window.saturating_sub(max_tokens);
    let system_tokens = system.map_or(0, |s| estimate_tokens(s));
    let message_budget = budget.saturating_sub(system_tokens);

    let mut result: Vec<LLMMessage> = messages.to_vec();

    // Remove oldest messages until we fit, but always keep the last user message
    while result.len() > 1 {
        let total: u32 = result.iter().map(|m| estimate_tokens(&m.content)).sum();
        if total <= message_budget {
            break;
        }
        // Find the last user message index to protect it
        let last_user_idx = result.iter().rposition(|m| m.role == "user");
        // Remove the oldest message that isn't the last user message
        let remove_idx = (0..result.len())
            .find(|&i| Some(i) != last_user_idx)
            .unwrap_or(0);
        result.remove(remove_idx);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("a]bc"), 1);
        // 400 chars → 100 tokens
        let text = "a".repeat(400);
        assert_eq!(estimate_tokens(&text), 100);
    }

    #[test]
    fn test_message_constructors() {
        let m = LLMMessage::user("hello");
        assert_eq!(m.role, "user");
        assert_eq!(m.content, "hello");
        assert!(m.tool_calls.is_none());

        let m = LLMMessage::assistant("hi");
        assert_eq!(m.role, "assistant");

        let m = LLMMessage::system("rules");
        assert_eq!(m.role, "system");

        let m = LLMMessage::tool_result("call-1", "result");
        assert_eq!(m.role, "tool");
        assert_eq!(m.tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn test_truncate_history_fits() {
        let msgs = vec![
            LLMMessage::user("short"),
            LLMMessage::assistant("reply"),
        ];
        let result = truncate_history(&msgs, None, 100000, 8192);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_truncate_history_removes_oldest() {
        // Create messages that exceed budget
        let long = "x".repeat(40000); // ~10000 tokens each
        let msgs = vec![
            LLMMessage::user(&long),
            LLMMessage::assistant(&long),
            LLMMessage::user("latest question"),
        ];
        // Budget: 20000 - 8192 = 11808 tokens for messages
        let result = truncate_history(&msgs, None, 20000, 8192);
        // Should keep the latest user message
        assert!(result.iter().any(|m| m.content == "latest question"));
        assert!(result.len() < 3);
    }

    #[test]
    fn test_truncate_history_preserves_last_user() {
        let long = "x".repeat(80000);
        let msgs = vec![
            LLMMessage::user(&long),
            LLMMessage::assistant(&long),
            LLMMessage::user("keep me"),
        ];
        let result = truncate_history(&msgs, Some("system prompt"), 10000, 4000);
        assert!(result.iter().any(|m| m.content == "keep me"));
    }

    #[test]
    fn test_truncate_history_accounts_for_system() {
        let system = "s".repeat(40000); // ~10000 tokens
        let msgs = vec![
            LLMMessage::user(&"a".repeat(20000)),
            LLMMessage::user("keep"),
        ];
        // Budget: 30000 - 8192 = 21808, minus 10000 system = 11808
        let result = truncate_history(&msgs, Some(&system), 30000, 8192);
        assert!(result.iter().any(|m| m.content == "keep"));
    }
}
