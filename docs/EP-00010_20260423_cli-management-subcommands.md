# EP-00010 — CLI Management Subcommands

## Problem / Pain Points
- No way to view individual conversations, only a summary in `status --section activity`
- Tasks and missions can only be created by agents via tool calls — no manual CLI creation
- No way to inspect task/mission details without querying SQLite directly
- Token usage view exists in `status --tokens` but per-conversation breakdowns are not visible

## Suggested Solution

### Phase 1: DB layer additions
- Add `get_mission(key)` to `missions.rs` (mirrors existing `get_task` pattern)
- Add `ConversationSummary` struct + `list_conversations(agent, limit)` to `conversations.rs`
  - GROUP BY conversation_id with aggregated turn count, token totals, first/last timestamps
- Add `load_conversation(conversation_id)` — all messages (not just is_final), for `--full` mode

### Phase 2: CLI definitions
- Add three new nested subcommand groups to `Commands` enum in `cli.rs`
- Each group uses a separate `#[derive(Subcommand)]` enum

```
barebone-agent tasks list [--status X] [--agent X] [--mission X] [--json]
barebone-agent tasks show <key> [--json]
barebone-agent tasks create --title "..." [--description --mission --agent --priority --schedule] [--json]

barebone-agent missions list [--status X] [--json]
barebone-agent missions show <key> [--json]
barebone-agent missions create --title "..." [--description] [--json]

barebone-agent conversations list [--agent X] [--limit N] [--json]
barebone-agent conversations show <id> [--full] [--json]
```

### Phase 3: Command handler modules
- `src/cmd_tasks.rs` — list (tabular), show (full detail), create (auto-key, print result)
- `src/cmd_missions.rs` — list (with task progress), show (mission + child tasks), create
- `src/cmd_conversations.rs` — list (summary table), show (chronological messages, `--full` for tool calls)
- All use `serde_json::json!()` for JSON mode, formatted strings for text mode
- Follow dual text/JSON pattern established in `status.rs`

### Phase 4: Main dispatch + wiring
- Add `mod cmd_tasks; mod cmd_missions; mod cmd_conversations;` to `main.rs`
- Extract `run_management_cmd(closure)` helper for shared DB-open boilerplate
- Add match arms in `main()` dispatching to each module's `run()`

### Phase 5: Tests
- DB tests: `get_mission`, `list_conversations`, `load_conversation` (in-memory DB)
- Command handler tests: list empty/with-data, show found/not-found, create success
- Manual: `barebone-agent tasks --help`, create mission + tasks, verify in list/show

### Key decisions
- Nested subcommands (`tasks list`) over flat (`task-list`) — cleaner help output, resource-verb pattern
- Separate modules (`cmd_tasks.rs`, etc.) over extending `status.rs` — different concern (CRUD vs dashboard)
- JSON output via `serde_json::json!()` not `#[derive(Serialize)]` on DB structs — keeps DB layer clean
- `conversations show --full` loads all messages including tool calls; default shows final messages only
- `missions show` includes child task list + progress count

### Reuse
- `db.list_tasks(agent, status, mission)` — existing, has all needed filters
- `db.get_task(key)` — existing
- `db.create_task(...)` — existing with auto-key generation
- `db.list_missions(status)` — existing
- `db.create_mission(...)` — existing with auto-key generation
- `db.get_mission_task_progress(key)` — existing for mission show
- `db.load_history(id, limit)` — existing for non-full conversation show

## Implementation Status
- [x] Phase 1: DB layer additions (get_mission, list_conversations, load_conversation)
- [x] Phase 2: CLI definitions (nested subcommand enums)
- [x] Phase 3: Command handler modules (cmd_tasks, cmd_missions, cmd_conversations)
- [x] Phase 4: Main dispatch + wiring
- [x] Phase 5: Tests (246 total, 15 new) + manual verification

## Status: DONE
