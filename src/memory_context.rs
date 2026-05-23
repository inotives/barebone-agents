//! Prior-work prompt helper.
//!
//! Prior-work search is currently a local-only extension point. The public
//! helper functions remain so callers keep a stable shape while local
//! prior-work search can be introduced in a later EP.

use tokio::sync::Mutex;

use crate::session::SessionManager;
use crate::tools::ToolRegistry;

/// Placeholder for future local prior-work search. Returns no entries today.
pub async fn build_prior_work_block(
    _registry: &ToolRegistry,
    _query: &str,
    _top_k: u32,
    _token_budget: u32,
) -> Vec<String> {
    Vec::new()
}

/// Cached variant: stores the result on `ActiveSession` so subsequent turns
/// in the same segment reuse it.
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

/// Format a "previous run result" block for recurring tasks (Decision C).
/// Returns empty string for empty / failure-prefixed results (Q4 default).
pub fn format_previous_run_result(result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("I'm sorry, all models failed") || trimmed.starts_with("LLM call failed")
    {
        return String::new();
    }
    let preview: String = trimmed.chars().take(1500).collect();
    format!("## Previous Run Result\n\n{}", preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_previous_run_result_empty() {
        assert!(format_previous_run_result("").is_empty());
        assert!(format_previous_run_result("   ").is_empty());
    }

    #[test]
    fn format_previous_run_result_skips_failures() {
        assert!(format_previous_run_result("I'm sorry, all models failed: x").is_empty());
        assert!(format_previous_run_result("LLM call failed: x").is_empty());
    }

    #[test]
    fn format_previous_run_result_formats() {
        let out = format_previous_run_result("done");
        assert_eq!(out, "## Previous Run Result\n\ndone");
    }

    #[test]
    fn format_previous_run_result_truncates() {
        let long = "x".repeat(2000);
        let out = format_previous_run_result(&long);
        assert!(out.len() < 1600);
        assert!(out.starts_with("## Previous Run Result"));
    }
}
