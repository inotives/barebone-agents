# EP-00011 — Extended CLI Subcommands

## Problem / Pain Points
- Cannot update task status/priority/agent from CLI — must go through agent tool calls
- Cannot update mission status from CLI
- No way to delete tasks or missions
- No CLI visibility into agent configurations (role, model, fallbacks, MCP servers)
- Token usage only available as aggregate in `status --tokens` — no per-model or daily breakdown
- No way to validate config files before running agents

## Suggested Solution

### Phase 1: tasks update + missions update
- `barebone-agent tasks update <key> [--status X] [--priority X] [--agent X] [--json]`
  - Reuses existing `db.update_task(key, status, result, agent_name, priority)`
- `barebone-agent missions update <key> [--status X] [--title X] [--description X] [--json]`
  - Reuses existing `db.update_mission(key, status, title, description)`
- Add `Update` variants to `TasksCommand` and `MissionsCommand` enums
- Add `run_update()` to `cmd_tasks.rs` and `cmd_missions.rs`

### Phase 2: tasks delete + missions delete
- `barebone-agent tasks delete <key> [--json]`
- `barebone-agent missions delete <key> [--json]`
  - Refuses if mission has tasks — must delete tasks first (safety)
- Add `delete_task(key)` and `delete_mission(key)` to DB layer
- Add `has_tasks(mission_key)` check to missions.rs

### Phase 3: agents list + show
- `barebone-agent agents list [--json]`
  - Discovers agents from `agents/` directory
  - Shows: name, role, model, last active, MCP server count
- `barebone-agent agents show <name> [--json]`
  - Full config: role, model, fallbacks, channels (discord enabled/guilds), MCP servers (name, command), skills
- New file: `src/cmd_agents.rs`
- New `AgentsCommand` enum in `cli.rs`
- Reuses `AgentConfig::load()`, `discover_agents()`, `db.get_agent_last_active()`

### Phase 4: tokens subcommand
- `barebone-agent tokens [--agent X] [--period today|week|total] [--by-model] [--by-day] [--json]`
  - Default: per-agent totals for today
  - `--by-model`: breakdown by model_used
  - `--by-day`: daily breakdown (last 7 days default, or controlled by --period)
- New DB queries:
  - `get_token_usage_by_model(agent, since)` — GROUP BY model_used
  - `get_token_usage_by_day(agent, limit)` — GROUP BY date(created_at)
- New file: `src/cmd_tokens.rs`
- New `TokensCommand` in `cli.rs` (or flat args, no sub-subcommands)

### Phase 5: config validate
- `barebone-agent config validate [--json]`
  - Checks: models.yml parses, each agent.yml parses, agent model exists in registry, fallback models exist in registry
  - Reports: OK/WARN/ERROR per check
- New file: `src/cmd_config.rs`
- New `ConfigCommand` enum in `cli.rs`

### Key decisions
- `missions delete` refuses with tasks present — no cascade, user must clean up explicitly
- `agents` subcommand reads config files directly, no DB writes
- `tokens` is its own top-level subcommand (not under `status`) for richer output
- `config validate` is read-only, non-destructive

### Reuse
- `db.update_task()` — existing, has status/result/agent_name/priority
- `db.update_mission()` — existing, has status/title/description
- `AgentConfig::load()` — existing config loader
- `discover_agents()` — existing in main.rs (will extract to shared util)
- `db.get_agent_last_active()` — existing in schema.rs
- `db.get_token_usage()` — existing for basic totals
- `ModelRegistry::load()` — existing for validation

## Implementation Status
- [x] Phase 1: tasks update + missions update
- [x] Phase 2: tasks delete + missions delete
- [x] Phase 3: agents list + show
- [x] Phase 4: tokens subcommand (summary, --by-model, --by-day)
- [x] Phase 5: config validate

## Status: DONE
