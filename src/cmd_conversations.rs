use serde_json::json;

use crate::cli::ConversationsCommand;
use crate::db::Database;

pub fn run(db: &Database, cmd: ConversationsCommand) -> Result<(), String> {
    match cmd {
        ConversationsCommand::List { agent, limit, json } => {
            run_list(db, agent.as_deref(), limit, json)
        }
        ConversationsCommand::Show { id, full, json } => run_show(db, &id, full, json),
    }
}

fn run_list(
    db: &Database,
    agent: Option<&str>,
    limit: u32,
    as_json: bool,
) -> Result<(), String> {
    let convs = db.list_conversations(agent, limit)?;

    if as_json {
        let arr: Vec<_> = convs
            .iter()
            .map(|c| {
                json!({
                    "conversation_id": c.conversation_id,
                    "agent": c.agent_name,
                    "channel": c.channel_type,
                    "turns": c.turn_count,
                    "messages": c.message_count,
                    "input_tokens": c.total_input_tokens,
                    "output_tokens": c.total_output_tokens,
                    "started": c.first_message_at,
                    "last_activity": c.last_message_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else if convs.is_empty() {
        println!("(no conversations)");
    } else {
        println!(
            "{:<40} {:<8} {:<8} {:<6} {:<12} {}",
            "CONVERSATION_ID", "AGENT", "CHANNEL", "TURNS", "TOKENS", "LAST ACTIVITY"
        );
        println!("{}", "-".repeat(90));
        for c in &convs {
            let tokens = c.total_input_tokens + c.total_output_tokens;
            println!(
                "{:<40} {:<8} {:<8} {:<6} {:<12} {}",
                truncate(&c.conversation_id, 38),
                c.agent_name,
                c.channel_type,
                c.turn_count,
                tokens,
                c.last_message_at,
            );
        }
    }
    Ok(())
}

fn run_show(db: &Database, id: &str, full: bool, as_json: bool) -> Result<(), String> {
    let messages = if full {
        db.load_conversation(id)?
    } else {
        db.load_history(id, 1000)?
    };

    if messages.is_empty() {
        return Err(format!("No messages found for conversation: {}", id));
    }

    if as_json {
        let arr: Vec<_> = messages
            .iter()
            .map(|m| {
                let mut obj = json!({
                    "role": m.role,
                    "content": m.content,
                    "turn_id": m.turn_id,
                    "is_final": m.is_final,
                    "created_at": m.created_at,
                });
                if m.input_tokens > 0 || m.output_tokens > 0 {
                    obj["input_tokens"] = json!(m.input_tokens);
                    obj["output_tokens"] = json!(m.output_tokens);
                }
                if let Some(model) = &m.model_used {
                    obj["model"] = json!(model);
                }
                obj
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else {
        let agent = &messages[0].agent_name;
        let channel = &messages[0].channel_type;
        println!(
            "Conversation: {}  (agent={}, channel={})",
            id, agent, channel
        );
        println!("{}", "-".repeat(70));

        for m in &messages {
            let role_tag = match m.role.as_str() {
                "user" => "[user]",
                "assistant" => "[assistant]",
                "tool" => "[tool]",
                _ => "[?]",
            };

            if full && !m.is_final {
                // Show non-final messages indented
                let preview = truncate(&m.content, 200);
                println!("  {} {} (turn={})", role_tag, m.created_at, m.turn_id);
                println!("    {}", preview);
            } else {
                println!("{} {}", role_tag, m.created_at);
                println!("{}", m.content);
            }
            println!();
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn save_msg(
        db: &Database,
        conv_id: &str,
        agent: &str,
        role: &str,
        content: &str,
        is_final: bool,
        turn_id: &str,
    ) {
        db.save_message(
            conv_id, agent, role, content, "cli", None, 100, 50, turn_id, is_final, None,
        )
        .unwrap();
    }

    #[test]
    fn test_list_empty() {
        let db = setup();
        run_list(&db, None, 20, false).unwrap();
    }

    #[test]
    fn test_list_with_data() {
        let db = setup();
        save_msg(&db, "conv-1", "ino", "user", "hello", true, "t1");
        save_msg(&db, "conv-1", "ino", "assistant", "hi", true, "t1");
        save_msg(&db, "conv-2", "robin", "user", "hey", true, "t2");

        run_list(&db, None, 20, false).unwrap();
        run_list(&db, Some("ino"), 20, false).unwrap();
        run_list(&db, None, 20, true).unwrap();
    }

    #[test]
    fn test_show_final_only() {
        let db = setup();
        save_msg(&db, "conv-1", "ino", "user", "question", true, "t1");
        save_msg(&db, "conv-1", "ino", "assistant", "tool call", false, "t1");
        save_msg(&db, "conv-1", "ino", "tool", "result", false, "t1");
        save_msg(&db, "conv-1", "ino", "assistant", "answer", true, "t1");

        // Without --full, should only show final messages
        run_show(&db, "conv-1", false, false).unwrap();
    }

    #[test]
    fn test_show_full() {
        let db = setup();
        save_msg(&db, "conv-1", "ino", "user", "question", true, "t1");
        save_msg(&db, "conv-1", "ino", "assistant", "tool call", false, "t1");
        save_msg(&db, "conv-1", "ino", "tool", "result", false, "t1");
        save_msg(&db, "conv-1", "ino", "assistant", "answer", true, "t1");

        // With --full, should show all messages
        run_show(&db, "conv-1", true, false).unwrap();
        run_show(&db, "conv-1", true, true).unwrap();
    }

    #[test]
    fn test_show_not_found() {
        let db = setup();
        let result = run_show(&db, "nonexistent", false, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a long string", 10), "this is a ...");
    }
}
