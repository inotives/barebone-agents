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
            last_activity: Instant::now(),
        };
        self.sessions.insert(conv_id.to_string(), session);

        context
    }

    /// Log a turn to the active session.
    pub async fn log_turn(&self, conv_id: &str, request: &str, response: &str) {
        let session = match self.sessions.get(conv_id) {
            Some(s) => s,
            None => return,
        };

        if session.group_id.is_none() {
            return; // No AKW group active
        }

        // Truncate to 500 chars
        let req_truncated: String = request.chars().take(500).collect();
        let resp_truncated: String = response.chars().take(500).collect();

        // group_log keys onto the active group internally when group_id is omitted.
        // Payload shape: {turns: [{request, response}]}.
        let _ = self
            .registry
            .execute(
                "mcp_akw__group_log",
                json!({
                    "turns": [{
                        "request": req_truncated,
                        "response": resp_truncated,
                    }],
                }),
            )
            .await;

        debug!(conv_id = %conv_id, "session turn logged");
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
