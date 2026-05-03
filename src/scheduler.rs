use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::agent_loop::AgentLoop;
use crate::db::{is_due, Database};
use crate::session::SessionManager;

/// Background heartbeat that executes due tasks.
pub async fn run_heartbeat(
    agent_loop: Arc<AgentLoop>,
    db: Arc<Database>,
    session_mgr: Arc<Mutex<SessionManager>>,
    agent_name: String,
    interval_secs: u64,
    root_dir: PathBuf,
) {
    info!(
        agent = %agent_name,
        interval = interval_secs,
        "heartbeat started"
    );

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));

    loop {
        interval.tick().await;

        debug!(agent = %agent_name, "heartbeat cycle");

        // 1. Auto-unblock tasks whose dependencies are done
        match db.check_unblock() {
            Ok(count) if count > 0 => {
                info!(unblocked = count, "tasks unblocked");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, "check_unblock failed");
            }
        }

        // 2. Get due tasks
        let due_tasks = match get_due_tasks(&db, &agent_name) {
            Ok(tasks) => tasks,
            Err(e) => {
                warn!(error = %e, "get_due_tasks failed");
                continue;
            }
        };

        if due_tasks.is_empty() {
            continue;
        }

        info!(count = due_tasks.len(), "due tasks found");

        // 3. Execute each due task
        for (key, title, description, schedule) in due_tasks {
            // Claim the task (skip if already assigned to us)
            let task = db.get_task(&key).ok().flatten();
            let already_ours = task
                .as_ref()
                .and_then(|t| t.agent_name.as_deref())
                .map(|a| a == agent_name)
                .unwrap_or(false);

            if !already_ours {
                match db.claim_task(&key, &agent_name) {
                    Ok(true) => {}
                    Ok(false) => {
                        debug!(task = %key, "task already claimed by another agent");
                        continue;
                    }
                    Err(e) => {
                        warn!(task = %key, error = %e, "claim_task failed");
                        continue;
                    }
                }
            }

            info!(task = %key, title = %title, "executing task");

            execute_task(
                &agent_loop,
                &db,
                &session_mgr,
                &root_dir,
                &key,
                &title,
                description.as_deref(),
                schedule.as_deref(),
            )
            .await;
        }
    }
}

/// Get tasks that are due for execution.
fn get_due_tasks(
    db: &Database,
    agent_name: &str,
) -> Result<Vec<(String, String, Option<String>, Option<String>)>, String> {
    let tasks = db.list_tasks(Some(agent_name), Some("todo"), None)?;

    let mut due = Vec::new();
    for task in tasks {
        let task_due = match &task.schedule {
            Some(schedule) => is_due(schedule, task.last_run_at.as_deref()),
            None => true, // No schedule = one-time task, always due if status is todo
        };

        if task_due {
            due.push((
                task.key,
                task.title,
                task.description,
                task.schedule,
            ));
        }
    }

    Ok(due)
}

/// Execute a single task via the agent loop.
async fn execute_task(
    agent_loop: &AgentLoop,
    db: &Database,
    session_mgr: &Mutex<SessionManager>,
    root_dir: &Path,
    key: &str,
    title: &str,
    description: Option<&str>,
    _schedule: Option<&str>,
) {
    // Set to in_progress
    if let Err(e) = db.update_task(key, Some("in_progress"), None, None, None) {
        warn!(task = %key, error = %e, "failed to set in_progress");
        return;
    }

    // Build task message
    let mut message = format!("## Task: {}\n", title);
    if let Some(desc) = description {
        message.push_str(&format!("\n{}\n", desc));
    }

    // Check for dependency results
    let task = db.get_task(key).unwrap_or(None);
    if let Some(task) = &task {
        if let Some(meta_str) = &task.metadata {
            if let Ok(meta) = serde_json::from_str::<crate::db::TaskMetadata>(meta_str) {
                if let Some(deps) = &meta.depends_on {
                    for dep_key in deps {
                        if let Ok(Some(dep_task)) = db.get_task(dep_key) {
                            if let Some(result) = &dep_task.result {
                                let preview: String = result.chars().take(500).collect();
                                message.push_str(&format!(
                                    "\n### Dependency {} result:\n{}\n",
                                    dep_key, preview
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Generate conversation ID for this task execution
    let conv_id = format!("task-{}-{}", key, chrono::Utc::now().timestamp());

    // Session start — capture recommended_context for system prompt injection.
    let recommended_context = {
        let mut mgr = session_mgr.lock().await;
        mgr.ensure_session(&conv_id, "task").await
    };

    // Per-segment preference selection (EP-00015 Decision A — cached).
    let selected_preferences = crate::preferences::select_for_segment_cached(
        session_mgr,
        &conv_id,
        &message,
        agent_loop.prefs_pool_dir(),
        agent_loop.prefs_min_match_hits(),
        agent_loop.prefs_token_budget(),
    )
    .await;

    // Per-segment prior-work search (EP-00015 Decision B — cached).
    let prior_work_query = format!("{} {}", title, description.unwrap_or(""));
    let prior_work = crate::memory_context::build_prior_work_cached(
        agent_loop.registry(),
        session_mgr,
        &conv_id,
        &prior_work_query,
        // Defaults — Phase 7 doc updates expose these via env, but for now
        // we use the EP-00015 spec values.
        3,
        4000,
    )
    .await;

    // Previous-run result for recurring tasks (EP-00015 Decision C).
    let previous_run_result = task
        .as_ref()
        .filter(|t| t.schedule.is_some())
        .and_then(|t| t.result.as_deref())
        .map(crate::memory_context::format_previous_run_result)
        .unwrap_or_default();

    // Execute via agent loop
    let result = agent_loop
        .run(
            &message,
            &conv_id,
            "task",
            None,
            &recommended_context,
            &selected_preferences,
            &prior_work,
            &previous_run_result,
        )
        .await;

    // Session log + end. Per EP-00015 Decision E2 the task channel intentionally
    // does NOT produce a session draft (would be one-line spam) — research drafts
    // and `tasks.result` already cover task content.
    {
        let mut mgr = session_mgr.lock().await;
        mgr.log_turn(&conv_id, &message, &result).await;
        mgr.end_session(&conv_id).await;
    }

    // Complete task in DB
    let result_truncated: String = result.chars().take(2000).collect();
    let task_failed = result.starts_with("I'm sorry, all models failed")
        || result.starts_with("LLM call failed");
    if task_failed {
        if let Err(e) = db.update_task(key, Some("blocked"), Some(&result_truncated), None, None) {
            error!(task = %key, error = %e, "failed to mark task as blocked");
        }
        warn!(task = %key, "task execution failed");
        return;
    }
    if let Err(e) = db.complete_task(key, &result_truncated) {
        error!(task = %key, error = %e, "failed to complete task");
        return;
    }
    info!(task = %key, "task completed");

    // Skip everything below for system tasks (Decision I).
    let task_metadata = task
        .as_ref()
        .and_then(|t| t.metadata.as_deref())
        .and_then(|m| serde_json::from_str::<crate::db::TaskMetadata>(m).ok());

    if task_metadata
        .as_ref()
        .and_then(|m| m.system)
        .unwrap_or(false)
    {
        debug!(task = %key, "system task — skipping research draft + reflection");
        return;
    }

    // EP-00015 Decision E — research draft persistence (opt-in).
    let want_draft = task_metadata
        .as_ref()
        .and_then(|m| m.persist_as_draft)
        .unwrap_or(false);
    if want_draft {
        // Refresh the task row to capture the latest result from complete_task.
        let fresh_task = match db.get_task(key) {
            Ok(Some(t)) => t,
            _ => {
                warn!(task = %key, "could not reload task for draft persistence");
                return;
            }
        };
        match crate::draft_writer::persist_research_draft(root_dir, agent_loop, &fresh_task).await {
            Ok(path) => info!(task = %key, path = %path.display(), "research draft persisted"),
            Err(e) => warn!(task = %key, error = %e, "research draft persistence failed"),
        }
    }

    // EP-00015 Decision F + G — reflection counter + maybe-fire (recurring tasks only).
    let is_recurring = task.as_ref().and_then(|t| t.schedule.as_deref()).is_some();
    if is_recurring {
        let outcome = crate::reflection::increment_and_maybe_reflect(
            root_dir,
            db,
            agent_loop,
            crate::reflection::ScopeKind::TaskKey,
            key,
            // Phase 7 will surface this via env; for now the EP default.
            5,
        )
        .await;
        debug!(task = %key, fired = outcome.fired, counter = outcome.counter, "reflection result");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Arc<Database> {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.register_agent("ino").unwrap();
        db
    }

    #[test]
    fn test_get_due_tasks_one_time() {
        let db = setup_db();
        // Create a todo task (one-time, no schedule)
        db.create_task("Do something", None, None, Some("ino"), None, None, None)
            .unwrap();
        db.update_task("TSK-00001", Some("todo"), None, None, None)
            .unwrap();

        let due = get_due_tasks(&db, "ino").unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, "TSK-00001");
    }

    #[test]
    fn test_get_due_tasks_not_todo() {
        let db = setup_db();
        // backlog task — not due
        db.create_task("Backlog", None, None, Some("ino"), None, None, None)
            .unwrap();

        let due = get_due_tasks(&db, "ino").unwrap();
        assert!(due.is_empty());
    }

    #[test]
    fn test_get_due_tasks_scheduled_not_yet() {
        let db = setup_db();
        // Create hourly task that just ran
        db.create_task("Hourly", None, None, Some("ino"), Some("hourly"), None, None)
            .unwrap();
        // Mark as just run by completing it (sets last_run_at)
        db.complete_task("TSK-00001", "done").unwrap();

        let due = get_due_tasks(&db, "ino").unwrap();
        assert!(due.is_empty()); // just ran, not due yet
    }

    #[test]
    fn test_get_due_tasks_different_agent() {
        let db = setup_db();
        db.register_agent("robin").unwrap();
        db.create_task("Ino task", None, None, Some("ino"), None, None, None)
            .unwrap();
        db.update_task("TSK-00001", Some("todo"), None, None, None)
            .unwrap();
        db.create_task("Robin task", None, None, Some("robin"), None, None, None)
            .unwrap();
        db.update_task("TSK-00002", Some("todo"), None, None, None)
            .unwrap();

        let ino_due = get_due_tasks(&db, "ino").unwrap();
        assert_eq!(ino_due.len(), 1);
        assert_eq!(ino_due[0].0, "TSK-00001");

        let robin_due = get_due_tasks(&db, "robin").unwrap();
        assert_eq!(robin_due.len(), 1);
        assert_eq!(robin_due[0].0, "TSK-00002");
    }

    #[test]
    fn test_get_due_tasks_scheduled_never_run() {
        let db = setup_db();
        // Hourly task that has never run → should be due
        db.create_task("Hourly", None, None, Some("ino"), Some("hourly"), None, None)
            .unwrap();

        let due = get_due_tasks(&db, "ino").unwrap();
        assert_eq!(due.len(), 1);
    }
}
