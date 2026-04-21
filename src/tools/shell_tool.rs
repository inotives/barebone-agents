use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

use super::registry::ToolRegistry;

/// Register the shell_execute tool.
pub fn register(registry: &mut ToolRegistry, workspace_dir: Arc<PathBuf>) {
    registry.register(
        "shell_execute",
        "Execute a shell command in the workspace directory. Returns stdout and stderr.",
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30)"
                }
            },
            "required": ["command"]
        }),
        move |args| {
            let ws = workspace_dir.clone();
            async move { shell_execute(&ws, args).await }
        },
    );
}

async fn shell_execute(workspace: &PathBuf, args: Value) -> String {
    let command = match args.get("command").and_then(|c| c.as_str()) {
        Some(c) => c.to_string(),
        None => return "Error: 'command' parameter required".to_string(),
    };
    let timeout_secs = args
        .get("timeout")
        .and_then(|t| t.as_u64())
        .unwrap_or(30);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(workspace)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let status = output.status.code().unwrap_or(-1);
            format!(
                "Exit code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                status,
                stdout.trim(),
                stderr.trim()
            )
        }
        Ok(Err(e)) => format!("Error executing command: {}", e),
        Err(_) => format!("Error: command timed out after {}s", timeout_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shell_execute_echo() {
        let ws = PathBuf::from("/tmp");
        let result = shell_execute(&ws, json!({"command": "echo hello"})).await;
        assert!(result.contains("Exit code: 0"));
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_shell_execute_failing_command() {
        let ws = PathBuf::from("/tmp");
        let result = shell_execute(&ws, json!({"command": "false"})).await;
        assert!(result.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn test_shell_execute_timeout() {
        let ws = PathBuf::from("/tmp");
        let result = shell_execute(&ws, json!({"command": "sleep 10", "timeout": 1})).await;
        assert!(result.contains("timed out"));
    }

    #[tokio::test]
    async fn test_shell_execute_missing_command() {
        let ws = PathBuf::from("/tmp");
        let result = shell_execute(&ws, json!({})).await;
        assert!(result.contains("'command' parameter required"));
    }

    #[tokio::test]
    async fn test_register_tool() {
        let ws = Arc::new(PathBuf::from("/tmp"));
        let mut registry = ToolRegistry::new();
        register(&mut registry, ws);
        assert!(registry.has("shell_execute"));
    }
}
