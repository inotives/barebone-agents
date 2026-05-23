use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};

use crate::tools::ToolRegistry;

/// Manages active local conversation sessions across channels.
pub struct SessionManager {
    sessions: HashMap<String, ActiveSession>,
    ttl: Duration,
}

struct ActiveSession {
    channel_type: String,
    recommended_context: Vec<String>,
    /// Per-segment preference selection cache (EP-00015 Decision A).
    /// Populated lazily on the first turn that calls `set_selected_preferences`.
    /// Empty Vec means "not yet computed"; cleared on session end.
    selected_preferences: Vec<String>,
    /// Per-segment prior-work cache (EP-00015 Decision B).
    /// `None` = not yet computed for this segment; `Some(vec)` = computed.
    prior_work: Option<Vec<String>>,
    /// Wall-clock segment-start timestamp; used by the session draft producer
    /// to bound the SQLite turn query (EP-00015 Decision E2).
    segment_started_at: chrono::DateTime<chrono::Utc>,
    last_activity: Instant,
}

impl SessionManager {
    pub fn new(
        _agent_name: &str,
        _project_id: Option<&str>,
        ttl_minutes: u32,
        _registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            ttl: Duration::from_secs(ttl_minutes as u64 * 60),
        }
    }

    /// Ensure a session exists for the given conversation.
    /// Returns recommended context. This is local-only and currently empty.
    pub async fn ensure_session(&mut self, conv_id: &str, channel_type: &str) -> Vec<String> {
        if let Some(session) = self.sessions.get_mut(conv_id) {
            if channel_type == "discord" && session.last_activity.elapsed() > self.ttl {
                self.sessions.remove(conv_id);
            } else {
                session.last_activity = Instant::now();
                return session.recommended_context.clone();
            }
        }

        let context = Vec::new();
        let session = ActiveSession {
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
    /// `None` previously -> set to `Some(prior_work)`; subsequent calls reuse.
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

    /// Get the segment's start timestamp for SQLite turn-window queries.
    pub fn get_segment_started_at(&self, conv_id: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        self.sessions.get(conv_id).map(|s| s.segment_started_at)
    }

    /// Get the channel_type the session was opened with.
    pub fn get_channel_type(&self, conv_id: &str) -> Option<String> {
        self.sessions.get(conv_id).map(|s| s.channel_type.clone())
    }

    /// All active conversation IDs. Snapshot — caller can iterate without
    /// holding the lock.
    pub fn active_conv_ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Log a turn to the active session.
    ///
    /// SQLite is the canonical turn store (already populated by
    /// `agent_loop.run`). This method is a tracing-only no-op kept for API
    /// compatibility with existing call sites.
    pub async fn log_turn(&self, conv_id: &str, _request: &str, _response: &str) {
        debug!(conv_id = %conv_id, "log_turn called (SQLite is canonical)");
    }

    /// End a specific session by conversation ID.
    pub async fn end_session(&mut self, conv_id: &str) {
        if self.sessions.remove(conv_id).is_some() {
            info!(conv_id = %conv_id, "session ended");
        }
    }

    /// End all active sessions (for shutdown).
    pub async fn end_all(&mut self) {
        self.sessions.clear();
        info!("all sessions ended");
    }

    /// Get recommended context for a conversation.
    pub fn get_recommended_context(&self, conv_id: &str) -> Vec<String> {
        self.sessions
            .get(conv_id)
            .map(|s| s.recommended_context.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_registry() -> Arc<ToolRegistry> {
        Arc::new(ToolRegistry::new())
    }

    #[tokio::test]
    async fn test_ensure_session_local_context_empty() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        let context = mgr.ensure_session("conv-1", "cli").await;
        assert!(context.is_empty());
        assert!(mgr.sessions.contains_key("conv-1"));
    }

    #[tokio::test]
    async fn test_ensure_session_reuse() {
        let mut mgr = SessionManager::new("ino", None, 30, mock_registry());
        mgr.ensure_session("conv-1", "cli").await;
        mgr.ensure_session("conv-1", "cli").await;
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
}
