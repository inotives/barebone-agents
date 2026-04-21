use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{debug, error};

/// Async handler type for tool execution.
pub type ToolHandler = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync,
>;

pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub handler: ToolHandler,
}

pub struct ToolRegistry {
    tools: HashMap<String, ToolDef>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool with its handler.
    pub fn register<F, Fut>(&mut self, name: &str, description: &str, parameters: Value, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = String> + Send + 'static,
    {
        let handler: ToolHandler = Arc::new(move |args| Box::pin(handler(args)));
        self.tools.insert(
            name.to_string(),
            ToolDef {
                name: name.to_string(),
                description: description.to_string(),
                parameters,
                handler,
            },
        );
        debug!(tool = %name, "registered tool");
    }

    /// Execute a tool by name. Never panics — returns error text on failure.
    pub async fn execute(&self, name: &str, arguments: Value) -> String {
        let tool = match self.tools.get(name) {
            Some(t) => t,
            None => {
                let msg = format!("Error: unknown tool '{}'", name);
                error!(%name, "tool not found");
                return msg;
            }
        };

        debug!(tool = %name, "executing tool");

        // Catch panics from the handler
        let handler = tool.handler.clone();
        let result = tokio::spawn(async move { (handler)(arguments).await }).await;

        match result {
            Ok(output) => output,
            Err(e) => {
                let msg = format!("Error: tool '{}' panicked: {}", name, e);
                error!(tool = %name, error = %e, "tool execution panicked");
                msg
            }
        }
    }

    /// Get tool definitions in OpenAI function-calling format.
    pub fn get_definitions(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect()
    }

    /// Check if a tool is registered.
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Get tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_execute() {
        let mut registry = ToolRegistry::new();
        registry.register(
            "echo",
            "Echo the input",
            json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            |args| async move {
                args.get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("no text")
                    .to_string()
            },
        );

        assert!(registry.has("echo"));
        assert_eq!(registry.len(), 1);

        let result = registry.execute("echo", json!({"text": "hello"})).await;
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let registry = ToolRegistry::new();
        let result = registry.execute("nonexistent", json!({})).await;
        assert!(result.starts_with("Error: unknown tool"));
    }

    #[tokio::test]
    async fn test_get_definitions() {
        let mut registry = ToolRegistry::new();
        registry.register(
            "test_tool",
            "A test tool",
            json!({"type": "object", "properties": {}}),
            |_| async { "ok".to_string() },
        );

        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0]["type"], "function");
        assert_eq!(defs[0]["function"]["name"], "test_tool");
        assert_eq!(defs[0]["function"]["description"], "A test tool");
    }

    #[tokio::test]
    async fn test_multiple_tools() {
        let mut registry = ToolRegistry::new();
        registry.register("a", "Tool A", json!({}), |_| async { "a".to_string() });
        registry.register("b", "Tool B", json!({}), |_| async { "b".to_string() });

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.execute("a", json!({})).await, "a");
        assert_eq!(registry.execute("b", json!({})).await, "b");
    }

    #[tokio::test]
    async fn test_tool_names() {
        let mut registry = ToolRegistry::new();
        registry.register("alpha", "A", json!({}), |_| async { String::new() });
        registry.register("beta", "B", json!({}), |_| async { String::new() });

        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn test_handler_panic_caught() {
        let mut registry = ToolRegistry::new();
        registry.register("panicker", "Will panic", json!({}), |_| async {
            panic!("intentional panic");
        });

        let result = registry.execute("panicker", json!({})).await;
        assert!(result.contains("Error"));
        assert!(result.contains("panicked"));
    }
}
