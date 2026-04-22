use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;
use crate::agent_loop::AgentLoop;

/// Run the CLI channel in REPL or one-shot mode.
/// Supports multiple agents with `@name` routing.
pub async fn run_cli(
    agents: &HashMap<String, Arc<AgentLoop>>,
    default_agent: &str,
    one_shot_message: Option<&str>,
) {
    // Generate initial conversation IDs per agent
    let mut conv_ids: HashMap<String, String> = agents
        .keys()
        .map(|name| (name.clone(), new_conv_id(name)))
        .collect();
    let mut prev_conv_ids: HashMap<String, Option<String>> = HashMap::new();

    if let Some(message) = one_shot_message {
        // One-shot mode — send to default agent
        if let Some(agent_loop) = agents.get(default_agent) {
            let conv_id = conv_ids.get(default_agent).unwrap();
            let response = agent_loop.run(message, conv_id, "cli", None).await;
            println!("{}", response);
        }
        return;
    }

    // REPL mode
    let agent_names: Vec<&str> = agents.keys().map(|s| s.as_str()).collect();
    if agents.len() == 1 {
        println!(
            "barebone-agent — chatting as '{}' (type /help for commands, /quit to exit)",
            default_agent
        );
    } else {
        println!(
            "barebone-agent — agents: {} (default: {}, use @name to route)",
            agent_names.join(", "),
            default_agent
        );
        println!("Type /help for commands, /quit to exit");
    }
    println!(
        "Conversation: {}\n",
        conv_ids.get(default_agent).unwrap()
    );

    loop {
        print!("{} > ", default_agent);
        io::stdout().flush().ok();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Error reading input: {}", e);
                break;
            }
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Handle commands
        match input {
            "/quit" | "/exit" | "/q" => {
                println!("Goodbye.");
                break;
            }
            "/help" => {
                println!("Commands:");
                if agents.len() > 1 {
                    println!("  @name <msg> — Send message to a specific agent");
                }
                println!("  /continue  — Start new conversation linked to current one");
                println!("  /new       — Start fresh conversation (no link)");
                println!("  /quit      — Exit");
                continue;
            }
            _ => {}
        }

        // Route: determine target agent and message
        let (target_name, message) = parse_route(input, default_agent, agents);

        let agent_loop = match agents.get(target_name) {
            Some(a) => a,
            None => {
                println!("Unknown agent '{}'. Available: {}", target_name, agent_names.join(", "));
                continue;
            }
        };

        // Handle per-agent /continue and /new
        if message == "/continue" {
            let old_id = conv_ids.get(target_name).cloned().unwrap_or_default();
            prev_conv_ids.insert(target_name.to_string(), Some(old_id));
            let new_id = new_conv_id(target_name);
            println!(
                "New conversation for {}: {} (linked to previous)",
                target_name, new_id
            );
            conv_ids.insert(target_name.to_string(), new_id);
            continue;
        }
        if message == "/new" {
            prev_conv_ids.remove(target_name);
            let new_id = new_conv_id(target_name);
            println!("New conversation for {}: {}", target_name, new_id);
            conv_ids.insert(target_name.to_string(), new_id);
            continue;
        }

        let conv_id = conv_ids.get(target_name).unwrap();
        let parent_id = prev_conv_ids
            .get(target_name)
            .and_then(|p| p.as_deref());

        let response = agent_loop.run(message, conv_id, "cli", parent_id).await;

        if agents.len() > 1 {
            println!("\n[{}] {}\n", target_name, response);
        } else {
            println!("\n{}\n", response);
        }

        // Clear parent_id after first message
        if prev_conv_ids.get(target_name).is_some() {
            prev_conv_ids.remove(target_name);
        }
    }
}

/// Parse `@name message` routing. Returns (target_agent_name, message).
fn parse_route<'a>(
    input: &'a str,
    default_agent: &'a str,
    agents: &'a HashMap<String, Arc<AgentLoop>>,
) -> (&'a str, &'a str) {
    if input.starts_with('@') {
        if let Some(space_idx) = input.find(' ') {
            let name = &input[1..space_idx];
            if agents.contains_key(name) {
                return (name, input[space_idx + 1..].trim());
            }
        } else {
            // Just "@name" with no message
            let name = &input[1..];
            if agents.contains_key(name) {
                return (name, "");
            }
        }
    }
    (default_agent, input)
}

fn new_conv_id(agent_name: &str) -> String {
    format!("cli-{}-{}", agent_name, &uuid::Uuid::new_v4().to_string()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_agents() -> HashMap<String, Arc<AgentLoop>> {
        // We can't easily create real AgentLoops in tests, so we test parse_route
        // with an empty map and specific keys
        HashMap::new()
    }

    #[test]
    fn test_parse_route_default() {
        let mut agents: HashMap<String, Arc<AgentLoop>> = HashMap::new();
        // We just need the keys to exist for routing
        // Since we can't construct AgentLoop easily, test the logic directly
        let (target, msg) = parse_route("hello world", "ino", &agents);
        assert_eq!(target, "ino");
        assert_eq!(msg, "hello world");
    }

    #[test]
    fn test_parse_route_at_unknown() {
        let agents: HashMap<String, Arc<AgentLoop>> = HashMap::new();
        let (target, msg) = parse_route("@unknown hello", "ino", &agents);
        // Unknown agent falls through to default
        assert_eq!(target, "ino");
        assert_eq!(msg, "@unknown hello");
    }

    #[test]
    fn test_new_conv_id_format() {
        let id = new_conv_id("ino");
        assert!(id.starts_with("cli-ino-"));
        assert_eq!(id.len(), "cli-ino-".len() + 8);
    }
}
