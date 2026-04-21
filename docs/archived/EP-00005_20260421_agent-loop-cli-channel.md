# EP-00005 — Agent Reasoning Loop + CLI Channel

## Problem / Pain Points
- All infrastructure is built (config, DB, LLM clients, tools) but nothing ties them together
- Need the core reasoning loop: receive message → build context → call LLM → execute tools → respond
- Need a CLI channel so we can interact with an agent and validate everything end-to-end
- AKW session management needed but should gracefully degrade to local fallback
- Cross-agent @mention context needs to pull from DB
- First runnable product — this EP makes the binary actually useful

## Suggested Solution

### Phase 1: Agent loop core
- `AgentLoop` struct holding: agent name, character sheet, LLM pool, fallback chain, tool registry, DB, settings
- `run(message, conversation_id, channel_type, parent_id?, task_key?, mission_key?) -> String`
- Message flow:
  1. Generate `turn_id = "turn-{uuid[:8]}"`
  2. Load history (last N is_final=1 messages from DB)
  3. Save user message (is_final=1)
  4. Build system prompt (AGENT.md base, context injection later)
  5. Truncate history to fit context window
  6. Call LLM via `chat_with_fallback`
  7. Tool loop: while tool_calls AND iteration < MAX_TOOL_ITERATIONS
     - Save assistant message (is_final=0)
     - Execute each tool, truncate result
     - Save tool results (is_final=0)
     - Call LLM again
  8. Save final response (is_final=1) with token usage
  9. Return response content

### Phase 2: Context injection
- Cross-agent @mention detection: regex `@(\w+)` → validate against registered agents
- Fetch context for mentioned agents: recent done tasks (5) + recent messages (5)
- Parent conversation context: load last 5 messages from parent conv_id
- Inject into system prompt as additional sections

### Phase 3: Session management (local fallback only)
- Session slot struct for conversation tracking
- Local markdown fallback files (no AKW MCP dependency for MVP)
- Log turns: append request/response summaries (500 char truncation)
- Session rotation when conversation_id changes
- Defer full AKW MCP integration to when agent-knowledge MCP server is connected

### Phase 4: Single-agent startup wiring
- Wire everything together in `main.rs` for the `run` subcommand:
  1. Load Settings from root .env
  2. Load model registry from models.yml
  3. Load agent config (agent.yml + .env + AGENT.md)
  4. Create LLMClientPool with merged env
  5. Init SQLite DB + register agent
  6. Create ToolRegistry + register all built-in tools
  7. Load MCP servers (if configured)
  8. Create AgentLoop
  9. Launch CLI channel

### Phase 5: CLI channel
- Single-agent REPL mode: read input → agent.run() → print response
- One-shot mode: `--message "prompt"` → run → print → exit
- Conversation IDs: `cli-{agent_name}-{uuid[:8]}`
- `/continue` command: start new conversation linked to previous via parent_id
- `/quit` or `/exit` to exit
- Add `--message` flag to `run` subcommand in clap

### Phase 6: Sample agent setup
- Create `agents/ino/` with agent.yml, AGENT.md, .env
- Create `.env.template` at root with all API key placeholders
- Verify end-to-end: `barebone-agent run --agent ino` → chat via CLI

### Key decisions
- AKW session management is local-only for now (MCP integration deferred)
- No skills injection yet (EP-00007)
- No Discord channel yet (EP-00009)
- No heartbeat yet (EP-00008)
- Focus on getting a working single-agent CLI loop first

## Implementation Status
- [x] Phase 1: Agent loop core (message flow, tool loop, DB persistence) + unit tests
- [x] Phase 2: Context injection (@mention, parent conv) + unit tests (5 tests)
- [ ] Phase 3: Session management (local fallback) — deferred, not needed for MVP
- [x] Phase 4: Single-agent startup wiring in main.rs
- [x] Phase 5: CLI channel (REPL + one-shot + /continue)
- [x] Phase 6: Sample agent setup + end-to-end validation (tested with NVIDIA NIM + Gemini)

## Status: DONE
