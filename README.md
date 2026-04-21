# barebone-agents

A local-first, LLM-agnostic AI agent harness written in Rust. Run multiple AI agents with their own personas, skills, and tools — coordinated through CLI and Discord.

The harness is the hands and body. LLMs are the brain. Rich functionality (knowledge, web search, trading, etc.) plugs in via MCP servers.

## Design Principles

- **Local-first** — single binary, no Docker, no external services required
- **LLM-agnostic** — unified client layer across Anthropic, Google Gemini, OpenAI, NVIDIA, OpenRouter, Groq, and Ollama
- **Lean harness** — core orchestration only, capabilities via MCP servers
- **File-based config** — YAML + markdown, human-readable and version-controlled
- **Async everywhere** — Tokio-based async I/O
- **Never crash** — all LLM/DB errors caught and logged, agent loop stays alive

## Features

- Multi-agent orchestration with role-based classes
- Tool calling with built-in tools and MCP server integration
- Sub-agent delegation (single and parallel)
- Task and mission management with scheduling
- Skill system with keyword-matched context injection
- CLI and Discord channels
- SQLite-backed conversation history and task persistence

## Getting Started

```bash
# Build
cargo build --release

# Run an agent
./target/release/inotagent run --agent ino

# Check status
./target/release/inotagent status
```

## Project Structure

```
barebone-agents/
├── Cargo.toml
├── .env                          # Shared secrets (gitignored)
├── config/
│   ├── models.yml                # LLM model registry
│   └── squad.yml                 # Team definitions
├── agents/
│   ├── _roles/                   # Role templates
│   ├── _template/                # Template for new agents
│   └── {name}/                   # Per-agent config
│       ├── AGENT.md              # Identity + persona
│       ├── agent.yml             # Model + channel config
│       └── .env                  # Agent-specific credentials
├── skills/
│   ├── global/                   # Always injected
│   ├── library/                  # Keyword-matched pool
│   └── drafts/                   # Pending review
├── data/                         # Runtime data (gitignored)
└── src/                          # Rust source
```

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.
