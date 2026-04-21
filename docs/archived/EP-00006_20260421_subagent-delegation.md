# EP-00006 — Sub-Agent Delegation System

## Problem / Pain Points
- Agents need to divide complex work into parallel sub-tasks
- Sub-agents must be ephemeral — no DB persistence, restricted tool access
- Need role-based profiles so sub-agents have focused personas (researcher, coder, analyst, etc.)
- Parallel delegation needs concurrency control to avoid overwhelming LLM APIs
- Sub-agents must not recursively delegate or access conversation history

## Suggested Solution

### Phase 1: SubAgentRunner core
- `SubAgentRunner` struct: LLM pool, restricted tool registry, role profile, model override
- Ephemeral tool-call loop (same logic as AgentLoop but no DB writes)
- Context size guard: stop if message context exceeds 50,000 chars
- Rate-limit sleep between iterations (`SUBAGENT_SLEEP_BETWEEN_SECS`)
- Max iterations cap (default 5)
- Returns final text response as `String`

### Phase 2: Role profiles
- Create `agents/_roles/` directory with 9 role templates (from spec section 12)
- Each role is a markdown file with persona rules and optional `auto_skills` frontmatter
- Roles: analyst, architect, coder, reviewer, docs-specialist, ops-engineer, qa-engineer, researcher, writer
- Default role (no role specified) uses a generic "assistant" profile
- Role loaded as system prompt for sub-agent

### Phase 3: Restricted tool registry
- Blocked tools list: `delegate`, `delegate_parallel`, `conversation_search`, and any `mcp_*` memory/knowledge tools
- Default allowed tools: `web_search`, `web_fetch`, `api_request`, `shell_execute`, `file_read`, `file_write`
- Optional `tools` parameter: caller can specify a custom subset
- Build restricted registry by cloning allowed handlers from parent registry

### Phase 4: delegate tool
- `delegate(task, role?, tools?, model?, max_iterations?)` — spawn single sub-agent
- Resolve role profile from `agents/_roles/{role}.md`
- Resolve model override (use specified model or parent's primary)
- Run SubAgentRunner, return result to parent agent

### Phase 5: delegate_parallel tool
- `delegate_parallel(tasks, tools?, model?, max_iterations?)` — spawn multiple sub-agents
- Each task in the array becomes a separate sub-agent
- `Semaphore(SUBAGENT_MAX_PARALLEL)` limits concurrent executions
- Results collected and returned as numbered list
- Rate-limit sleep between spawns

### Key decisions
- Sub-agents are fully ephemeral — no DB persistence, no conversation history
- Sub-agents CANNOT delegate (no recursive delegation)
- Role profiles are simple markdown files — no YAML config needed
- Tool registry is rebuilt per sub-agent (not shared) to enforce restrictions
- Model override allows using cheaper/faster models for sub-tasks

## Implementation Status
- [x] Phase 1: SubAgentRunner core (ephemeral loop, context guard, rate limit)
- [x] Phase 2: Role profiles (9 roles in agents/_roles/) + loading logic + unit tests (2 tests)
- [x] Phase 3: Restricted tool registry (blocked list, allowed subset) + unit tests (4 tests)
- [x] Phase 4: delegate tool + live tested with researcher role
- [x] Phase 5: delegate_parallel tool (semaphore concurrency) + unit tests (2 tests)

## Status: DONE
