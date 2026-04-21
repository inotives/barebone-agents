use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tracing::debug;

use super::types::{LLMClient, LLMMessage, LLMResponse, TokenUsage, ToolCall};

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

pub struct GeminiClient {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl GeminiClient {
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
impl LLMClient for GeminiClient {
    async fn chat(
        &self,
        messages: &[LLMMessage],
        system: Option<&str>,
        tools: Option<&[Value]>,
    ) -> Result<LLMResponse, String> {
        let contents: Vec<Value> = messages.iter().map(convert_message).collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": self.max_tokens,
            }
        });

        // System instruction as config parameter
        if let Some(sys) = system {
            body["system_instruction"] = json!({
                "parts": [{"text": sys}]
            });
        }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                let declarations: Vec<Value> = tools.iter().map(convert_tool_def).collect();
                body["tools"] = json!([{
                    "function_declarations": declarations
                }]);
            }
        }

        let url = format!(
            "{}/{}:generateContent?key={}",
            GEMINI_API_BASE, self.model, self.api_key
        );

        debug!(model = %self.model, "sending request to Gemini API");

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let preview = &body_text[..body_text.len().min(500)];
            return Err(format!("Gemini API error {}: {}", status, preview));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

        parse_response(&json)
    }
}

fn convert_message(msg: &LLMMessage) -> Value {
    let role = match msg.role.as_str() {
        "assistant" => "model",
        "tool" => "user",
        other => other,
    };

    let mut parts: Vec<Value> = Vec::new();

    if !msg.content.is_empty() {
        if msg.role == "tool" {
            // Tool results sent as function_response parts
            parts.push(json!({
                "functionResponse": {
                    "name": msg.tool_call_id.as_deref().unwrap_or("unknown"),
                    "response": {"result": msg.content}
                }
            }));
        } else {
            parts.push(json!({"text": msg.content}));
        }
    }

    if let Some(tool_calls) = &msg.tool_calls {
        for tc in tool_calls {
            parts.push(json!({
                "functionCall": {
                    "name": tc.name,
                    "args": tc.arguments,
                }
            }));
        }
    }

    json!({"role": role, "parts": parts})
}

/// Convert OpenAI tool format to Gemini FunctionDeclaration.
/// Strips `default` from properties (Gemini rejects it).
fn convert_tool_def(tool: &Value) -> Value {
    let func = tool.get("function").unwrap_or(tool);
    let empty = json!("");
    let default_params = json!({"type": "object"});
    let name = func.get("name").unwrap_or(&empty);
    let description = func.get("description").unwrap_or(&empty);
    let params = func.get("parameters").unwrap_or(&default_params).clone();

    // Strip "default" from all properties
    let params = strip_defaults(&params);

    json!({
        "name": name,
        "description": description,
        "parameters": params,
    })
}

fn strip_defaults(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                if k == "default" {
                    continue;
                }
                new_map.insert(k.clone(), strip_defaults(v));
            }
            Value::Object(new_map)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(strip_defaults).collect()),
        other => other.clone(),
    }
}

fn parse_response(json: &Value) -> Result<LLMResponse, String> {
    let candidate = json
        .get("candidates")
        .and_then(|c| c.get(0))
        .ok_or("No candidates in response")?;

    let parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .ok_or("No parts in candidate")?;

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for part in parts {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            text_parts.push(text.to_string());
        }
        if let Some(fc) = part.get("functionCall") {
            if let Some(name) = fc.get("name").and_then(|n| n.as_str()) {
                let arguments = fc
                    .get("args")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));

                // Synthetic tool ID (Gemini doesn't generate IDs)
                let id = synthetic_tool_id(name, &arguments);

                tool_calls.push(ToolCall {
                    id,
                    name: name.to_string(),
                    arguments,
                });
            }
        }
    }

    let usage_meta = json.get("usageMetadata");
    let input_tokens = usage_meta
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;
    let output_tokens = usage_meta
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;

    let stop_reason = candidate
        .get("finishReason")
        .and_then(|r| r.as_str())
        .unwrap_or("unknown")
        .to_lowercase();

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

/// Generate a synthetic tool call ID: `call_{name}_{hash}`
fn synthetic_tool_id(name: &str, arguments: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    arguments.to_string().hash(&mut hasher);
    format!("call_{}_{:x}", name, hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_user_message() {
        let msg = LLMMessage::user("hello");
        let json = convert_message(&msg);
        assert_eq!(json["role"], "user");
        assert_eq!(json["parts"][0]["text"], "hello");
    }

    #[test]
    fn test_convert_assistant_as_model() {
        let msg = LLMMessage::assistant("response");
        let json = convert_message(&msg);
        assert_eq!(json["role"], "model");
        assert_eq!(json["parts"][0]["text"], "response");
    }

    #[test]
    fn test_convert_tool_result_as_function_response() {
        let msg = LLMMessage::tool_result("web_search", "results here");
        let json = convert_message(&msg);
        assert_eq!(json["role"], "user");
        let fr = &json["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "web_search");
        assert_eq!(fr["response"]["result"], "results here");
    }

    #[test]
    fn test_convert_assistant_with_function_call() {
        let msg = LLMMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "web_search".into(),
                arguments: json!({"query": "test"}),
            }]),
            tool_call_id: None,
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "model");
        assert_eq!(json["parts"][0]["functionCall"]["name"], "web_search");
    }

    #[test]
    fn test_strip_defaults() {
        let input = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "default": "test"},
                "count": {"type": "integer", "default": 5}
            }
        });
        let stripped = strip_defaults(&input);
        assert!(stripped["properties"]["query"].get("default").is_none());
        assert_eq!(stripped["properties"]["query"]["type"], "string");
        assert!(stripped["properties"]["count"].get("default").is_none());
    }

    #[test]
    fn test_convert_tool_def_strips_defaults() {
        let tool = json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "q": {"type": "string", "default": "hello"}
                    }
                }
            }
        });
        let gemini = convert_tool_def(&tool);
        assert_eq!(gemini["name"], "search");
        assert!(gemini["parameters"]["properties"]["q"]
            .get("default")
            .is_none());
    }

    #[test]
    fn test_parse_response_text() {
        let json = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello from Gemini!"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 20
            }
        });

        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.content, "Hello from Gemini!");
        assert_eq!(resp.stop_reason, "stop"); // lowercased
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.output_tokens, 20);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_function_call() {
        let json = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "web_search",
                            "args": {"query": "rust lang"}
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 50,
                "candidatesTokenCount": 10
            }
        });

        let resp = parse_response(&json).unwrap();
        let tc = resp.tool_calls.unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].name, "web_search");
        assert!(tc[0].id.starts_with("call_web_search_"));
    }

    #[test]
    fn test_synthetic_tool_id_deterministic() {
        let id1 = synthetic_tool_id("search", &json!({"q": "a"}));
        let id2 = synthetic_tool_id("search", &json!({"q": "a"}));
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_synthetic_tool_id_different() {
        let id1 = synthetic_tool_id("search", &json!({"q": "a"}));
        let id2 = synthetic_tool_id("search", &json!({"q": "b"}));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_parse_response_no_candidates() {
        let json = json!({"candidates": []});
        assert!(parse_response(&json).is_err());
    }
}
