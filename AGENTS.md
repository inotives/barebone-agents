# Agent Workflow Guide 

## Overview 

Project level coding agent workflows and guidelines

---

## Project Folder Structure

Project folder structure. Update this over each development phases. 

```
barebone-agents/
├── Cargo.toml
├── .env                              # Secrets (gitignored)
├── .env.template
├── .gitignore
├── AGENTS.md
├── README.md
│
├── config/
│   ├── models.yml                    # LLM model registry
│   └── squad.yml                     # Team definitions (future)
│
├── agents/
│   ├── _roles/                       # Sub-agent persona templates (delegate role:)
│   ├── _skills/                      # Local task-matched skill pool (hot-reload)
│   └── ino/                          # Default agent
│       ├── AGENT.md                  # Identity + persona
│       ├── agent.yml                 # Model + channel config
│       └── .env                      # Agent-specific credentials
│
├── data/                             # Runtime data (gitignored)
│   └── barebone-agent.db            # SQLite database
│
├── src/
│   ├── main.rs                       # Entry point + startup wiring
│   ├── cli.rs                        # CLI argument parsing (clap)
│   ├── agent_loop.rs                 # Agent reasoning loop
│   ├── config/                       # Config loading (env, models, agent, squad)
│   ├── db/                           # SQLite layer (conversations, tasks, missions, schedule)
│   ├── llm/                          # LLM clients (OpenAI-compat, Anthropic, Gemini, pool)
│   ├── tools/                        # Tool registry + built-in tools + MCP client
│   └── channels/                     # CLI channel
│
├── docs/
│   ├── specs.md                      # Permanent project spec
│   ├── EP-XXXXX_*.md                 # Active execution plans
│   └── archived/                     # Completed EPs
│
└── .agents/
    └── commands/                     # Slash command templates
```

---

## Coding Guidelines

Behavioral guidelines to reduce common LLM coding mistakes and LLM coding pitfalls.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

## Workflows 

### Development

1. **Create execution plan**: Run `/create-ep <topic>` for non-trivial changes.
   File: `docs/EP-XXXXX_<YYYYMMDD>_<topic>.md`
   Lifecycle: `IN PROGRESS` → `IN REVIEW` → `DONE` → `/archive-ep` moves to `docs/archived/`
2. **Branch**: `git checkout -b feature/<topic>`
3. **Implement** phase by phase, test after each
4. **Run tests**: `cargo test` before any commit
5. **Pre-commit check**: Run `/pre-commit` before committing
6. **PR + merge**

### Quality

- **File size check**: Keep files under 800 lines. Run `/file-size-check` periodically
- **Pre-commit check**: Run `/pre-commit` before committing — type check + file size scan

### Docs to Keep Updated

Before committing, ensure these are current:
1. `AGENTS.md` — project structure matches actual files
2. `docs/SPECS.md` — reflects any schema, config, or convention changes
3. `docs/EP-XXXXX_*.md` — update current phase checklists 

---

## Preferences 

User preferences, add as per needed. 

### Commit Messages
Git history is the changelog. Commit messages must be structured:
```
<type>: <short summary>

- <change 1>
- <change 2>
```
Types: `feat`, `fix`, `refactor`, `chore`, `docs`. No co-author lines.

---

## Slash Commands

Custom skills available in `.agents/commands/`:

| Command | Description |
|---|---|
| `/pre-commit` | Run type check + file size scan + doc checklist before committing |
| `/file-size-check` | Scan for files over 800 lines with refactor suggestions |
| `/create-ep <topic>` | Create a new execution plan in `docs/` |
| `/archive-ep` | Move completed (DONE) EPs to `docs/archived/` |

## CLI Commands

The `barebone-agent` binary exposes these subcommands (run from the repo root):

| Command | Description |
|---|---|
| `run --agent <name>` / `--all` | Run agent(s); `-m "<msg>"` for one-shot |
| `status [--agent X] [--section ...]` | Agent dashboard |
| `tasks {list,show,create,update,delete}` | Manage tasks |
| `missions {list,show,create,update,delete}` | Manage missions |
| `conversations {list,show}` | View conversation history |
| `agents {list,show}` | View agent configurations |
| `tokens [--by-model] [--by-day]` | Token usage breakdown |
| `config validate` | Validate configuration files |
| `skill {search,pull,list}` | Pull AKW skills into `agents/_skills/` |
| `role {search,pull,list}` | Pull AKW personas into `agents/_roles/` |

Pull verbs accept `--agent <name>` (pick the AKW MCP config from a specific `agent.yml`), `--force` (overwrite an existing local file), and `--rename <slug>` (write under a different filename).