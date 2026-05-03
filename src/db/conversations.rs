use super::schema::Database;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub id: i64,
    pub conversation_id: String,
    pub agent_name: String,
    pub role: String,
    pub content: String,
    pub channel_type: String,
    pub model_used: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub turn_id: String,
    pub is_final: bool,
    pub metadata: Option<String>,
    pub created_at: String,
}

pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub agent_name: String,
    pub channel_type: String,
    pub turn_count: i64,
    pub message_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub first_message_at: String,
    pub last_message_at: String,
}

impl Database {
    /// Save a message to the conversations table.
    pub fn save_message(
        &self,
        conversation_id: &str,
        agent_name: &str,
        role: &str,
        content: &str,
        channel_type: &str,
        model_used: Option<&str>,
        input_tokens: i64,
        output_tokens: i64,
        turn_id: &str,
        is_final: bool,
        metadata: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (conversation_id, agent_name, role, content, channel_type, \
             model_used, input_tokens, output_tokens, turn_id, is_final, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                conversation_id,
                agent_name,
                role,
                content,
                channel_type,
                model_used,
                input_tokens,
                output_tokens,
                turn_id,
                is_final,
                metadata,
            ],
        )
        .map_err(|e| format!("Failed to save message: {}", e))?;
        Ok(conn.last_insert_rowid())
    }

    /// Load conversation history for LLM context (is_final=1 only, most recent N).
    pub fn load_history(
        &self,
        conversation_id: &str,
        limit: u32,
    ) -> Result<Vec<ConversationMessage>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, agent_name, role, content, channel_type, \
                 model_used, input_tokens, output_tokens, turn_id, is_final, metadata, created_at \
                 FROM conversations \
                 WHERE conversation_id = ?1 AND is_final = 1 \
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(|e| format!("Failed to prepare load_history: {}", e))?;

        let mut messages: Vec<ConversationMessage> = stmt
            .query_map(rusqlite::params![conversation_id, limit], |row| {
                Ok(ConversationMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    channel_type: row.get(5)?,
                    model_used: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    turn_id: row.get(9)?,
                    is_final: row.get(10)?,
                    metadata: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to query history: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Reverse to chronological order
        messages.reverse();
        Ok(messages)
    }

    /// Load all final user/assistant messages for a conversation in a time
    /// window. Used by the EP-00015 session-draft producer (`session_draft.rs`)
    /// to gather the segment's turns from SQLite.
    ///
    /// Both bounds are inclusive (RFC3339 strings comparable to
    /// `created_at`). Tool messages are excluded — they're per-turn detail,
    /// not user-facing turns.
    pub fn load_final_turns_in_window(
        &self,
        conversation_id: &str,
        started_at: &str,
        ended_at: &str,
    ) -> Result<Vec<ConversationMessage>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, agent_name, role, content, channel_type, \
                 model_used, input_tokens, output_tokens, turn_id, is_final, metadata, created_at \
                 FROM conversations \
                 WHERE conversation_id = ?1 AND is_final = 1 \
                 AND role IN ('user', 'assistant') \
                 AND created_at >= ?2 AND created_at <= ?3 \
                 ORDER BY id",
            )
            .map_err(|e| format!("Failed to prepare turns_in_window: {}", e))?;

        let messages = stmt
            .query_map(
                rusqlite::params![conversation_id, started_at, ended_at],
                |row| {
                    Ok(ConversationMessage {
                        id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        agent_name: row.get(2)?,
                        role: row.get(3)?,
                        content: row.get(4)?,
                        channel_type: row.get(5)?,
                        model_used: row.get(6)?,
                        input_tokens: row.get(7)?,
                        output_tokens: row.get(8)?,
                        turn_id: row.get(9)?,
                        is_final: row.get(10)?,
                        metadata: row.get(11)?,
                        created_at: row.get(12)?,
                    })
                },
            )
            .map_err(|e| format!("Failed to query turns_in_window: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(messages)
    }

    /// Get the conversation's turn count (distinct turn_id values) for any
    /// purpose that needs "is this the first turn?" detection (EP-00015
    /// Phase 3 — first-turn prior-work search gate).
    pub fn get_conversation_turn_count(&self, conversation_id: &str) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(DISTINCT turn_id) FROM conversations WHERE conversation_id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to count turns: {}", e))
    }

    /// Load all messages in a turn (for debug/audit).
    pub fn load_full_turn(&self, turn_id: &str) -> Result<Vec<ConversationMessage>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, agent_name, role, content, channel_type, \
                 model_used, input_tokens, output_tokens, turn_id, is_final, metadata, created_at \
                 FROM conversations WHERE turn_id = ?1 ORDER BY id",
            )
            .map_err(|e| format!("Failed to prepare load_full_turn: {}", e))?;

        let messages = stmt
            .query_map([turn_id], |row| {
                Ok(ConversationMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    channel_type: row.get(5)?,
                    model_used: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    turn_id: row.get(9)?,
                    is_final: row.get(10)?,
                    metadata: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to query turn: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }

    /// Get aggregated token usage for an agent, optionally filtered by date.
    pub fn get_token_usage(
        &self,
        agent_name: &str,
        since: Option<&str>,
    ) -> Result<TokenUsage, String> {
        let conn = self.conn.lock().unwrap();
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(since) =
            since
        {
            (
                "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
                 FROM conversations WHERE agent_name = ?1 AND created_at >= ?2",
                vec![
                    Box::new(agent_name.to_string()),
                    Box::new(since.to_string()),
                ],
            )
        } else {
            (
                "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
                 FROM conversations WHERE agent_name = ?1",
                vec![Box::new(agent_name.to_string())],
            )
        };

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        conn.query_row(sql, params_ref.as_slice(), |row| {
            Ok(TokenUsage {
                input_tokens: row.get(0)?,
                output_tokens: row.get(1)?,
            })
        })
        .map_err(|e| format!("Failed to get token usage: {}", e))
    }

    /// Get parent_id from the first message's metadata in a conversation.
    pub fn get_parent_id(&self, conversation_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        let metadata: Option<String> = conn
            .query_row(
                "SELECT metadata FROM conversations WHERE conversation_id = ?1 ORDER BY id LIMIT 1",
                [conversation_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get parent_id: {}", e))?;

        if let Some(meta_str) = metadata {
            if let Ok(json) = serde_json::from_str::<Value>(&meta_str) {
                return Ok(json.get("parent_id").and_then(|v| v.as_str()).map(String::from));
            }
        }
        Ok(None)
    }

    /// Load recent final messages for an agent (cross-agent context).
    pub fn load_recent_messages(
        &self,
        agent_name: &str,
        limit: u32,
    ) -> Result<Vec<ConversationMessage>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, agent_name, role, content, channel_type, \
                 model_used, input_tokens, output_tokens, turn_id, is_final, metadata, created_at \
                 FROM conversations \
                 WHERE agent_name = ?1 AND is_final = 1 \
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(|e| format!("Failed to prepare load_recent_messages: {}", e))?;

        let mut messages: Vec<ConversationMessage> = stmt
            .query_map(rusqlite::params![agent_name, limit], |row| {
                Ok(ConversationMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    channel_type: row.get(5)?,
                    model_used: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    turn_id: row.get(9)?,
                    is_final: row.get(10)?,
                    metadata: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to query recent messages: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        messages.reverse();
        Ok(messages)
    }

    /// List conversations grouped by conversation_id with summary stats.
    pub fn list_conversations(
        &self,
        agent_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ConversationSummary>, String> {
        let conn = self.conn.lock().unwrap();

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(agent) =
            agent_name
        {
            (
                "SELECT conversation_id, agent_name, channel_type, \
                 COUNT(DISTINCT turn_id) as turn_count, \
                 COUNT(*) as message_count, \
                 COALESCE(SUM(input_tokens), 0) as total_input_tokens, \
                 COALESCE(SUM(output_tokens), 0) as total_output_tokens, \
                 MIN(created_at) as first_message_at, \
                 MAX(created_at) as last_message_at \
                 FROM conversations WHERE agent_name = ?1 \
                 GROUP BY conversation_id \
                 ORDER BY last_message_at DESC LIMIT ?2"
                    .to_string(),
                vec![
                    Box::new(agent.to_string()),
                    Box::new(limit),
                ],
            )
        } else {
            (
                "SELECT conversation_id, agent_name, channel_type, \
                 COUNT(DISTINCT turn_id) as turn_count, \
                 COUNT(*) as message_count, \
                 COALESCE(SUM(input_tokens), 0) as total_input_tokens, \
                 COALESCE(SUM(output_tokens), 0) as total_output_tokens, \
                 MIN(created_at) as first_message_at, \
                 MAX(created_at) as last_message_at \
                 FROM conversations \
                 GROUP BY conversation_id \
                 ORDER BY last_message_at DESC LIMIT ?1"
                    .to_string(),
                vec![Box::new(limit)],
            )
        };

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare list_conversations: {}", e))?;

        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok(ConversationSummary {
                    conversation_id: row.get(0)?,
                    agent_name: row.get(1)?,
                    channel_type: row.get(2)?,
                    turn_count: row.get(3)?,
                    message_count: row.get(4)?,
                    total_input_tokens: row.get(5)?,
                    total_output_tokens: row.get(6)?,
                    first_message_at: row.get(7)?,
                    last_message_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Failed to query conversations: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Get token usage grouped by model.
    pub fn get_token_usage_by_model(
        &self,
        agent_name: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<(String, i64, i64)>, String> {
        let conn = self.conn.lock().unwrap();

        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(agent) = agent_name {
            params.push(Box::new(agent.to_string()));
            conditions.push(format!("agent_name = ?{}", params.len()));
        }
        if let Some(since) = since {
            params.push(Box::new(since.to_string()));
            conditions.push(format!("created_at >= ?{}", params.len()));
        }
        conditions.push("model_used IS NOT NULL".to_string());

        let where_clause = format!(" WHERE {}", conditions.join(" AND "));

        let sql = format!(
            "SELECT model_used, COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
             FROM conversations{} GROUP BY model_used ORDER BY SUM(input_tokens) + SUM(output_tokens) DESC",
            where_clause
        );

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare get_token_usage_by_model: {}", e))?;

        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| format!("Failed to query token usage by model: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Get token usage grouped by day.
    pub fn get_token_usage_by_day(
        &self,
        agent_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, i64, i64)>, String> {
        let conn = self.conn.lock().unwrap();

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(agent) =
            agent_name
        {
            (
                "SELECT DATE(created_at) as day, \
                 COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
                 FROM conversations WHERE agent_name = ?1 \
                 GROUP BY day ORDER BY day DESC LIMIT ?2"
                    .to_string(),
                vec![Box::new(agent.to_string()), Box::new(limit)],
            )
        } else {
            (
                "SELECT DATE(created_at) as day, \
                 COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
                 FROM conversations \
                 GROUP BY day ORDER BY day DESC LIMIT ?1"
                    .to_string(),
                vec![Box::new(limit)],
            )
        };

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare get_token_usage_by_day: {}", e))?;

        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| format!("Failed to query token usage by day: {}", e))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Load all messages in a conversation (including non-final), ordered by id.
    pub fn load_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<ConversationMessage>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, agent_name, role, content, channel_type, \
                 model_used, input_tokens, output_tokens, turn_id, is_final, metadata, created_at \
                 FROM conversations WHERE conversation_id = ?1 ORDER BY id",
            )
            .map_err(|e| format!("Failed to prepare load_conversation: {}", e))?;

        let messages = stmt
            .query_map([conversation_id], |row| {
                Ok(ConversationMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    channel_type: row.get(5)?,
                    model_used: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    turn_id: row.get(9)?,
                    is_final: row.get(10)?,
                    metadata: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to query conversation: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn save_test_msg(db: &Database, conv_id: &str, role: &str, content: &str, is_final: bool) {
        db.save_message(
            conv_id, "ino", role, content, "cli", None, 100, 50, "turn-abc", is_final, None,
        )
        .unwrap();
    }

    #[test]
    fn test_save_and_load_history() {
        let db = setup();
        save_test_msg(&db, "conv-1", "user", "hello", true);
        save_test_msg(&db, "conv-1", "assistant", "hi there", true);

        let history = db.load_history("conv-1", 20).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].role, "assistant");
    }

    #[test]
    fn test_load_history_excludes_non_final() {
        let db = setup();
        save_test_msg(&db, "conv-1", "user", "hello", true);
        save_test_msg(&db, "conv-1", "assistant", "tool call", false);
        save_test_msg(&db, "conv-1", "tool", "result", false);
        save_test_msg(&db, "conv-1", "assistant", "final answer", true);

        let history = db.load_history("conv-1", 20).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].content, "final answer");
    }

    #[test]
    fn test_load_history_respects_limit() {
        let db = setup();
        for i in 0..10 {
            save_test_msg(&db, "conv-1", "user", &format!("msg-{}", i), true);
        }

        let history = db.load_history("conv-1", 3).unwrap();
        assert_eq!(history.len(), 3);
        // Should be the most recent 3, in chronological order
        assert_eq!(history[0].content, "msg-7");
        assert_eq!(history[2].content, "msg-9");
    }

    #[test]
    fn test_load_history_separate_conversations() {
        let db = setup();
        save_test_msg(&db, "conv-1", "user", "msg-a", true);
        save_test_msg(&db, "conv-2", "user", "msg-b", true);

        let h1 = db.load_history("conv-1", 20).unwrap();
        let h2 = db.load_history("conv-2", 20).unwrap();
        assert_eq!(h1.len(), 1);
        assert_eq!(h2.len(), 1);
        assert_eq!(h1[0].content, "msg-a");
        assert_eq!(h2[0].content, "msg-b");
    }

    #[test]
    fn test_load_full_turn() {
        let db = setup();
        db.save_message(
            "conv-1", "ino", "user", "hello", "cli", None, 0, 0, "turn-001", true, None,
        )
        .unwrap();
        db.save_message(
            "conv-1", "ino", "assistant", "calling tool", "cli", None, 0, 0, "turn-001", false, None,
        )
        .unwrap();
        db.save_message(
            "conv-1", "ino", "tool", "tool result", "cli", None, 0, 0, "turn-001", false, None,
        )
        .unwrap();
        db.save_message(
            "conv-1", "ino", "assistant", "final", "cli", None, 0, 0, "turn-001", true, None,
        )
        .unwrap();

        let turn = db.load_full_turn("turn-001").unwrap();
        assert_eq!(turn.len(), 4);
        assert_eq!(turn[0].role, "user");
        assert_eq!(turn[3].role, "assistant");
    }

    #[test]
    fn test_get_token_usage() {
        let db = setup();
        db.save_message(
            "conv-1", "ino", "user", "q1", "cli", None, 100, 0, "t1", true, None,
        )
        .unwrap();
        db.save_message(
            "conv-1", "ino", "assistant", "a1", "cli", Some("model-1"), 0, 200, "t1", true, None,
        )
        .unwrap();

        let usage = db.get_token_usage("ino", None).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
    }

    #[test]
    fn test_get_token_usage_empty() {
        let db = setup();
        let usage = db.get_token_usage("nobody", None).unwrap();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_get_parent_id() {
        let db = setup();
        let meta = r#"{"parent_id": "conv-0"}"#;
        db.save_message(
            "conv-1", "ino", "user", "hello", "cli", None, 0, 0, "t1", true, Some(meta),
        )
        .unwrap();

        let parent = db.get_parent_id("conv-1").unwrap();
        assert_eq!(parent, Some("conv-0".to_string()));
    }

    #[test]
    fn test_get_parent_id_none() {
        let db = setup();
        db.save_message(
            "conv-1", "ino", "user", "hello", "cli", None, 0, 0, "t1", true, None,
        )
        .unwrap();

        let parent = db.get_parent_id("conv-1").unwrap();
        assert!(parent.is_none());
    }

    #[test]
    fn test_load_recent_messages() {
        let db = setup();
        db.register_agent("ino").unwrap();
        db.register_agent("robin").unwrap();

        db.save_message(
            "conv-1", "ino", "user", "ino msg", "cli", None, 0, 0, "t1", true, None,
        )
        .unwrap();
        db.save_message(
            "conv-2", "robin", "user", "robin msg", "cli", None, 0, 0, "t2", true, None,
        )
        .unwrap();

        let msgs = db.load_recent_messages("ino", 5).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "ino msg");
    }

    #[test]
    fn test_save_message_returns_id() {
        let db = setup();
        let id1 = db
            .save_message("c1", "ino", "user", "hi", "cli", None, 0, 0, "t1", true, None)
            .unwrap();
        let id2 = db
            .save_message("c1", "ino", "assistant", "yo", "cli", None, 0, 0, "t1", true, None)
            .unwrap();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }
}
