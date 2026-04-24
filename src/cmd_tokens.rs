use serde_json::json;

use crate::db::Database;
use crate::status::TokenPeriod;

pub fn run(
    db: &Database,
    agent: Option<&str>,
    period: &str,
    by_model: bool,
    by_day: bool,
    as_json: bool,
) -> Result<(), String> {
    let token_period = TokenPeriod::parse(period)?;
    let since = token_period.since();

    if by_model {
        run_by_model(db, agent, since.as_deref(), &token_period, as_json)
    } else if by_day {
        run_by_day(db, agent, as_json)
    } else {
        run_summary(db, agent, since.as_deref(), &token_period, as_json)
    }
}

fn run_summary(
    db: &Database,
    agent: Option<&str>,
    since: Option<&str>,
    period: &TokenPeriod,
    as_json: bool,
) -> Result<(), String> {
    let agents = match agent {
        Some(a) => vec![a.to_string()],
        None => db.get_registered_agents()?,
    };

    if as_json {
        let arr: Vec<_> = agents
            .iter()
            .map(|a| {
                let usage = db.get_token_usage(a, since).unwrap_or(crate::db::DbTokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                });
                json!({
                    "agent": a,
                    "period": period.label(),
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "total": usage.input_tokens + usage.output_tokens,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else {
        println!("Token usage (period: {})", period.label());
        println!();
        println!(
            "{:<12} {:>14} {:>14} {:>14}",
            "AGENT", "INPUT", "OUTPUT", "TOTAL"
        );
        println!("{}", "-".repeat(56));
        for a in &agents {
            let usage = db.get_token_usage(a, since).unwrap_or(crate::db::DbTokenUsage {
                input_tokens: 0,
                output_tokens: 0,
            });
            println!(
                "{:<12} {:>14} {:>14} {:>14}",
                a,
                usage.input_tokens,
                usage.output_tokens,
                usage.input_tokens + usage.output_tokens,
            );
        }
    }
    Ok(())
}

fn run_by_model(
    db: &Database,
    agent: Option<&str>,
    since: Option<&str>,
    period: &TokenPeriod,
    as_json: bool,
) -> Result<(), String> {
    let rows = db.get_token_usage_by_model(agent, since)?;

    if as_json {
        let arr: Vec<_> = rows
            .iter()
            .map(|(model, input, output)| {
                json!({
                    "model": model,
                    "period": period.label(),
                    "input_tokens": input,
                    "output_tokens": output,
                    "total": input + output,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else if rows.is_empty() {
        println!("(no token usage)");
    } else {
        println!(
            "Token usage by model (period: {}{})",
            period.label(),
            agent.map(|a| format!(", agent: {}", a)).unwrap_or_default()
        );
        println!();
        println!(
            "{:<30} {:>14} {:>14} {:>14}",
            "MODEL", "INPUT", "OUTPUT", "TOTAL"
        );
        println!("{}", "-".repeat(74));
        for (model, input, output) in &rows {
            println!(
                "{:<30} {:>14} {:>14} {:>14}",
                model,
                input,
                output,
                input + output,
            );
        }
    }
    Ok(())
}

fn run_by_day(
    db: &Database,
    agent: Option<&str>,
    as_json: bool,
) -> Result<(), String> {
    let rows = db.get_token_usage_by_day(agent, 30)?;

    if as_json {
        let arr: Vec<_> = rows
            .iter()
            .map(|(day, input, output)| {
                json!({
                    "date": day,
                    "input_tokens": input,
                    "output_tokens": output,
                    "total": input + output,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else if rows.is_empty() {
        println!("(no token usage)");
    } else {
        println!(
            "Token usage by day{}",
            agent.map(|a| format!(" (agent: {})", a)).unwrap_or_default()
        );
        println!();
        println!(
            "{:<14} {:>14} {:>14} {:>14}",
            "DATE", "INPUT", "OUTPUT", "TOTAL"
        );
        println!("{}", "-".repeat(58));
        for (day, input, output) in &rows {
            println!(
                "{:<14} {:>14} {:>14} {:>14}",
                day,
                input,
                output,
                input + output,
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.save_message(
            "c1", "ino", "user", "hi", "cli", Some("model-a"), 100, 0, "t1", true, None,
        )
        .unwrap();
        db.save_message(
            "c1", "ino", "assistant", "hello", "cli", Some("model-a"), 0, 200, "t1", true, None,
        )
        .unwrap();
        db.save_message(
            "c2", "robin", "user", "hey", "cli", Some("model-b"), 50, 0, "t2", true, None,
        )
        .unwrap();
        db.save_message(
            "c2", "robin", "assistant", "yo", "cli", Some("model-b"), 0, 150, "t2", true, None,
        )
        .unwrap();
        db
    }

    #[test]
    fn test_summary() {
        let db = setup();
        run_summary(&db, None, None, &TokenPeriod::parse("total").unwrap(), false).unwrap();
    }

    #[test]
    fn test_summary_filtered() {
        let db = setup();
        run_summary(&db, Some("ino"), None, &TokenPeriod::parse("total").unwrap(), false).unwrap();
    }

    #[test]
    fn test_by_model() {
        let db = setup();
        let rows = db.get_token_usage_by_model(None, None).unwrap();
        assert_eq!(rows.len(), 2);
        // model-a: 100 input + 200 output = 300 total (sorted first)
        assert_eq!(rows[0].0, "model-a");
        assert_eq!(rows[0].1, 100);
        assert_eq!(rows[0].2, 200);
    }

    #[test]
    fn test_by_model_filtered() {
        let db = setup();
        let rows = db.get_token_usage_by_model(Some("robin"), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "model-b");
    }

    #[test]
    fn test_by_day() {
        let db = setup();
        let rows = db.get_token_usage_by_day(None, 7).unwrap();
        assert_eq!(rows.len(), 1); // all in same day (in-memory test)
    }

    #[test]
    fn test_by_day_json() {
        let db = setup();
        run_by_day(&db, None, true).unwrap();
    }
}
