# EP-00007 — Skills System

## Problem / Pain Points
- Agents need baseline operational knowledge (how to use tools, manage tasks, delegate)
- Rich skill library belongs in the agent-knowledge MCP server, not in the harness
- Core skills should be embedded in the binary — no external file dependencies
- System prompt needs skill injection without bloating the context window

## Suggested Solution

### Phase 1: Core skills as markdown files in config
- Store core skills as `.md` files in `config/skills/`
- `config/skills/task_management.md` (~550 tokens) — how to create/manage tasks, priorities, delegation guidelines
- `config/skills/agent_runbook.md` (~800 tokens) — communication style, workflow routing, tool usage patterns
- Load all `.md` files from `config/skills/` at startup, cache contents
- These are always injected into the system prompt

### Phase 2: Skill injection into system prompt
- Inject core skills in `build_system_prompt()` after character sheet, before context sections
- Format: `## Core Skills\n{content of all files in config/skills/}`
- Token budget check: ensure core skills + character sheet fit within context window

### Phase 3: MCP skill integration hooks
- Agent can search/equip skills from agent-knowledge MCP via existing MCP tool calls
- `agent.yml` `skills` field lists pre-equipped skill slugs to request from MCP at session start
- Equipped MCP skills injected as `## Equipped Skills\n...` section
- Token budget: `SKILLS_TOKEN_BUDGET` caps total equipped skill tokens

### Key decisions
- Core skills are markdown files in `config/skills/` — editable without recompiling
- Rich skill library lives in agent-knowledge MCP server
- `skill_list` and `skill_create` tools are NOT built-in — they're MCP tools from agent-knowledge
- Keeps the harness lean: only operational knowledge that every agent needs

## Implementation Status
- [x] Phase 1: Core skills as markdown files in config/skills/ (task_management + agent_runbook)
- [x] Phase 2: Skill injection in build_system_prompt() + unit tests (7 tests)
- [ ] Phase 3: MCP skill integration hooks (pre-equip from agent.yml) — deferred to when AKW MCP is wired

## Status: DONE
