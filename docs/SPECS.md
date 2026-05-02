# inotagent Rust Rewrite — Full Project Specification

> Reference spec for rewriting inotagent in Rust. Covers all functionality from the Python v0.13 codebase.
> Naming convention: **agent** (was adventurer), **boss** (was guildmaster).

---

## 1. Overview

A local-first, LLM-agnostic AI agent harness. Run multiple AI agents with their own personas, skills, and tools — coordinated through CLI and Discord.

Each agent has its own identity (`AGENT.md`), model config (`agent.yml`), and credentials (`.env`). LLM providers can be swapped via config — no code changes. Agents collaborate through shared tasks, knowledge, and class-based delegation.

### Design Principles

- **Local-first** — no Docker, no external services required
- **LLM-agnostic** — unified client layer across cloud and local providers
- **File-based config** — YAML + markdown, human-readable and version-controlled
- **Async everywhere** — all I/O is async (Tokio equivalent)
- **Never crash** — all LLM/DB errors caught and logged, agent loop stays alive
- **Extensible** — new providers, channels, and tools added without restructuring

---

## 2. Project Structure

```
inotagent-rs/
├── Cargo.toml
├── .env                              # Shared secrets (gitignored)
├── .env.template
│
├── config/
│   ├── models.yml                    # LLM model registry
│   └── squad.yml                     # Team definitions
│
├── agents/
│   ├── _roles/                     # Role templates (9 classes)
│   ├── _template/                    # Template for new agents
│   ├── ino/                          # Default agent (leader)
│   │   ├── AGENT.md              # Identity + persona + rules
│   │   ├── agent.yml                 # Model selection + channel config
│   │   └── .env                      # Agent-specific credentials
│   └── robin/                        # Engineer agent
│
├── skills/
│   ├── global/                       # Always injected (3 skills)
│   ├── library/                      # Keyword-matched pool
│   └── drafts/                       # Pending review
│
├── data/                             # Runtime data (gitignored)
│   ├── inotagent.db                  # SQLite database
│   └── sessions/                     # Local session fallback
│
├── src/
│   ├── main.rs                       # Entry point + wiring
│   ├── cli.rs                        # CLI argument parsing
│   ├── loop.rs                       # Agent reasoning loop
│   ├── config/                       # Config loading
│   ├── llm/                          # LLM client layer
│   ├── db/                           # SQLite layer
│   ├── tools/                        # Tool registry + built-in tools
│   ├── skills/                       # Skill cache + injection
│   ├── scheduler/                    # Heartbeat + task execution
│   ├── channels/                     # CLI + Discord channels
│   └── status/                       # Status CLI queries + formatters
│
├── docs/
└── tests/
```

---

## 3. Config System

### 3.1 Environment Loading

**Root `.env`** — shared across all agents:
```bash
NVIDIA_API_KEY=...
ANTHROPIC_API_KEY=...
OPENAI_API_KEY=...
GOOGLE_GEMINI_API_KEY=...
GROQ_API_KEY=...
OPENROUTER_API_KEY=...
SQLITE_DB_PATH=./data/inotagent.db
LOG_LEVEL=INFO
```

**Per-agent `.env`** (`agents/{name}/.env`) — overrides root for that agent:
```bash
DISCORD_BOT_TOKEN=...
GITHUB_TOKEN=...
```

**Merge strategy**: `merged_env = root_env ∪ agent_env` (agent overrides root). Must NOT mutate process env — store as dict per agent for multi-agent safety.

### 3.2 Settings (from root `.env`)

| Setting | Type | Default | Description |
|---|---|---|---|
| `SQLITE_DB_PATH` | string | `./data/inotagent.db` | Database file path |
| `LOG_LEVEL` | string | `INFO` | Logging level |
| `SESSION_TTL_MINUTES` | int | `30` | Conversation session timeout |
| `HISTORY_LIMIT` | int | `20` | Max messages loaded for LLM context |
| `MAX_TOOL_ITERATIONS` | int | `10` | Max tool-call loop iterations |
| `TOOL_RESULT_MAX_CHARS` | int | `5000` | Truncation limit for tool results sent to LLM |
| `WORKSPACE_DIR` | string | `./workspace` | Sandbox dir for file/shell tools |
| `SESSION_FALLBACK_DIR` | string | `./data/sessions` | Local fallback when AKW unavailable |
| `SUBAGENT_MAX_PARALLEL` | int | `3` | Max concurrent sub-agents |
| `SUBAGENT_SLEEP_BETWEEN_SECS` | float | `5.0` | Rate limit sleep between sub-agent iterations |
| `SKILLS_DIR` | string | `./skills` | Skills directory |
| `SKILLS_TOKEN_BUDGET` | int | `4000` | Token budget for dynamic skill injection |
| `SKILLS_MIN_MATCH_HITS` | int | `2` | Min keyword overlaps for skill match |
| `HEARTBEAT_INTERVAL` | int | `60` | Seconds between heartbeat cycles |
| `PLATFORM_NAME` | string | `inotagent` | Platform identifier |

### 3.3 Model Registry (`config/models.yml`)

```yaml
models:
  - id: nvidia-minimax-2.7           # Registry key
    provider: nvidia                   # Client routing
    model: minimaxai/minimax-m2.7      # Actual model ID sent to API
    api_key_env: NVIDIA_API_KEY        # Env var name for API key
    base_url: https://integrate.api.nvidia.com/v1  # Required for OpenAI-compat
    context_window: 192000             # Max context tokens
    max_tokens: 8192                   # Max response tokens
    temperature: null                  # Optional override (omit from API if null)

  - id: claude-sonnet-4
    provider: anthropic
    model: claude-sonnet-4-20250514
    api_key_env: ANTHROPIC_API_KEY
    context_window: 200000
    max_tokens: 16384

  - id: gemini-2.5-flash
    provider: google
    model: gemini-2.5-flash-preview-04-17
    api_key_env: GOOGLE_GEMINI_API_KEY
    context_window: 1048576
    max_tokens: 65536
```

**Provider categories** (determines which client to use):
- `anthropic` → Anthropic SDK client
- `google` → Google Gemini SDK client
- `nvidia`, `openrouter`, `openai`, `groq`, `ollama` → OpenAI-compatible HTTP client

### 3.4 Agent Config (`agents/{name}/agent.yml`)

```yaml
class: leader                          # Role class (matches _roles/ templates)
model: nvidia-minimax-2.7              # Primary model ID
fallbacks:                             # Fallback chain (tried in order)
  - openrouter-deepseek-v3
  - gemini-2.5-flash

channels:
  discord:
    enabled: true
    allow_from: ["user-id-1"]          # Allowed Discord user IDs
    guilds:
      "guild-id":
        requireMention: true           # Must @mention to trigger

skills:                                # Equipped skills from library/
  - sprint_planning
  - code_review
  - architecture_decisions

mcp_servers:                           # External MCP server connections
  - name: akw
    command: agent-knowledge-server
  - name: github
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: ${GITHUB_TOKEN}    # Env var substitution
    tools:                             # Optional allowlist
      - create_issue
      - search_repositories
```

### 3.5 Squad Config (`config/squad.yml`)

```yaml
name: "inoTagent Squad"
teams:
  alpha:
    leader: ino
    members: [robin]
```

### 3.6 Character Sheet (`agents/{name}/AGENT.md`)

Loaded as the system prompt. Contains identity, operating rules, safety rules, knowledge management instructions, delegation guidelines, task management instructions, and skill usage.

Template variables: `{{AGENT_NAME}}` replaced at load time.

### 3.7 Config Resolution Order

1. Root `.env` → shared secrets and platform settings
2. `agents/{name}/.env` → agent-specific credentials (overrides root)
3. `config/models.yml` → model registry
4. `agents/{name}/agent.yml` → per-agent model, channels, skills, MCP
5. `agents/{name}/AGENT.md` → system prompt

---

## 4. LLM Client Layer

### 4.1 Core Types

```rust
struct TokenUsage {
    input_tokens: u32,
    output_tokens: u32,
}

struct ToolCall {
    id: String,              // Unique ID from LLM (synthetic for Gemini)
    name: String,            // Tool name to invoke
    arguments: serde_json::Value,  // Parsed JSON arguments
}

struct LLMMessage {
    role: String,            // "user" | "assistant" | "system" | "tool"
    content: String,
    tool_calls: Option<Vec<ToolCall>>,   // Only on assistant messages
    tool_call_id: Option<String>,        // Only on tool result messages
}

struct LLMResponse {
    content: String,
    usage: TokenUsage,
    model: String,           // Model ID actually used (after fallback)
    stop_reason: String,     // "end_turn" | "max_tokens" | "tool_calls" | provider-specific
    tool_calls: Option<Vec<ToolCall>>,
}
```

### 4.2 Client Pool & Fallback

**LLMClientPool** — initialized once at startup per agent:
1. For each model in registry, resolve API key from merged env
2. Create appropriate client based on provider
3. Skip models with missing API keys (log warning)

**Fallback chain** (`chat_with_fallback`):
1. Build chain: `[primary_model] + fallbacks`
2. Try each in order until success
3. On success: set `response.model = model_id`
4. If all fail: return error (never crash)

### 4.3 OpenAI-Compatible Client

**Covers**: NVIDIA NIM, OpenRouter, OpenAI, Groq, Ollama

**HTTP client**: Single connection-pooled client per model (equivalent to httpx.AsyncClient)

**Request format**:
```
POST {base_url}/chat/completions
Authorization: Bearer {api_key}
Content-Type: application/json

{
  "model": "{model_name}",
  "messages": [...],
  "max_tokens": N,
  "temperature": N (omit if null),
  "tools": [...] (if provided)
}
```

**Message conversion**:
- System: `{"role": "system", "content": "..."}`
- User: `{"role": "user", "content": "..."}`
- Assistant with tools: `{"role": "assistant", "content": "...", "tool_calls": [{"id": "...", "type": "function", "function": {"name": "...", "arguments": "JSON_STRING"}}]}`
- Tool result: `{"role": "tool", "tool_call_id": "...", "content": "..."}`

**Tool definition format** (same for all providers internally):
```json
{
  "type": "function",
  "function": {
    "name": "tool_name",
    "description": "...",
    "parameters": {"type": "object", "properties": {...}, "required": [...]}
  }
}
```

**Response parsing**:
- Content: `choices[0].message.content`
- Tool calls: `choices[0].message.tool_calls` (arguments are JSON strings → parse to dict)
- Tokens: `usage.prompt_tokens`, `usage.completion_tokens`
- Stop reason: `choices[0].finish_reason` (raw string)
- Strip `<think>...</think>` blocks (DeepSeek reasoning tags)

**Error handling**: Non-200 status → error with status code + first 500 chars of body

### 4.4 Anthropic Client

**SDK**: Official Anthropic client (or REST equivalent)

**Message conversion**:
- System: Separate `system` parameter (NOT in messages array)
- User: `{"role": "user", "content": "..."}`
- Assistant with tools: Content blocks array — `[{"type": "text", "text": "..."}, {"type": "tool_use", "id": "...", "name": "...", "input": {...}}]`
- Tool result: User message with `[{"type": "tool_result", "tool_use_id": "...", "content": "..."}]`

**Tool definition conversion** (from OpenAI format):
```json
{"name": "...", "description": "...", "input_schema": {"type": "object", "properties": {...}}}
```

**Response parsing**:
- Content blocks: iterate, concatenate text blocks, extract tool_use blocks
- Tokens: `usage.input_tokens`, `usage.output_tokens`
- Stop reason: `stop_reason` (e.g., "end_turn", "tool_use")
- Tool call arguments are already dicts (not JSON strings)

### 4.5 Google Gemini Client

**SDK**: Official Google Generative AI client (or REST equivalent)

**Message conversion**:
- System: `config.system_instruction` parameter
- User: Content with `role: "user"`, parts with text
- Assistant with tools: Content with `role: "model"`, parts with text + function_call
- Tool result: Content with `role: "user"`, part with function_response (name = tool_call_id, response = `{"result": "..."}`)

**Tool definition conversion**:
- Strip `default` from properties (Gemini rejects it)
- Wrap in `FunctionDeclaration(name, description, parameters)`

**Response parsing**:
- Iterate candidate parts: extract text and function_call
- **Synthetic tool IDs**: Gemini doesn't generate IDs → create `call_{name}_{hash}`
- Tokens: `usage_metadata.prompt_token_count`, `usage_metadata.candidates_token_count`
- Stop reason: Enum name lowercased

### 4.6 Token Estimation

**Heuristic**: `len(text) / 4 ≈ token_count` (no tokenizer library used)

**Context window management** (`_truncate_history`):
```
budget = context_window - max_tokens
system_tokens = len(system) / 4
message_budget = budget - system_tokens
# Always keep most recent user message
# Remove oldest messages until messages fit budget
```

### 4.7 Streaming

**Not implemented** — all calls are single request/response. No SSE parsing.

---

## 5. Database Layer (SQLite)

### 5.1 Schema

Created inline at startup via `CREATE TABLE IF NOT EXISTS` (no migration tool).

```sql
-- Agent registry
CREATE TABLE IF NOT EXISTS agents (
    name       TEXT PRIMARY KEY,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- Conversation messages (two-tier storage)
CREATE TABLE IF NOT EXISTS conversations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    role            TEXT NOT NULL,          -- "user" | "assistant" | "tool"
    content         TEXT NOT NULL,
    channel_type    TEXT NOT NULL,          -- "cli" | "discord" | "task"
    model_used      TEXT,
    input_tokens    INTEGER DEFAULT 0,
    output_tokens   INTEGER DEFAULT 0,
    turn_id         TEXT NOT NULL,
    is_final        BOOLEAN DEFAULT 0,     -- 1=replay to LLM, 0=audit only
    metadata        TEXT,                  -- JSON
    created_at      DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_conv_id ON conversations(conversation_id);
CREATE INDEX IF NOT EXISTS idx_conv_agent ON conversations(agent_name);
CREATE INDEX IF NOT EXISTS idx_conv_turn ON conversations(turn_id);
CREATE INDEX IF NOT EXISTS idx_conv_final ON conversations(conversation_id, is_final);

-- Missions (multi-task groups)
CREATE TABLE IF NOT EXISTS missions (
    key         TEXT PRIMARY KEY,           -- MIS-{5-digit}
    title       TEXT NOT NULL,
    description TEXT,
    status      TEXT DEFAULT 'active',     -- active | paused | completed
    created_by  TEXT,
    metadata    TEXT,                       -- JSON
    created_at  DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at  DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- Tasks
CREATE TABLE IF NOT EXISTS tasks (
    key              TEXT PRIMARY KEY,      -- TSK-{5-digit} or MIS-{key}-T{5-digit}
    mission_key      TEXT,
    title            TEXT NOT NULL,
    description      TEXT,
    status           TEXT DEFAULT 'backlog', -- backlog|todo|in_progress|done|blocked
    priority         TEXT DEFAULT 'medium',  -- critical|high|medium|low
    agent_name       TEXT,
    schedule         TEXT,                   -- hourly|daily@HH:MM|weekly@DAY@HH:MM|every:Nh|every:Nm
    last_run_at      DATETIME,
    result           TEXT,
    metadata         TEXT,                   -- JSON: {class, team, depends_on}
    created_at       DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at       DATETIME DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_task_agent ON tasks(agent_name);
CREATE INDEX IF NOT EXISTS idx_task_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_task_mission ON tasks(mission_key);
CREATE INDEX IF NOT EXISTS idx_task_schedule ON tasks(schedule);
```

**Connection settings**: WAL mode, foreign keys ON, single connection per process.

### 5.2 Two-Tier Message Storage

- **`is_final=1`**: User messages and final assistant responses — loaded for LLM context
- **`is_final=0`**: Intermediate messages (assistant with tool calls, tool results) — audit only
- **`turn_id`**: Groups all messages in one user→response cycle (format: `turn-{uuid[:8]}`)

### 5.3 Conversation Functions

| Function | SQL | Purpose |
|---|---|---|
| `save_message(...)` | INSERT INTO conversations | Save any message (user, assistant, tool) |
| `load_history(conv_id, limit=20)` | SELECT WHERE is_final=1 ORDER BY id DESC LIMIT N (reversed) | Load clean history for LLM |
| `load_full_turn(turn_id)` | SELECT WHERE turn_id=? | Debug/audit full turn |
| `get_token_usage(agent, since?)` | SUM(input_tokens), SUM(output_tokens) | Token aggregation |
| `get_parent_id(conv_id)` | First message metadata→parent_id | Conversation chaining |
| `load_recent_messages(agent, limit=5)` | Recent is_final=1 messages | Cross-agent context |
| `get_registered_agents(db)` | SELECT name FROM agents | @mention validation |

### 5.4 Task Functions

| Function | SQL | Purpose |
|---|---|---|
| `create_task(...)` | INSERT with auto-key | Create task (auto-status: blocked/todo/backlog) |
| `update_task(key, fields)` | Dynamic UPDATE | Update task fields |
| `get_task(key)` | SELECT WHERE key=? | Get single task |
| `list_tasks(agent?, status?, mission?)` | SELECT with filters | List tasks |
| `get_due_tasks(agent, class?, team?)` | Complex JOIN + Python filter | Tasks ready for execution |
| `claim_task(key, agent)` | UPDATE WHERE agent IS NULL | Atomic claim for class-matched tasks |
| `check_unblock()` | Check all blocked tasks' dependencies | Auto-transition blocked→todo |
| `complete_task(key, result)` | SET done (one-time) or reset todo (recurring) | Task completion |

### 5.5 Mission Functions

| Function | SQL | Purpose |
|---|---|---|
| `create_mission(title, desc, created_by)` | INSERT with auto-key MIS-{5-digit} | Create mission |
| `update_mission(key, fields)` | Dynamic UPDATE | Update mission |
| `list_missions(status?)` | SELECT with optional filter | List missions |

### 5.6 Task Key Generation

- Standalone: `TSK-{5-digit}` — auto-increment from `MAX(SUBSTR(key, 5))`
- Within mission: `{mission_key}-T{5-digit}` — auto-increment per mission

### 5.7 Task Status Determination on Create

```
if metadata.depends_on exists → "blocked"
else if metadata.class OR schedule → "todo"
else → "backlog"
```

### 5.8 Schedule Patterns

| Pattern | Example | Due Logic |
|---|---|---|
| (none) | — | Due if status=todo (one-time) |
| `hourly` | — | Now - last_run ≥ 3600s |
| `daily@HH:MM` | `daily@09:00` | Now ≥ today's target AND last_run < today's target |
| `weekly@DAY@HH:MM` | `weekly@mon@09:00` | Past this week's target |
| `every:Nh` / `every:Nm` | `every:4h`, `every:30m` | Now - last_run ≥ interval |

---

## 6. Agent Reasoning Loop

### 6.1 Entry Point

```rust
async fn run(
    &mut self,
    message: &str,
    conversation_id: &str,
    channel_type: &str,       // "cli" | "discord" | "task"
    parent_id: Option<&str>,  // For conversation chaining
    task_key: Option<&str>,
    mission_key: Option<&str>,
) -> String
```

### 6.2 Full Message Flow

```
1.  _ensure_akw_group(conv_id, channel_type, task_key, mission_key)
2.  Generate turn_id = "turn-{uuid[:8]}"
3.  Load history (last 20 is_final=1 messages)
4.  Save user message (is_final=1)
5.  Build system prompt:
      a. Base = AGENT.md content
      b. += Global skills (always)
      c. += Matched skills (keyword match against agent's equipped pool)
      d. += AKW knowledge context (recommended_context from group_start)
      e. += Parent conversation context (if chained, last 5 messages)
      f. += Cross-agent @mention context (recent tasks + messages)
6.  Truncate history to fit context window (chars/4 heuristic)
7.  Call LLM via chat_with_fallback(messages, system, tools)
8.  TOOL LOOP (while tool_calls AND iteration < MAX_TOOL_ITERATIONS):
      a. Save assistant message (is_final=0) with tool_calls metadata
      b. For each tool_call:
           - Execute via tool_registry.execute(name, arguments)
           - Truncate result to TOOL_RESULT_MAX_CHARS
           - Save tool result message (is_final=0) with execution metadata
      c. Append assistant + tool results to messages
      d. Call LLM again
      e. Increment iteration
9.  Save final assistant response (is_final=1) with token usage
10. Log turn to AKW group (request[:500], response[:500])
11. If channel_type == "task": End task group segment
12. Return response content
```

### 6.3 Two-Slot AKW Group Design

Two independent group slots prevent task groups from fragmenting conversations:

```rust
struct SessionSlot {
    group_id: Option<String>,       // AKW group ID (None if fallback)
    conv_id: Option<String>,        // Conversation ID when group started
    context: Vec<String>,           // Recommended context from AKW
    fallback_path: Option<PathBuf>, // Local .md file path
    turn_count: u32,                // For fallback numbering
}
```

- **`akw_conv_session`** — for CLI/Discord conversations
- **`akw_task_session`** — for heartbeat task executions

**Slot selection**: `channel_type == "task" → task_session`, otherwise `conv_session`

**Group rotation**: When `conversation_id` changes from `slot.conv_id`, close old group segment and start new one.

### 6.4 AKW Group Lifecycle

AKW persistence is keyed on a "group" — one logical unit of work — with multiple segments over time. We map our conversations onto groups.

1. **Start**: `mcp_akw__group_start({agent, metadata: {conv_id, channel, project_id}})`
   - On success: Store `group_id`, cache `recommended_context`
   - On failure: Create local markdown fallback file
2. **Log turns**: After each run, call `mcp_akw__group_log({request, response})` — keys onto the active group internally
3. **End**: On conversation change or shutdown, call `mcp_akw__group_end({})` — closes the active segment

**Fallback file format**:
```markdown
---
group_type: conversation
agent: ino
channel: cli
conv_id: cli-ino-abc12345
started_at: 2026-04-21T12:00:00Z
---

## Turn 1
**Request:** (truncated to 500 chars)
**Response:** (truncated to 500 chars)
```

### 6.5 Context Injection

Appended to system prompt in order:

1. **Global skills** — always included (3 skills, ~2450 tokens total)
2. **Dynamic skills** — keyword-matched from agent's equipped pool within token budget
3. **AKW knowledge** — `recommended_context` from `group_start()` (cached per group)
4. **Parent conversation** — last 5 messages from parent conv (if chained)
5. **Cross-agent @mentions** — recent tasks + messages from mentioned agents

### 6.6 Cross-Agent Context

**Detection**: Regex `@(\w+)` in message → validate against registered agents, exclude self

**Fetch**: For each mentioned agent, get:
- Recent completed tasks (status=done, limit 5) with result preview (200 chars)
- Recent final messages (limit 5) with content preview (300 chars)

Injected as `## Context from {name}` section.

---

## 7. Tool System

### 7.1 Tool Registry

```rust
struct ToolDef {
    name: String,
    description: String,
    parameters: serde_json::Value,    // JSON Schema
    handler: AsyncHandler,            // async fn(args) -> String
}

impl ToolRegistry {
    fn register(&mut self, name, description, parameters, handler);
    async fn execute(&self, name: &str, arguments: Value) -> String;  // Never panics
    fn get_definitions(&self) -> Vec<Value>;  // OpenAI format
}
```

**Error safety**: `execute()` catches all errors, returns error text string. Never crashes.

### 7.2 Built-in Tools (18 total)

#### Web/Network Tools

| Tool | Parameters | Description |
|---|---|---|
| `web_search` | `query: string, max_results?: int(5)` | DuckDuckGo search, returns titles/snippets/URLs |
| `web_fetch` | `url: string, wait_for?: string` | Headless browser fetch (Playwright equiv), returns text (5000 chars) |
| `api_request` | `url: string, method?: enum, headers?: object, body?: string, timeout?: int(30)` | HTTP request, returns status + headers + body (5000 chars) |

#### File I/O Tools

| Tool | Parameters | Description |
|---|---|---|
| `file_read` | `path: string, max_lines?: int(200)` | Read file within workspace. Path traversal protection |
| `file_write` | `path: string, content: string` | Write file within workspace. Creates parent dirs |
| `shell_execute` | `command: string, timeout?: int(30)` | Run shell command in workspace. Returns stdout/stderr |

#### Conversation Tool

| Tool | Parameters | Description |
|---|---|---|
| `conversation_search` | `query: string, max_results?: int(10)` | Keyword search past conversations (is_final=1 only) |

#### Task/Mission Tools

| Tool | Parameters | Description |
|---|---|---|
| `task_create` | `title: string, description?, mission_key?, agent_name?, agent_class?, depends_on?: string[], schedule?, priority?: enum` | Create task with auto-status |
| `task_get` | `key: string` | Get task details |
| `task_update` | `key: string, status?, result?, agent_name?, priority?` | Update task fields |
| `task_list` | `status?, mission_key?, agent_name?` | List tasks (default: self, "all" for cross-agent) |
| `mission_create` | `title: string, description?, tasks?: array` | Create mission with embedded tasks |
| `mission_update` | `key: string, status?, title?, description?` | Update mission |
| `mission_list` | `status?` | List missions |

#### Skill Tools

| Tool | Parameters | Description |
|---|---|---|
| `skill_list` | `tag?` | List skills grouped by tier |
| `skill_create` | `slug: string, description: string, tags: string[], content: string, token_estimate?: int` | Create draft skill |

#### Delegation Tools

| Tool | Parameters | Description |
|---|---|---|
| `delegate` | `task: string, role?, tools?: string[], model?, max_iterations?: int(5)` | Spawn single sub-agent |
| `delegate_parallel` | `tasks: array, tools?, model?, max_iterations?: int(5)` | Spawn parallel sub-agents (semaphore-limited) |

### 7.3 Sub-Agent (Delegate) System

**SubAgentRunner** spawns ephemeral agents with restricted tools:

**Blocked tools** (never available to sub-agents):
- `conversation_search`, `delegate`, `delegate_parallel`
- All memory/knowledge tools

**Default tools**: `web_search`, `web_fetch`, `api_request`, `shell_execute`, `file_read`, `file_write`

**Execution**:
1. Load role profile from `agents/_roles/{role}.md` (with optional auto_skills)
2. Build restricted tool registry (subset of parent's handlers)
3. Run ephemeral tool-call loop (no DB persistence)
4. Stop if message context exceeds 50,000 chars
5. Rate-limit sleep between iterations (`SUBAGENT_SLEEP_BETWEEN_SECS`)

**Parallel**: `Semaphore(SUBAGENT_MAX_PARALLEL)` limits concurrency.

### 7.4 MCP Client

**MCPToolLoader** connects to external MCP servers and registers their tools dynamically.

**Connection**: Spawn server process via stdio, establish MCP session, list tools.

**Tool naming**: `mcp_{server_name}__{tool_name}`

**Env var substitution**: `${VAR}` in server config env → resolved from agent's merged env.

**Allowlist**: Optional `tools` list in server config filters discovered tools.

**Programmatic calls** (`call_tool`): Used by harness code (not LLM) for session lifecycle. Returns first TextContent block only (avoids corrupting JSON).

**Error handling**: Failed servers logged as warnings, don't block other servers.

---

## 8. Skills System

### 8.1 Skill File Format

```markdown
---
name: code-review
description: Self-review checklist before PR
tags: [review, quality]
token_estimate: 350
source: optional-attribution
---

## Code Review

> ~350 tokens — Brief intro

### When to Use
...
```

**Frontmatter fields**:
- `name` — kebab-case slug
- `description` — one-line summary
- `tags` — list for keyword matching
- `token_estimate` — auto-calculated as `len(content) / 4` if omitted

### 8.2 Three Tiers

| Tier | Path | Behavior |
|---|---|---|
| **Global** | `skills/global/` (3 files) | Always injected into every conversation |
| **Library** | `skills/library/` (~40 files) | Keyword-matched from agent's equipped pool |
| **Drafts** | `skills/drafts/` (~95 files) | Never auto-injected, pending review |

### 8.3 Global Skills

1. **`task_management.md`** (~550 tokens) — Task creation, delegation, lifecycle, priority guide
2. **`adventurer_runbook.md`** (~800 tokens) — Communication style, workflow routing, idle behavior
3. **`knowledge_management.md`** (~1100 tokens) — AKW search/save/research lifecycle

### 8.4 Skill Injection Flow

1. **Always include** global skills content (cached at startup)
2. **Check override**: Parse message for `use skill: <name>` or `apply skill: <name>`
3. **Extract keywords**: Regex `[a-zA-Z0-9_-]+`, remove 45 stop-words, min length 2, lowercase
4. **Match against pool**: Only agent's equipped skills from `agent.yml`
5. **Score by tag overlap**: Require ≥2 keyword hits (≥1 if pool ≤30 skills)
6. **Token budget**: Select highest-scoring skills until `SKILLS_TOKEN_BUDGET` exhausted
7. **Inject**: `## Global Skills\n...` + `## Active Skills\n...` appended to system prompt

### 8.5 Skill Cache

Built once at startup:
- Scan all tiers, parse YAML frontmatter
- Index by slug with tags, token estimate, file path, tier
- Global skills content loaded eagerly and cached
- Library/draft content loaded lazily (on-demand by slug)

---

## 9. Scheduler (Heartbeat)

### 9.1 Heartbeat Loop

Background task running every `HEARTBEAT_INTERVAL` seconds per agent.

**Each cycle**:
1. `check_unblock()` — auto-transition blocked→todo when dependencies met
2. `get_due_tasks(agent, class, team)` — get tasks ready for execution
3. For each due task:
   - Claim if class-matched and unclaimed (atomic UPDATE)
   - Execute via `_execute_task()`

### 9.2 Task Execution

1. Set task status → `in_progress`
2. Build message: task title + description + dependency results (if depends_on)
3. Call `agent_loop.run(message, conv_id="task-{key}-{timestamp}", channel_type="task")`
4. On success: `complete_task(key, result[:2000])`
5. On failure: `update_task(key, status="blocked", result="Error: ...")`

### 9.3 Task Completion Logic

- **One-time** (no schedule): Set status=`done`
- **Recurring** (has schedule): Reset status=`todo`, update `last_run_at`

---

## 10. Channels

### 10.1 CLI Channel

**Modes**:
- **Single-agent REPL**: Input loop with `/continue` command for conversation chaining
- **One-shot**: `--message "prompt"` → print response → exit
- **Multi-agent REPL**: `@name` prefix routes to specific agent

**Conversation IDs**: `cli-{agent_name}-{uuid[:8]}`

**`/continue` command**: Links new conversation to previous via `parent_id` metadata.

### 10.2 Discord Channel

**Features**:
- Per-guild configuration (requireMention, allowFrom)
- Session tracking (in-memory, TTL-based)
- First-mention routing (multi-bot: only first mentioned responds)
- 2000 char message splitting
- Typing indicator during processing

**Session key**: `discord-{dm|thread|channel}-{id}-sess-{uuid[:8]}`

**Session TTL**: `SESSION_TTL_MINUTES` (default 30 min), new session on expiry.

### 10.3 Message Type

```rust
struct IncomingMessage {
    text: String,
    sender_id: String,
    sender_name: String,
    conversation_id: String,
    channel_type: String,         // "cli" | "discord"
    metadata: Option<HashMap<String, String>>,  // e.g., parent_id
}
```

---

## 11. Status CLI

Read-only dashboard for monitoring. Separate entry point (`inotagent-status`).

### 11.1 CLI Flags

```
--agent NAME           Filter to specific agent
--tokens PERIOD        Token usage: today (default) | week | total
--json                 JSON output
--section SECTION      Single section: agents | tokens | tasks | missions | knowledge | skills | activity
--skills               Shorthand for --section skills
```

### 11.2 Sections

| Section | Data Source | Content |
|---|---|---|
| **agents** | SQLite + agent.yml | Name, class, model, last active (relative age) |
| **tokens** | SQLite conversations | Input/output/total tokens by agent, period filter |
| **tasks** | SQLite tasks | Status counts + active task list (priority sorted) |
| **missions** | SQLite missions | Key, status, title, done/total tasks |
| **knowledge** | Filesystem scan | Pages per scope, stale count, last updated |
| **skills** | Filesystem scan | Tiers with slugs, tokens, descriptions |
| **activity** | SQLite conversations + tasks | Recent events timeline |

### 11.3 JSON Output

All sections combined into single JSON object with keys: `agents`, `token_usage`, `task_counts`, `active_tasks`, `missions`, `knowledge`, `skills`, `recent_activity`.

---

## 12. Agent Classes (Role Templates)

Templates in `agents/_roles/`. Each defines persona rules and optional `auto_skills`.

| Class | auto_skills | Focus |
|---|---|---|
| **analyst** | — | Data analysis, tables, benchmarks, anomaly detection |
| **architect** | architecture_decisions, system_design, technical_design_doc, spec_driven_proposal | Trade-offs, modularity, ADRs |
| **coder** | code_review, test_writing, git_conventions, systematic_debugging | Clean code, conventions, testing |
| **reviewer** | code_review, code_review_advanced, pre_landing_review, security_audit | Confidence-based filtering, severity categories |
| **docs-specialist** | report_format, executive_summary, writing_plans | Documentation close to code |
| **ops-engineer** | deployment_monitoring, performance_benchmark, ship_workflow, finishing_dev_branch | Monitor, automate, recover |
| **qa-engineer** | test_driven_development, test_writing, testing_practices, bug_analysis | Test-first, 80%+ coverage |
| **researcher** | research_methodology, report_format | Multi-source verification, citations |
| **writer** | — | Clear, concise, structured writing |

---

## 13. Startup & Shutdown

### 13.1 Single-Agent Startup

```
1.  Load Settings from root .env
2.  Load models registry from models.yml
3.  Load agent config (agent.yml + .env + AGENT.md)
4.  Create LLMClientPool, init clients with merged env
5.  Init SQLite DB (inline schema)
6.  Register agent name in DB
7.  Start WebFetcher (headless browser)
8.  Build SkillCache (scan all tiers)
9.  Create ToolRegistry (register all tools)
10. Load MCP servers (if configured)
11. Create AgentLoop
12. Create channels (CLI ± Discord)
13. Create Heartbeat background task
14. Run channels (await)
```

### 13.2 Multi-Agent Startup

**Shared**: models registry, SkillCache, DB connection, WebFetcher

**Per-agent**: LLMClientPool, ToolRegistry, MCP loaders, AgentLoop, Heartbeat, Discord bot

**CLI routing**: Single CLI instance, `@name` prefix dispatches to agent loops.

### 13.3 Graceful Shutdown

Reverse order:
1. Stop heartbeats
2. Stop channels
3. Close AKW sessions (both slots per agent)
4. Close MCP loaders
5. Close WebFetcher
6. Close LLM clients
7. Close DB

---

## 14. Error Handling Philosophy

| Component | Strategy |
|---|---|
| **LLM calls** | Fallback chain, return error message to user if all fail |
| **Tool execution** | Catch all errors, return error text (never crash loop) |
| **AKW sessions** | Fall back to local markdown files |
| **MCP servers** | Failed servers logged, don't block others |
| **DB operations** | Log errors, return error messages |
| **Heartbeat tasks** | Mark task as blocked with error, continue cycle |

**Core principle**: The agent loop must never crash. All external failures are caught and degraded gracefully.

---

## 15. Test Coverage Summary (Python Reference)

221 tests across 18 test files:

| Area | Tests | Files |
|---|---|---|
| Conversations DB | 14 | test_conversations_db.py |
| Tasks DB | 15 | test_tasks_db.py |
| Schedule parser | 16 | test_schedule_parser.py |
| Heartbeat | 10 | test_heartbeat.py |
| Class claiming | 9 | test_class_claiming.py |
| Task chaining | 8 | test_task_chaining.py |
| Tool registry | 8 | test_tool_registry.py |
| Loop + tools | 8+9 | test_loop_tools.py, test_loop.py |
| Delegation | 15 | test_delegate.py |
| MCP client | 15 | test_mcp_client.py |
| LLM factory | 12 | test_llm_factory.py |
| Skills (loader) | 18 | test_skill_loader.py |
| Skills (injector) | 11 | test_skill_injector.py |
| Skills (tools) | 7 | test_skill_tools.py |
| Cross-agent context | 16 | test_cross_adventurer_context.py |
| Agent config | 9 | test_adventurer_config.py |
| Status CLI | 21 | test_status.py |

**Testing patterns**: Temp directories for DB, AsyncMock for LLM calls, helper builders for mock responses, no shared conftest.

---

## 16. Terminology Mapping

| Python (current) | Rust (new) |
|---|---|
| adventurer | agent |
| guildmaster | boss |
| guild | squad |
| `guild/` directory | `agents/` directory |
| `adventurer.yml` | `agent.yml` |
| `adventurer_name` | `agent_name` |
| `guild.yml` | `squad.yml` |
| `AdventurerLoop` | `AgentLoop` |
| `AdventurerConfig` | `AgentConfig` |
| `inotagent-recruit` | `inotagent-recruit` (same) |
| `inotagent-status` | `inotagent-status` (same) |

---

## 17. Suggested Rust Crates

| Purpose | Crate |
|---|---|
| Async runtime | `tokio` |
| HTTP client | `reqwest` |
| SQLite | `sqlx` (async) or `rusqlite` |
| JSON | `serde`, `serde_json` |
| YAML | `serde_yaml` |
| CLI args | `clap` |
| Env loading | `dotenvy` |
| Discord | `serenity` or `poise` |
| Logging | `tracing` + `tracing-subscriber` |
| Headless browser | `chromiumoxide` or `headless_chrome` |
| WebSocket (MCP) | `tokio-tungstenite` |
| DuckDuckGo search | Custom HTTP (no official crate) |
| Regex | `regex` |
| UUID | `uuid` |
| Frontmatter parsing | `gray_matter` or custom |
