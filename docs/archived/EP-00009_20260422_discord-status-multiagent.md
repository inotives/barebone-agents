# EP-00009 — Discord Channel + Status Subcommand + Multi-Agent

## Problem / Pain Points
- Only CLI channel exists — need Discord for remote/mobile interaction
- No way to monitor agent health, token usage, task progress without querying DB directly
- Only single-agent mode — need multi-agent startup with shared resources and CLI routing
- Discord needs TTL-based sessions, message splitting, typing indicators, and per-guild config

## Suggested Solution

### Phase 1: Discord channel
- Discord bot using `serenity` or `poise` crate
- Per-guild configuration from agent.yml (requireMention, allowFrom)
- TTL-based session tracking (SESSION_TTL_MINUTES) via SessionManager
- First-mention routing in multi-bot scenarios
- 2000 char message splitting for long responses
- Typing indicator while agent is processing
- Session key: `discord-{dm|thread|channel}-{id}-sess-{uuid[:8]}`

### Phase 2: Status subcommand
- `barebone-agent status` — read-only dashboard
- Sections: agents, tokens, tasks, missions, activity
- CLI flags: `--agent NAME`, `--tokens PERIOD`, `--json`, `--section SECTION`
- Token usage: today (default) | week | total
- Agents: name, role, model, last active
- Tasks: status counts + active task list (priority sorted)
- Missions: key, status, title, done/total tasks
- Activity: recent events timeline from conversations + tasks
- JSON output mode for programmatic consumption

### Phase 3: Multi-agent startup
- Shared resources: model registry, DB connection
- Per-agent: LLMClientPool, ToolRegistry, MCP loaders, AgentLoop, Heartbeat, Discord bot
- CLI routing: single CLI instance, `@name` prefix dispatches to agent loops
- `barebone-agent run --agent ino,robin` or `--all` to start multiple agents
- Each agent gets its own heartbeat background task

### Phase 4: Graceful shutdown (multi-agent)
- Signal handler for SIGINT/SIGTERM
- Stop all heartbeats, end all sessions, close MCP connections per agent
- Reverse startup order

### Key decisions
- `poise` over raw `serenity` — nicer command framework, built on serenity
- Status reads directly from SQLite — no separate status service
- Multi-agent CLI uses `@name` prefix, unrouted messages go to first/default agent
- Discord bots run as separate tokio tasks per agent

## Implementation Status
- [x] Phase 1: Discord channel (bot, sessions, message splitting, guild config) + unit tests
- [x] Phase 2: Status subcommand (agents, tokens, tasks, missions, activity, JSON) + unit tests
- [x] Phase 3: Multi-agent startup (shared resources, CLI routing, multiple heartbeats)
- [x] Phase 4: Graceful shutdown for multi-agent

## Status: DONE
