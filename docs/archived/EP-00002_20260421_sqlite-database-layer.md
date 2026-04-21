# EP-00002 — SQLite Database Layer

## Problem / Pain Points
- The agent loop, task system, and status dashboard all depend on persistent storage
- Need two-tier message storage (is_final flag) to separate LLM context from audit trail
- Task system needs auto-key generation, status determination, dependency tracking, and schedule parsing
- Must use WAL mode for concurrent read/write safety and single connection per process
- No migration tool — schema created inline at startup

## Suggested Solution

### Phase 1: Database connection + schema init
- Add `rusqlite` dependency (sync with `spawn_blocking` for async compatibility)
- `Database` struct wrapping a single `rusqlite::Connection`
- Inline `CREATE TABLE IF NOT EXISTS` for all 4 tables (agents, conversations, tasks, missions) + indexes
- WAL mode, foreign keys ON
- `register_agent(name)` to upsert agent into registry

### Phase 2: Conversation CRUD
- `save_message(conv_id, agent, role, content, channel_type, model, tokens, turn_id, is_final, metadata)`
- `load_history(conv_id, limit=20)` — is_final=1 only, DESC + reverse for chronological order
- `load_full_turn(turn_id)` — all messages in a turn (debug/audit)
- `get_token_usage(agent, since?)` — SUM input/output tokens
- `get_parent_id(conv_id)` — extract parent_id from first message metadata
- `load_recent_messages(agent, limit=5)` — cross-agent context
- `get_registered_agents()` — list all agent names for @mention validation

### Phase 3: Task CRUD + key generation
- Auto-key: `TSK-{5-digit}` standalone, `{mission_key}-T{5-digit}` within mission
- `create_task(title, desc?, mission_key?, agent?, schedule?, priority?, metadata?)` with auto-status logic:
  - depends_on in metadata → "blocked"
  - class in metadata OR schedule present → "todo"
  - else → "backlog"
- `update_task(key, fields)` — dynamic field update
- `get_task(key)` — single task lookup
- `list_tasks(agent?, status?, mission?)` — filtered listing
- `claim_task(key, agent)` — atomic claim (WHERE agent_name IS NULL)
- `check_unblock()` — scan blocked tasks, transition to todo when deps are done
- `complete_task(key, result)` — set done (one-time) or reset to todo (recurring)

### Phase 4: Mission CRUD
- Auto-key: `MIS-{5-digit}`
- `create_mission(title, desc?, created_by?)`
- `update_mission(key, fields)`
- `list_missions(status?)`

### Phase 5: Schedule parsing
- Parse schedule strings: `hourly`, `daily@HH:MM`, `weekly@DAY@HH:MM`, `every:Nh`, `every:Nm`
- `is_due(schedule, last_run_at)` — determine if a task is due for execution
- `get_due_tasks(agent, class?, team?)` — combine schedule check with task filters

### Key decisions
- **rusqlite** over sqlx — simpler, no compile-time query checks needed, sync is fine with `spawn_blocking`
- Schema created inline (no migration tool, matches spec)
- Single `Connection` wrapped in `Mutex` for thread safety
- All DB functions return `Result<T, String>` for consistency with config layer

## Implementation Status
- [x] Phase 1: Database connection + schema init + agent registry + unit tests (6 tests)
- [x] Phase 2: Conversation CRUD (save, load_history, load_full_turn, token usage, etc.) + unit tests (11 tests)
- [x] Phase 3: Task CRUD + auto-key generation + status logic + dependency tracking + unit tests (14 tests)
- [x] Phase 4: Mission CRUD + auto-key generation + unit tests (5 tests)
- [x] Phase 5: Schedule parsing + due task detection + unit tests (16 tests)

## Status: DONE
