use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::registry::ToolRegistry;

/// Register file_read and file_write tools.
pub fn register(registry: &mut ToolRegistry, workspace_dir: Arc<PathBuf>) {
    let ws = workspace_dir.clone();
    registry.register(
        "file_read",
        "Read a file within the workspace directory. Returns file contents.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace"
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 200)"
                }
            },
            "required": ["path"]
        }),
        move |args| {
            let ws = ws.clone();
            async move { file_read(&ws, args) }
        },
    );

    let ws = workspace_dir.clone();
    registry.register(
        "file_write",
        "Write content to a file within the workspace directory. Creates parent directories.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write"
                }
            },
            "required": ["path", "content"]
        }),
        move |args| {
            let ws = ws.clone();
            async move { file_write(&ws, args) }
        },
    );
}

fn file_read(workspace: &Path, args: Value) -> String {
    let rel_path = match args.get("path").and_then(|p| p.as_str()) {
        Some(p) => p,
        None => return "Error: 'path' parameter required".to_string(),
    };
    let max_lines = args
        .get("max_lines")
        .and_then(|m| m.as_u64())
        .unwrap_or(200) as usize;

    let full_path = match resolve_safe_path(workspace, rel_path) {
        Ok(p) => p,
        Err(e) => return e,
    };

    match std::fs::read_to_string(&full_path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().take(max_lines).collect();
            if lines.len() < content.lines().count() {
                format!(
                    "{}\n... (truncated at {} lines)",
                    lines.join("\n"),
                    max_lines
                )
            } else {
                lines.join("\n")
            }
        }
        Err(e) => format!("Error reading file: {}", e),
    }
}

fn file_write(workspace: &Path, args: Value) -> String {
    let rel_path = match args.get("path").and_then(|p| p.as_str()) {
        Some(p) => p,
        None => return "Error: 'path' parameter required".to_string(),
    };
    let content = match args.get("content").and_then(|c| c.as_str()) {
        Some(c) => c,
        None => return "Error: 'content' parameter required".to_string(),
    };

    let full_path = match resolve_safe_path(workspace, rel_path) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Create parent directories
    if let Some(parent) = full_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("Error creating directories: {}", e);
        }
    }

    match std::fs::write(&full_path, content) {
        Ok(()) => format!("Written {} bytes to {}", content.len(), rel_path),
        Err(e) => format!("Error writing file: {}", e),
    }
}

/// Resolve a path within workspace, preventing traversal attacks.
fn resolve_safe_path(workspace: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    let joined = workspace.join(rel_path);

    // Normalize by resolving components (handle ".." without requiring existence)
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(c) => {
                resolved.push(c);
            }
            other => {
                resolved.push(other);
            }
        }
    }

    if !resolved.starts_with(&workspace) {
        return Err(format!(
            "Error: path '{}' escapes workspace directory",
            rel_path
        ));
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        // Canonicalize to resolve symlinks (e.g. /tmp -> /private/tmp on macOS)
        let ws = dir.path().canonicalize().unwrap();
        (dir, ws)
    }

    #[test]
    fn test_resolve_safe_path_normal() {
        let (_dir, ws) = setup();
        let result = resolve_safe_path(&ws, "test.txt");
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with(&ws));
    }

    #[test]
    fn test_resolve_safe_path_subdirectory() {
        let (_dir, ws) = setup();
        let result = resolve_safe_path(&ws, "sub/dir/file.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_safe_path_traversal_blocked() {
        let (_dir, ws) = setup();
        let result = resolve_safe_path(&ws, "../../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("escapes workspace"));
    }

    #[test]
    fn test_resolve_safe_path_dotdot_inside() {
        let (_dir, ws) = setup();
        // sub/../file.txt should resolve to file.txt within workspace
        let result = resolve_safe_path(&ws, "sub/../file.txt");
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with(&ws));
    }

    #[test]
    fn test_file_write_and_read() {
        let (_dir, ws) = setup();
        let write_result = file_write(&ws, json!({"path": "test.txt", "content": "hello world"}));
        assert!(write_result.contains("Written"));

        let read_result = file_read(&ws, json!({"path": "test.txt"}));
        assert_eq!(read_result, "hello world");
    }

    #[test]
    fn test_file_write_creates_dirs() {
        let (_dir, ws) = setup();
        let result = file_write(
            &ws,
            json!({"path": "deep/nested/dir/file.txt", "content": "nested"}),
        );
        assert!(result.contains("Written"));
        assert!(ws.join("deep/nested/dir/file.txt").exists());
    }

    #[test]
    fn test_file_read_max_lines() {
        let (_dir, ws) = setup();
        let content = (0..100).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        file_write(&ws, json!({"path": "many.txt", "content": content}));

        let result = file_read(&ws, json!({"path": "many.txt", "max_lines": 5}));
        assert!(result.contains("line 4"));
        assert!(result.contains("truncated"));
        assert!(!result.contains("line 5"));
    }

    #[test]
    fn test_file_read_not_found() {
        let (_dir, ws) = setup();
        let result = file_read(&ws, json!({"path": "nonexistent.txt"}));
        assert!(result.starts_with("Error"));
    }

    #[test]
    fn test_file_read_missing_param() {
        let (_dir, ws) = setup();
        let result = file_read(&ws, json!({}));
        assert!(result.contains("'path' parameter required"));
    }

    #[test]
    fn test_file_write_traversal_blocked() {
        let (_dir, ws) = setup();
        let result = file_write(
            &ws,
            json!({"path": "../../evil.txt", "content": "bad"}),
        );
        assert!(result.contains("escapes workspace"));
    }

    #[tokio::test]
    async fn test_register_tools() {
        let ws = Arc::new(PathBuf::from("/tmp/test-workspace"));
        let mut registry = ToolRegistry::new();
        register(&mut registry, ws);
        assert!(registry.has("file_read"));
        assert!(registry.has("file_write"));
    }
}
