# EP-00004 — Tool Registry + MCP Client

## Problem / Pain Points
- The agent loop needs a registry to discover, define, and execute tools
- Tools must produce OpenAI-format definitions for LLM consumption
- Tool execution must never crash — all errors caught and returned as text
- MCP is the primary mechanism for extending agent capabilities (lean harness philosophy)
- MCP servers need stdio-based spawning, tool discovery, and dynamic registration
- Built-in tools (file I/O, shell, task/mission) provide baseline workspace operations

## Suggested Solution

### Phase 1: Tool registry framework
- `ToolDef` struct: name, description, parameters (JSON Schema), async handler
- `ToolRegistry`: register, execute (never panics), get_definitions (OpenAI format)
- Handler type: `async fn(Value) -> Result<String, String>` wrapped in `Box`
- `execute()` catches all errors, returns error text string

### Phase 2: File I/O + shell tools
- `file_read(path, max_lines?)` — read file within workspace, path traversal protection
- `file_write(path, content)` — write file within workspace, create parent dirs
- `shell_execute(command, timeout?)` — run shell command in workspace, returns stdout/stderr
- All paths resolved relative to `WORKSPACE_DIR` with traversal guard

### Phase 3: Web/network tools
- `web_search(query, max_results?)` — DuckDuckGo search, returns titles/snippets/URLs
- `web_fetch(url)` — HTTP GET, extract text content, truncate to TOOL_RESULT_MAX_CHARS
- `api_request(url, method?, headers?, body?, timeout?)` — HTTP request, returns status + headers + body
- These are standard information pickup tools — built-in, not MCP

### Phase 4: Task/mission tools
- `task_create`, `task_get`, `task_update`, `task_list` — wrappers over DB layer
- `mission_create`, `mission_update`, `mission_list` — wrappers over DB layer
- `conversation_search(query, max_results?)` — keyword search past conversations (is_final=1)
- These tools receive a shared `Arc<Database>` reference

### Phase 5: MCP client
- `McpConnection` — spawn server process via stdio, JSON-RPC over stdin/stdout
- MCP protocol: `initialize` → `tools/list` → `tools/call`
- `McpToolLoader` — connect to configured servers, discover tools, register into ToolRegistry
- Tool naming: `mcp_{server_name}__{tool_name}`
- Env var substitution in server config (already implemented in AgentConfig)
- Optional `tools` allowlist filtering
- `call_tool(server, tool, args)` — programmatic calls for harness use (session lifecycle etc.)
- Failed servers logged as warnings, don't block others

### Key decisions
- Handlers are `Box<dyn Fn(Value) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>`
- Tools that need DB/state receive it via closure capture (Arc references)
- web_search, web_fetch, api_request are built-in (standard info tools); domain tools (trading, diagrams, etc.) are MCP
- skill_list, skill_create deferred to EP-00007 (Skills system)
- delegate, delegate_parallel deferred to EP-00008 (Delegation)
- MCP uses raw JSON-RPC over stdio — no MCP SDK crate dependency

## Implementation Status
- [x] Phase 1: Tool registry framework (register, execute, get_definitions) + unit tests (6 tests)
- [x] Phase 2: File I/O + shell tools + path traversal protection + unit tests (16 tests)
- [x] Phase 3: Web/network tools (web_search, web_fetch, api_request) + unit tests (7 tests)
- [x] Phase 4: Task/mission/conversation tools + unit tests (15 tests)
- [x] Phase 5: MCP client (stdio spawn, JSON-RPC, tool discovery, registration) + unit tests (5 tests)

## Status: DONE
