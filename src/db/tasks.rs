use super::schema::Database;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Task {
    pub key: String,
    pub mission_key: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub agent_name: Option<String>,
    pub schedule: Option<String>,
    pub last_run_at: Option<String>,
    pub result: Option<String>,
    pub metadata: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskMetadata {
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,
    /// EP-00015 Decision E: opt-in flag to persist task output as a research
    /// draft at `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md`.
    /// Default `None` (treated as `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_as_draft: Option<bool>,
    /// EP-00015 Decision I: marks system-generated tasks (e.g. reflection
    /// follow-ups). System tasks bypass reflection counters and prior-work
    /// injection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<bool>,
}

impl Database {
    /// Generate the next task key.
    /// Standalone: TSK-{5-digit}, within mission: {mission_key}-T{5-digit}
    fn next_task_key(&self, mission_key: Option<&str>) -> Result<String, String> {
        let conn = self.conn.lock().unwrap();
        match mission_key {
            Some(mk) => {
                let prefix = format!("{}-T", mk);
                let max: Option<String> = conn
                    .query_row(
                        "SELECT key FROM tasks WHERE mission_key = ?1 ORDER BY key DESC LIMIT 1",
                        [mk],
                        |row| row.get(0),
                    )
                    .ok();

                let next_num = if let Some(last_key) = max {
                    let num_part = last_key.rsplit('T').next().unwrap_or("0");
                    num_part.parse::<u32>().unwrap_or(0) + 1
                } else {
                    1
                };
                Ok(format!("{}{:05}", prefix, next_num))
            }
            None => {
                let max: Option<String> = conn
                    .query_row(
                        "SELECT key FROM tasks WHERE key LIKE 'TSK-%' AND mission_key IS NULL \
                         ORDER BY key DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok();

                let next_num = if let Some(last_key) = max {
                    let num_part = &last_key[4..]; // skip "TSK-"
                    num_part.parse::<u32>().unwrap_or(0) + 1
                } else {
                    1
                };
                Ok(format!("TSK-{:05}", next_num))
            }
        }
    }

    /// Determine initial status based on metadata and schedule.
    fn determine_status(metadata: Option<&TaskMetadata>, schedule: Option<&str>) -> &'static str {
        if let Some(meta) = metadata {
            if let Some(deps) = &meta.depends_on {
                if !deps.is_empty() {
                    return "blocked";
                }
            }
            if meta.class.is_some() {
                return "todo";
            }
        }
        if schedule.is_some() {
            return "todo";
        }
        "backlog"
    }

    /// Create a new task with auto-generated key and auto-determined status.
    pub fn create_task(
        &self,
        title: &str,
        description: Option<&str>,
        mission_key: Option<&str>,
        agent_name: Option<&str>,
        schedule: Option<&str>,
        priority: Option<&str>,
        metadata: Option<&TaskMetadata>,
    ) -> Result<String, String> {
        let key = self.next_task_key(mission_key)?;
        let status = Self::determine_status(metadata, schedule);
        let priority = priority.unwrap_or("medium");
        let metadata_json = metadata
            .map(|m| serde_json::to_string(m).unwrap_or_default());

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (key, mission_key, title, description, status, priority, \
             agent_name, schedule, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                key,
                mission_key,
                title,
                description,
                status,
                priority,
                agent_name,
                schedule,
                metadata_json,
            ],
        )
        .map_err(|e| format!("Failed to create task: {}", e))?;
        Ok(key)
    }

    /// Get a single task by key.
    pub fn get_task(&self, key: &str) -> Result<Option<Task>, String> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT key, mission_key, title, description, status, priority, agent_name, \
             schedule, last_run_at, result, metadata, created_at, updated_at \
             FROM tasks WHERE key = ?1",
            [key],
            |row| {
                Ok(Task {
                    key: row.get(0)?,
                    mission_key: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    status: row.get(4)?,
                    priority: row.get(5)?,
                    agent_name: row.get(6)?,
                    schedule: row.get(7)?,
                    last_run_at: row.get(8)?,
                    result: row.get(9)?,
                    metadata: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            },
        );

        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get task: {}", e)),
        }
    }

    /// Update task fields dynamically. Only non-None fields are updated.
    pub fn update_task(
        &self,
        key: &str,
        status: Option<&str>,
        result: Option<&str>,
        agent_name: Option<&str>,
        priority: Option<&str>,
    ) -> Result<(), String> {
        let mut sets = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(v) = status {
            sets.push("status = ?");
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = result {
            sets.push("result = ?");
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = agent_name {
            sets.push("agent_name = ?");
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = priority {
            sets.push("priority = ?");
            params.push(Box::new(v.to_string()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        sets.push("updated_at = CURRENT_TIMESTAMP");

        // Renumber placeholders
        let numbered_sets: Vec<String> = sets
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if s.contains('?') {
                    s.replace('?', &format!("?{}", i + 1))
                } else {
                    s.to_string()
                }
            })
            .collect();

        params.push(Box::new(key.to_string()));
        let key_idx = params.len();

        let sql = format!(
            "UPDATE tasks SET {} WHERE key = ?{}",
            numbered_sets.join(", "),
            key_idx
        );

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let conn = self.conn.lock().unwrap();
        conn.execute(&sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to update task: {}", e))?;
        Ok(())
    }

    /// List tasks with optional filters.
    pub fn list_tasks(
        &self,
        agent_name: Option<&str>,
        status: Option<&str>,
        mission_key: Option<&str>,
    ) -> Result<Vec<Task>, String> {
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(v) = agent_name {
            params.push(Box::new(v.to_string()));
            conditions.push(format!("agent_name = ?{}", params.len()));
        }
        if let Some(v) = status {
            params.push(Box::new(v.to_string()));
            conditions.push(format!("status = ?{}", params.len()));
        }
        if let Some(v) = mission_key {
            params.push(Box::new(v.to_string()));
            conditions.push(format!("mission_key = ?{}", params.len()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT key, mission_key, title, description, status, priority, agent_name, \
             schedule, last_run_at, result, metadata, created_at, updated_at \
             FROM tasks{} ORDER BY created_at",
            where_clause
        );

        let conn = self.conn.lock().unwrap();
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare list_tasks: {}", e))?;

        let tasks = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok(Task {
                    key: row.get(0)?,
                    mission_key: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    status: row.get(4)?,
                    priority: row.get(5)?,
                    agent_name: row.get(6)?,
                    schedule: row.get(7)?,
                    last_run_at: row.get(8)?,
                    result: row.get(9)?,
                    metadata: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to query tasks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tasks)
    }

    /// Atomically claim an unclaimed task for an agent.
    pub fn claim_task(&self, key: &str, agent_name: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute(
                "UPDATE tasks SET agent_name = ?1, updated_at = CURRENT_TIMESTAMP \
                 WHERE key = ?2 AND agent_name IS NULL",
                rusqlite::params![agent_name, key],
            )
            .map_err(|e| format!("Failed to claim task: {}", e))?;
        Ok(rows > 0)
    }

    /// Check all blocked tasks and unblock those whose dependencies are done.
    pub fn check_unblock(&self) -> Result<u32, String> {
        let conn = self.conn.lock().unwrap();

        // Get all blocked tasks
        let mut stmt = conn
            .prepare("SELECT key, metadata FROM tasks WHERE status = 'blocked'")
            .map_err(|e| format!("Failed to prepare check_unblock: {}", e))?;

        let blocked: Vec<(String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query blocked tasks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        let mut unblocked = 0;
        for (key, meta_str) in &blocked {
            let deps = meta_str
                .as_deref()
                .and_then(|s| serde_json::from_str::<TaskMetadata>(s).ok())
                .and_then(|m| m.depends_on);

            let all_done = if let Some(dep_keys) = deps {
                dep_keys.iter().all(|dk| {
                    conn.query_row(
                        "SELECT status FROM tasks WHERE key = ?1",
                        [dk],
                        |row| row.get::<_, String>(0),
                    )
                    .map(|s| s == "done")
                    .unwrap_or(false)
                })
            } else {
                true // no deps, shouldn't be blocked
            };

            if all_done {
                conn.execute(
                    "UPDATE tasks SET status = 'todo', updated_at = CURRENT_TIMESTAMP WHERE key = ?1",
                    [key],
                )
                .map_err(|e| format!("Failed to unblock task: {}", e))?;
                unblocked += 1;
            }
        }

        Ok(unblocked)
    }

    /// Delete a task by key.
    pub fn delete_task(&self, key: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute("DELETE FROM tasks WHERE key = ?1", [key])
            .map_err(|e| format!("Failed to delete task: {}", e))?;
        if rows == 0 {
            return Err(format!("Task not found: {}", key));
        }
        Ok(())
    }

    /// Complete a task. One-time tasks → done, recurring tasks → reset to todo.
    pub fn complete_task(&self, key: &str, result: &str) -> Result<(), String> {
        let task = self
            .get_task(key)?
            .ok_or_else(|| format!("Task {} not found", key))?;

        let conn = self.conn.lock().unwrap();
        if task.schedule.is_some() {
            // Recurring: reset to todo, update last_run_at
            conn.execute(
                "UPDATE tasks SET status = 'todo', result = ?1, \
                 last_run_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
                 WHERE key = ?2",
                rusqlite::params![result, key],
            )
            .map_err(|e| format!("Failed to complete recurring task: {}", e))?;
        } else {
            // One-time: set to done
            conn.execute(
                "UPDATE tasks SET status = 'done', result = ?1, \
                 updated_at = CURRENT_TIMESTAMP WHERE key = ?2",
                rusqlite::params![result, key],
            )
            .map_err(|e| format!("Failed to complete task: {}", e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_task_standalone() {
        let db = setup();
        let key = db.create_task("Test task", None, None, None, None, None, None).unwrap();
        assert_eq!(key, "TSK-00001");

        let key2 = db.create_task("Second task", None, None, None, None, None, None).unwrap();
        assert_eq!(key2, "TSK-00002");
    }

    #[test]
    fn test_create_task_in_mission() {
        let db = setup();
        db.create_mission("Mission 1", None, None).unwrap();
        let key = db
            .create_task("Task A", None, Some("MIS-00001"), None, None, None, None)
            .unwrap();
        assert_eq!(key, "MIS-00001-T00001");

        let key2 = db
            .create_task("Task B", None, Some("MIS-00001"), None, None, None, None)
            .unwrap();
        assert_eq!(key2, "MIS-00001-T00002");
    }

    #[test]
    fn test_status_backlog_default() {
        let db = setup();
        let key = db.create_task("Basic task", None, None, None, None, None, None).unwrap();
        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "backlog");
    }

    #[test]
    fn test_status_todo_with_schedule() {
        let db = setup();
        let key = db
            .create_task("Hourly task", None, None, None, Some("hourly"), None, None)
            .unwrap();
        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "todo");
    }

    #[test]
    fn test_status_todo_with_class() {
        let db = setup();
        let meta = TaskMetadata {
            class: Some("coder".to_string()),
            ..Default::default()
        };
        let key = db
            .create_task("Class task", None, None, None, None, None, Some(&meta))
            .unwrap();
        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "todo");
    }

    #[test]
    fn test_status_blocked_with_depends() {
        let db = setup();
        let dep_key = db.create_task("Dep task", None, None, None, None, None, None).unwrap();
        let meta = TaskMetadata {
            depends_on: Some(vec![dep_key.clone()]),
            ..Default::default()
        };
        let key = db
            .create_task("Blocked task", None, None, None, None, None, Some(&meta))
            .unwrap();
        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "blocked");
    }

    #[test]
    fn test_get_task_not_found() {
        let db = setup();
        let task = db.get_task("TSK-99999").unwrap();
        assert!(task.is_none());
    }

    #[test]
    fn test_update_task() {
        let db = setup();
        let key = db.create_task("Test", None, None, None, None, None, None).unwrap();
        db.update_task(&key, Some("in_progress"), None, Some("ino"), Some("high"))
            .unwrap();

        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "in_progress");
        assert_eq!(task.agent_name.as_deref(), Some("ino"));
        assert_eq!(task.priority, "high");
    }

    #[test]
    fn test_list_tasks_with_filters() {
        let db = setup();
        db.create_task("Task A", None, None, Some("ino"), None, None, None).unwrap();
        db.create_task("Task B", None, None, Some("robin"), None, None, None).unwrap();
        db.create_task("Task C", None, None, Some("ino"), None, None, None).unwrap();

        let all = db.list_tasks(None, None, None).unwrap();
        assert_eq!(all.len(), 3);

        let ino_tasks = db.list_tasks(Some("ino"), None, None).unwrap();
        assert_eq!(ino_tasks.len(), 2);
    }

    #[test]
    fn test_claim_task() {
        let db = setup();
        let key = db.create_task("Unclaimed", None, None, None, None, None, None).unwrap();

        let claimed = db.claim_task(&key, "ino").unwrap();
        assert!(claimed);

        // Second claim should fail (already claimed)
        let claimed2 = db.claim_task(&key, "robin").unwrap();
        assert!(!claimed2);

        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.agent_name.as_deref(), Some("ino"));
    }

    #[test]
    fn test_check_unblock() {
        let db = setup();
        let dep = db.create_task("Dep", None, None, None, None, None, None).unwrap();
        let meta = TaskMetadata {
            depends_on: Some(vec![dep.clone()]),
            ..Default::default()
        };
        let blocked = db
            .create_task("Blocked", None, None, None, None, None, Some(&meta))
            .unwrap();

        // Not yet done
        let count = db.check_unblock().unwrap();
        assert_eq!(count, 0);
        assert_eq!(db.get_task(&blocked).unwrap().unwrap().status, "blocked");

        // Complete dependency
        db.complete_task(&dep, "done").unwrap();

        // Now unblock
        let count = db.check_unblock().unwrap();
        assert_eq!(count, 1);
        assert_eq!(db.get_task(&blocked).unwrap().unwrap().status, "todo");
    }

    #[test]
    fn test_complete_one_time_task() {
        let db = setup();
        let key = db.create_task("One-time", None, None, None, None, None, None).unwrap();
        db.update_task(&key, Some("in_progress"), None, None, None).unwrap();
        db.complete_task(&key, "All done").unwrap();

        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "done");
        assert_eq!(task.result.as_deref(), Some("All done"));
    }

    #[test]
    fn test_complete_recurring_task() {
        let db = setup();
        let key = db
            .create_task("Recurring", None, None, None, Some("hourly"), None, None)
            .unwrap();
        db.update_task(&key, Some("in_progress"), None, None, None).unwrap();
        db.complete_task(&key, "Cycle done").unwrap();

        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "todo"); // reset, not done
        assert_eq!(task.result.as_deref(), Some("Cycle done"));
        assert!(task.last_run_at.is_some());
    }

    #[test]
    fn test_complete_task_not_found() {
        let db = setup();
        let result = db.complete_task("TSK-99999", "nope");
        assert!(result.is_err());
    }
}
