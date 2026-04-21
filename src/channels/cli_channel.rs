use std::io::{self, Write};
use crate::agent_loop::AgentLoop;

/// Run the CLI channel in REPL or one-shot mode.
pub async fn run_cli(agent_loop: &AgentLoop, one_shot_message: Option<&str>) {
    let agent_name = &agent_loop.agent_name;

    // Generate initial conversation ID
    let mut conv_id = new_conv_id(agent_name);
    let mut prev_conv_id: Option<String> = None;

    if let Some(message) = one_shot_message {
        // One-shot mode
        let response = agent_loop.run(message, &conv_id, "cli", None).await;
        println!("{}", response);
        return;
    }

    // REPL mode
    println!("barebone-agent — chatting as '{}' (type /help for commands, /quit to exit)", agent_name);
    println!("Conversation: {}\n", conv_id);

    loop {
        print!("{} > ", agent_name);
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
            "/continue" => {
                prev_conv_id = Some(conv_id.clone());
                conv_id = new_conv_id(agent_name);
                println!(
                    "New conversation: {} (linked to previous)",
                    conv_id
                );
                continue;
            }
            "/new" => {
                prev_conv_id = None;
                conv_id = new_conv_id(agent_name);
                println!("New conversation: {}", conv_id);
                continue;
            }
            "/help" => {
                println!("Commands:");
                println!("  /continue  — Start new conversation linked to current one");
                println!("  /new       — Start fresh conversation (no link)");
                println!("  /quit      — Exit");
                continue;
            }
            _ => {}
        }

        let parent_id = prev_conv_id.as_deref();
        let response = agent_loop.run(input, &conv_id, "cli", parent_id).await;

        println!("\n{}\n", response);

        // Clear parent_id after first message (it's stored in DB metadata)
        if prev_conv_id.is_some() {
            prev_conv_id = None;
        }
    }
}

fn new_conv_id(agent_name: &str) -> String {
    format!("cli-{}-{}", agent_name, &uuid::Uuid::new_v4().to_string()[..8])
}
