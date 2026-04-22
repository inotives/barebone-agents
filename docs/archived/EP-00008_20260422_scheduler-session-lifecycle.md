# EP-00008 — Scheduler (Heartbeat) + AKW Session Lifecycle

## Problem / Pain Points
- Scheduled and recurring tasks need background execution without user interaction
- Blocked tasks need auto-unblocking when dependencies complete
- ALL conversations need session logging to AKW for knowledge capture
- Different channels have different session lifecycles (CLI explicit, Discord TTL-based, task per-execution)
- AKW recommended_context from session_start should enrich the system prompt
- When AKW MCP is unavailable, sessions are best-effort (SQLite is the primary conversation store)

## Suggested Solution

### Phase 1: Session manager
- `SessionManager` with active sessions map: `HashMap<String, ActiveSession>`
- `ActiveSession`: session_id (Option), last_activity timestamp, channel_type
- `ensure_session(conv_id, channel_type)` — called before each agent_loop.run():
  - CLI REPL: start on REPL start, end on /quit or /new or /continue (explicit, no TTL)
  - CLI one-shot: start before message, end immediately after response
  - Discord: start on first message, end on TTL expiry (SESSION_TTL_MINUTES), auto-rotate
  - Task: start before execution, end after completion
- Session rotation: when conv_id changes or TTL expires → end old, start new
- Wire into AgentLoop

### Phase 2: AKW session lifecycle (via MCP)
- **Start**: `mcp_akw__session_start(agent, project?, metadata?)` 
  - On success: store session_id, cache recommended_context for system prompt injection
  - On failure: skip (best-effort, SQLite is primary store)
- **Log**: `mcp_akw__session_log(session_id, request[:500], response[:500])` after each turn
  - Best-effort — failure doesn't affect the conversation
- **End**: `mcp_akw__session_end(session_id)` on session close
- Orphan cleanup handled by AKW on next session_start

### Phase 3: Recommended context injection
- `session_start` returns `recommended_context` (relevant knowledge from AKW)
- Cache per session, inject into system prompt after core skills, before @mention context
- Refreshed only on session rotation (not every turn)

### Phase 4: Heartbeat loop
- Background tokio task running every `HEARTBEAT_INTERVAL` seconds
- Each cycle:
  1. `check_unblock()` — auto-transition blocked→todo when deps done
  2. `get_due_tasks(agent)` — get scheduled tasks ready for execution
  3. For each due task: claim → execute → complete
- `get_due_tasks` uses `is_due(schedule, last_run_at)` from db::schedule

### Phase 5: Task execution via heartbeat
- Set task status → `in_progress`
- Build message: task title + description + dependency results
- Session: start before, end after
- Call `agent_loop.run(message, conv_id="task-{key}-{timestamp}", channel_type="task")`
- On success: `complete_task(key, result[:2000])`
- On failure: `update_task(key, status="blocked", result="Error: ...")`

### Phase 6: Graceful shutdown
- On SIGINT/SIGTERM: stop heartbeat, end all active sessions, close MCP connections

### Key decisions
- No local fallback files for sessions — SQLite is the primary conversation store, AKW is supplementary
- AKW integration is best-effort — failures never crash the agent
- Session manager is event-driven: explicit lifecycle for CLI/task, TTL-based for Discord
- CLI REPL has no TTL — user explicitly controls session via /quit, /new, /continue
- CLI one-shot sessions are ephemeral — start and end within one command

### Session lifecycle summary

| Channel | Start | Log | End |
|---|---|---|---|
| CLI REPL | REPL start, /new, /continue | Each turn | /quit, /new, /continue, shutdown |
| CLI one-shot | Before message | One turn | After response |
| Discord | First message or TTL expired | Each turn | TTL expiry |
| Task | Before task execution | Each turn | After task completes |

## Implementation Status
- [x] Phase 1: Session manager (ensure_session, rotation, TTL) + unit tests (8 tests)
- [x] Phase 2: AKW session lifecycle (start/log/end via MCP) — best-effort, wired into session manager
- [ ] Phase 3: Recommended context injection into system prompt — deferred to when AKW MCP is connected
- [x] Phase 4: Heartbeat loop (background task, check_unblock, get_due_tasks) + unit tests (5 tests)
- [x] Phase 5: Task execution via heartbeat — live tested end-to-end
- [x] Phase 6: Graceful shutdown (abort heartbeat, end all sessions)

## Status: DONE
