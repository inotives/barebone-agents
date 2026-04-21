use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use super::types::{LLMClient, LLMMessage, LLMResponse, TokenUsage, ToolCall};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(api_key: &str, model: &str, max_tokens: u32) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_tokens,
        }
    }
}

#[async_trait]
impl LLMClient for AnthropicClient {
    async fn chat(
        &self,
        messages: &[LLMMessage],
        system: Option<&str>,
        tools: Option<&[Value]>,
    ) -> Result<LLMResponse, String> {
        let msg_array: Vec<Value> = messages.iter().map(convert_message).collect();

        let mut body = json!({
            "model": self.model,
            "messages": msg_array,
            "max_tokens": self.max_tokens,
        });

        // System prompt is a separate parameter, not in messages
        if let Some(sys) = system {
            body["system"] = json!(sys);
        }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                let converted: Vec<Value> = tools.iter().map(convert_tool_def).collect();
                body["tools"] = json!(converted);
            }
        }

        debug!(model = %self.model, "sending request to Anthropic API");

        let resp = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let preview = &body_text[..body_text.len().min(500)];
            return Err(format!("Anthropic API error {}: {}", status, preview));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

        parse_response(&json)
    }
}

fn convert_message(msg: &LLMMessage) -> Value {
    match msg.role.as_str() {
        "assistant" => {
            let mut content_blocks: Vec<Value> = Vec::new();

            if !msg.content.is_empty() {
                content_blocks.push(json!({"type": "text", "text": msg.content}));
            }

            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    content_blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
            }

            json!({"role": "assistant", "content": content_blocks})
        }
        "tool" => {
            // Tool results are sent as user messages with tool_result content blocks
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                    "content": msg.content,
                }]
            })
        }
        _ => {
            json!({"role": msg.role, "content": msg.content})
        }
    }
}

/// Convert OpenAI tool format to Anthropic format.
fn convert_tool_def(tool: &Value) -> Value {
    let func = tool.get("function").unwrap_or(tool);
    json!({
        "name": func.get("name").unwrap_or(&json!("")),
        "description": func.get("description").unwrap_or(&json!("")),
        "input_schema": func.get("parameters").unwrap_or(&json!({"type": "object"})),
    })
}

fn parse_response(json: &Value) -> Result<LLMResponse, String> {
    let content_blocks = json
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or("No content in response")?;

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in content_blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                if let (Some(id), Some(name)) = (
                    block.get("id").and_then(|i| i.as_str()),
                    block.get("name").and_then(|n| n.as_str()),
                ) {
                    let arguments = block
                        .get("input")
                        .cloned()
                        .unwrap_or(Value::Object(Default::default()));
                    tool_calls.push(ToolCall {
                        id: id.to_string(),
                        name: name.to_string(),
                        arguments,
                    });
                }
            }
            _ => {}
        }
    }

    let usage = json.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;

    let stop_reason = json
        .get("stop_reason")
        .and_then(|r| r.as_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(LLMResponse {
        content: text_parts.join(""),
        usage: TokenUsage {
            input_tokens,
            output_tokens,
        },
        model: String::new(),
        stop_reason,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_user_message() {
        let msg = LLMMessage::user("hello");
        let json = convert_message(&msg);
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn test_convert_assistant_with_tool_use() {
        let msg = LLMMessage {
            role: "assistant".into(),
            content: "Let me search".into(),
            tool_calls: Some(vec![ToolCall {
                id: "toolu_01".into(),
                name: "web_search".into(),
                arguments: json!({"query": "rust"}),
            }]),
            tool_call_id: None,
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "assistant");
        let blocks = json["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Let me search");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "toolu_01");
        assert_eq!(blocks[1]["name"], "web_search");
    }

    #[test]
    fn test_convert_tool_result_as_user() {
        let msg = LLMMessage::tool_result("toolu_01", "search results");
        let json = convert_message(&msg);
        // Tool results become user messages in Anthropic format
        assert_eq!(json["role"], "user");
        let blocks = json["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "toolu_01");
        assert_eq!(blocks[0]["content"], "search results");
    }

    #[test]
    fn test_convert_tool_def() {
        let openai_tool = json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    },
                    "required": ["query"]
                }
            }
        });
        let anthropic = convert_tool_def(&openai_tool);
        assert_eq!(anthropic["name"], "web_search");
        assert_eq!(anthropic["description"], "Search the web");
        assert_eq!(anthropic["input_schema"]["type"], "object");
    }

    #[test]
    fn test_parse_response_text_only() {
        let json = json!({
            "content": [
                {"type": "text", "text": "Hello from Claude!"}
            ],
            "usage": {"input_tokens": 50, "output_tokens": 10},
            "stop_reason": "end_turn"
        });

        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.content, "Hello from Claude!");
        assert_eq!(resp.stop_reason, "end_turn");
        assert_eq!(resp.usage.input_tokens, 50);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_tool_use() {
        let json = json!({
            "content": [
                {"type": "text", "text": "I'll search for that."},
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "web_search",
                    "input": {"query": "rust programming"}
                }
            ],
            "usage": {"input_tokens": 100, "output_tokens": 30},
            "stop_reason": "tool_use"
        });

        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.content, "I'll search for that.");
        assert_eq!(resp.stop_reason, "tool_use");
        let tc = resp.tool_calls.unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "toolu_01");
        assert_eq!(tc[0].name, "web_search");
        assert_eq!(tc[0].arguments["query"], "rust programming");
    }

    #[test]
    fn test_parse_response_no_content() {
        let json = json!({"stop_reason": "end_turn"});
        assert!(parse_response(&json).is_err());
    }
}
