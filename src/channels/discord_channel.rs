use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use poise::serenity_prelude as serenity;
use serenity::all::Message;

use crate::agent_loop::AgentLoop;
use crate::config::agent::DiscordConfig;
use crate::session::SessionManager;

/// Shared state accessible from the poise event handler.
pub struct BotState {
    agent_loop: Arc<AgentLoop>,
    session_mgr: Arc<Mutex<SessionManager>>,
    config: DiscordConfig,
}

type PoiseData = BotState;
type PoiseError = Box<dyn std::error::Error + Send + Sync>;

/// Split a message into chunks that fit within Discord's 2000-char limit.
pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Try to split at the last newline within the limit
        let split_at = remaining[..max_len]
            .rfind('\n')
            .unwrap_or_else(|| {
                // Fall back to last space
                remaining[..max_len]
                    .rfind(' ')
                    .unwrap_or(max_len)
            });

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..].trim_start();
    }

    chunks
}

/// Determine the channel kind label from message context.
/// Returns "dm", "thread", or "channel".
pub fn channel_kind(msg: &Message) -> &'static str {
    if msg.guild_id.is_none() {
        return "dm";
    }
    if msg.thread.is_some() {
        return "thread";
    }
    "channel"
}

/// Generate a session-key / conversation ID for Discord.
/// Format: `discord-{dm|thread|channel}-{id}-sess-{uuid[:8]}`
pub fn make_session_key(kind: &str, channel_id: serenity::ChannelId) -> String {
    let short_uuid = &uuid::Uuid::new_v4().to_string()[..8];
    format!("discord-{}-{}-sess-{}", kind, channel_id, short_uuid)
}

/// Check whether the message is allowed based on the `allowFrom` user-ID list.
/// An empty list means all users are allowed.
fn is_user_allowed(config: &DiscordConfig, user_id: &str) -> bool {
    config.allow_from.is_empty() || config.allow_from.iter().any(|id| id == user_id)
}

/// Check whether this message requires a bot mention and, if so, whether it has one.
/// Returns `true` if the message should be processed.
fn passes_mention_gate(
    config: &DiscordConfig,
    msg: &Message,
    bot_id: serenity::UserId,
) -> bool {
    let guild_id = match msg.guild_id {
        Some(id) => id.to_string(),
        None => return true, // DMs always pass
    };

    let require_mention = config
        .guilds
        .get(&guild_id)
        .map(|g| g.require_mention)
        .unwrap_or(false);

    if !require_mention {
        return true;
    }

    msg.mentions.iter().any(|u| u.id == bot_id)
}

/// Strip the bot mention from the message content so the agent sees clean text.
fn strip_mention(content: &str, bot_id: serenity::UserId) -> String {
    content
        .replace(&format!("<@{}>", bot_id), "")
        .replace(&format!("<@!{}>", bot_id), "")
        .trim()
        .to_string()
}

/// The poise event handler — processes every non-bot message.
async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, PoiseData, PoiseError>,
    data: &PoiseData,
) -> Result<(), PoiseError> {
    if let serenity::FullEvent::Message { new_message } = event {
        // Ignore bot messages
        if new_message.author.bot {
            return Ok(());
        }

        let author_id = new_message.author.id.to_string();

        // allowFrom gate
        if !is_user_allowed(&data.config, &author_id) {
            debug!(user = %author_id, "message from non-allowed user, ignoring");
            return Ok(());
        }

        let bot_id = ctx.cache.current_user().id;

        // requireMention gate
        if !passes_mention_gate(&data.config, new_message, bot_id) {
            return Ok(());
        }

        let content = strip_mention(&new_message.content, bot_id);
        if content.is_empty() {
            return Ok(());
        }

        let channel_id = new_message.channel_id;

        // Build or reuse session key.
        // We use the channel_id as the conversation-grouping key so messages
        // in the same channel / thread share a conversation.
        let conv_id = {
            let mut mgr = data.session_mgr.lock().await;
            let conv_key = format!("discord-ch-{}", channel_id);
            mgr.ensure_session(&conv_key, "discord").await;
            conv_key
        };

        info!(
            agent = %data.agent_loop.agent_name,
            user = %new_message.author.name,
            channel = %channel_id,
            "discord message received"
        );

        // Show typing indicator
        let typing = channel_id.start_typing(&ctx.http);

        // Run agent loop
        let response = data
            .agent_loop
            .run(&content, &conv_id, "discord", None)
            .await;

        // Stop typing
        drop(typing);

        // Log turn to session manager
        {
            let mgr = data.session_mgr.lock().await;
            mgr.log_turn(&conv_id, &content, &response).await;
        }

        // Split and send response
        let chunks = split_message(&response, 2000);
        for chunk in &chunks {
            if let Err(e) = channel_id.say(&ctx.http, chunk).await {
                error!(error = %e, "failed to send Discord message");
            }
        }
    }

    Ok(())
}

/// Start the Discord bot. Returns a JoinHandle that runs until the bot disconnects.
pub async fn run_discord(
    agent_loop: Arc<AgentLoop>,
    session_mgr: Arc<Mutex<SessionManager>>,
    config: DiscordConfig,
    bot_token: &str,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let token = bot_token.to_string();

    let data = BotState {
        agent_loop,
        session_mgr,
        config,
    };

    let intents = serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::DIRECT_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|_ctx, _ready, _framework| {
            Box::pin(async move {
                info!("Discord bot connected");
                Ok(data)
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(&token, intents)
        .framework(framework)
        .await
        .map_err(|e| format!("Failed to create Discord client: {}", e))?;

    let handle = tokio::spawn(async move {
        if let Err(e) = client.start().await {
            error!(error = %e, "Discord bot error");
        }
    });

    info!("Discord bot task spawned");
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_split_message_short() {
        let chunks = split_message("Hello world", 2000);
        assert_eq!(chunks, vec!["Hello world"]);
    }

    #[test]
    fn test_split_message_exact_limit() {
        let msg = "a".repeat(2000);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2000);
    }

    #[test]
    fn test_split_message_over_limit_newline() {
        let mut msg = "a".repeat(1500);
        msg.push('\n');
        msg.push_str(&"b".repeat(1500));
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "a".repeat(1500));
        assert_eq!(chunks[1], "b".repeat(1500));
    }

    #[test]
    fn test_split_message_over_limit_space() {
        let mut msg = "a".repeat(1500);
        msg.push(' ');
        msg.push_str(&"b".repeat(1500));
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "a".repeat(1500));
        assert_eq!(chunks[1], "b".repeat(1500));
    }

    #[test]
    fn test_split_message_no_break_point() {
        let msg = "a".repeat(5000);
        let chunks = split_message(&msg, 2000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 2000);
        assert_eq!(chunks[2].len(), 1000);
    }

    #[test]
    fn test_split_message_empty() {
        let chunks = split_message("", 2000);
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn test_is_user_allowed_empty_list() {
        let config = DiscordConfig {
            enabled: true,
            allow_from: vec![],
            guilds: HashMap::new(),
        };
        assert!(is_user_allowed(&config, "anyone"));
    }

    #[test]
    fn test_is_user_allowed_in_list() {
        let config = DiscordConfig {
            enabled: true,
            allow_from: vec!["123".into(), "456".into()],
            guilds: HashMap::new(),
        };
        assert!(is_user_allowed(&config, "123"));
        assert!(is_user_allowed(&config, "456"));
    }

    #[test]
    fn test_is_user_allowed_not_in_list() {
        let config = DiscordConfig {
            enabled: true,
            allow_from: vec!["123".into()],
            guilds: HashMap::new(),
        };
        assert!(!is_user_allowed(&config, "999"));
    }

    #[test]
    fn test_strip_mention() {
        let bot_id = serenity::UserId::new(12345);
        assert_eq!(strip_mention("<@12345> hello", bot_id), "hello");
        assert_eq!(strip_mention("<@!12345> hello", bot_id), "hello");
        assert_eq!(strip_mention("hello <@12345>", bot_id), "hello");
        assert_eq!(strip_mention("just text", bot_id), "just text");
    }

    #[test]
    fn test_make_session_key_dm() {
        let key = make_session_key("dm", serenity::ChannelId::new(100));
        assert!(key.starts_with("discord-dm-100-sess-"));
        assert_eq!(key.len(), "discord-dm-100-sess-".len() + 8);
    }

    #[test]
    fn test_make_session_key_thread() {
        let key = make_session_key("thread", serenity::ChannelId::new(200));
        assert!(key.starts_with("discord-thread-200-sess-"));
    }

    #[test]
    fn test_make_session_key_channel() {
        let key = make_session_key("channel", serenity::ChannelId::new(300));
        assert!(key.starts_with("discord-channel-300-sess-"));
    }
}
