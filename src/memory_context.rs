//! Prior-work search helper (EP-00015 Decision B + Phase 3).
//!
//! Runs a single `mcp_akw__memory_search` against `task.title + description`
//! (or the first user message of a conversation segment), excludes the
//! `2_knowledges/preferences/**` paths to avoid duplicating the User
//! Preferences block, fetches each hit's full content via `memory_read`,
//! and packs results into a token budget.
//!
//! Returns `Vec<String>` where each entry is one hit's content prefixed by
//! its AKW path. Empty Vec when AKW is absent / no hits / read failures —
//! the caller injects `## Prior Work` only when the Vec is non-empty.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::preferences;
use crate::session::SessionManager;
use crate::tools::ToolRegistry;

const PRIOR_WORK_PATH_EXCLUDES: &[&str] = &["2_knowledges/preferences/"];

/// Hard cap on the harness query — anything past this is just noise to BM25.
const QUERY_CHAR_CAP: usize = 200;

/// Run a memory search across the prior-work tiers and return formatted entries.
///
/// `query` will be trimmed and capped to `QUERY_CHAR_CAP` chars internally.
pub async fn build_prior_work_block(
    registry: &ToolRegistry,
    query: &str,
    top_k: u32,
    token_budget: u32,
) -> Vec<String> {
    if !registry.has("mcp_akw__memory_search") {
        debug!("prior-work: AKW not configured, skipping");
        return Vec::new();
    }

    let trimmed = trim_query(query);
    if trimmed.is_empty() {
        return Vec::new();
    }

    let hits = match search_across_tiers(registry, &trimmed, top_k as usize).await {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "prior-work: memory_search failed");
            return Vec::new();
        }
    };

    // Filter out preference paths (Decision B) — they're already injected via
    // `## User Preferences`, and re-surfacing them would duplicate.
    let hits: Vec<MemoryHit> = hits
        .into_iter()
        .filter(|h| !PRIOR_WORK_PATH_EXCLUDES.iter().any(|prefix| h.path.starts_with(prefix)))
        .collect();

    if hits.is_empty() {
        return Vec::new();
    }

    // Read each hit's full content + pack into the budget.
    let mut entries = Vec::new();
    let mut used: usize = 0;
    let cap = token_budget as usize;
    for hit in hits {
        let body = match read_full(registry, &hit.path).await {
            Some(b) => b,
            None => continue,
        };
        let entry = format!("### {}\n\n{}", hit.path, body.trim());
        let len = entry.len();
        if used + len > cap {
            // Truncate this entry to fit; if even truncated entry is tiny,
            // skip it. Per Decision B "truncate longest first" — but for v1
            // a simpler "stop at the cap" is fine.
            let remaining = cap.saturating_sub(used);
            if remaining < 200 {
                break;
            }
            let mut chunk = entry.chars().take(remaining).collect::<String>();
            chunk.push_str("\n\n[... truncated]");
            entries.push(chunk);
            used += remaining;
            break;
        }
        entries.push(entry);
        used += len;
    }
    entries
}

/// Cached variant: stores the result on `ActiveSession` so subsequent turns
/// in the same segment reuse it (per Decision B's "first turn" guidance).
pub async fn build_prior_work_cached(
    registry: &ToolRegistry,
    session_mgr: &Mutex<SessionManager>,
    conv_id: &str,
    query: &str,
    top_k: u32,
    token_budget: u32,
) -> Vec<String> {
    {
        let mgr = session_mgr.lock().await;
        if let Some(cached) = mgr.get_prior_work(conv_id) {
            return cached;
        }
    }
    let entries = build_prior_work_block(registry, query, top_k, token_budget).await;
    {
        let mut mgr = session_mgr.lock().await;
        mgr.set_prior_work(conv_id, entries.clone());
    }
    entries
}

/// One memory_search hit.
struct MemoryHit {
    path: String,
}

fn trim_query(query: &str) -> String {
    let trimmed = query.trim();
    if trimmed.len() <= QUERY_CHAR_CAP {
        return trimmed.to_string();
    }
    trimmed.chars().take(QUERY_CHAR_CAP).collect()
}

/// Search across the configured tiers, dedupe by path, return top_k.
///
/// Per Decision B: union of `knowledge`, `research_draft`, `session_archived`.
/// Per-tier search results are merged; we don't try to interleave by score
/// because BM25 scores aren't comparable across separate searches.
async fn search_across_tiers(
    registry: &ToolRegistry,
    query: &str,
    top_k: usize,
) -> Result<Vec<MemoryHit>, String> {
    let tiers = ["knowledge", "research_draft", "session_archived"];

    let mut all = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for tier in &tiers {
        let raw = registry
            .execute(
                "mcp_akw__memory_search",
                serde_json::json!({"query": query, "tier": tier, "limit": top_k}),
            )
            .await;

        let json: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let items: Vec<Value> = if let Some(arr) = json.get("result").and_then(|r| r.as_array()) {
            arr.clone()
        } else if let Some(arr) = json.get("results").and_then(|r| r.as_array()) {
            arr.clone()
        } else if let Some(arr) = json.as_array() {
            arr.clone()
        } else {
            continue;
        };

        for item in items {
            let Some(path) = item.get("path").and_then(|p| p.as_str()) else {
                continue;
            };
            if !seen.insert(path.to_string()) {
                continue;
            }
            all.push(MemoryHit { path: path.to_string() });
            if all.len() >= top_k {
                break;
            }
        }
        if all.len() >= top_k {
            break;
        }
    }

    Ok(all)
}

/// Read a memory page's full content via the registry. Returns None on error
/// or empty content.
async fn read_full(registry: &ToolRegistry, path: &str) -> Option<String> {
    let raw = registry
        .execute(
            "mcp_akw__memory_read",
            serde_json::json!({"path": path}),
        )
        .await;

    if let Ok(json) = serde_json::from_str::<Value>(&raw) {
        if let Some(c) = json.get("content").and_then(|v| v.as_str()) {
            if c.trim().is_empty() {
                return None;
            }
            return Some(c.to_string());
        }
    }
    if !raw.trim().is_empty() && !raw.starts_with("Error") {
        Some(raw)
    } else {
        None
    }
}

/// Format a "previous run result" block for recurring tasks (Decision C).
/// Returns empty string for empty / failure-prefixed results (Q4 default).
pub fn format_previous_run_result(result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("I'm sorry, all models failed")
        || trimmed.starts_with("LLM call failed")
    {
        return String::new();
    }
    let preview: String = trimmed.chars().take(1500).collect();
    format!("## Previous Run Result\n\n{}", preview)
}

// Suppress "preferences not used here" warnings if compiled with feature gates.
#[allow(dead_code)]
fn _ensure_preferences_compiles(_: &Arc<preferences::Preference>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_query_short() {
        assert_eq!(trim_query("hello"), "hello");
    }

    #[test]
    fn trim_query_caps_long() {
        let long = "a".repeat(500);
        let out = trim_query(&long);
        assert_eq!(out.len(), QUERY_CHAR_CAP);
    }

    #[test]
    fn format_previous_run_result_empty() {
        assert!(format_previous_run_result("").is_empty());
        assert!(format_previous_run_result("   ").is_empty());
    }

    #[test]
    fn format_previous_run_result_skips_failures() {
        assert!(format_previous_run_result("I'm sorry, all models failed: x").is_empty());
        assert!(format_previous_run_result("LLM call failed during tool loop").is_empty());
    }

    #[test]
    fn format_previous_run_result_normal() {
        let out = format_previous_run_result("The market closed up 2.5% today.");
        assert!(out.starts_with("## Previous Run Result"));
        assert!(out.contains("market closed up"));
    }

    #[test]
    fn format_previous_run_result_truncates_long() {
        let long = "x".repeat(3000);
        let out = format_previous_run_result(&long);
        // Header + cap of 1500 chars.
        assert!(out.contains("## Previous Run Result"));
        assert!(out.len() < 1700);
    }
}
