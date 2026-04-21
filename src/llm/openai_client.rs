use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use super::types::{LLMClient, LLMMessage, LLMResponse, TokenUsage, ToolCall};

pub struct OpenAIClient {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: Option<f64>,
}

impl OpenAIClient {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        max_tokens: u32,
        temperature: Option<f64>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_tokens,
            temperature,
        }
    }
}

#[async_trait]
impl LLMClient for OpenAIClient {
    async fn chat(
        &self,
        messages: &[LLMMessage],
        system: Option<&str>,
        tools: Option<&[Value]>,
    ) -> Result<LLMResponse, String> {
        let mut msg_array: Vec<Value> = Vec::new();

        // System message first
        if let Some(sys) = system {
            msg_array.push(json!({"role": "system", "content": sys}));
        }

        // Convert messages
        for msg in messages {
            msg_array.push(convert_message(msg));
        }

        let mut body = json!({
            "model": self.model,
            "messages": msg_array,
            "max_tokens": self.max_tokens,
        });

        if let Some(temp) = self.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                body["tools"] = json!(tools);
            }
        }

        debug!(model = %self.model, "sending request to OpenAI-compatible API");

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let preview = &body_text[..body_text.len().min(500)];
            return Err(format!("API error {}: {}", status, preview));
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
            let mut m = json!({"role": "assistant"});
            if !msg.content.is_empty() {
                m["content"] = json!(msg.content);
            }
            if let Some(tool_calls) = &msg.tool_calls {
                let tc: Vec<Value> = tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                m["tool_calls"] = json!(tc);
            }
            m
        }
        "tool" => {
            json!({
                "role": "tool",
                "tool_call_id": msg.tool_call_id.as_deref().unwrap_or(""),
                "content": msg.content,
            })
        }
        _ => {
            json!({"role": msg.role, "content": msg.content})
        }
    }
}

fn parse_response(json: &Value) -> Result<LLMResponse, String> {
    let choice = json
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or("No choices in response")?;

    let message = choice.get("message").ok_or("No message in choice")?;

    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // Strip <think>...</think> blocks (DeepSeek reasoning tags)
    let content = strip_think_tags(&content);

    let stop_reason = choice
        .get("finish_reason")
        .and_then(|r| r.as_str())
        .unwrap_or("unknown")
        .to_string();

    let tool_calls = message
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let func = tc.get("function")?;
                    let id = tc.get("id")?.as_str()?.to_string();
                    let name = func.get("name")?.as_str()?.to_string();
                    let args_str = func.get("arguments")?.as_str().unwrap_or("{}");
                    let arguments =
                        serde_json::from_str(args_str).unwrap_or(Value::Object(Default::default()));
                    Some(ToolCall {
                        id,
                        name,
                        arguments,
                    })
                })
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty());

    let usage = json.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;

    Ok(LLMResponse {
        content,
        usage: TokenUsage {
            input_tokens,
            output_tokens,
        },
        model: String::new(), // filled by pool/fallback
        stop_reason,
        tool_calls,
    })
}

fn strip_think_tags(text: &str) -> String {
    let re = Regex::new(r"(?s)<think>.*?</think>").unwrap();
    re.replace_all(text, "").trim().to_string()
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
    fn test_convert_assistant_with_tool_calls() {
        let msg = LLMMessage {
            role: "assistant".into(),
            content: "I'll search for that".into(),
            tool_calls: Some(vec![ToolCall {
                id: "call-1".into(),
                name: "web_search".into(),
                arguments: json!({"query": "rust"}),
            }]),
            tool_call_id: None,
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["tool_calls"][0]["id"], "call-1");
        assert_eq!(json["tool_calls"][0]["type"], "function");
        assert_eq!(json["tool_calls"][0]["function"]["name"], "web_search");
    }

    #[test]
    fn test_convert_tool_result() {
        let msg = LLMMessage::tool_result("call-1", "search results here");
        let json = convert_message(&msg);
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call-1");
        assert_eq!(json["content"], "search results here");
    }

    #[test]
    fn test_parse_response_basic() {
        let json = json!({
            "choices": [{
                "message": {
                    "content": "Hello!",
                    "role": "assistant"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });

        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.stop_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.output_tokens, 50);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let json = json!({
            "choices": [{
                "message": {
                    "content": "",
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "web_search",
                            "arguments": "{\"query\": \"rust lang\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 50, "completion_tokens": 20}
        });

        let resp = parse_response(&json).unwrap();
        let tc = resp.tool_calls.unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_abc123");
        assert_eq!(tc[0].name, "web_search");
        assert_eq!(tc[0].arguments["query"], "rust lang");
    }

    #[test]
    fn test_parse_response_no_choices() {
        let json = json!({"choices": []});
        assert!(parse_response(&json).is_err());
    }

    #[test]
    fn test_strip_think_tags() {
        let input = "<think>internal reasoning here</think>The actual answer.";
        assert_eq!(strip_think_tags(input), "The actual answer.");
    }

    #[test]
    fn test_strip_think_tags_multiline() {
        let input = "<think>\nstep 1\nstep 2\n</think>\nHere is the result.";
        assert_eq!(strip_think_tags(input), "Here is the result.");
    }

    #[test]
    fn test_strip_think_tags_none() {
        let input = "No thinking tags here.";
        assert_eq!(strip_think_tags(input), "No thinking tags here.");
    }

    #[test]
    fn test_strip_think_tags_multiple() {
        let input = "<think>a</think>First. <think>b</think>Second.";
        assert_eq!(strip_think_tags(input), "First. Second.");
    }
}
