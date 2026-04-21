use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::McpServerConfig;
use super::registry::ToolRegistry;

/// A connection to a single MCP server via stdio.
pub struct McpConnection {
    name: String,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    request_id: Arc<Mutex<u64>>,
}

impl McpConnection {
    /// Spawn an MCP server process and establish a connection.
    pub async fn connect(
        config: &McpServerConfig,
        resolved_env: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .envs(resolved_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn MCP server '{}': {}", config.name, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("No stdin for MCP server '{}'", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("No stdout for MCP server '{}'", config.name))?;

        let conn = Self {
            name: config.name.clone(),
            child: Arc::new(Mutex::new(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            request_id: Arc::new(Mutex::new(0)),
        };

        // Initialize the MCP session
        conn.initialize().await?;

        info!(server = %config.name, "MCP server connected");
        Ok(conn)
    }

    /// Send a JSON-RPC request and read the response.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = {
            let mut counter = self.request_id.lock().await;
            *counter += 1;
            *counter
        };

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to serialize request: {}", e))?;
        line.push('\n');

        debug!(server = %self.name, method = %method, "MCP request");

        // Send request
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("Failed to write to MCP server: {}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("Failed to flush MCP server stdin: {}", e))?;
        }

        // Read response (skip notifications — they have no "id")
        let mut stdout = self.stdout.lock().await;
        loop {
            let mut response_line = String::new();
            let bytes = stdout
                .read_line(&mut response_line)
                .await
                .map_err(|e| format!("Failed to read from MCP server: {}", e))?;

            if bytes == 0 {
                return Err("MCP server closed connection".to_string());
            }

            let response: Value = serde_json::from_str(response_line.trim())
                .map_err(|e| format!("Invalid JSON from MCP server: {}", e))?;

            // Skip notifications (no "id" field)
            if response.get("id").is_some() {
                if let Some(error) = response.get("error") {
                    return Err(format!("MCP error: {}", error));
                }
                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// Initialize the MCP session.
    async fn initialize(&self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "barebone-agent",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
        .await?;

        // Send initialized notification (no response expected)
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let mut line = serde_json::to_string(&notification).unwrap();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.ok();
        stdin.flush().await.ok();

        Ok(())
    }

    /// List available tools from the server.
    pub async fn list_tools(&self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools)
    }

    /// Call a tool on the server.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;

        // Extract first TextContent block
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter().find_map(|block| {
                    if block.get("type")?.as_str()? == "text" {
                        block.get("text")?.as_str().map(String::from)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| result.to_string());

        Ok(content)
    }

    /// Shut down the MCP server.
    pub async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        info!(server = %self.name, "MCP server shut down");
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Load MCP servers from config and register their tools into the registry.
pub async fn load_mcp_servers(
    configs: &[McpServerConfig],
    merged_env: &HashMap<String, String>,
    registry: &mut ToolRegistry,
) -> Vec<McpConnection> {
    let mut connections = Vec::new();

    for config in configs {
        let resolved_env = config.resolve_env(merged_env);

        match McpConnection::connect(config, &resolved_env).await {
            Ok(conn) => {
                match conn.list_tools().await {
                    Ok(tools) => {
                        let allowlist = &config.tools;
                        let registered = register_mcp_tools(
                            registry,
                            &conn.name,
                            &tools,
                            allowlist,
                            &conn,
                        );
                        info!(
                            server = %config.name,
                            tools = registered,
                            "MCP tools registered"
                        );
                    }
                    Err(e) => {
                        warn!(server = %config.name, error = %e, "failed to list MCP tools");
                    }
                }
                connections.push(conn);
            }
            Err(e) => {
                warn!(server = %config.name, error = %e, "failed to connect MCP server");
            }
        }
    }

    connections
}

/// Register discovered MCP tools into the tool registry.
fn register_mcp_tools(
    registry: &mut ToolRegistry,
    server_name: &str,
    tools: &[Value],
    allowlist: &[String],
    conn: &McpConnection,
) -> usize {
    let mut count = 0;
    let stdin = conn.stdin.clone();
    let stdout = conn.stdout.clone();
    let request_id = conn.request_id.clone();
    let conn_name = conn.name.clone();

    for tool in tools {
        let tool_name = match tool.get("name").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => continue,
        };

        // Apply allowlist filter
        if !allowlist.is_empty() && !allowlist.iter().any(|a| a == tool_name) {
            continue;
        }

        let full_name = format!("mcp_{}__{}", server_name, tool_name);
        let description = tool
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        let parameters = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or(json!({"type": "object"}));

        let remote_name = tool_name.to_string();
        let stdin = stdin.clone();
        let stdout = stdout.clone();
        let request_id = request_id.clone();
        let cn = conn_name.clone();

        registry.register(
            &full_name,
            &description,
            parameters,
            move |args| {
                let remote_name = remote_name.clone();
                let stdin = stdin.clone();
                let stdout = stdout.clone();
                let request_id = request_id.clone();
                let cn = cn.clone();

                async move {
                    match mcp_tool_call(&cn, &remote_name, args, &stdin, &stdout, &request_id)
                        .await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            error!(tool = %remote_name, error = %e, "MCP tool call failed");
                            format!("Error: {}", e)
                        }
                    }
                }
            },
        );

        count += 1;
    }

    count
}

async fn mcp_tool_call(
    _server_name: &str,
    tool_name: &str,
    arguments: Value,
    stdin: &Mutex<tokio::process::ChildStdin>,
    stdout: &Mutex<BufReader<tokio::process::ChildStdout>>,
    request_id: &Mutex<u64>,
) -> Result<String, String> {
    let id = {
        let mut counter = request_id.lock().await;
        *counter += 1;
        *counter
    };

    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments,
        }
    });

    let mut line = serde_json::to_string(&request)
        .map_err(|e| format!("Failed to serialize: {}", e))?;
    line.push('\n');

    {
        let mut stdin = stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|e| format!("Write failed: {}", e))?;
        stdin.flush().await.map_err(|e| format!("Flush failed: {}", e))?;
    }

    let mut stdout = stdout.lock().await;
    loop {
        let mut response_line = String::new();
        let bytes = stdout
            .read_line(&mut response_line)
            .await
            .map_err(|e| format!("Read failed: {}", e))?;

        if bytes == 0 {
            return Err("MCP server closed connection".to_string());
        }

        let response: Value = serde_json::from_str(response_line.trim())
            .map_err(|e| format!("Invalid JSON: {}", e))?;

        if response.get("id").is_some() {
            if let Some(error) = response.get("error") {
                return Err(format!("MCP error: {}", error));
            }

            let result = response.get("result").unwrap_or(&Value::Null);
            let content = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|block| {
                        if block.get("type")?.as_str()? == "text" {
                            block.get("text")?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_else(|| result.to_string());

            return Ok(content);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_naming() {
        let full = format!("mcp_{}__{}", "github", "create_issue");
        assert_eq!(full, "mcp_github__create_issue");
    }

    #[test]
    fn test_allowlist_filtering() {
        let tools = vec![
            json!({"name": "create_issue", "description": "Create issue"}),
            json!({"name": "list_repos", "description": "List repos"}),
            json!({"name": "delete_repo", "description": "Delete repo"}),
        ];
        let allowlist = vec!["create_issue".to_string(), "list_repos".to_string()];

        let filtered: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name")?.as_str())
            .filter(|name| allowlist.is_empty() || allowlist.iter().any(|a| a == name))
            .collect();

        assert_eq!(filtered, vec!["create_issue", "list_repos"]);
    }

    #[test]
    fn test_empty_allowlist_passes_all() {
        let tools = vec![
            json!({"name": "a"}),
            json!({"name": "b"}),
        ];
        let allowlist: Vec<String> = vec![];

        let filtered: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name")?.as_str())
            .filter(|name| allowlist.is_empty() || allowlist.iter().any(|a| a == *name))
            .collect();

        assert_eq!(filtered, vec!["a", "b"]);
    }

    #[test]
    fn test_extract_text_content() {
        let result = json!({
            "content": [
                {"type": "text", "text": "Hello from MCP"},
                {"type": "image", "data": "..."}
            ]
        });

        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter().find_map(|block| {
                    if block.get("type")?.as_str()? == "text" {
                        block.get("text")?.as_str().map(String::from)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();

        assert_eq!(text, "Hello from MCP");
    }

    #[test]
    fn test_extract_text_content_fallback() {
        let result = json!({"content": [{"type": "image", "data": "..."}]});

        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter().find_map(|block| {
                    if block.get("type")?.as_str()? == "text" {
                        block.get("text")?.as_str().map(String::from)
                    } else {
                        None
                    }
                })
            });

        assert!(text.is_none());
    }
}
