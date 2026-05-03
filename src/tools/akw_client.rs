use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};
use tracing::debug;

use crate::config::settings;
use crate::config::{AgentConfig, McpServerConfig};

use super::mcp::McpConnection;

/// Which AKW catalog a search/get operation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Skill,
    Role,
}

impl Kind {
    pub fn search_tool(&self) -> &'static str {
        match self {
            Kind::Skill => "skill_search",
            Kind::Role => "agent_search",
        }
    }

    pub fn get_tool(&self) -> &'static str {
        match self {
            Kind::Skill => "skill_get",
            Kind::Role => "agent_get",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Role => "role",
        }
    }
}

/// One AKW search result.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub slug: String,
    pub path: String,
    pub description: Option<String>,
    pub score: f32,
}

/// A fetched AKW document (skill or role).
#[derive(Debug, Clone)]
pub struct FetchedDoc {
    pub slug: String,
    pub path: String,
    pub frontmatter: Option<serde_yaml::Value>,
    pub body: String,
    /// Original markdown source (frontmatter + body), as written to disk on `pull`.
    pub raw: String,
}

#[derive(Debug)]
pub enum AkwError {
    /// No agent.yml declares an `akw` MCP server.
    NotConfigured(String),
    /// `--agent <name>` was passed but the agent dir or config is missing/invalid.
    AgentNotFound(String),
    /// Spawning or initializing the AKW MCP server failed.
    Spawn(String),
    /// Calling a tool on the server failed.
    Call { tool: String, message: String },
    /// Parsing the AKW response failed.
    Parse { tool: String, message: String },
}

impl std::fmt::Display for AkwError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AkwError::NotConfigured(m) => write!(f, "AKW MCP not configured: {}", m),
            AkwError::AgentNotFound(m) => write!(f, "Agent not found: {}", m),
            AkwError::Spawn(m) => write!(f, "{}", m),
            AkwError::Call { tool, message } => write!(f, "AKW tool '{}' failed: {}", tool, message),
            AkwError::Parse { tool, message } => {
                write!(f, "Failed to parse AKW '{}' response: {}", tool, message)
            }
        }
    }
}

impl std::error::Error for AkwError {}

/// Resolve which agent.yml provides the AKW MCP server config.
///
/// If `agent_override` is set, that agent's config is used (or an error if it
/// has no `akw` server). Otherwise, scan `agents/*/agent.yml` in sorted order
/// and pick the first that declares `name: akw`.
pub fn resolve_akw_config(
    root_dir: &Path,
    agent_override: Option<&str>,
) -> Result<(String, McpServerConfig), AkwError> {
    if let Some(name) = agent_override {
        let dir = settings::agent_dir(root_dir, name);
        if !dir.join("agent.yml").exists() {
            return Err(AkwError::AgentNotFound(format!(
                "agents/{}/agent.yml does not exist",
                name
            )));
        }
        let cfg = AgentConfig::load(&dir)
            .map_err(|e| AkwError::AgentNotFound(format!("agents/{}: {}", name, e)))?;
        let akw = cfg
            .mcp_servers
            .into_iter()
            .find(|m| m.name == "akw")
            .ok_or_else(|| {
                AkwError::NotConfigured(format!(
                    "agent '{}' declares no 'akw' MCP server",
                    name
                ))
            })?;
        return Ok((name.to_string(), akw));
    }

    let agents_dir = root_dir.join("agents");
    let entries = std::fs::read_dir(&agents_dir).map_err(|e| {
        AkwError::NotConfigured(format!("failed to read {}: {}", agents_dir.display(), e))
    })?;

    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.is_dir() || !p.join("agent.yml").exists() {
                return None;
            }
            p.file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.starts_with('_') && !n.starts_with('.'))
                .map(String::from)
        })
        .collect();
    names.sort();

    for name in &names {
        if let Ok(cfg) = AgentConfig::load(&agents_dir.join(name)) {
            if let Some(akw) = cfg.mcp_servers.into_iter().find(|m| m.name == "akw") {
                return Ok((name.clone(), akw));
            }
        }
    }

    Err(AkwError::NotConfigured(
        "no agent.yml declares an 'akw' MCP server (try --agent <name>)".to_string(),
    ))
}

/// Standalone AKW MCP client. Spawns the server, exposes typed methods, shuts
/// down via `shutdown()`. Intended for one-shot CLI verbs (`skill pull`,
/// `role search`, etc.) — not the long-lived agent runtime.
pub struct AkwClient {
    conn: McpConnection,
    /// The agent whose `agent.yml` provided the MCP config (for user-facing logs).
    pub source_agent: String,
}

impl AkwClient {
    /// Resolve config, spawn the server, complete MCP handshake.
    ///
    /// `root_dir` is the repo root (the dir containing `agents/`).
    /// `agent_override` selects a specific agent's config; `None` scans.
    pub async fn connect(
        root_dir: &Path,
        agent_override: Option<&str>,
    ) -> Result<Self, AkwError> {
        let (agent_name, mcp_cfg) = resolve_akw_config(root_dir, agent_override)?;

        let agent_dir = settings::agent_dir(root_dir, &agent_name);
        let root_env = settings::load_env_file(&root_dir.join(".env"));
        let merged_env = settings::merge_env(&root_env, &agent_dir);
        let resolved_env = mcp_cfg.resolve_env(&merged_env);

        let conn = McpConnection::connect(&mcp_cfg, &resolved_env)
            .await
            .map_err(AkwError::Spawn)?;

        debug!(agent = %agent_name, "AKW MCP standalone client connected");
        Ok(Self {
            conn,
            source_agent: agent_name,
        })
    }

    /// Return the path the source agent's config came from (for user logs).
    pub fn source_path(&self) -> String {
        format!("agents/{}/agent.yml", self.source_agent)
    }

    pub async fn search(
        &self,
        kind: Kind,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, AkwError> {
        let tool = kind.search_tool();
        let raw = self
            .conn
            .call_tool(tool, json!({ "query": query, "limit": limit }))
            .await
            .map_err(|e| AkwError::Call {
                tool: tool.to_string(),
                message: e,
            })?;
        debug!(tool = %tool, response = %raw, "AKW search raw response");
        parse_search(&raw, tool)
    }

    pub async fn get(&self, kind: Kind, slug_or_path: &str) -> Result<FetchedDoc, AkwError> {
        // AKW's *_get tools require a full memory path. If the caller passed a
        // bare slug, search for it and look for an exact slug match in the top
        // results. AKW's search is content-ranked so unrelated entries can win
        // — we won't substitute a different slug silently.
        let path = if slug_or_path.contains('/') {
            slug_or_path.to_string()
        } else {
            let hits = self.search(kind, slug_or_path, 5).await?;
            hits.into_iter()
                .find(|h| h.slug == slug_or_path)
                .map(|h| h.path)
                .ok_or_else(|| AkwError::Call {
                    tool: kind.get_tool().to_string(),
                    message: format!(
                        "AKW search did not surface a {} with slug '{}'. \
                         Run `barebone-agent {} search <query>` to discover the path, \
                         then `pull <path>` (paths look like `3_intelligences/{}/.../{}.md`).",
                        kind.label(),
                        slug_or_path,
                        kind.label(),
                        match kind { Kind::Skill => "skills", Kind::Role => "agents" },
                        slug_or_path
                    ),
                })?
        };

        let tool = kind.get_tool();
        let arg_key = match kind {
            Kind::Skill => "skill_path",
            Kind::Role => "agent_path",
        };
        let raw = self
            .conn
            .call_tool(tool, json!({ arg_key: &path }))
            .await
            .map_err(|e| AkwError::Call {
                tool: tool.to_string(),
                message: e,
            })?;
        debug!(tool = %tool, response = %raw, "AKW get raw response");
        check_tool_error(&raw, tool)?;
        parse_doc(&raw, &path, tool)
    }

    pub async fn shutdown(self) {
        self.conn.shutdown().await;
    }
}

/// Derive a flat-pool slug from an AKW memory path.
///
/// Examples:
/// - `3_intelligences/skills/workflow/incident_commander/SKILL.md` → `incident_commander`
/// - `3_intelligences/agents/engineering/sre.md` → `sre`
/// - `incident_commander` (already a slug) → `incident_commander`
pub fn slug_from_path(path: &str) -> String {
    let p = Path::new(path);

    if p.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
        if let Some(parent) = p.parent() {
            if let Some(name) = parent.file_name().and_then(|s| s.to_str()) {
                return name.to_string();
            }
        }
    }

    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string()
}

fn parse_search(raw: &str, tool: &str) -> Result<Vec<SearchHit>, AkwError> {
    let json: Value = serde_json::from_str(raw).map_err(|e| AkwError::Parse {
        tool: tool.to_string(),
        message: format!("invalid JSON: {}", e),
    })?;

    // Tolerate {"results": [...]}, {"result": [...]}, a bare array, or a
    // single object with `path` (AKW returns one object when there's one hit).
    let items: Vec<Value> = if let Some(arr) = json.get("results").and_then(|r| r.as_array()) {
        arr.clone()
    } else if let Some(arr) = json.get("result").and_then(|r| r.as_array()) {
        arr.clone()
    } else if let Some(arr) = json.as_array() {
        arr.clone()
    } else if json.get("path").is_some() {
        vec![json]
    } else {
        return Ok(Vec::new());
    };

    let hits = items
        .iter()
        .map(|item| {
            let path = item
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            let slug = item
                .get("slug")
                .and_then(|s| s.as_str())
                .map(String::from)
                .unwrap_or_else(|| slug_from_path(&path));
            let description = item
                .get("description")
                .or_else(|| item.get("title"))
                .or_else(|| item.get("summary"))
                .and_then(|d| d.as_str())
                .map(String::from);
            let score = item
                .get("score")
                .and_then(|s| s.as_f64())
                .unwrap_or(0.0) as f32;
            SearchHit {
                slug,
                path,
                description,
                score,
            }
        })
        .collect();

    Ok(hits)
}

fn parse_doc(raw: &str, hint: &str, tool: &str) -> Result<FetchedDoc, AkwError> {
    // Two shapes seen in practice:
    //   1. JSON: {"content": "<full markdown>", "path": "..."}
    //   2. Bare markdown text returned in the text content block.
    let (content, path) = match serde_json::from_str::<Value>(raw) {
        Ok(json) if json.get("content").is_some() => {
            let content = json
                .get("content")
                .and_then(|c| c.as_str())
                .map(String::from)
                .unwrap_or_else(|| raw.to_string());
            let path = json
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or(hint)
                .to_string();
            (content, path)
        }
        _ => (raw.to_string(), hint.to_string()),
    };

    if content.trim().is_empty() {
        return Err(AkwError::Parse {
            tool: tool.to_string(),
            message: format!("empty document for '{}'", hint),
        });
    }

    let (frontmatter, body) = split_frontmatter(&content);
    let slug = slug_from_path(&path);
    Ok(FetchedDoc {
        slug,
        path,
        frontmatter,
        body,
        raw: content,
    })
}

/// Detect tool-side error responses that AKW returns as either:
/// - plain text starting with `Error executing tool` or `Error:`
/// - a JSON object with an `error` field
fn check_tool_error(raw: &str, tool: &str) -> Result<(), AkwError> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with("Error executing tool") || trimmed.starts_with("Error:") {
        return Err(AkwError::Call {
            tool: tool.to_string(),
            message: raw.trim().to_string(),
        });
    }
    if let Ok(json) = serde_json::from_str::<Value>(raw) {
        if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
            return Err(AkwError::Call {
                tool: tool.to_string(),
                message: err.to_string(),
            });
        }
    }
    Ok(())
}

/// Split a markdown file into (yaml frontmatter, body). Returns `(None, raw)`
/// when no `---\n` fence is present at the top of the file.
pub fn split_frontmatter(raw: &str) -> (Option<serde_yaml::Value>, String) {
    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let fm_str = &rest[..end];
            let body = rest[end + "\n---\n".len()..]
                .trim_start_matches('\n')
                .to_string();
            let fm: Option<serde_yaml::Value> = serde_yaml::from_str(fm_str).ok();
            return (fm, body);
        }
    }
    (None, raw.to_string())
}

// Suppress unused-import warnings if the module is consumed only by tests
// in some build profiles.
#[allow(dead_code)]
fn _ensure_hashmap_used(_: &HashMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_from_skill_path() {
        assert_eq!(
            slug_from_path("3_intelligences/skills/workflow/incident_commander/SKILL.md"),
            "incident_commander"
        );
    }

    #[test]
    fn slug_from_role_path() {
        assert_eq!(
            slug_from_path("3_intelligences/agents/engineering/sre.md"),
            "sre"
        );
    }

    #[test]
    fn slug_from_bare_slug() {
        assert_eq!(slug_from_path("incident_commander"), "incident_commander");
    }

    #[test]
    fn split_frontmatter_present() {
        let raw = "---\nname: foo\nkeywords:\n  - bar\n---\n\nbody text\n";
        let (fm, body) = split_frontmatter(raw);
        assert!(fm.is_some());
        assert_eq!(body.trim(), "body text");
    }

    #[test]
    fn split_frontmatter_absent() {
        let raw = "# Just a heading\n\nNo frontmatter.";
        let (fm, body) = split_frontmatter(raw);
        assert!(fm.is_none());
        assert_eq!(body, raw);
    }

    #[test]
    fn parse_search_results_array() {
        let raw = r#"{"results": [
            {"path": "3_intelligences/skills/x/foo/SKILL.md", "description": "Foo skill", "score": 0.9},
            {"path": "3_intelligences/skills/y/bar/SKILL.md", "description": "Bar skill", "score": 0.7}
        ]}"#;
        let hits = parse_search(raw, "skill_search").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].slug, "foo");
        assert_eq!(hits[0].description.as_deref(), Some("Foo skill"));
        assert!((hits[0].score - 0.9).abs() < 0.001);
    }

    #[test]
    fn parse_search_bare_array() {
        let raw = r#"[{"path": "a/b/sre.md"}]"#;
        let hits = parse_search(raw, "agent_search").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "sre");
    }

    #[test]
    fn parse_search_single_object() {
        let raw = r#"{"path": "3_intelligences/skills/workflow/runbook_generator/SKILL.md", "title": "Print runbook", "summary": "name: runbook_generator", "score": 3.07}"#;
        let hits = parse_search(raw, "skill_search").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "runbook_generator");
        assert_eq!(hits[0].description.as_deref(), Some("Print runbook"));
    }

    #[test]
    fn parse_search_summary_fallback() {
        let raw = r#"[{"path": "p/x/foo/SKILL.md", "summary": "fallback desc"}]"#;
        let hits = parse_search(raw, "skill_search").unwrap();
        assert_eq!(hits[0].description.as_deref(), Some("fallback desc"));
    }

    #[test]
    fn parse_search_empty_object() {
        let raw = r#"{}"#;
        let hits = parse_search(raw, "skill_search").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn parse_doc_json_content() {
        let raw = r#"{"path": "3_intelligences/skills/workflow/foo/SKILL.md", "content": "---\nname: foo\n---\n\nbody"}"#;
        let doc = parse_doc(raw, "foo", "skill_get").unwrap();
        assert_eq!(doc.slug, "foo");
        assert!(doc.frontmatter.is_some());
        assert_eq!(doc.body.trim(), "body");
    }

    #[test]
    fn parse_doc_bare_markdown() {
        let raw = "---\nname: bar\n---\n\nrole body";
        let doc = parse_doc(raw, "bar", "agent_get").unwrap();
        assert_eq!(doc.slug, "bar");
        assert_eq!(doc.body.trim(), "role body");
    }

    #[test]
    fn parse_doc_empty_fails() {
        let err = parse_doc("", "foo", "skill_get").unwrap_err();
        assert!(matches!(err, AkwError::Parse { .. }));
    }

    #[test]
    fn resolve_akw_config_missing_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("agents")).unwrap();
        let err = resolve_akw_config(dir.path(), Some("nope")).unwrap_err();
        assert!(matches!(err, AkwError::AgentNotFound(_)));
    }

    #[test]
    fn resolve_akw_config_no_akw_server() {
        let dir = tempfile::TempDir::new().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(agents.join("alice")).unwrap();
        std::fs::write(
            agents.join("alice/agent.yml"),
            "role: coder\nmodel: x\n",
        )
        .unwrap();
        let err = resolve_akw_config(dir.path(), None).unwrap_err();
        assert!(matches!(err, AkwError::NotConfigured(_)));
    }

    #[test]
    fn resolve_akw_config_finds_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(agents.join("alice")).unwrap();
        std::fs::create_dir_all(agents.join("bob")).unwrap();
        // alice has no akw; bob has akw — sorted scan should pick bob.
        std::fs::write(agents.join("alice/agent.yml"), "role: coder\nmodel: x\n").unwrap();
        std::fs::write(
            agents.join("bob/agent.yml"),
            "role: coder\nmodel: x\nmcp_servers:\n  - name: akw\n    command: uv\n    args: [run]\n",
        )
        .unwrap();
        let (name, cfg) = resolve_akw_config(dir.path(), None).unwrap();
        assert_eq!(name, "bob");
        assert_eq!(cfg.name, "akw");
    }

    #[test]
    fn resolve_akw_config_explicit_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(agents.join("alice")).unwrap();
        std::fs::write(
            agents.join("alice/agent.yml"),
            "role: coder\nmodel: x\nmcp_servers:\n  - name: akw\n    command: uv\n    args: [run]\n",
        )
        .unwrap();
        let (name, _) = resolve_akw_config(dir.path(), Some("alice")).unwrap();
        assert_eq!(name, "alice");
    }
}
