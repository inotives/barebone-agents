use std::path::Path;

use chrono::{Duration, Utc};
use serde_json::json;

use crate::config::{AgentConfig, ModelRegistry};
use crate::config::settings::agent_dir;
use crate::db::{Database, DbTokenUsage};

/// Which sections to render.
pub enum Section {
    All,
    Agents,
    Tokens,
    Tasks,
    Missions,
    Activity,
}

impl Section {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "agents" => Ok(Self::Agents),
            "tokens" => Ok(Self::Tokens),
            "tasks" => Ok(Self::Tasks),
            "missions" => Ok(Self::Missions),
            "activity" => Ok(Self::Activity),
            _ => Err(format!(
                "Unknown section '{}'. Valid: agents, tokens, tasks, missions, activity",
                s
            )),
        }
    }
}

/// Token period filter.
pub enum TokenPeriod {
    Today,
    Week,
    Total,
}

impl TokenPeriod {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "today" => Ok(Self::Today),
            "week" => Ok(Self::Week),
            "total" => Ok(Self::Total),
            _ => Err(format!(
                "Unknown token period '{}'. Valid: today, week, total",
                s
            )),
        }
    }

    /// Return the `since` date string for SQL, or None for total.
    pub fn since(&self) -> Option<String> {
        match self {
            Self::Today => {
                Some(Utc::now().format("%Y-%m-%d").to_string())
            }
            Self::Week => {
                let week_ago = Utc::now() - Duration::days(7);
                Some(week_ago.format("%Y-%m-%d").to_string())
            }
            Self::Total => None,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Today => "today",
            Self::Week => "week",
            Self::Total => "total",
        }
    }
}

pub struct StatusQuery {
    pub agent_filter: Option<String>,
    pub token_period: TokenPeriod,
    pub section: Section,
    pub json: bool,
}

/// Run the status dashboard and print output.
pub fn run_status(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    query: &StatusQuery,
) {
    if query.json {
        let value = build_json(db, root_dir, model_registry, query);
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        let text = build_text(db, root_dir, model_registry, query);
        print!("{}", text);
    }
}

fn build_json(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    query: &StatusQuery,
) -> serde_json::Value {
    let mut obj = json!({});

    let show_all = matches!(query.section, Section::All);

    if show_all || matches!(query.section, Section::Agents) {
        obj["agents"] = json_agents(db, root_dir, model_registry, query.agent_filter.as_deref());
    }
    if show_all || matches!(query.section, Section::Tokens) {
        obj["tokens"] = json_tokens(db, &query.token_period, query.agent_filter.as_deref());
    }
    if show_all || matches!(query.section, Section::Tasks) {
        obj["tasks"] = json_tasks(db, query.agent_filter.as_deref());
    }
    if show_all || matches!(query.section, Section::Missions) {
        obj["missions"] = json_missions(db);
    }
    if show_all || matches!(query.section, Section::Activity) {
        obj["activity"] = json_activity(db, query.agent_filter.as_deref());
    }

    obj
}

fn build_text(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    query: &StatusQuery,
) -> String {
    let mut out = String::new();
    let show_all = matches!(query.section, Section::All);

    out.push_str("=== barebone-agent status ===\n\n");

    if show_all || matches!(query.section, Section::Agents) {
        out.push_str(&text_agents(db, root_dir, model_registry, query.agent_filter.as_deref()));
    }
    if show_all || matches!(query.section, Section::Tokens) {
        out.push_str(&text_tokens(db, &query.token_period, query.agent_filter.as_deref()));
    }
    if show_all || matches!(query.section, Section::Tasks) {
        out.push_str(&text_tasks(db, query.agent_filter.as_deref()));
    }
    if show_all || matches!(query.section, Section::Missions) {
        out.push_str(&text_missions(db));
    }
    if show_all || matches!(query.section, Section::Activity) {
        out.push_str(&text_activity(db, query.agent_filter.as_deref()));
    }

    out
}

// --- Agents section ---

fn get_agent_info(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    filter: Option<&str>,
) -> Vec<(String, String, String, Option<String>)> {
    let agents = db.get_registered_agents().unwrap_or_default();
    agents
        .into_iter()
        .filter(|name| filter.is_none() || filter == Some(name.as_str()))
        .map(|name| {
            let dir = agent_dir(root_dir, &name);
            let (role, model) = AgentConfig::load(&dir)
                .map(|cfg| (cfg.role, cfg.model))
                .unwrap_or_else(|_| ("unknown".into(), "unknown".into()));
            let last_active = db.get_agent_last_active(&name).unwrap_or(None);
            (name, role, model, last_active)
        })
        .collect()
}

fn text_agents(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    filter: Option<&str>,
) -> String {
    let agents = get_agent_info(db, root_dir, model_registry, filter);
    let mut out = String::from("[Agents]\n");
    if agents.is_empty() {
        out.push_str("  (none registered)\n");
    } else {
        for (name, role, model, last_active) in &agents {
            let active = last_active.as_deref().unwrap_or("never");
            out.push_str(&format!(
                "  {:<12} role={:<10} model={:<25} last_active={}\n",
                name, role, model, active
            ));
        }
    }
    out.push('\n');
    out
}

fn json_agents(
    db: &Database,
    root_dir: &Path,
    model_registry: &ModelRegistry,
    filter: Option<&str>,
) -> serde_json::Value {
    let agents = get_agent_info(db, root_dir, model_registry, filter);
    json!(agents
        .iter()
        .map(|(name, role, model, last_active)| {
            json!({
                "name": name,
                "role": role,
                "model": model,
                "last_active": last_active,
            })
        })
        .collect::<Vec<_>>())
}

// --- Tokens section ---

fn text_tokens(db: &Database, period: &TokenPeriod, filter: Option<&str>) -> String {
    let mut out = format!("[Tokens — {}]\n", period.label());
    let since = period.since();
    let agents = db.get_registered_agents().unwrap_or_default();
    let agents: Vec<_> = agents
        .into_iter()
        .filter(|n| filter.is_none() || filter == Some(n.as_str()))
        .collect();

    if agents.is_empty() {
        out.push_str("  (no agents)\n");
    } else {
        for name in &agents {
            let usage = db
                .get_token_usage(name, since.as_deref())
                .unwrap_or(DbTokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                });
            let total = usage.input_tokens + usage.output_tokens;
            out.push_str(&format!(
                "  {:<12} input={:<10} output={:<10} total={}\n",
                name, usage.input_tokens, usage.output_tokens, total
            ));
        }
    }
    out.push('\n');
    out
}

fn json_tokens(
    db: &Database,
    period: &TokenPeriod,
    filter: Option<&str>,
) -> serde_json::Value {
    let since = period.since();
    let agents = db.get_registered_agents().unwrap_or_default();
    let agents: Vec<_> = agents
        .into_iter()
        .filter(|n| filter.is_none() || filter == Some(n.as_str()))
        .collect();

    json!({
        "period": period.label(),
        "agents": agents.iter().map(|name| {
            let usage = db.get_token_usage(name, since.as_deref())
                .unwrap_or(DbTokenUsage {
                    input_tokens: 0, output_tokens: 0,
                });
            json!({
                "name": name,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total": usage.input_tokens + usage.output_tokens,
            })
        }).collect::<Vec<_>>()
    })
}

// --- Tasks section ---

fn text_tasks(db: &Database, filter: Option<&str>) -> String {
    let mut out = String::from("[Tasks]\n");

    // Status counts
    let counts = db.get_task_status_counts(filter).unwrap_or_default();
    if counts.is_empty() {
        out.push_str("  (no tasks)\n");
    } else {
        out.push_str("  Status counts: ");
        let parts: Vec<String> = counts
            .iter()
            .map(|(status, count)| format!("{}={}", status, count))
            .collect();
        out.push_str(&parts.join(", "));
        out.push('\n');

        // Active tasks (in_progress, todo) sorted by priority
        let active = db
            .list_tasks(filter, None, None)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.status == "in_progress" || t.status == "todo")
            .collect::<Vec<_>>();

        if !active.is_empty() {
            out.push_str("  Active:\n");
            for task in &active {
                let agent = task.agent_name.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "    {} [{}] {} (pri={}, agent={})\n",
                    task.key, task.status, task.title, task.priority, agent
                ));
            }
        }
    }
    out.push('\n');
    out
}

fn json_tasks(db: &Database, filter: Option<&str>) -> serde_json::Value {
    let counts = db.get_task_status_counts(filter).unwrap_or_default();
    let active = db
        .list_tasks(filter, None, None)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.status == "in_progress" || t.status == "todo")
        .collect::<Vec<_>>();

    json!({
        "status_counts": counts.iter().map(|(s, c)| json!({"status": s, "count": c})).collect::<Vec<_>>(),
        "active": active.iter().map(|t| json!({
            "key": t.key,
            "title": t.title,
            "status": t.status,
            "priority": t.priority,
            "agent": t.agent_name,
        })).collect::<Vec<_>>()
    })
}

// --- Missions section ---

fn text_missions(db: &Database) -> String {
    let mut out = String::from("[Missions]\n");
    let missions = db.list_missions(None).unwrap_or_default();

    if missions.is_empty() {
        out.push_str("  (no missions)\n");
    } else {
        for m in &missions {
            let (done, total) = db
                .get_mission_task_progress(&m.key)
                .unwrap_or((0, 0));
            out.push_str(&format!(
                "  {} [{}] {} ({}/{})\n",
                m.key, m.status, m.title, done, total
            ));
        }
    }
    out.push('\n');
    out
}

fn json_missions(db: &Database) -> serde_json::Value {
    let missions = db.list_missions(None).unwrap_or_default();
    json!(missions
        .iter()
        .map(|m| {
            let (done, total) = db.get_mission_task_progress(&m.key).unwrap_or((0, 0));
            json!({
                "key": m.key,
                "title": m.title,
                "status": m.status,
                "done": done,
                "total": total,
            })
        })
        .collect::<Vec<_>>())
}

// --- Activity section ---

fn text_activity(db: &Database, filter: Option<&str>) -> String {
    let mut out = String::from("[Activity]\n");
    let events = db.get_recent_activity(filter, 15).unwrap_or_default();

    if events.is_empty() {
        out.push_str("  (no recent activity)\n");
    } else {
        for (time, agent, channel, role, content) in &events {
            let preview = content.replace('\n', " ");
            out.push_str(&format!(
                "  {} {}/{} [{}] {}\n",
                time, agent, channel, role, preview
            ));
        }
    }
    out.push('\n');
    out
}

fn json_activity(db: &Database, filter: Option<&str>) -> serde_json::Value {
    let events = db.get_recent_activity(filter, 15).unwrap_or_default();
    json!(events
        .iter()
        .map(|(time, agent, channel, role, content)| {
            json!({
                "time": time,
                "agent": agent,
                "channel": channel,
                "role": role,
                "content": content,
            })
        })
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Database, tempfile::TempDir) {
        let db = Database::open_in_memory().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        (db, dir)
    }

    #[test]
    fn test_token_period_parse() {
        assert!(matches!(TokenPeriod::parse("today"), Ok(TokenPeriod::Today)));
        assert!(matches!(TokenPeriod::parse("week"), Ok(TokenPeriod::Week)));
        assert!(matches!(TokenPeriod::parse("total"), Ok(TokenPeriod::Total)));
        assert!(TokenPeriod::parse("invalid").is_err());
    }

    #[test]
    fn test_token_period_since() {
        let today = TokenPeriod::Today;
        assert!(today.since().is_some());
        let total = TokenPeriod::Total;
        assert!(total.since().is_none());
    }

    #[test]
    fn test_section_parse() {
        assert!(matches!(Section::parse("agents"), Ok(Section::Agents)));
        assert!(matches!(Section::parse("tokens"), Ok(Section::Tokens)));
        assert!(matches!(Section::parse("tasks"), Ok(Section::Tasks)));
        assert!(matches!(Section::parse("missions"), Ok(Section::Missions)));
        assert!(matches!(Section::parse("activity"), Ok(Section::Activity)));
        assert!(Section::parse("invalid").is_err());
    }

    #[test]
    fn test_text_agents_empty() {
        let (db, dir) = setup();
        let registry = ModelRegistry { models: vec![] };
        let out = text_agents(&db, dir.path(), &registry, None);
        assert!(out.contains("(none registered)"));
    }

    #[test]
    fn test_text_tokens_empty() {
        let (db, _dir) = setup();
        let out = text_tokens(&db, &TokenPeriod::Today, None);
        assert!(out.contains("[Tokens — today]"));
        assert!(out.contains("(no agents)"));
    }

    #[test]
    fn test_text_tokens_with_data() {
        let (db, _dir) = setup();
        db.register_agent("ino").unwrap();
        db.save_message("c1", "ino", "user", "hi", "cli", None, 500, 0, "t1", true, None).unwrap();
        db.save_message("c1", "ino", "assistant", "yo", "cli", Some("m1"), 0, 200, "t1", true, None).unwrap();

        let out = text_tokens(&db, &TokenPeriod::Total, None);
        assert!(out.contains("ino"));
        assert!(out.contains("500"));
        assert!(out.contains("200"));
    }

    #[test]
    fn test_text_tasks_empty() {
        let (db, _dir) = setup();
        let out = text_tasks(&db, None);
        assert!(out.contains("(no tasks)"));
    }

    #[test]
    fn test_text_tasks_with_data() {
        let (db, _dir) = setup();
        db.create_task("Build feature", None, None, Some("ino"), None, None, None).unwrap();
        db.update_task("TSK-00001", Some("in_progress"), None, None, None).unwrap();
        db.create_task("Write tests", None, None, Some("ino"), None, None, None).unwrap();

        let out = text_tasks(&db, None);
        assert!(out.contains("Status counts:"));
        assert!(out.contains("TSK-00001"));
        assert!(out.contains("Build feature"));
    }

    #[test]
    fn test_text_missions_empty() {
        let (db, _dir) = setup();
        let out = text_missions(&db);
        assert!(out.contains("(no missions)"));
    }

    #[test]
    fn test_text_missions_with_progress() {
        let (db, _dir) = setup();
        let mk = db.create_mission("Ship v1", None, None).unwrap();
        db.create_task("Task A", None, Some(&mk), None, None, None, None).unwrap();
        let tk = db.create_task("Task B", None, Some(&mk), None, None, None, None).unwrap();
        db.complete_task(&tk, "done").unwrap();

        let out = text_missions(&db);
        assert!(out.contains("Ship v1"));
        assert!(out.contains("(1/2)"));
    }

    #[test]
    fn test_text_activity_empty() {
        let (db, _dir) = setup();
        let out = text_activity(&db, None);
        assert!(out.contains("(no recent activity)"));
    }

    #[test]
    fn test_text_activity_with_data() {
        let (db, _dir) = setup();
        db.register_agent("ino").unwrap();
        db.save_message("c1", "ino", "user", "hello there", "cli", None, 0, 0, "t1", true, None).unwrap();

        let out = text_activity(&db, None);
        assert!(out.contains("ino"));
        assert!(out.contains("hello there"));
    }

    #[test]
    fn test_json_output() {
        let (db, dir) = setup();
        let registry = ModelRegistry { models: vec![] };
        db.register_agent("ino").unwrap();

        let query = StatusQuery {
            agent_filter: None,
            token_period: TokenPeriod::Total,
            section: Section::All,
            json: true,
        };

        let value = build_json(&db, dir.path(), &registry, &query);
        assert!(value.get("agents").is_some());
        assert!(value.get("tokens").is_some());
        assert!(value.get("tasks").is_some());
        assert!(value.get("missions").is_some());
        assert!(value.get("activity").is_some());
    }

    #[test]
    fn test_json_single_section() {
        let (db, dir) = setup();
        let registry = ModelRegistry { models: vec![] };

        let query = StatusQuery {
            agent_filter: None,
            token_period: TokenPeriod::Total,
            section: Section::Tokens,
            json: true,
        };

        let value = build_json(&db, dir.path(), &registry, &query);
        assert!(value.get("tokens").is_some());
        assert!(value.get("agents").is_none());
    }

    #[test]
    fn test_agent_filter() {
        let (db, _dir) = setup();
        db.register_agent("ino").unwrap();
        db.register_agent("robin").unwrap();
        db.save_message("c1", "ino", "user", "hi", "cli", None, 100, 0, "t1", true, None).unwrap();
        db.save_message("c2", "robin", "user", "hey", "cli", None, 200, 0, "t2", true, None).unwrap();

        let out = text_tokens(&db, &TokenPeriod::Total, Some("ino"));
        assert!(out.contains("ino"));
        assert!(!out.contains("robin"));
    }
}
