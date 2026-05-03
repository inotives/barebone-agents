//! Preference local pool — reader + selector (EP-00015 Decision A).
//!
//! Mirrors the structure of `_skills` and `_roles`: read every `*.md` under
//! `agents/_preferences/`, parse frontmatter (`keywords`, `scope`, `summary`),
//! select per task/conversation by keyword + body match against the message.
//!
//! The selector reuses the equipped-skills greedy-by-score algorithm with
//! one extra wrinkle: any preference with `scope: global` is always included
//! regardless of keyword match (cheap baseline like personal style rules).
//!
//! Pending preferences live at `data/drafts/2_knowledges/preferences/` and are
//! intentionally NOT read by this module — they're review-gated and only enter
//! the active pool after `barebone-agent prefs promote`.

use std::collections::HashSet;
use std::path::Path;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::session::SessionManager;

/// One file from the local preference pool (`agents/_preferences/<slug>.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preference {
    pub slug: String,
    pub keywords: Vec<String>,
    pub scope: Option<String>,
    pub summary: Option<String>,
    pub body: String,
    /// Estimated tokens for the body content (len/4).
    pub token_estimate: u32,
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "or", "the", "to", "of", "in", "on", "at", "by", "for",
    "is", "are", "was", "were", "be", "been", "being", "do", "does", "did",
    "has", "have", "had", "i", "you", "we", "they", "it", "this", "that",
    "with", "from", "as", "but", "if", "then", "so", "not",
];

fn tokenize_message(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Read every `*.md` under `pool_dir`. Missing dir → empty pool, no error.
pub fn load_preference_pool(pool_dir: &Path) -> Vec<Preference> {
    if !pool_dir.exists() {
        debug!(path = %pool_dir.display(), "preference pool dir not found; empty pool");
        return Vec::new();
    }

    let entries = match std::fs::read_dir(pool_dir) {
        Ok(it) => it,
        Err(e) => {
            warn!(error = %e, path = %pool_dir.display(), "failed to read preference dir");
            return Vec::new();
        }
    };

    let mut pool = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        // Skip `.template.md` and other dot-prefixed sentinels.
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        if slug.starts_with('.') {
            continue;
        }

        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "skipping preference file");
                continue;
            }
        };

        pool.push(parse_preference(&slug, &raw));
    }

    pool.sort_by(|a, b| a.slug.cmp(&b.slug));
    debug!(count = pool.len(), "preference pool loaded");
    pool
}

fn parse_preference(slug: &str, raw: &str) -> Preference {
    let (frontmatter, body) = if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let fm = &rest[..end];
            let body_start = end + "\n---\n".len();
            (Some(fm), rest[body_start..].trim_start_matches('\n').to_string())
        } else {
            (None, raw.to_string())
        }
    } else {
        (None, raw.to_string())
    };

    let mut keywords: Vec<String> = Vec::new();
    let mut scope: Option<String> = None;
    let mut summary: Option<String> = None;

    if let Some(fm) = frontmatter {
        match serde_yaml::from_str::<serde_yaml::Value>(fm) {
            Ok(value) => {
                // Prefer `keywords:`; fall back to `tags:`.
                let kw_field = value.get("keywords").or_else(|| value.get("tags"));
                if let Some(kw) = kw_field {
                    if let Some(arr) = kw.as_sequence() {
                        keywords = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                            .collect();
                    } else if let Some(s) = kw.as_str() {
                        keywords = s
                            .split(|c: char| c == ',' || c.is_whitespace())
                            .filter(|t| !t.is_empty())
                            .map(|t| t.to_lowercase())
                            .collect();
                    }
                }
                if let Some(s) = value.get("scope").and_then(|d| d.as_str()) {
                    scope = Some(s.to_lowercase());
                }
                if let Some(s) = value.get("summary").and_then(|d| d.as_str()) {
                    summary = Some(s.to_string());
                }
            }
            Err(e) => {
                warn!(slug = %slug, error = %e, "preference frontmatter parse failed; treating whole file as body");
            }
        }
    }

    let token_estimate = (body.len() / 4) as u32;
    Preference {
        slug: slug.to_string(),
        keywords,
        scope,
        summary,
        body,
        token_estimate,
    }
}

fn score_preference(pref: &Preference, message_tokens: &HashSet<String>) -> u32 {
    let mut tokens: HashSet<String> = pref.keywords.iter().cloned().collect();
    for word in pref.body.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        tokens.insert(word.to_lowercase());
    }
    message_tokens.intersection(&tokens).count() as u32
}

/// Pick preferences relevant to `message` from `pool`.
///
/// - `scope: global` preferences are **always** included regardless of match
///   or budget (these encode standing personal rules — they should always reach
///   the prompt). Globals are added first, then ranked-by-score scoped prefs
///   fill the remaining budget.
/// - `min_hits`: scoped prefs with fewer than this many distinct token matches
///   are dropped.
/// - `token_budget`: stop adding scoped prefs once cumulative `token_estimate`
///   would exceed the remaining budget. Globals don't count against budget —
///   if the user wants something always-included, respect that intent.
pub fn select_preferences(
    pool: &[Preference],
    message: &str,
    min_hits: u32,
    token_budget: u32,
) -> Vec<Preference> {
    // 1. Always include globals.
    let mut chosen: Vec<Preference> = pool
        .iter()
        .filter(|p| p.scope.as_deref() == Some("global"))
        .cloned()
        .collect();
    chosen.sort_by(|a, b| a.slug.cmp(&b.slug));

    let message_tokens = tokenize_message(message);
    if message_tokens.is_empty() {
        return chosen;
    }

    // 2. Score scoped prefs.
    let mut scored: Vec<(u32, &Preference)> = pool
        .iter()
        .filter(|p| p.scope.as_deref() != Some("global"))
        .map(|p| (score_preference(p, &message_tokens), p))
        .filter(|(hits, _)| *hits >= min_hits)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug.cmp(&b.1.slug)));

    // 3. Pack scoped prefs into the budget.
    let mut used: u32 = 0;
    for (_, pref) in scored {
        if used + pref.token_estimate > token_budget {
            continue;
        }
        used += pref.token_estimate;
        chosen.push(pref.clone());
    }

    chosen
}

/// Format selected preferences as one string per pref (caller wraps with header).
/// Returns an empty Vec for an empty input slice.
pub fn format_preferences(prefs: &[Preference]) -> Vec<String> {
    prefs
        .iter()
        .map(|p| {
            let body = p.body.trim();
            if let Some(scope) = &p.scope {
                format!("### {} (scope: {})\n\n{}", p.slug, scope, body)
            } else {
                format!("### {}\n\n{}", p.slug, body)
            }
        })
        .collect()
}

/// Convenience: format selected preferences as a complete `## User Preferences`
/// system-prompt block. Empty input → empty string.
pub fn format_for_prompt(prefs: &[Preference]) -> String {
    if prefs.is_empty() {
        return String::new();
    }
    let parts = format_preferences(prefs);
    format!("## User Preferences\n\n{}", parts.join("\n\n---\n\n"))
}

/// Per-segment cached preference selection (EP-00015 Decision A).
///
/// On first call for a given `conv_id`, loads the local pool, runs
/// `select_preferences` against `message`, formats per-pref bodies, caches on
/// the session. Subsequent calls return the cached value — preferences stay
/// stable for the segment lifetime regardless of how the user message evolves.
pub async fn select_for_segment_cached(
    session_mgr: &Mutex<SessionManager>,
    conv_id: &str,
    message: &str,
    pool_dir: &Path,
    min_hits: u32,
    token_budget: u32,
) -> Vec<String> {
    {
        let mgr = session_mgr.lock().await;
        let cached = mgr.get_selected_preferences(conv_id);
        if !cached.is_empty() {
            return cached;
        }
    }

    // Compute outside the lock — pool reading hits the filesystem.
    let pool = load_preference_pool(pool_dir);
    if pool.is_empty() {
        return Vec::new();
    }
    let chosen = select_preferences(&pool, message, min_hits, token_budget);
    let formatted = format_preferences(&chosen);

    if !formatted.is_empty() {
        let mut mgr = session_mgr.lock().await;
        mgr.set_selected_preferences(conv_id, formatted.clone());
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_pref(dir: &Path, slug: &str, content: &str) {
        std::fs::write(dir.join(format!("{}.md", slug)), content).unwrap();
    }

    #[test]
    fn parse_pref_with_full_frontmatter() {
        let raw = "---\nkeywords: [git, commit, style]\nscope: git\nsummary: Git commit style\n---\n\nUse imperative mood.";
        let p = parse_preference("git_commit_style", raw);
        assert_eq!(p.keywords, vec!["git", "commit", "style"]);
        assert_eq!(p.scope.as_deref(), Some("git"));
        assert_eq!(p.summary.as_deref(), Some("Git commit style"));
        assert!(p.body.starts_with("Use imperative"));
    }

    #[test]
    fn parse_pref_keywords_string_form() {
        let raw = "---\nkeywords: alpha, beta gamma\nscope: x\n---\n\nbody";
        let p = parse_preference("p", raw);
        assert_eq!(p.keywords, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn parse_pref_falls_back_to_tags() {
        let raw = "---\ntags: [foo, bar]\n---\n\nbody";
        let p = parse_preference("p", raw);
        assert_eq!(p.keywords, vec!["foo", "bar"]);
    }

    #[test]
    fn parse_pref_no_frontmatter() {
        let raw = "Just a body. No frontmatter.";
        let p = parse_preference("bare", raw);
        assert!(p.keywords.is_empty());
        assert!(p.scope.is_none());
        assert_eq!(p.body, raw);
    }

    #[test]
    fn select_global_always_included() {
        let pool = vec![
            Preference {
                slug: "always".into(),
                keywords: vec!["never_appears_in_message".into()],
                scope: Some("global".into()),
                summary: None,
                body: "Always inject me.".into(),
                token_estimate: 5,
            },
            Preference {
                slug: "scoped".into(),
                keywords: vec!["foo".into(), "bar".into()],
                scope: Some("git".into()),
                summary: None,
                body: "Body".into(),
                token_estimate: 3,
            },
        ];
        let picked = select_preferences(&pool, "completely unrelated", 2, 4000);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "always");
    }

    #[test]
    fn select_scoped_filtered_by_min_hits() {
        let pool = vec![Preference {
            slug: "scoped".into(),
            keywords: vec!["foo".into(), "bar".into()],
            scope: Some("test".into()),
            summary: None,
            body: "x".into(),
            token_estimate: 1,
        }];
        // Only one match → below min_hits of 2.
        let picked = select_preferences(&pool, "foo unrelated", 2, 4000);
        assert!(picked.is_empty());
    }

    #[test]
    fn select_respects_budget_for_scoped() {
        let pool = vec![
            Preference {
                slug: "alpha".into(),
                keywords: vec!["foo".into(), "bar".into()],
                scope: Some("test".into()),
                summary: None,
                body: "x".repeat(400),
                token_estimate: 100,
            },
            Preference {
                slug: "beta".into(),
                keywords: vec!["foo".into(), "bar".into()],
                scope: Some("test".into()),
                summary: None,
                body: "y".repeat(400),
                token_estimate: 100,
            },
        ];
        let picked = select_preferences(&pool, "foo bar", 2, 100);
        // Budget 100 allows only one.
        assert_eq!(picked.len(), 1);
    }

    #[test]
    fn select_global_bypasses_budget() {
        let pool = vec![
            Preference {
                slug: "g".into(),
                keywords: Vec::new(),
                scope: Some("global".into()),
                summary: None,
                body: "x".repeat(8000),
                token_estimate: 2000,
            },
            Preference {
                slug: "scoped".into(),
                keywords: vec!["foo".into(), "bar".into()],
                scope: Some("test".into()),
                summary: None,
                body: "x".into(),
                token_estimate: 1,
            },
        ];
        let picked = select_preferences(&pool, "foo bar", 2, 50);
        // Global counted; scoped also fits (1 token < 50 remaining unenforced for global).
        assert!(picked.iter().any(|p| p.slug == "g"));
        assert!(picked.iter().any(|p| p.slug == "scoped"));
    }

    #[test]
    fn select_empty_message_returns_only_globals() {
        let pool = vec![
            Preference {
                slug: "g".into(),
                keywords: Vec::new(),
                scope: Some("global".into()),
                summary: None,
                body: "global body".into(),
                token_estimate: 5,
            },
            Preference {
                slug: "scoped".into(),
                keywords: vec!["foo".into()],
                scope: Some("test".into()),
                summary: None,
                body: "scoped".into(),
                token_estimate: 2,
            },
        ];
        let picked = select_preferences(&pool, "", 1, 4000);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "g");
    }

    #[test]
    fn load_pool_skips_dot_prefixed() {
        let dir = TempDir::new().unwrap();
        let pool_dir = dir.path().join("_preferences");
        std::fs::create_dir_all(&pool_dir).unwrap();
        write_pref(&pool_dir, "real", "---\nscope: x\n---\n\nbody");
        // Hidden / template — must be skipped.
        std::fs::write(pool_dir.join(".template.md"), "---\nscope: y\n---\n\ntemplate").unwrap();

        let pool = load_preference_pool(&pool_dir);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].slug, "real");
    }

    #[test]
    fn load_pool_missing_dir() {
        let pool = load_preference_pool(Path::new("/nonexistent/_preferences"));
        assert!(pool.is_empty());
    }

    #[test]
    fn load_pool_end_to_end() {
        let dir = TempDir::new().unwrap();
        let pool_dir = dir.path().join("_preferences");
        std::fs::create_dir_all(&pool_dir).unwrap();
        write_pref(
            &pool_dir,
            "global_style",
            "---\nscope: global\nsummary: Always be terse\n---\n\nBe terse.",
        );
        write_pref(
            &pool_dir,
            "git_style",
            "---\nkeywords: [git, commit]\nscope: git\n---\n\nUse imperative mood for commits.",
        );

        let pool = load_preference_pool(&pool_dir);
        assert_eq!(pool.len(), 2);

        let picked = select_preferences(&pool, "i need to git commit a fix", 2, 4000);
        // Both should match: global always, git_style by keywords.
        assert_eq!(picked.len(), 2);
        assert!(picked.iter().any(|p| p.slug == "global_style"));
        assert!(picked.iter().any(|p| p.slug == "git_style"));
    }

    #[test]
    fn format_for_prompt_output() {
        let prefs = vec![Preference {
            slug: "git_style".into(),
            keywords: Vec::new(),
            scope: Some("git".into()),
            summary: None,
            body: "Use imperative mood.".into(),
            token_estimate: 5,
        }];
        let out = format_for_prompt(&prefs);
        assert!(out.starts_with("## User Preferences"));
        assert!(out.contains("### git_style (scope: git)"));
        assert!(out.contains("Use imperative mood."));
    }

    #[test]
    fn format_for_prompt_empty() {
        assert!(format_for_prompt(&[]).is_empty());
    }
}
