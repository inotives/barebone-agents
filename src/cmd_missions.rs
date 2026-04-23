use serde_json::json;

use crate::cli::MissionsCommand;
use crate::db::Database;

pub fn run(db: &Database, cmd: MissionsCommand) -> Result<(), String> {
    match cmd {
        MissionsCommand::List { status, json } => run_list(db, status.as_deref(), json),
        MissionsCommand::Show { key, json } => run_show(db, &key, json),
        MissionsCommand::Create {
            title,
            description,
            json,
        } => run_create(db, &title, description.as_deref(), json),
    }
}

fn run_list(db: &Database, status: Option<&str>, as_json: bool) -> Result<(), String> {
    let missions = db.list_missions(status)?;

    if as_json {
        let arr: Vec<_> = missions
            .iter()
            .map(|m| {
                let (done, total) = db.get_mission_task_progress(&m.key).unwrap_or((0, 0));
                json!({
                    "key": m.key,
                    "title": m.title,
                    "status": m.status,
                    "created_by": m.created_by,
                    "progress": { "done": done, "total": total },
                    "created_at": m.created_at,
                    "updated_at": m.updated_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else if missions.is_empty() {
        println!("(no missions)");
    } else {
        println!(
            "{:<12} {:<12} {:<10} {}",
            "KEY", "STATUS", "PROGRESS", "TITLE"
        );
        println!("{}", "-".repeat(60));
        for m in &missions {
            let (done, total) = db.get_mission_task_progress(&m.key).unwrap_or((0, 0));
            let progress = format!("{}/{}", done, total);
            println!(
                "{:<12} {:<12} {:<10} {}",
                m.key, m.status, progress, m.title
            );
        }
    }
    Ok(())
}

fn run_show(db: &Database, key: &str, as_json: bool) -> Result<(), String> {
    let mission = db
        .get_mission(key)?
        .ok_or_else(|| format!("Mission not found: {}", key))?;

    let tasks = db.list_tasks(None, None, Some(key))?;
    let (done, total) = db.get_mission_task_progress(key).unwrap_or((0, 0));

    if as_json {
        let task_arr: Vec<_> = tasks
            .iter()
            .map(|t| {
                json!({
                    "key": t.key,
                    "title": t.title,
                    "status": t.status,
                    "priority": t.priority,
                    "agent": t.agent_name,
                })
            })
            .collect();
        let val = json!({
            "key": mission.key,
            "title": mission.title,
            "description": mission.description,
            "status": mission.status,
            "created_by": mission.created_by,
            "progress": { "done": done, "total": total },
            "tasks": task_arr,
            "created_at": mission.created_at,
            "updated_at": mission.updated_at,
        });
        println!("{}", serde_json::to_string_pretty(&val).unwrap());
    } else {
        println!("Key:         {}", mission.key);
        println!("Title:       {}", mission.title);
        println!("Status:      {}", mission.status);
        println!("Progress:    {}/{}", done, total);
        if let Some(desc) = &mission.description {
            println!("Description: {}", desc);
        }
        if let Some(by) = &mission.created_by {
            println!("Created by:  {}", by);
        }
        println!("Created:     {}", mission.created_at);
        println!("Updated:     {}", mission.updated_at);

        if !tasks.is_empty() {
            println!();
            println!("Tasks:");
            println!(
                "  {:<16} {:<12} {:<10} {:<10} {}",
                "KEY", "STATUS", "PRIORITY", "AGENT", "TITLE"
            );
            println!("  {}", "-".repeat(66));
            for t in &tasks {
                let agent = t.agent_name.as_deref().unwrap_or("-");
                println!(
                    "  {:<16} {:<12} {:<10} {:<10} {}",
                    t.key, t.status, t.priority, agent, t.title
                );
            }
        }
    }
    Ok(())
}

fn run_create(db: &Database, title: &str, description: Option<&str>, as_json: bool) -> Result<(), String> {
    let key = db.create_mission(title, description, Some("cli"))?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"key": key})).unwrap()
        );
    } else {
        println!("Created mission {}", key);
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
        run_list(&db, None, false).unwrap();
    }

    #[test]
    fn test_create_and_show() {
        let db = setup();
        run_create(&db, "Test mission", Some("A description"), false).unwrap();

        let missions = db.list_missions(None).unwrap();
        assert_eq!(missions.len(), 1);
        assert_eq!(missions[0].title, "Test mission");
        assert_eq!(missions[0].created_by.as_deref(), Some("cli"));

        run_show(&db, &missions[0].key, false).unwrap();
        run_show(&db, &missions[0].key, true).unwrap();
    }

    #[test]
    fn test_show_not_found() {
        let db = setup();
        let result = run_show(&db, "MIS-99999", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_show_with_tasks() {
        let db = setup();
        let mkey = db.create_mission("Mission X", None, None).unwrap();
        db.create_task("Task 1", None, Some(&mkey), None, None, None, None)
            .unwrap();
        db.create_task("Task 2", None, Some(&mkey), None, None, None, None)
            .unwrap();

        // Should show mission + 2 tasks
        run_show(&db, &mkey, false).unwrap();
        run_show(&db, &mkey, true).unwrap();
    }
}
