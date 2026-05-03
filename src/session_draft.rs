//! Session-summary draft persistence (EP-00015 Decision E2).
//!
//! Called from `SessionManager::end_session` — pulls the segment's turns from
//! SQLite, runs a cheap LLM summarization, writes a markdown draft to
//! `data/drafts/sessions/<group_first_8>-<segment_compact_iso>.md`. The
//! pusher (Decision A2) backs it up to AKW on its next cycle.
//!
//! Skipped for `channel_type == "task"` per the channel filter — task channels
//! produce research drafts (Decision E) and have trivial single-round-trip
//! sessions. Writing one session draft per task execution would be noise.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use tracing::{debug, info, warn};

use crate::agent_loop::AgentLoop;
use crate::db::{ConversationMessage, Database};

const DRAFT_DIR: &str = "data/drafts/sessions";

const DEFAULT_PER_TURN_BYTE_CAP: usize = 2048;
const DEFAULT_TOTAL_APPENDIX_BYTE_CAP: usize = 50_000;

/// Write a session-summary draft for the segment that just ended.
///
/// Returns `Ok(None)` when the draft is intentionally skipped (task channel
/// or no turns found). Returns `Ok(Some(path))` on a successful write.
pub async fn write_session_draft(
    root_dir: &Path,
    agent_loop: &AgentLoop,
    db: &Database,
    conv_id: &str,
    group_id: Option<&str>,
    channel_type: &str,
    segment_started_at: DateTime<Utc>,
    segment_ended_at: DateTime<Utc>,
    include_turns: bool,
) -> Result<Option<PathBuf>, String> {
    if channel_type == "task" {
        debug!(conv_id, "session_draft: task channel, skipping");
        return Ok(None);
    }

    let started_iso = segment_started_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let ended_iso = segment_ended_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let turns = db
        .load_final_turns_in_window(conv_id, &started_iso, &ended_iso)
        .map_err(|e| format!("load_final_turns_in_window: {}", e))?;

    if turns.is_empty() {
        debug!(conv_id, "session_draft: no turns in segment window, skipping");
        return Ok(None);
    }

    let summary = run_summary(agent_loop, &turns).await;

    let dir = root_dir.join(DRAFT_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;

    let group_short = group_id
        .map(|gid| gid.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "no-group".to_string());
    let segment_compact = segment_started_at
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let primary = dir.join(format!("{}-{}.md", group_short, segment_compact));
    let target = pick_unique_path(&primary);

    let body = render_session_draft(
        agent_loop.agent_name.as_str(),
        conv_id,
        group_id,
        channel_type,
        segment_started_at,
        segment_ended_at,
        &turns,
        &summary,
        include_turns,
    );

    std::fs::write(&target, &body)
        .map_err(|e| format!("failed to write {}: {}", target.display(), e))?;
    info!(
        path = %target.display(),
        bytes = body.len(),
        turns = turns.len(),
        "session draft written"
    );
    Ok(Some(target))
}

fn pick_unique_path(primary: &Path) -> PathBuf {
    if !primary.exists() {
        return primary.to_path_buf();
    }
    let stem = primary
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    let parent = primary.parent().unwrap_or_else(|| Path::new("."));
    for n in 2..1000 {
        let cand = parent.join(format!("{}-{}.md", stem, n));
        if !cand.exists() {
            return cand;
        }
    }
    parent.join(format!("{}-{}.md", stem, chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)))
}

/// LLM summarization of the segment's turns.
async fn run_summary(agent_loop: &AgentLoop, turns: &[ConversationMessage]) -> String {
    let system = "You write concise session summaries for an archived agent conversation. \
        Output 4-8 sentences in plain markdown. Cover: what the user asked, what the agent did, \
        any decisions or commitments made, anything left unresolved. Do not invent details.";
    let mut user = String::with_capacity(2048);
    user.push_str("Conversation turns (oldest first):\n\n");
    for turn in turns.iter().take(20) {
        let role = match turn.role.as_str() {
            "user" => "User",
            "assistant" => "Agent",
            other => other,
        };
        let content: String = turn.content.chars().take(2000).collect();
        user.push_str(&format!("**{}**: {}\n\n", role, content));
    }

    let response = agent_loop.cheap_call(system, &user).await;
    if response.starts_with("LLM call failed") || response.starts_with("I'm sorry, all models failed") {
        warn!("session_draft: LLM summarization failed, using minimal stub");
        return format!(
            "(LLM summarization unavailable — {} turns recorded; see Turns appendix.)",
            turns.len()
        );
    }
    response
}

#[allow(clippy::too_many_arguments)]
fn render_session_draft(
    agent_name: &str,
    conv_id: &str,
    group_id: Option<&str>,
    channel_type: &str,
    started: DateTime<Utc>,
    ended: DateTime<Utc>,
    turns: &[ConversationMessage],
    summary: &str,
    include_turns: bool,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("agent: {}\n", agent_name));
    if let Some(gid) = group_id {
        out.push_str(&format!("group_id: {}\n", gid));
    }
    out.push_str(&format!("conv_id: {}\n", conv_id));
    out.push_str(&format!("channel_type: {}\n", channel_type));
    out.push_str(&format!(
        "segment_started_at: {}\n",
        started.to_rfc3339_opts(SecondsFormat::Secs, true)
    ));
    out.push_str(&format!(
        "segment_ended_at: {}\n",
        ended.to_rfc3339_opts(SecondsFormat::Secs, true)
    ));
    out.push_str(&format!("turn_count: {}\n", turns.len()));
    out.push_str("source: session_draft\n");
    out.push_str("---\n\n");
    out.push_str("## Summary\n\n");
    out.push_str(summary.trim());
    out.push('\n');

    if include_turns {
        out.push_str("\n## Turns\n\n");
        out.push_str(&render_turns_appendix(turns, conv_id));
    }
    out
}

fn render_turns_appendix(turns: &[ConversationMessage], conv_id: &str) -> String {
    let per_turn_cap = DEFAULT_PER_TURN_BYTE_CAP;
    let total_cap = DEFAULT_TOTAL_APPENDIX_BYTE_CAP;

    // Render newest-last; if total exceeds cap, drop oldest first.
    let mut rendered: Vec<String> = Vec::with_capacity(turns.len());
    for turn in turns {
        let role = match turn.role.as_str() {
            "user" => "User",
            "assistant" => "Agent",
            other => other,
        };
        let body = if turn.content.len() > per_turn_cap {
            let truncated: String = turn.content.chars().take(per_turn_cap).collect();
            format!(
                "**{}** ({}): {}\n\n[... truncated, see SQLite conv_id={}]",
                role, turn.created_at, truncated, conv_id
            )
        } else {
            format!("**{}** ({}): {}", role, turn.created_at, turn.content)
        };
        rendered.push(body);
    }

    // Compose under the total cap, dropping oldest first if needed.
    let mut total: usize = rendered.iter().map(|s| s.len() + 2).sum::<usize>();
    let mut omitted = 0usize;
    while total > total_cap && !rendered.is_empty() {
        let dropped = rendered.remove(0);
        total = total.saturating_sub(dropped.len() + 2);
        omitted += 1;
    }

    let mut out = String::new();
    if omitted > 0 {
        out.push_str(&format!(
            "[... {} earlier turn(s) omitted, see SQLite conv_id={}]\n\n",
            omitted, conv_id
        ));
    }
    out.push_str(&rendered.join("\n\n"));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: &str, content: &str, created_at: &str) -> ConversationMessage {
        ConversationMessage {
            id: 1,
            conversation_id: "c1".into(),
            agent_name: "ino".into(),
            role: role.into(),
            content: content.into(),
            channel_type: "cli".into(),
            model_used: None,
            input_tokens: 0,
            output_tokens: 0,
            turn_id: "t".into(),
            is_final: true,
            metadata: None,
            created_at: created_at.into(),
        }
    }

    #[test]
    fn pick_unique_path_reuses_when_free() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("a.md");
        assert_eq!(pick_unique_path(&primary), primary);
    }

    #[test]
    fn pick_unique_path_collides() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("a.md");
        std::fs::write(&primary, "x").unwrap();
        let suffixed = pick_unique_path(&primary);
        assert!(suffixed.file_name().unwrap().to_string_lossy().ends_with("-2.md"));
    }

    #[test]
    fn render_session_draft_basic() {
        let turns = vec![
            turn("user", "hello", "2026-05-03T10:00:00Z"),
            turn("assistant", "hi there", "2026-05-03T10:00:01Z"),
        ];
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-03T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ended = chrono::DateTime::parse_from_rfc3339("2026-05-03T10:00:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let out = render_session_draft(
            "ino",
            "c1",
            Some("group-abc12345"),
            "cli",
            started,
            ended,
            &turns,
            "Brief summary.",
            true,
        );
        assert!(out.contains("agent: ino"));
        assert!(out.contains("group_id: group-abc12345"));
        assert!(out.contains("conv_id: c1"));
        assert!(out.contains("channel_type: cli"));
        assert!(out.contains("turn_count: 2"));
        assert!(out.contains("## Summary"));
        assert!(out.contains("Brief summary."));
        assert!(out.contains("## Turns"));
        assert!(out.contains("**User**"));
        assert!(out.contains("**Agent**"));
    }

    #[test]
    fn render_omits_turns_appendix_when_disabled() {
        let turns = vec![turn("user", "hi", "2026-05-03T00:00:00Z")];
        let started = chrono::Utc::now();
        let ended = chrono::Utc::now();
        let out = render_session_draft(
            "ino",
            "c1",
            None,
            "cli",
            started,
            ended,
            &turns,
            "summary",
            false,
        );
        assert!(!out.contains("## Turns"));
    }

    #[test]
    fn turns_appendix_truncates_long_turn() {
        let big = "x".repeat(5000);
        let turns = vec![turn("assistant", &big, "2026-05-03T00:00:00Z")];
        let out = render_turns_appendix(&turns, "c1");
        assert!(out.contains("[... truncated"));
        assert!(out.len() < 5500); // capped
    }

    #[test]
    fn turns_appendix_drops_oldest_over_total_cap() {
        let mut turns = Vec::new();
        for i in 0..200 {
            turns.push(turn("user", &format!("turn-{} {}", i, "y".repeat(800)), "2026-05-03T00:00:00Z"));
        }
        let out = render_turns_appendix(&turns, "c1");
        assert!(out.contains("earlier turn"));
    }
}
