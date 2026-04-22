mod cli_channel;
pub mod discord_channel;

pub use cli_channel::run_cli;
pub use discord_channel::run_discord;
