use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;
use tracing::info;

pub struct Database {
    pub conn: Mutex<Connection>,
}

impl Database {
    /// Open (or create) the database at the given path with WAL mode and foreign keys.
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create db directory: {}", e))?;
        }

        let conn = Connection::open(path)
            .map_err(|e| format!("Failed to open database: {}", e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| format!("Failed to set pragmas: {}", e))?;

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;

        info!(path = %path.display(), "database opened");
        Ok(db)
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|e| format!("Failed to open in-memory database: {}", e))?;

        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| format!("Failed to set pragmas: {}", e))?;

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("Failed to initialize schema: {}", e))?;
        Ok(())
    }

    /// Register an agent name in the agents table (upsert).
    pub fn register_agent(&self, name: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO agents (name) VALUES (?1)",
            [name],
        )
        .map_err(|e| format!("Failed to register agent: {}", e))?;
        Ok(())
    }

    /// List all registered agent names.
    pub fn get_registered_agents(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM agents ORDER BY name")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;
        let names = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query agents: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(names)
    }

    /// Get the most recent message timestamp for an agent.
    pub fn get_agent_last_active(&self, agent_name: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT MAX(created_at) FROM conversations WHERE agent_name = ?1",
            [agent_name],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get last active: {}", e))
    }

    /// Get task status counts, optionally filtered by agent.
    pub fn get_task_status_counts(
        &self,
        agent_name: Option<&str>,
    ) -> Result<Vec<(String, i64)>, String> {
        let conn = self.conn.lock().unwrap();
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match agent_name {
            Some(name) => (
                "SELECT status, COUNT(*) FROM tasks WHERE agent_name = ?1 GROUP BY status ORDER BY status",
                vec![Box::new(name.to_string())],
            ),
            None => (
                "SELECT status, COUNT(*) FROM tasks GROUP BY status ORDER BY status",
                vec![],
            ),
        };

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare task counts: {}", e))?;
        let counts = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to query task counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(counts)
    }

    /// Get task count per status for a specific mission.
    pub fn get_mission_task_progress(
        &self,
        mission_key: &str,
    ) -> Result<(i64, i64), String> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE mission_key = ?1",
                [mission_key],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count mission tasks: {}", e))?;
        let done: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE mission_key = ?1 AND status = 'done'",
                [mission_key],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count done tasks: {}", e))?;
        Ok((done, total))
    }

    /// Get recent activity (final messages) across agents, ordered by time desc.
    pub fn get_recent_activity(
        &self,
        agent_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, String, String, String, String)>, String> {
        let conn = self.conn.lock().unwrap();
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match agent_name {
            Some(name) => (
                "SELECT created_at, agent_name, channel_type, role, \
                 SUBSTR(content, 1, 120) \
                 FROM conversations \
                 WHERE agent_name = ?1 AND is_final = 1 \
                 ORDER BY id DESC LIMIT ?2",
                vec![
                    Box::new(name.to_string()),
                    Box::new(limit),
                ],
            ),
            None => (
                "SELECT created_at, agent_name, channel_type, role, \
                 SUBSTR(content, 1, 120) \
                 FROM conversations \
                 WHERE is_final = 1 \
                 ORDER BY id DESC LIMIT ?1",
                vec![Box::new(limit)],
            ),
        };

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare activity: {}", e))?;
        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| format!("Failed to query activity: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS agents (
    name       TEXT PRIMARY KEY,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS conversations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    channel_type    TEXT NOT NULL,
    model_used      TEXT,
    input_tokens    INTEGER DEFAULT 0,
    output_tokens   INTEGER DEFAULT 0,
    turn_id         TEXT NOT NULL,
    is_final        BOOLEAN DEFAULT 0,
    metadata        TEXT,
    created_at      DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_conv_id ON conversations(conversation_id);
CREATE INDEX IF NOT EXISTS idx_conv_agent ON conversations(agent_name);
CREATE INDEX IF NOT EXISTS idx_conv_turn ON conversations(turn_id);
CREATE INDEX IF NOT EXISTS idx_conv_final ON conversations(conversation_id, is_final);

CREATE TABLE IF NOT EXISTS missions (
    key         TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT,
    status      TEXT DEFAULT 'active',
    created_by  TEXT,
    metadata    TEXT,
    created_at  DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at  DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS tasks (
    key              TEXT PRIMARY KEY,
    mission_key      TEXT,
    title            TEXT NOT NULL,
    description      TEXT,
    status           TEXT DEFAULT 'backlog',
    priority         TEXT DEFAULT 'medium',
    agent_name       TEXT,
    schedule         TEXT,
    last_run_at      DATETIME,
    result           TEXT,
    metadata         TEXT,
    created_at       DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at       DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_task_agent ON tasks(agent_name);
CREATE INDEX IF NOT EXISTS idx_task_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_task_mission ON tasks(mission_key);
CREATE INDEX IF NOT EXISTS idx_task_schedule ON tasks(schedule);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_in_memory() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.conn.lock().is_ok());
    }

    #[test]
    fn test_open_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let db = Database::open(&path).unwrap();
        assert!(path.exists());
        drop(db);
    }

    #[test]
    fn test_open_creates_parent_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dir").join("test.db");
        let _db = Database::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_register_agent() {
        let db = Database::open_in_memory().unwrap();
        db.register_agent("ino").unwrap();
        db.register_agent("robin").unwrap();
        // Duplicate should not error
        db.register_agent("ino").unwrap();

        let agents = db.get_registered_agents().unwrap();
        assert_eq!(agents, vec!["ino", "robin"]);
    }

    #[test]
    fn test_get_registered_agents_empty() {
        let db = Database::open_in_memory().unwrap();
        let agents = db.get_registered_agents().unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_schema_idempotent() {
        let db = Database::open_in_memory().unwrap();
        // Running init_schema again should not fail
        db.init_schema().unwrap();
    }
}
