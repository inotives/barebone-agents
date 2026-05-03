//! User keyword triggers (EP-00015 Decision H).
//!
//! Detects "save as preference" / "please remember" style phrases in a user
//! message and writes the last assistant turn (extracted/normalized via a
//! cheap LLM call) directly to the **active** preference pool — no review
//! gate, since the user's explicit intent is a sufficient gate.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::agent_loop::AgentLoop;
use crate::db::Database;

const ACTIVE_DIR: &str = "agents/_preferences";

/// Outcome of a manual-save trigger.
#[derive(Debug, Clone)]
pub struct SaveOutcome {
    pub path: PathBuf,
    pub slug: String,
}

/// Detect any of the manual-save keywords in `message`.
pub fn detect_save_preference(message: &str) -> bool {
    static_regex().is_match(message)
}

fn static_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b(save\s+as\s+preference|please\s+remember|remember\s+this|save\s+this\s+preference)\b")
            .expect("regex compiles")
    })
}

/// Last final-assistant message for `conv_id` — what the user is referring to
/// when they say "save this as preference". Returns `None` if none.
fn fetch_last_assistant_message(db: &Database, conv_id: &str) -> Option<String> {
    let history = db.load_history(conv_id, 20).ok()?;
    history
        .into_iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content)
}

/// On a keyword match, persist a preference. Returns `Ok(Some(...))` on
/// success, `Ok(None)` when the trigger was suppressed (e.g. no prior
/// assistant turn), and `Err(...)` on a real failure.
pub async fn handle_save_preference(
    root_dir: &Path,
    agent_loop: &AgentLoop,
    db: &Database,
    conv_id: &str,
) -> Result<Option<SaveOutcome>, String> {
    let assistant_text = match fetch_last_assistant_message(db, conv_id) {
        Some(t) => t,
        None => {
            return Ok(None);
        }
    };

    let extracted = extract_preference_fields(agent_loop, &assistant_text).await;
    let slug = extracted.slug;
    let body = extracted.body;
    let scope = extracted.scope.unwrap_or_else(|| "global".to_string());
    let summary = extracted.summary;

    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let base = format!("manual-{}-{}", timestamp, slug);
    let dir = root_dir.join(ACTIVE_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;
    let target = pick_unique_path(&dir, &base);

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("scope: {}\n", scope));
    if !extracted.keywords.is_empty() {
        out.push_str("keywords:\n");
        for k in &extracted.keywords {
            out.push_str(&format!("  - {}\n", k));
        }
    }
    if let Some(s) = &summary {
        out.push_str(&format!("summary: {}\n", yaml_safe(s)));
    }
    out.push_str("source: manual_save\n");
    out.push_str(&format!(
        "saved_at: {}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    out.push_str("---\n\n");
    out.push_str(body.trim());
    if !out.ends_with('\n') {
        out.push('\n');
    }

    std::fs::write(&target, &out)
        .map_err(|e| format!("failed to write {}: {}", target.display(), e))?;
    info!(path = %target.display(), bytes = out.len(), "manual preference saved");

    // Reset the agent_conv counter (Decision H).
    let agent_name = agent_loop.agent_name.clone();
    let _ = db.reset_reflection_counter("agent_conv", "_global", &agent_name);

    let final_slug = target
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&base)
        .to_string();
    Ok(Some(SaveOutcome {
        path: target,
        slug: final_slug,
    }))
}

#[derive(Debug, Default, Clone)]
struct ExtractedPreference {
    slug: String,
    body: String,
    scope: Option<String>,
    summary: Option<String>,
    keywords: Vec<String>,
}

async fn extract_preference_fields(agent_loop: &AgentLoop, assistant_text: &str) -> ExtractedPreference {
    let system = "Extract a saved preference from the assistant message below. \
        Return ONLY a JSON object with these keys: \
        `slug` (kebab-case ≤30 chars), \
        `scope` (e.g. \"global\", \"git\", \"research-finance\"), \
        `keywords` (array of ≤6 lowercase-hyphen strings), \
        `summary` (one-line summary, ≤120 chars), \
        `body` (the markdown body — usually a normalized rewrite of the assistant message). \
        Do not wrap the response in code fences.";
    let user = format!(
        "Assistant message to remember:\n\n{}",
        assistant_text.chars().take(3000).collect::<String>()
    );
    let response = agent_loop.cheap_call(system, &user).await;

    if response.starts_with("LLM call failed") || response.starts_with("I'm sorry, all models failed")
    {
        warn!("triggers: LLM extraction failed; using assistant message verbatim");
        return ExtractedPreference {
            slug: "memo".into(),
            body: assistant_text.to_string(),
            scope: Some("global".into()),
            summary: None,
            keywords: vec![],
        };
    }

    parse_extraction_json(&response).unwrap_or_else(|| ExtractedPreference {
        slug: "memo".into(),
        body: assistant_text.to_string(),
        scope: Some("global".into()),
        summary: None,
        keywords: vec![],
    })
}

#[derive(Debug, Deserialize)]
struct ExtractJson {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    body: String,
}

fn parse_extraction_json(raw: &str) -> Option<ExtractedPreference> {
    let cleaned = strip_code_fence(raw);
    let parsed: ExtractJson = serde_json::from_str(cleaned).ok().or_else(|| {
        // Tolerant fallback via Value parsing.
        let json: Value = serde_json::from_str(cleaned).ok()?;
        Some(ExtractJson {
            slug: json.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            scope: json.get("scope").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            keywords: json
                .get("keywords")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            summary: json.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            body: json.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
    })?;
    let slug = if parsed.slug.is_empty() { "memo".to_string() } else { sanitize_slug(&parsed.slug) };
    let body = if parsed.body.is_empty() {
        // No body extracted — fall through to caller's verbatim path.
        return None;
    } else {
        parsed.body
    };
    Some(ExtractedPreference {
        slug,
        body,
        scope: if parsed.scope.is_empty() { None } else { Some(parsed.scope) },
        summary: if parsed.summary.is_empty() { None } else { Some(parsed.summary) },
        keywords: parsed.keywords,
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

fn sanitize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev = false;
        } else if !prev && !out.is_empty() {
            out.push('-');
            prev = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    let truncated: String = trimmed.chars().take(30).collect();
    if truncated.is_empty() {
        "memo".to_string()
    } else {
        truncated.trim_end_matches('-').to_string()
    }
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

/// Build the user-facing acknowledgement string for Decision H.
pub fn acknowledgement_message(path: &Path, root_dir: &Path) -> String {
    let display = path
        .strip_prefix(root_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    format!(
        "Saved as preference at `{}` (active). Will sync to AKW within the hour. \
         Edit or remove the file directly to revise.",
        display
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_basic_phrases() {
        assert!(detect_save_preference("save as preference"));
        assert!(detect_save_preference("Please remember this for next time"));
        assert!(detect_save_preference("save this preference please"));
        assert!(detect_save_preference("ok now remember this"));
    }

    #[test]
    fn detect_case_insensitive() {
        assert!(detect_save_preference("SAVE AS PREFERENCE"));
        assert!(detect_save_preference("Remember This."));
    }

    #[test]
    fn detect_negative_cases() {
        assert!(!detect_save_preference("hello world"));
        assert!(!detect_save_preference("can you preserve this"));
        assert!(!detect_save_preference("savepreference"));
    }

    #[test]
    fn parse_extraction_full_object() {
        let raw = r#"{"slug":"git-style","scope":"git","keywords":["git","style"],"summary":"Use imperative","body":"Use imperative mood for commits."}"#;
        let p = parse_extraction_json(raw).unwrap();
        assert_eq!(p.slug, "git-style");
        assert_eq!(p.scope.as_deref(), Some("git"));
        assert_eq!(p.keywords, vec!["git", "style"]);
        assert!(p.body.contains("imperative"));
    }

    #[test]
    fn parse_extraction_with_fence() {
        let raw = "```json\n{\"slug\":\"x\",\"body\":\"b\"}\n```";
        let p = parse_extraction_json(raw).unwrap();
        assert_eq!(p.slug, "x");
        assert_eq!(p.body, "b");
    }

    #[test]
    fn parse_extraction_empty_body_fails() {
        let raw = r#"{"slug":"x","body":""}"#;
        assert!(parse_extraction_json(raw).is_none());
    }

    #[test]
    fn parse_extraction_invalid() {
        assert!(parse_extraction_json("not json").is_none());
    }

    #[test]
    fn sanitize_slug_basic() {
        assert_eq!(sanitize_slug("Git Commit Style"), "git-commit-style");
        assert_eq!(sanitize_slug("foo!! bar"), "foo-bar");
        assert_eq!(sanitize_slug(""), "memo");
        assert_eq!(sanitize_slug("---"), "memo");
    }

    #[test]
    fn sanitize_slug_caps_length() {
        let long = "x".repeat(100);
        assert_eq!(sanitize_slug(&long).len(), 30);
    }

    #[test]
    fn pick_unique_path_collision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p1 = pick_unique_path(tmp.path(), "abc");
        std::fs::write(&p1, "x").unwrap();
        let p2 = pick_unique_path(tmp.path(), "abc");
        assert!(p2.file_name().unwrap().to_string_lossy().contains("-2.md"));
    }

    #[test]
    fn acknowledgement_uses_relative_path() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("agents/_preferences/manual-x.md");
        let msg = acknowledgement_message(&path, root.path());
        assert!(msg.contains("agents/_preferences/manual-x.md"));
        assert!(msg.contains("(active)"));
    }
}
