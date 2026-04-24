use serde_json::json;

use crate::cli::TasksCommand;
use crate::db::Database;

pub fn run(db: &Database, cmd: TasksCommand) -> Result<(), String> {
    match cmd {
        TasksCommand::List {
            status,
            agent,
            mission,
            json,
        } => run_list(db, agent.as_deref(), status.as_deref(), mission.as_deref(), json),
        TasksCommand::Show { key, json } => run_show(db, &key, json),
        TasksCommand::Create {
            title,
            description,
            mission,
            agent,
            priority,
            schedule,
            json,
        } => run_create(
            db,
            &title,
            description.as_deref(),
            mission.as_deref(),
            agent.as_deref(),
            &priority,
            schedule.as_deref(),
            json,
        ),
        TasksCommand::Update {
            key,
            status,
            priority,
            agent,
            json,
        } => run_update(
            db,
            &key,
            status.as_deref(),
            priority.as_deref(),
            agent.as_deref(),
            json,
        ),
        TasksCommand::Delete { key, json } => run_delete(db, &key, json),
    }
}

fn run_list(
    db: &Database,
    agent: Option<&str>,
    status: Option<&str>,
    mission: Option<&str>,
    as_json: bool,
) -> Result<(), String> {
    let tasks = db.list_tasks(agent, status, mission)?;

    if as_json {
        let arr: Vec<_> = tasks
            .iter()
            .map(|t| {
                json!({
                    "key": t.key,
                    "title": t.title,
                    "status": t.status,
                    "priority": t.priority,
                    "agent": t.agent_name,
                    "mission": t.mission_key,
                    "schedule": t.schedule,
                    "created_at": t.created_at,
                    "updated_at": t.updated_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else if tasks.is_empty() {
        println!("(no tasks)");
    } else {
        println!(
            "{:<16} {:<12} {:<10} {:<10} {}",
            "KEY", "STATUS", "PRIORITY", "AGENT", "TITLE"
        );
        println!("{}", "-".repeat(70));
        for t in &tasks {
            let agent = t.agent_name.as_deref().unwrap_or("-");
            println!(
                "{:<16} {:<12} {:<10} {:<10} {}",
                t.key, t.status, t.priority, agent, t.title
            );
        }
    }
    Ok(())
}

fn run_show(db: &Database, key: &str, as_json: bool) -> Result<(), String> {
    let task = db
        .get_task(key)?
        .ok_or_else(|| format!("Task not found: {}", key))?;

    if as_json {
        let val = json!({
            "key": task.key,
            "title": task.title,
            "description": task.description,
            "status": task.status,
            "priority": task.priority,
            "agent": task.agent_name,
            "mission": task.mission_key,
            "schedule": task.schedule,
            "last_run_at": task.last_run_at,
            "result": task.result,
            "metadata": task.metadata.as_deref()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok()),
            "created_at": task.created_at,
            "updated_at": task.updated_at,
        });
        println!("{}", serde_json::to_string_pretty(&val).unwrap());
    } else {
        println!("Key:         {}", task.key);
        println!("Title:       {}", task.title);
        println!("Status:      {}", task.status);
        println!("Priority:    {}", task.priority);
        println!(
            "Agent:       {}",
            task.agent_name.as_deref().unwrap_or("-")
        );
        println!(
            "Mission:     {}",
            task.mission_key.as_deref().unwrap_or("-")
        );
        if let Some(desc) = &task.description {
            println!("Description: {}", desc);
        }
        if let Some(sched) = &task.schedule {
            println!("Schedule:    {}", sched);
        }
        if let Some(last) = &task.last_run_at {
            println!("Last run:    {}", last);
        }
        if let Some(result) = &task.result {
            println!("Result:      {}", result);
        }
        if let Some(meta) = &task.metadata {
            println!("Metadata:    {}", meta);
        }
        println!("Created:     {}", task.created_at);
        println!("Updated:     {}", task.updated_at);
    }
    Ok(())
}

fn run_create(
    db: &Database,
    title: &str,
    description: Option<&str>,
    mission: Option<&str>,
    agent: Option<&str>,
    priority: &str,
    schedule: Option<&str>,
    as_json: bool,
) -> Result<(), String> {
    let key = db.create_task(title, description, mission, agent, schedule, Some(priority), None)?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"key": key})).unwrap()
        );
    } else {
        println!("Created task {}", key);
    }
    Ok(())
}

fn run_update(
    db: &Database,
    key: &str,
    status: Option<&str>,
    priority: Option<&str>,
    agent: Option<&str>,
    as_json: bool,
) -> Result<(), String> {
    // Verify task exists
    db.get_task(key)?
        .ok_or_else(|| format!("Task not found: {}", key))?;

    if status.is_none() && priority.is_none() && agent.is_none() {
        return Err("No fields to update. Use --status, --priority, or --agent".into());
    }

    db.update_task(key, status, None, agent, priority)?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"key": key, "updated": true})).unwrap()
        );
    } else {
        println!("Updated task {}", key);
    }
    Ok(())
}

fn run_delete(db: &Database, key: &str, as_json: bool) -> Result<(), String> {
    db.get_task(key)?
        .ok_or_else(|| format!("Task not found: {}", key))?;

    db.delete_task(key)?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"key": key, "deleted": true})).unwrap()
        );
    } else {
        println!("Deleted task {}", key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_list_empty() {
        let db = setup();
        // Should not error on empty
        run_list(&db, None, None, None, false).unwrap();
    }

    #[test]
    fn test_list_json_empty() {
        let db = setup();
        run_list(&db, None, None, None, true).unwrap();
    }

    #[test]
    fn test_create_and_show() {
        let db = setup();
        run_create(&db, "Test task", Some("A description"), None, None, "high", None, false)
            .unwrap();

        let tasks = db.list_tasks(None, None, None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Test task");
        assert_eq!(tasks[0].priority, "high");

        run_show(&db, &tasks[0].key, false).unwrap();
        run_show(&db, &tasks[0].key, true).unwrap();
    }

    #[test]
    fn test_show_not_found() {
        let db = setup();
        let result = run_show(&db, "TSK-99999", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_with_filters() {
        let db = setup();
        db.create_task("Task A", None, None, Some("ino"), None, Some("high"), None)
            .unwrap();
        db.create_task("Task B", None, None, Some("robin"), None, Some("low"), None)
            .unwrap();

        // Filter by agent
        let tasks = db.list_tasks(Some("ino"), None, None).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Task A");
    }

    #[test]
    fn test_update() {
        let db = setup();
        let key = db
            .create_task("Task", None, None, None, None, Some("low"), None)
            .unwrap();

        run_update(&db, &key, Some("in_progress"), Some("high"), Some("ino"), false).unwrap();

        let task = db.get_task(&key).unwrap().unwrap();
        assert_eq!(task.status, "in_progress");
        assert_eq!(task.priority, "high");
        assert_eq!(task.agent_name.as_deref(), Some("ino"));
    }

    #[test]
    fn test_update_not_found() {
        let db = setup();
        let result = run_update(&db, "TSK-99999", Some("done"), None, None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_no_fields() {
        let db = setup();
        let key = db
            .create_task("Task", None, None, None, None, None, None)
            .unwrap();
        let result = run_update(&db, &key, None, None, None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_delete() {
        let db = setup();
        let key = db
            .create_task("Task", None, None, None, None, None, None)
            .unwrap();

        run_delete(&db, &key, false).unwrap();
        assert!(db.get_task(&key).unwrap().is_none());
    }

    #[test]
    fn test_delete_not_found() {
        let db = setup();
        let result = run_delete(&db, "TSK-99999", false);
        assert!(result.is_err());
    }
}
