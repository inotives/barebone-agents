use serde_json::{json, Value};
use std::sync::Arc;

use crate::db::{Database, TaskMetadata};
use super::registry::ToolRegistry;

/// Register task, mission, and conversation tools.
pub fn register(registry: &mut ToolRegistry, db: Arc<Database>, agent_name: String) {
    register_task_tools(registry, db.clone(), agent_name);
    register_mission_tools(registry, db.clone());
    register_conversation_search(registry, db);
}

fn register_task_tools(registry: &mut ToolRegistry, db: Arc<Database>, agent_name: String) {
    let d = db.clone();
    let an = agent_name.clone();
    registry.register(
        "task_create",
        "Create a new task with auto-generated key and auto-determined status.",
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Task title"},
                "description": {"type": "string", "description": "Task description"},
                "mission_key": {"type": "string", "description": "Mission key to attach to"},
                "agent_name": {"type": "string", "description": "Assign to agent (default: self)"},
                "agent_class": {"type": "string", "description": "Class-based assignment"},
                "depends_on": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Task keys this depends on"
                },
                "schedule": {"type": "string", "description": "Schedule pattern (hourly, daily@HH:MM, etc.)"},
                "priority": {
                    "type": "string",
                    "enum": ["critical", "high", "medium", "low"],
                    "description": "Priority (default: medium)"
                }
            },
            "required": ["title"]
        }),
        move |args| {
            let d = d.clone();
            let an = an.clone();
            async move { task_create(&d, &an, args) }
        },
    );

    let d = db.clone();
    registry.register(
        "task_get",
        "Get details of a task by its key.",
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "Task key (e.g. TSK-00001)"}
            },
            "required": ["key"]
        }),
        move |args| {
            let d = d.clone();
            async move { task_get(&d, args) }
        },
    );

    let d = db.clone();
    registry.register(
        "task_update",
        "Update fields on an existing task.",
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "Task key"},
                "status": {
                    "type": "string",
                    "enum": ["backlog", "todo", "in_progress", "done", "blocked"]
                },
                "result": {"type": "string", "description": "Task result text"},
                "agent_name": {"type": "string", "description": "Reassign to agent"},
                "priority": {
                    "type": "string",
                    "enum": ["critical", "high", "medium", "low"]
                }
            },
            "required": ["key"]
        }),
        move |args| {
            let d = d.clone();
            async move { task_update(&d, args) }
        },
    );

    let d = db.clone();
    let an = agent_name.clone();
    registry.register(
        "task_list",
        "List tasks with optional filters. Defaults to own tasks.",
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "Filter by status"},
                "mission_key": {"type": "string", "description": "Filter by mission"},
                "agent_name": {"type": "string", "description": "Filter by agent (use 'all' for all agents)"}
            }
        }),
        move |args| {
            let d = d.clone();
            let an = an.clone();
            async move { task_list(&d, &an, args) }
        },
    );
}

fn register_mission_tools(registry: &mut ToolRegistry, db: Arc<Database>) {
    let d = db.clone();
    registry.register(
        "mission_create",
        "Create a new mission with auto-generated key.",
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Mission title"},
                "description": {"type": "string", "description": "Mission description"}
            },
            "required": ["title"]
        }),
        move |args| {
            let d = d.clone();
            async move { mission_create(&d, args) }
        },
    );

    let d = db.clone();
    registry.register(
        "mission_update",
        "Update fields on an existing mission.",
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "Mission key (e.g. MIS-00001)"},
                "status": {"type": "string", "enum": ["active", "paused", "completed"]},
                "title": {"type": "string"},
                "description": {"type": "string"}
            },
            "required": ["key"]
        }),
        move |args| {
            let d = d.clone();
            async move { mission_update(&d, args) }
        },
    );

    let d = db.clone();
    registry.register(
        "mission_list",
        "List missions with optional status filter.",
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "Filter by status (active, paused, completed)"}
            }
        }),
        move |args| {
            let d = d.clone();
            async move { mission_list(&d, args) }
        },
    );
}

fn register_conversation_search(registry: &mut ToolRegistry, db: Arc<Database>) {
    registry.register(
        "conversation_search",
        "Search past conversations by keyword. Only searches final messages (not intermediate tool calls).",
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search keyword or phrase"},
                "max_results": {"type": "integer", "description": "Maximum results (default: 10)"}
            },
            "required": ["query"]
        }),
        move |args| {
            let d = db.clone();
            async move { conversation_search(&d, args) }
        },
    );
}

// --- Tool implementations ---

fn task_create(db: &Database, default_agent: &str, args: Value) -> String {
    let title = match args.get("title").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return "Error: 'title' parameter required".to_string(),
    };
    let description = args.get("description").and_then(|d| d.as_str());
    let mission_key = args.get("mission_key").and_then(|m| m.as_str());
    let agent = args
        .get("agent_name")
        .and_then(|a| a.as_str())
        .unwrap_or(default_agent);
    let schedule = args.get("schedule").and_then(|s| s.as_str());
    let priority = args.get("priority").and_then(|p| p.as_str());

    let metadata = {
        let class = args.get("agent_class").and_then(|c| c.as_str()).map(String::from);
        let depends_on = args
            .get("depends_on")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });
        if class.is_some() || depends_on.is_some() {
            Some(TaskMetadata {
                class,
                depends_on,
                ..Default::default()
            })
        } else {
            None
        }
    };

    match db.create_task(
        title,
        description,
        mission_key,
        Some(agent),
        schedule,
        priority,
        metadata.as_ref(),
    ) {
        Ok(key) => format!("Created task {}", key),
        Err(e) => format!("Error: {}", e),
    }
}

fn task_get(db: &Database, args: Value) -> String {
    let key = match args.get("key").and_then(|k| k.as_str()) {
        Some(k) => k,
        None => return "Error: 'key' parameter required".to_string(),
    };

    match db.get_task(key) {
        Ok(Some(task)) => {
            json!({
                "key": task.key,
                "title": task.title,
                "description": task.description,
                "status": task.status,
                "priority": task.priority,
                "agent_name": task.agent_name,
                "schedule": task.schedule,
                "result": task.result,
                "mission_key": task.mission_key,
                "created_at": task.created_at,
                "updated_at": task.updated_at,
            })
            .to_string()
        }
        Ok(None) => format!("Task {} not found", key),
        Err(e) => format!("Error: {}", e),
    }
}

fn task_update(db: &Database, args: Value) -> String {
    let key = match args.get("key").and_then(|k| k.as_str()) {
        Some(k) => k,
        None => return "Error: 'key' parameter required".to_string(),
    };

    let status = args.get("status").and_then(|s| s.as_str());
    let result = args.get("result").and_then(|r| r.as_str());
    let agent_name = args.get("agent_name").and_then(|a| a.as_str());
    let priority = args.get("priority").and_then(|p| p.as_str());

    match db.update_task(key, status, result, agent_name, priority) {
        Ok(()) => format!("Updated task {}", key),
        Err(e) => format!("Error: {}", e),
    }
}

fn task_list(db: &Database, default_agent: &str, args: Value) -> String {
    let agent = args.get("agent_name").and_then(|a| a.as_str());
    let agent_filter = match agent {
        Some("all") => None,
        Some(name) => Some(name),
        None => Some(default_agent),
    };
    let status = args.get("status").and_then(|s| s.as_str());
    let mission_key = args.get("mission_key").and_then(|m| m.as_str());

    match db.list_tasks(agent_filter, status, mission_key) {
        Ok(tasks) => {
            if tasks.is_empty() {
                return "No tasks found.".to_string();
            }
            let items: Vec<Value> = tasks
                .iter()
                .map(|t| {
                    json!({
                        "key": t.key,
                        "title": t.title,
                        "status": t.status,
                        "priority": t.priority,
                        "agent_name": t.agent_name,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "Error formatting".into())
        }
        Err(e) => format!("Error: {}", e),
    }
}

fn mission_create(db: &Database, args: Value) -> String {
    let title = match args.get("title").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return "Error: 'title' parameter required".to_string(),
    };
    let description = args.get("description").and_then(|d| d.as_str());

    match db.create_mission(title, description, None) {
        Ok(key) => format!("Created mission {}", key),
        Err(e) => format!("Error: {}", e),
    }
}

fn mission_update(db: &Database, args: Value) -> String {
    let key = match args.get("key").and_then(|k| k.as_str()) {
        Some(k) => k,
        None => return "Error: 'key' parameter required".to_string(),
    };

    let status = args.get("status").and_then(|s| s.as_str());
    let title = args.get("title").and_then(|t| t.as_str());
    let description = args.get("description").and_then(|d| d.as_str());

    match db.update_mission(key, status, title, description) {
        Ok(()) => format!("Updated mission {}", key),
        Err(e) => format!("Error: {}", e),
    }
}

fn mission_list(db: &Database, args: Value) -> String {
    let status = args.get("status").and_then(|s| s.as_str());

    match db.list_missions(status) {
        Ok(missions) => {
            if missions.is_empty() {
                return "No missions found.".to_string();
            }
            let items: Vec<Value> = missions
                .iter()
                .map(|m| {
                    json!({
                        "key": m.key,
                        "title": m.title,
                        "status": m.status,
                        "description": m.description,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "Error formatting".into())
        }
        Err(e) => format!("Error: {}", e),
    }
}

fn conversation_search(db: &Database, args: Value) -> String {
    let query = match args.get("query").and_then(|q| q.as_str()) {
        Some(q) => q,
        None => return "Error: 'query' parameter required".to_string(),
    };
    let max_results = args
        .get("max_results")
        .and_then(|m| m.as_u64())
        .unwrap_or(10) as u32;

    // Search using SQLite LIKE (simple keyword search)
    let conn = db.conn.lock().unwrap();
    let pattern = format!("%{}%", query);
    let mut stmt = match conn.prepare(
        "SELECT conversation_id, role, content, created_at \
         FROM conversations \
         WHERE is_final = 1 AND content LIKE ?1 \
         ORDER BY id DESC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(e) => return format!("Error: {}", e),
    };

    let results: Vec<Value> = match stmt.query_map(rusqlite::params![pattern, max_results], |row| {
        Ok(json!({
            "conversation_id": row.get::<_, String>(0)?,
            "role": row.get::<_, String>(1)?,
            "content": row.get::<_, String>(2)?,
            "created_at": row.get::<_, String>(3)?,
        }))
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(e) => return format!("Error: {}", e),
    };

    if results.is_empty() {
        return "No matching conversations found.".to_string();
    }

    serde_json::to_string_pretty(&results).unwrap_or_else(|_| "Error formatting".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Arc<Database> {
        Arc::new(Database::open_in_memory().unwrap())
    }

    #[test]
    fn test_task_create() {
        let db = setup();
        let result = task_create(&db, "ino", json!({"title": "Test task"}));
        assert!(result.contains("Created task TSK-00001"));
    }

    #[test]
    fn test_task_create_with_deps() {
        let db = setup();
        task_create(&db, "ino", json!({"title": "Dep"}));
        let result = task_create(
            &db,
            "ino",
            json!({"title": "Blocked", "depends_on": ["TSK-00001"]}),
        );
        assert!(result.contains("TSK-00002"));
        let task = db.get_task("TSK-00002").unwrap().unwrap();
        assert_eq!(task.status, "blocked");
    }

    #[test]
    fn test_task_create_missing_title() {
        let db = setup();
        let result = task_create(&db, "ino", json!({}));
        assert!(result.contains("'title' parameter required"));
    }

    #[test]
    fn test_task_get() {
        let db = setup();
        db.create_task("My task", Some("desc"), None, Some("ino"), None, None, None)
            .unwrap();
        let result = task_get(&db, json!({"key": "TSK-00001"}));
        assert!(result.contains("My task"));
        assert!(result.contains("desc"));
    }

    #[test]
    fn test_task_get_not_found() {
        let db = setup();
        let result = task_get(&db, json!({"key": "TSK-99999"}));
        assert!(result.contains("not found"));
    }

    #[test]
    fn test_task_update() {
        let db = setup();
        db.create_task("T", None, None, None, None, None, None).unwrap();
        let result = task_update(&db, json!({"key": "TSK-00001", "status": "in_progress"}));
        assert!(result.contains("Updated"));
    }

    #[test]
    fn test_task_list_default_agent() {
        let db = setup();
        db.create_task("A", None, None, Some("ino"), None, None, None).unwrap();
        db.create_task("B", None, None, Some("robin"), None, None, None).unwrap();

        let result = task_list(&db, "ino", json!({}));
        assert!(result.contains("A"));
        assert!(!result.contains("B"));
    }

    #[test]
    fn test_task_list_all_agents() {
        let db = setup();
        db.create_task("A", None, None, Some("ino"), None, None, None).unwrap();
        db.create_task("B", None, None, Some("robin"), None, None, None).unwrap();

        let result = task_list(&db, "ino", json!({"agent_name": "all"}));
        assert!(result.contains("A"));
        assert!(result.contains("B"));
    }

    #[test]
    fn test_mission_create() {
        let db = setup();
        let result = mission_create(&db, json!({"title": "Sprint 1"}));
        assert!(result.contains("Created mission MIS-00001"));
    }

    #[test]
    fn test_mission_list() {
        let db = setup();
        db.create_mission("M1", None, None).unwrap();
        let result = mission_list(&db, json!({}));
        assert!(result.contains("M1"));
    }

    #[test]
    fn test_mission_update() {
        let db = setup();
        db.create_mission("M1", None, None).unwrap();
        let result = mission_update(&db, json!({"key": "MIS-00001", "status": "completed"}));
        assert!(result.contains("Updated"));
    }

    #[test]
    fn test_conversation_search() {
        let db = setup();
        db.save_message("c1", "ino", "user", "hello world", "cli", None, 0, 0, "t1", true, None)
            .unwrap();
        db.save_message("c1", "ino", "assistant", "tool call", "cli", None, 0, 0, "t1", false, None)
            .unwrap();
        db.save_message("c1", "ino", "assistant", "goodbye world", "cli", None, 0, 0, "t1", true, None)
            .unwrap();

        let result = conversation_search(&db, json!({"query": "world"}));
        assert!(result.contains("hello world"));
        assert!(result.contains("goodbye world"));
        // Should NOT include non-final messages
        assert!(!result.contains("tool call"));
    }

    #[test]
    fn test_conversation_search_no_results() {
        let db = setup();
        let result = conversation_search(&db, json!({"query": "nonexistent"}));
        assert!(result.contains("No matching"));
    }

    #[tokio::test]
    async fn test_register_all_tools() {
        let db = setup();
        let mut registry = ToolRegistry::new();
        register(&mut registry, db, "ino".to_string());

        assert!(registry.has("task_create"));
        assert!(registry.has("task_get"));
        assert!(registry.has("task_update"));
        assert!(registry.has("task_list"));
        assert!(registry.has("mission_create"));
        assert!(registry.has("mission_update"));
        assert!(registry.has("mission_list"));
        assert!(registry.has("conversation_search"));
        assert_eq!(registry.len(), 8);
    }
}
