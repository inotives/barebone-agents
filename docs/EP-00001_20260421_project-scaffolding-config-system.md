# EP-00001 — Project Scaffolding + Config System

## Problem / Pain Points
- No project structure exists yet — need Cargo workspace, directory layout, and dependencies
- The config system is the foundation everything else builds on: env loading, model registry, agent config, character sheets
- Multiple config sources must merge correctly (root .env + per-agent .env) without mutating process env
- YAML parsing for models.yml, agent.yml, squad.yml must be type-safe with serde
- Template variable substitution needed for AGENT.md (`{{AGENT_NAME}}`)

## Suggested Solution

### Phase 1: Cargo project + directory structure
- Initialize `Cargo.toml` with core dependencies (tokio, serde, serde_json, serde_yaml, dotenvy, clap, tracing, tracing-subscriber)
- Create directory skeleton matching spec section 2
- Set up single binary with clap subcommands (`run`, `status`)
- Configure tracing-based logging

### Phase 2: Environment loading
- `Settings` struct parsed from root `.env` with all fields from spec section 3.2
- Per-agent `.env` loading with merge strategy: `merged_env = root_env ∪ agent_env` (agent overrides root)
- Env stored as `HashMap<String, String>` per agent — never mutate process env

### Phase 3: Model registry (`config/models.yml`)
- `ModelConfig` struct: id, provider, model, api_key_env, base_url, context_window, max_tokens, temperature
- `Provider` enum: Anthropic, Google, Nvidia, OpenRouter, OpenAI, Groq, Ollama
- Parse and validate at startup, skip models with missing API keys (log warning)

### Phase 4: Agent config (`agents/{name}/agent.yml`)
- `AgentConfig` struct: class, model, fallbacks, channels, skills, mcp_servers
- MCP server config with env var substitution (`${VAR}` syntax)
- Discord channel config (enabled, allow_from, guilds with requireMention)

### Phase 5: Character sheet + Squad config
- Load `AGENT.md` with `{{AGENT_NAME}}` replacement
- Parse `config/squad.yml` for team definitions

### Key decisions
- **clap** for CLI argument parsing with derive macros
- **dotenvy** for .env parsing (not dotenv — dotenvy is maintained fork)
- **serde + serde_yaml** for YAML config, serde_json for JSON
- **tracing** for structured logging (not log/env_logger)
- All config types derive `Debug, Clone, Deserialize`

## Implementation Status
- [ ] Phase 1: Cargo project + directory structure + clap subcommands + tracing
- [ ] Phase 2: Settings struct + env loading + per-agent merge + unit tests
- [ ] Phase 3: Model registry parsing + Provider enum + unit tests
- [ ] Phase 4: Agent config parsing + MCP server config + unit tests
- [ ] Phase 5: Character sheet loading + squad config + unit tests

## Status: IN PROGRESS
