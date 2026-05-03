//! Research / report draft persistence (EP-00015 Decision E).
//!
//! Writes task output to `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md`
//! when a task has `metadata.persist_as_draft: true`. The pusher (Decision A2)
//! mirrors it to AKW on its next cycle — this module never calls AKW directly.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::{debug, info, warn};

use crate::agent_loop::AgentLoop;
use crate::db::Task;

const DRAFT_DIR: &str = "data/drafts/2_researches";
const SLUG_MAX: usize = 40;

/// Persist a task's result as a local research draft. No-op if the file
/// already exists with `-N` suffixing on collision (Decision E policy).
///
/// Returns the absolute path written, or an error string.
pub async fn persist_research_draft(
    root_dir: &Path,
    agent_loop: &AgentLoop,
    task: &Task,
) -> Result<PathBuf, String> {
    let result = task
        .result
        .as_deref()
        .ok_or_else(|| "task has no result to persist".to_string())?;

    if result.trim().is_empty() {
        return Err("task result is empty".to_string());
    }

    let extracted = extract_draft_fields(agent_loop, &task.title, result).await;

    let slug = slugify(&task.title);
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M").to_string();
    let base_name = format!("{}-{}-{}", task.key, timestamp, slug);
    let dir = root_dir.join(DRAFT_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;

    let target = pick_unique_path(&dir, &base_name);
    let body = render_draft(task, &extracted);
    std::fs::write(&target, &body)
        .map_err(|e| format!("failed to write {}: {}", target.display(), e))?;
    info!(path = %target.display(), bytes = body.len(), "research draft written");
    Ok(target)
}

/// Slugify a task title: lowercase, alphanumeric + hyphen, max 40 chars.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-');
    let truncated = trimmed.chars().take(SLUG_MAX).collect::<String>();
    let final_str = truncated.trim_end_matches('-').to_string();
    if final_str.is_empty() {
        "untitled".to_string()
    } else {
        final_str
    }
}

/// Find a non-existent path under `dir` starting with `base_name`. Appends
/// `-2`, `-3`, ... on collision (Decisions E/G/H).
fn pick_unique_path(dir: &Path, base_name: &str) -> PathBuf {
    let primary = dir.join(format!("{}.md", base_name));
    if !primary.exists() {
        return primary;
    }
    for n in 2..1000 {
        let candidate = dir.join(format!("{}-{}.md", base_name, n));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Astonishing collision count — fall back to nanosecond suffix.
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    dir.join(format!("{}-{}.md", base_name, ns))
}

#[derive(Debug, Default, Clone)]
struct DraftFields {
    title: String,
    summary: Option<String>,
    tags: Vec<String>,
    body: String,
}

/// Ask the LLM to produce {title, summary, tags, body} JSON from the task's
/// raw result. Falls back to the raw result as the body if the LLM call fails
/// or returns un-parseable JSON — we never want a draft write to fail because
/// the summarization model misbehaved.
async fn extract_draft_fields(agent_loop: &AgentLoop, task_title: &str, result: &str) -> DraftFields {
    let system = "You produce JSON metadata for archived research drafts. \
        Return ONLY a JSON object with these keys: \
        `title` (string, ≤80 chars), `summary` (string, ≤200 chars), \
        `tags` (array of strings, ≤6 entries, lowercase-hyphen), \
        `body` (string, the full markdown body suitable for a wiki page). \
        Do not wrap your response in code fences. \
        If the task result is already well-structured markdown, the `body` \
        field may simply contain it verbatim.";
    let user = format!(
        "Task title: {}\n\nTask result:\n\n{}\n\nProduce the JSON metadata.",
        task_title, result
    );

    let response = agent_loop.cheap_call(system, &user).await;

    if response.starts_with("LLM call failed") || response.starts_with("I'm sorry, all models failed")
    {
        warn!("draft_writer: LLM call failed, using raw result as body");
        return DraftFields {
            title: task_title.to_string(),
            summary: None,
            tags: Vec::new(),
            body: result.to_string(),
        };
    }

    parse_draft_json(&response).unwrap_or_else(|| {
        debug!("draft_writer: LLM did not return valid JSON, using raw result");
        DraftFields {
            title: task_title.to_string(),
            summary: None,
            tags: Vec::new(),
            body: result.to_string(),
        }
    })
}

fn parse_draft_json(raw: &str) -> Option<DraftFields> {
    // Tolerate code-fence wrapping just in case.
    let cleaned = strip_code_fence(raw);
    let json: Value = serde_json::from_str(cleaned).ok()?;
    let title = json.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let summary = json.get("summary").and_then(|v| v.as_str()).map(String::from);
    let tags = json
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let body = json.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if title.is_empty() && body.is_empty() {
        return None;
    }
    Some(DraftFields {
        title,
        summary,
        tags,
        body,
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

fn render_draft(task: &Task, fields: &DraftFields) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    let title = if fields.title.is_empty() {
        task.title.clone()
    } else {
        fields.title.clone()
    };
    out.push_str(&format!("title: {}\n", yaml_safe(&title)));
    out.push_str(&format!("task_key: {}\n", task.key));
    if let Some(s) = &fields.summary {
        out.push_str(&format!("summary: {}\n", yaml_safe(s)));
    }
    if !fields.tags.is_empty() {
        out.push_str("tags:\n");
        for t in &fields.tags {
            out.push_str(&format!("  - {}\n", yaml_safe(t)));
        }
    }
    out.push_str(&format!(
        "generated_at: {}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    out.push_str("source: research_draft\n");
    out.push_str("---\n\n");
    if !fields.body.is_empty() {
        out.push_str(&fields.body);
    } else if let Some(r) = &task.result {
        out.push_str(r);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn yaml_safe(s: &str) -> String {
    let one_line = s.replace('\n', " ").replace('\r', " ");
    if one_line.contains(':') || one_line.contains('"') || one_line.contains('\'') {
        let escaped = one_line.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        one_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(key: &str, title: &str, result: Option<&str>) -> Task {
        Task {
            key: key.to_string(),
            mission_key: None,
            title: title.to_string(),
            description: None,
            status: "done".into(),
            priority: "medium".into(),
            agent_name: Some("ino".into()),
            schedule: None,
            last_run_at: None,
            result: result.map(String::from),
            metadata: None,
            created_at: "2026-05-03T00:00:00Z".into(),
            updated_at: "2026-05-03T00:00:00Z".into(),
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Daily NVDA snapshot"), "daily-nvda-snapshot");
    }

    #[test]
    fn slugify_strips_special_chars() {
        assert_eq!(slugify("Q3 2026 — Earnings: NVDA"), "q3-2026-earnings-nvda");
    }

    #[test]
    fn slugify_caps_at_max() {
        let title = "x".repeat(120);
        let slug = slugify(&title);
        assert_eq!(slug.len(), SLUG_MAX);
    }

    #[test]
    fn slugify_empty_input() {
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("   "), "untitled");
    }

    #[test]
    fn pick_unique_path_appends_suffix_on_collision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p1 = pick_unique_path(tmp.path(), "abc");
        std::fs::write(&p1, "first").unwrap();
        let p2 = pick_unique_path(tmp.path(), "abc");
        assert!(p2.file_name().unwrap().to_string_lossy().ends_with("-2.md"));
    }

    #[test]
    fn parse_draft_json_full() {
        let raw = r#"{"title": "T", "summary": "S", "tags": ["a", "b"], "body": "B"}"#;
        let parsed = parse_draft_json(raw).unwrap();
        assert_eq!(parsed.title, "T");
        assert_eq!(parsed.summary.as_deref(), Some("S"));
        assert_eq!(parsed.tags, vec!["a", "b"]);
        assert_eq!(parsed.body, "B");
    }

    #[test]
    fn parse_draft_json_with_code_fence() {
        let raw = "```json\n{\"title\":\"T\",\"body\":\"B\"}\n```";
        let parsed = parse_draft_json(raw).unwrap();
        assert_eq!(parsed.title, "T");
        assert_eq!(parsed.body, "B");
    }

    #[test]
    fn parse_draft_json_invalid() {
        assert!(parse_draft_json("not json").is_none());
    }

    #[test]
    fn render_draft_includes_frontmatter_and_body() {
        let task = make_task("TSK-00007", "NVDA snapshot", Some("raw result"));
        let fields = DraftFields {
            title: "NVDA Daily Snapshot".into(),
            summary: Some("Up 2%".into()),
            tags: vec!["finance".into(), "nvda".into()],
            body: "Full body here.".into(),
        };
        let out = render_draft(&task, &fields);
        assert!(out.starts_with("---\n"));
        assert!(out.contains("title: NVDA Daily Snapshot"));
        assert!(out.contains("task_key: TSK-00007"));
        assert!(out.contains("summary: Up 2%"));
        assert!(out.contains("- finance"));
        assert!(out.contains("- nvda"));
        assert!(out.contains("source: research_draft"));
        assert!(out.contains("Full body here."));
    }

    #[test]
    fn render_draft_falls_back_to_raw_when_body_empty() {
        let task = make_task("TSK-00008", "x", Some("raw text"));
        let fields = DraftFields {
            title: "X".into(),
            summary: None,
            tags: vec![],
            body: String::new(),
        };
        let out = render_draft(&task, &fields);
        assert!(out.contains("raw text"));
    }
}
