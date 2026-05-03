use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::tools::ToolRegistry;

/// Manages AKW sessions across channels.
/// Best-effort — if AKW MCP is unavailable, silently skips.
///
/// On the wire, AKW now exposes a "group" lifecycle (group_start/group_log/group_end);
/// we keep `session` naming on our side because that's our domain (a CLI/Discord conversation).
pub struct SessionManager {
    sessions: HashMap<String, ActiveSession>,
    agent_name: String,
    project_id: Option<String>,
    ttl: Duration,
    registry: Arc<ToolRegistry>,
}

struct ActiveSession {
    group_id: Option<String>,
    conv_id: String,
    channel_type: String,
    recommended_context: Vec<String>,
    /// Per-segment preference selection cache (EP-00015 Decision A).
    /// Populated lazily on the first turn that calls `set_selected_preferences`.
    /// Empty Vec means "not yet computed"; cleared on session end.
    selected_preferences: Vec<String>,
    /// Per-segment prior-work cache (EP-00015 Decision B).
    /// `None` = not yet computed for this segment; `Some(vec)` = computed
    /// (vec may be empty if AKW returned no hits or AKW is absent).
    prior_work: Option<Vec<String>>,
    /// Wall-clock segment-start timestamp; used by the session draft producer
    /// to bound the SQLite turn query (EP-00015 Decision E2).
    segment_started_at: chrono::DateTime<chrono::Utc>,
    last_activity: Instant,
}

impl SessionManager {
    pub fn new(
        agent_name: &str,
        project_id: Option<&str>,
        ttl_minutes: u32,
        registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            agent_name: agent_name.to_string(),
            project_id: project_id.map(String::from),
            ttl: Duration::from_secs(ttl_minutes as u64 * 60),
            registry,
        }
    }

    /// Ensure a session exists for the given conversation.
    /// Returns recommended context from AKW (empty if unavailable).
    pub async fn ensure_session(
        &mut self,
        conv_id: &str,
        channel_type: &str,
    ) -> Vec<String> {
        // Check if we have an active session for this conv_id
        if let Some(session) = self.sessions.get_mut(conv_id) {
            // For Discord, check TTL
            if channel_type == "discord" && session.last_activity.elapsed() > self.ttl {
                // TTL expired — end old session, start new one
                let had_group = session.group_id.is_some();
                self.sessions.remove(conv_id);
                if had_group {
                    self.end_akw_session().await;
                }
                // Fall through to create new session
            } else {
                // Session still active — refresh activity
                session.last_activity = Instant::now();
                return session.recommended_context.clone();
            }
        }

        // Start new session
        let (group_id, context) = self.start_akw_session(conv_id, channel_type).await;

        let session = ActiveSession {
            group_id,
            conv_id: conv_id.to_string(),
            channel_type: channel_type.to_string(),
            recommended_context: context.clone(),
            selected_preferences: Vec::new(),
            prior_work: None,
            segment_started_at: chrono::Utc::now(),
            last_activity: Instant::now(),
        };
        self.sessions.insert(conv_id.to_string(), session);

        context
    }

    /// Cache the per-segment prior-work selection (EP-00015 Decision B).
    /// `None` previously → set to `Some(prior_work)`; subsequent calls reuse.
    pub fn set_prior_work(&mut self, conv_id: &str, prior_work: Vec<String>) {
        if let Some(session) = self.sessions.get_mut(conv_id) {
            session.prior_work = Some(prior_work);
        }
    }

    /// Get the cached prior-work selection. `None` = not yet computed for
    /// this segment; caller should compute and cache via `set_prior_work`.
    pub fn get_prior_work(&self, conv_id: &str) -> Option<Vec<String>> {
        self.sessions
            .get(conv_id)
            .and_then(|s| s.prior_work.clone())
    }

    /// Cache the per-segment preference selection (EP-00015 Decision A).
    /// Subsequent turns within the same segment reuse this cache.
    pub fn set_selected_preferences(&mut self, conv_id: &str, prefs: Vec<String>) {
        if let Some(session) = self.sessions.get_mut(conv_id) {
            session.selected_preferences = prefs;
        }
    }

    /// Retrieve the cached preference selection for this segment.
    /// Empty Vec = not yet populated (or no matches).
    pub fn get_selected_preferences(&self, conv_id: &str) -> Vec<String> {
        self.sessions
            .get(conv_id)
            .map(|s| s.selected_preferences.clone())
            .unwrap_or_default()
    }

    /// Get the active session's group_id for AKW-bound writes.
    pub fn get_group_id(&self, conv_id: &str) -> Option<String> {
        self.sessions
            .get(conv_id)
            .and_then(|s| s.group_id.clone())
    }

    /// Get the segment's start timestamp for SQLite turn-window queries.
    pub fn get_segment_started_at(&self, conv_id: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        self.sessions.get(conv_id).map(|s| s.segment_started_at)
    }

    /// Get the channel_type the session was opened with.
    pub fn get_channel_type(&self, conv_id: &str) -> Option<String> {
        self.sessions
            .get(conv_id)
            .map(|s| s.channel_type.clone())
    }

    /// All active conversation IDs. Snapshot — caller can iterate without
    /// holding the lock.
    pub fn active_conv_ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Log a turn to the active session.
    ///
    /// EP-00015 Decision E2: per-turn `mcp_akw__group_log` is dropped. SQLite
    /// is the canonical turn store (already populated by `agent_loop.run`).
    /// This method is now a tracing-only no-op kept for API compatibility
    /// with existing call sites; it will be removed once all callers stop
    /// invoking it.
    pub async fn log_turn(&self, conv_id: &str, _request: &str, _response: &str) {
        debug!(conv_id = %conv_id, "log_turn called (no-op since EP-00015 — SQLite is canonical)");
    }

    /// End a specific session by conversation ID.
    pub async fn end_session(&mut self, conv_id: &str) {
        if let Some(session) = self.sessions.remove(conv_id) {
            if session.group_id.is_some() {
                self.end_akw_session().await;
            }
            info!(conv_id = %conv_id, "session ended");
        }
    }

    /// End all active sessions (for shutdown).
    pub async fn end_all(&mut self) {
        let conv_ids: Vec<String> = self.sessions.keys().cloned().collect();
        for conv_id in conv_ids {
            if let Some(session) = self.sessions.remove(&conv_id) {
                if session.group_id.is_some() {
                    self.end_akw_session().await;
                }
            }
        }
        info!("all sessions ended");
    }

    /// Get recommended context for a conversation (cached from session_start).
    pub fn get_recommended_context(&self, conv_id: &str) -> Vec<String> {
        self.sessions
            .get(conv_id)
            .map(|s| s.recommended_context.clone())
            .unwrap_or_default()
    }

    /// Check if AKW MCP tools are available.
    fn has_akw(&self) -> bool {
        self.registry.has("mcp_akw__group_start")
    }

    async fn start_akw_session(
        &self,
        conv_id: &str,
        channel_type: &str,
    ) -> (Option<String>, Vec<String>) {
        if !self.has_akw() {
            return (None, Vec::new());
        }

        // group_start args: agent + metadata. project_id moves into metadata
        // (no longer a top-level field).
        let mut metadata = json!({
            "conv_id": conv_id,
            "channel": channel_type,
        });
        if let Some(pid) = &self.project_id {
            metadata["project_id"] = json!(pid);
        }

        let args = json!({
            "agent": self.agent_name,
            "metadata": metadata,
        });

        let result = self
            .registry
            .execute("mcp_akw__group_start", args)
            .await;

        // Parse response for group_id and recommended_context
        if let Ok(json) = serde_json::from_str::<Value>(&result) {
            let group_id = json
                .get("group_id")
                .and_then(|id| id.as_str())
                .map(String::from);

            let context = json
                .get("recommended_context")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            item.get("content")
                                .and_then(|c| c.as_str())
                                .map(String::from)
                        })
                        .collect()
                })
                .unwrap_or_default();

            if let Some(ref gid) = group_id {
                info!(group_id = %gid, conv_id = %conv_id, "AKW group started");
            }

            (group_id, context)
        } else {
            warn!(conv_id = %conv_id, "failed to parse AKW group_start response");
            (None, Vec::new())
        }
    }

    async fn end_akw_session(&self) {
        if !self.has_akw() {
            return;
        }

        // group_end takes no args — closes the active segment for the active group.
        let _ = self
            .registry
            .execute("mcp_akw__group_end", json!({}))
            .await;

        debug!("AKW group segment ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_registry() -> Arc<ToolRegistry> {
        Arc::new(ToolRegistry::new())
    }

    #[tokio::test]
    async fn test_ensure_session_no_akw() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        let context = mgr.ensure_session("conv-1", "cli").await;
        assert!(context.is_empty()); // no AKW, empty context
        assert!(mgr.sessions.contains_key("conv-1"));
    }

    #[tokio::test]
    async fn test_ensure_session_reuse() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        mgr.ensure_session("conv-1", "cli").await;
        mgr.ensure_session("conv-1", "cli").await; // should reuse
        assert_eq!(mgr.sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_ensure_session_multiple_convs() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        mgr.ensure_session("conv-1", "cli").await;
        mgr.ensure_session("conv-2", "cli").await;
        assert_eq!(mgr.sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_end_session() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        mgr.ensure_session("conv-1", "cli").await;
        assert_eq!(mgr.sessions.len(), 1);

        mgr.end_session("conv-1").await;
        assert_eq!(mgr.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_end_all() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        mgr.ensure_session("conv-1", "cli").await;
        mgr.ensure_session("conv-2", "discord").await;
        assert_eq!(mgr.sessions.len(), 2);

        mgr.end_all().await;
        assert_eq!(mgr.sessions.len(), 0);
    }

    #[tokio::test]
    async fn test_get_recommended_context_empty() {
        let mgr = SessionManager::new("ino", None, 30, mock_registry());
        let ctx = mgr.get_recommended_context("nonexistent");
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_has_akw_false() {
        let mgr = SessionManager::new("ino", None, 30, mock_registry());
        assert!(!mgr.has_akw());
    }

    #[test]
    fn test_has_akw_true() {
        let mut reg = ToolRegistry::new();
        reg.register("mcp_akw__group_start", "Start", json!({}), |_| async {
            "ok".into()
        });
        let mgr = SessionManager::new("ino", None, 30, Arc::new(reg));
        assert!(mgr.has_akw());
    }
}
