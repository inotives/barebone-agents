//! Counter-triggered pattern reflection (EP-00015 Decision F + G).
//!
//! On task completion (`task_key` scope) or conversation segment end
//! (`agent_conv` scope), increment the per-scope counter. Once the counter
//! hits the threshold, retrieve the last N artifacts from local sources, run
//! a structured-output LLM call to detect a pattern, and on hit write a
//! pending preference draft to `data/drafts/2_knowledges/preferences/`.
//!
//! Failure modes:
//! - LLM call returns one of the known failure prefixes → counter is **not**
//!   reset (next attempt retries).
//! - JSON parse failure on a non-prefix response → counter is reset (over-eager
//!   retries on a flaky LLM aren't worth the cost).
//! - `pattern_found=false` → counter is reset.
//! - `task_key` scope with no draft history → log info, reset counter (no LLM
//!   call). Reflection on a task requires `metadata.persist_as_draft: true`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::agent_loop::AgentLoop;
use crate::db::Database;

const PENDING_DIR: &str = "data/drafts/2_knowledges/preferences";

#[derive(Debug, Clone, Copy)]
pub enum ScopeKind {
    /// Recurring task — `scope_key = task.key`.
    TaskKey,
    /// Conversation segment — `scope_key = "_global"`.
    AgentConv,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::TaskKey => "task_key",
            ScopeKind::AgentConv => "agent_conv",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReflectionOutcome {
    /// Final post-event counter value (0 if reset, or the pre-threshold value
    /// if not yet at threshold).
    pub counter: i64,
    /// True when reflection actually fired (threshold reached and artifacts
    /// retrieved). Says nothing about whether a pattern was detected.
    pub fired: bool,
    /// Path of the written pending preference, if any.
    pub draft_path: Option<PathBuf>,
}

/// Increment the counter for `(scope, scope_key, agent_name)`. If the
/// post-increment count >= `threshold`, retrieve artifacts and run reflection.
///
/// Returns a `ReflectionOutcome` describing what happened. Errors are logged
/// and folded into a "did nothing" outcome (we never want reflection to break
/// the calling task / segment).
pub async fn increment_and_maybe_reflect(
    root_dir: &Path,
    db: &Database,
    agent_loop: &AgentLoop,
    scope: ScopeKind,
    scope_key: &str,
    threshold: u32,
) -> ReflectionOutcome {
    let agent_name = agent_loop.agent_name.clone();

    let counter = match db.increment_reflection_counter(scope.as_str(), scope_key, &agent_name) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "reflection: counter increment failed");
            return ReflectionOutcome {
                counter: 0,
                fired: false,
                draft_path: None,
            };
        }
    };
    debug!(scope = ?scope, scope_key, counter, threshold, "reflection counter incremented");

    if counter < threshold as i64 {
        return ReflectionOutcome {
            counter,
            fired: false,
            draft_path: None,
        };
    }

    // Retrieve artifacts.
    let artifacts = match scope {
        ScopeKind::TaskKey => collect_task_artifacts(root_dir, scope_key),
        ScopeKind::AgentConv => collect_session_artifacts(root_dir, &agent_name),
    };

    if artifacts.is_empty() {
        info!(
            scope = ?scope,
            scope_key,
            "no artifact history; skipping reflection (set metadata.persist_as_draft for tasks)"
        );
        let _ = db.reset_reflection_counter(scope.as_str(), scope_key, &agent_name);
        return ReflectionOutcome {
            counter,
            fired: false,
            draft_path: None,
        };
    }

    info!(scope = ?scope, scope_key, count = artifacts.len(), "reflection firing");

    let response = run_reflection_llm(agent_loop, &artifacts).await;

    if response.starts_with("LLM call failed") || response.starts_with("I'm sorry, all models failed")
    {
        warn!("reflection: LLM failure prefix detected; counter NOT reset (will retry on next event)");
        return ReflectionOutcome {
            counter,
            fired: true,
            draft_path: None,
        };
    }

    let parsed = match parse_reflection_json(&response) {
        Some(p) => p,
        None => {
            debug!("reflection: LLM did not return valid JSON; treating as pattern_found=false");
            let _ = db.reset_reflection_counter(scope.as_str(), scope_key, &agent_name);
            return ReflectionOutcome {
                counter,
                fired: true,
                draft_path: None,
            };
        }
    };

    if !parsed.pattern_found {
        info!(scope = ?scope, scope_key, "reflection: no pattern; counter reset");
        let _ = db.reset_reflection_counter(scope.as_str(), scope_key, &agent_name);
        return ReflectionOutcome {
            counter,
            fired: true,
            draft_path: None,
        };
    }

    // Write the pending preference.
    let draft_path = match write_pending_preference(root_dir, &parsed) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "reflection: failed to write pending preference");
            // Counter is NOT reset here — the LLM did detect a pattern, but
            // we couldn't persist it. Next attempt retries.
            return ReflectionOutcome {
                counter,
                fired: true,
                draft_path: None,
            };
        }
    };
    info!(scope = ?scope, scope_key, path = %draft_path.display(), "reflection: pending preference written");
    let _ = db.reset_reflection_counter(scope.as_str(), scope_key, &agent_name);

    ReflectionOutcome {
        counter,
        fired: true,
        draft_path: Some(draft_path),
    }
}

#[derive(Debug, Deserialize)]
struct ReflectionJson {
    pattern_found: bool,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    preference_body: String,
    #[serde(default)]
    evidence_paths: Vec<String>,
}

fn parse_reflection_json(raw: &str) -> Option<ReflectionJson> {
    let cleaned = strip_code_fence(raw);
    serde_json::from_str::<ReflectionJson>(cleaned).ok().or_else(|| {
        // Try to be tolerant of extra fields by parsing as Value first.
        let json: Value = serde_json::from_str(cleaned).ok()?;
        let pattern_found = json.get("pattern_found")?.as_bool()?;
        let scope = json.get("scope").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let preference_body = json
            .get("preference_body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let evidence_paths = json
            .get("evidence_paths")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Some(ReflectionJson {
            pattern_found,
            scope,
            preference_body,
            evidence_paths,
        })
    })
}

fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
    stripped.trim()
}

fn collect_task_artifacts(root_dir: &Path, task_key: &str) -> Vec<(PathBuf, String)> {
    let dir = root_dir.join("data/drafts/2_researches");
    if !dir.exists() {
        return Vec::new();
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let prefix = format!("{}-", task_key);
    let mut hits: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix))
                .unwrap_or(false)
        })
        .filter_map(|p| {
            let modified = p.metadata().and_then(|m| m.modified()).ok()?;
            Some((p, modified))
        })
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
    hits.into_iter()
        .take(10)
        .filter_map(|(p, _)| std::fs::read_to_string(&p).ok().map(|s| (p, s)))
        .collect()
}

fn collect_session_artifacts(root_dir: &Path, agent_name: &str) -> Vec<(PathBuf, String)> {
    let dir = root_dir.join("data/drafts/sessions");
    if !dir.exists() {
        return Vec::new();
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut hits: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .filter_map(|p| {
            let raw = std::fs::read_to_string(&p).ok()?;
            // Filter by frontmatter agent.
            if !frontmatter_agent_matches(&raw, agent_name) {
                return None;
            }
            let modified = p.metadata().and_then(|m| m.modified()).ok()?;
            Some((p, modified))
        })
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1));
    hits.into_iter()
        .take(10)
        .filter_map(|(p, _)| std::fs::read_to_string(&p).ok().map(|s| (p, s)))
        .collect()
}

fn frontmatter_agent_matches(raw: &str, agent_name: &str) -> bool {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return false;
    };
    let Some(end) = rest.find("\n---\n") else {
        return false;
    };
    let fm = &rest[..end];
    for line in fm.lines() {
        if let Some(value) = line.strip_prefix("agent:") {
            let v = value.trim().trim_matches('"').trim_matches('\'');
            return v == agent_name;
        }
    }
    false
}

async fn run_reflection_llm(agent_loop: &AgentLoop, artifacts: &[(PathBuf, String)]) -> String {
    let system = "You are an analyst looking for stable patterns across recent agent artifacts. \
        Read the artifacts below and decide whether a stable pattern exists worth saving as a \
        user preference (e.g. consistent style choice, recurring constraint, repeated decision). \
        Output ONLY a JSON object with this schema: \
        {\
          \"pattern_found\": boolean, \
          \"scope\": string,            // e.g. \"research-finance\", \"git-style\", \"global\" \
          \"preference_body\": string,  // markdown body for the preference (~3-8 sentences) \
          \"evidence_paths\": [string]  // local paths of artifacts that evidence the pattern \
        }. \
        If no clear pattern: pattern_found=false, leave other fields empty. \
        Do not wrap your response in code fences.";
    let mut user = String::with_capacity(8192);
    user.push_str("Recent artifacts (newest first):\n\n");
    for (path, content) in artifacts {
        user.push_str(&format!("---\n### {}\n\n", path.display()));
        // Cap each artifact to 4KB so the prompt doesn't explode.
        let preview: String = content.chars().take(4000).collect();
        user.push_str(&preview);
        if content.len() > 4000 {
            user.push_str("\n\n[... truncated]");
        }
        user.push_str("\n\n");
    }
    user.push_str("Return your JSON now.");
    agent_loop.cheap_call(system, &user).await
}

fn write_pending_preference(
    root_dir: &Path,
    parsed: &ReflectionJson,
) -> Result<PathBuf, String> {
    let scope_slug = if parsed.scope.is_empty() {
        "untagged".to_string()
    } else {
        slugify_scope(&parsed.scope)
    };
    let date = chrono::Utc::now().format("%Y%m%d").to_string();
    let dir = root_dir.join(PENDING_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;
    let base = format!("{}-{}", scope_slug, date);
    let target = pick_unique_path(&dir, &base);

    let mut body = String::new();
    body.push_str("---\n");
    body.push_str("type: preference\n");
    body.push_str(&format!("scope: {}\n", parsed.scope));
    if !parsed.evidence_paths.is_empty() {
        body.push_str("evidence_paths:\n");
        for p in &parsed.evidence_paths {
            body.push_str(&format!("  - {}\n", p));
        }
    }
    body.push_str("source: reflection\n");
    body.push_str(&format!(
        "generated_at: {}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    body.push_str("---\n\n");
    body.push_str(parsed.preference_body.trim());
    if !body.ends_with('\n') {
        body.push('\n');
    }

    std::fs::write(&target, &body)
        .map_err(|e| format!("failed to write {}: {}", target.display(), e))?;
    Ok(target)
}

fn slugify_scope(scope: &str) -> String {
    let mut out = String::with_capacity(scope.len());
    let mut prev = false;
    for c in scope.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev = false;
        } else if !prev && !out.is_empty() {
            out.push('-');
            prev = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "untagged".to_string()
    } else {
        trimmed
    }
}

fn pick_unique_path(dir: &Path, base_name: &str) -> PathBuf {
    let primary = dir.join(format!("{}.md", base_name));
    if !primary.exists() {
        return primary;
    }
    for n in 2..1000 {
        let cand = dir.join(format!("{}-{}.md", base_name, n));
        if !cand.exists() {
            return cand;
        }
    }
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    dir.join(format!("{}-{}.md", base_name, ns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.register_agent("ino").unwrap();
        db
    }

    #[test]
    fn counter_increment_and_reset() {
        let db = setup_db();
        let c1 = db.increment_reflection_counter("task_key", "TSK-1", "ino").unwrap();
        assert_eq!(c1, 1);
        let c2 = db.increment_reflection_counter("task_key", "TSK-1", "ino").unwrap();
        assert_eq!(c2, 2);
        db.reset_reflection_counter("task_key", "TSK-1", "ino").unwrap();
        assert_eq!(db.get_reflection_counter("task_key", "TSK-1", "ino").unwrap(), 0);
    }

    #[test]
    fn counter_separate_scopes_are_independent() {
        let db = setup_db();
        db.increment_reflection_counter("task_key", "TSK-1", "ino").unwrap();
        db.increment_reflection_counter("task_key", "TSK-2", "ino").unwrap();
        db.increment_reflection_counter("agent_conv", "_global", "ino").unwrap();
        assert_eq!(db.get_reflection_counter("task_key", "TSK-1", "ino").unwrap(), 1);
        assert_eq!(db.get_reflection_counter("task_key", "TSK-2", "ino").unwrap(), 1);
        assert_eq!(db.get_reflection_counter("agent_conv", "_global", "ino").unwrap(), 1);
    }

    #[test]
    fn parse_reflection_json_pattern_true() {
        let raw = r#"{"pattern_found": true, "scope": "git-style", "preference_body": "Use imperative mood.", "evidence_paths": ["a.md", "b.md"]}"#;
        let p = parse_reflection_json(raw).unwrap();
        assert!(p.pattern_found);
        assert_eq!(p.scope, "git-style");
        assert_eq!(p.preference_body, "Use imperative mood.");
        assert_eq!(p.evidence_paths.len(), 2);
    }

    #[test]
    fn parse_reflection_json_pattern_false() {
        let raw = r#"{"pattern_found": false}"#;
        let p = parse_reflection_json(raw).unwrap();
        assert!(!p.pattern_found);
    }

    #[test]
    fn parse_reflection_json_invalid() {
        assert!(parse_reflection_json("not json").is_none());
    }

    #[test]
    fn parse_reflection_json_with_fence() {
        let raw = "```json\n{\"pattern_found\": true, \"scope\": \"x\"}\n```";
        let p = parse_reflection_json(raw).unwrap();
        assert!(p.pattern_found);
    }

    #[test]
    fn slugify_scope_basic() {
        assert_eq!(slugify_scope("git-style"), "git-style");
        assert_eq!(slugify_scope("Research Finance"), "research-finance");
        assert_eq!(slugify_scope(""), "untagged");
    }

    #[test]
    fn frontmatter_agent_matches_works() {
        let raw = "---\nagent: ino\nconv_id: c1\n---\n\nbody";
        assert!(frontmatter_agent_matches(raw, "ino"));
        assert!(!frontmatter_agent_matches(raw, "robin"));
    }

    #[test]
    fn frontmatter_agent_matches_no_frontmatter() {
        assert!(!frontmatter_agent_matches("# bare", "ino"));
    }

    #[test]
    fn write_pending_preference_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let parsed = ReflectionJson {
            pattern_found: true,
            scope: "git-style".into(),
            preference_body: "Use imperative.".into(),
            evidence_paths: vec!["x.md".into()],
        };
        let path = write_pending_preference(tmp.path(), &parsed).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("scope: git-style"));
        assert!(content.contains("source: reflection"));
        assert!(content.contains("Use imperative."));
        assert!(content.contains("x.md"));
    }

    #[test]
    fn write_pending_preference_collision_uses_suffix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let parsed = ReflectionJson {
            pattern_found: true,
            scope: "git-style".into(),
            preference_body: "first".into(),
            evidence_paths: vec![],
        };
        let p1 = write_pending_preference(tmp.path(), &parsed).unwrap();
        let p2 = write_pending_preference(tmp.path(), &parsed).unwrap();
        assert_ne!(p1, p2);
        assert!(p2.file_name().unwrap().to_string_lossy().contains("-2.md"));
    }

    #[test]
    fn collect_task_artifacts_returns_empty_when_no_drafts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let arts = collect_task_artifacts(tmp.path(), "TSK-99");
        assert!(arts.is_empty());
    }

    #[test]
    fn collect_task_artifacts_filters_by_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("data/drafts/2_researches");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("TSK-7-202605030900-x.md"), "---\n---\n\nA").unwrap();
        std::fs::write(dir.join("TSK-7-202605040900-y.md"), "---\n---\n\nB").unwrap();
        std::fs::write(dir.join("TSK-8-202605030900-z.md"), "---\n---\n\nC").unwrap();
        let arts = collect_task_artifacts(tmp.path(), "TSK-7");
        assert_eq!(arts.len(), 2);
    }

    #[test]
    fn collect_session_artifacts_filters_by_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("data/drafts/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("aaa-20260503T100000Z.md"),
            "---\nagent: ino\n---\n\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join("bbb-20260503T100000Z.md"),
            "---\nagent: robin\n---\n\nbody",
        )
        .unwrap();
        let arts = collect_session_artifacts(tmp.path(), "ino");
        assert_eq!(arts.len(), 1);
    }
}
