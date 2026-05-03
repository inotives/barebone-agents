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
- Local user-preference pool with scope-based selection
- Local-first artifact storage (research drafts, session summaries, preferences) with optional AKW backup
- Counter-triggered pattern reflection — agent notices recurring patterns across runs and proposes preferences
- CLI and Discord channels
- SQLite-backed conversation history and task persistence

## Getting Started

Two install paths: a **one-click installer** that handles system deps + Rust + build + scaffolding, or a **manual dev setup** for hacking on the harness itself.

### Option A — One-click install (recommended for users)

The bootstrap script handles everything: detects your package manager (apt/dnf/pacman/brew), installs build tooling, installs the Rust toolchain if missing, builds the release binary, scaffolds `.env`, creates the runtime directories, and validates config. Idempotent — safe to re-run.

```bash
git clone https://github.com/inotives/barebone-agents.git
cd barebone-agents

# Pick the variant that fits your needs:
./install.sh                                # base: system deps + rust + build + .env
./install.sh --with-akw                     # also clone + uv-sync agent-knowledge-wikia
./install.sh --with-systemd                 # also write a user systemd unit for ino
./install.sh --with-akw --with-systemd      # everything

# Non-interactive (CI / scripted):
./install.sh --non-interactive

# Skip system deps if you've already installed them:
SKIP_SYSTEM_DEPS=1 ./install.sh
```

**After install**:

1. Add an LLM provider key to `.env` (any one is enough; NVIDIA is the broadest fallback for `ino`):
   ```bash
   echo "NVIDIA_API_KEY=<your-key>" >> .env
   ```
2. (Optional, Discord) Add bot token to `agents/ino/.env`:
   ```bash
   echo "DISCORD_BOT_TOKEN=<token>" >> agents/ino/.env
   ```
   Then enable Discord in `agents/ino/agent.yml` under `channels.discord.enabled: true`.
3. Smoke test:
   ```bash
   ./target/release/barebone-agent run --agent ino -m 'say hi in one word'
   ```

### Option B — Manual dev setup (recommended for contributors)

For hacking on the Rust source. Skips the bootstrap conveniences in favour of an explicit, step-by-step setup you can debug.

**1. System dependencies**

```bash
# macOS (Homebrew)
brew install pkg-config openssl ca-certificates git curl

# Debian/Ubuntu
sudo apt-get install -y build-essential pkg-config libssl-dev ca-certificates git curl

# Fedora/RHEL
sudo dnf install -y gcc gcc-c++ make pkg-config openssl-devel ca-certificates git curl

# Arch
sudo pacman -S --needed base-devel pkg-config openssl ca-certificates git curl
```

**2. Rust toolchain**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source "$HOME/.cargo/env"
rustc --version    # confirm toolchain is on PATH
```

**3. Clone and build**

```bash
git clone https://github.com/inotives/barebone-agents.git
cd barebone-agents
cargo build                              # debug build (faster compiles, slower runtime)
cargo build --release                    # release build (slower compile, what you'll run)
```

**4. Scaffold environment**

```bash
cp .env.template .env
# Edit .env and add at least one LLM key, e.g.:
#   NVIDIA_API_KEY=<your-key>

# Per-agent secrets (Discord tokens etc.):
mkdir -p agents/ino
cat > agents/ino/.env <<'EOF'
DISCORD_BOT_TOKEN=
EOF

# Pre-create the local-first artifact dirs (the harness creates these on demand,
# but pre-creating makes the layout explicit):
mkdir -p data/drafts/{2_researches,2_knowledges/preferences,sessions,notes}
```

**5. Validate config**

```bash
./target/release/barebone-agent config validate
```

**6. Optional — agent-knowledge-wikia (AKW MCP server)**

The harness boots fine without AKW; the EP-00015 features (preference selection, prior-work search, draft persistence) all work locally. Set up AKW only if you want durable AKW backup or cross-agent knowledge sharing.

```bash
# Install uv (Python package manager AKW uses)
curl -LsSf https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"

# Clone AKW alongside this repo
cd ..
git clone https://github.com/inotives/agent-knowledge-wikia.git
cd agent-knowledge-wikia
uv sync

# Update agents/ino/agent.yml — replace any hardcoded path with your AKW path
# (the install.sh --with-akw flag does this automatically)
```

**7. Run**

```bash
./target/release/barebone-agent run --agent ino                            # interactive REPL + Discord
./target/release/barebone-agent run --agent ino -m "say hi in one word"    # one-shot
./target/release/barebone-agent status                                     # dashboard
./target/release/barebone-agent --log-level debug run --agent ino          # verbose tracing
```

**8. Tests**

```bash
cargo test                # all tests (393+)
cargo test agent_loop     # filter by module
```

### Quick start (if you already have everything)

```bash
cargo build --release && ./target/release/barebone-agent run --agent ino
```

## Project Structure

```
barebone-agents/
├── Cargo.toml
├── .env                          # Shared secrets (gitignored)
├── .env.template                 # Reference for env vars
├── config/
│   ├── models.yml                # LLM model registry
│   └── skills/                   # Core skills (always injected)
├── agents/
│   ├── _roles/                   # Sub-agent persona templates
│   ├── _skills/                  # Local task-matched skill pool (hot-reload)
│   ├── _preferences/             # Local user-preference pool (scope-based)
│   └── {name}/                   # Per-agent config
│       ├── AGENT.md              # Identity + persona
│       ├── agent.yml             # Model + channel + MCP config
│       └── .env                  # Agent-specific credentials
├── data/                         # Runtime data (gitignored)
│   ├── barebone-agent.db         # SQLite (conversations, tasks, counters)
│   ├── .akw_push_manifest.json   # Local-first pusher state
│   └── drafts/                   # Local artifact storage
│       ├── 2_researches/         #   research drafts (task output, opt-in)
│       ├── 2_knowledges/preferences/  # pending preferences (review-gated)
│       ├── sessions/             #   conversation summaries
│       └── notes/                #   ad-hoc notes
├── docs/
│   ├── SPECS.md                  # Permanent spec
│   ├── EP-XXXXX_*.md             # Active execution plans
│   └── archived/                 # Completed EPs
└── src/                          # Rust source
```

## CLI Cheatsheet

```bash
barebone-agent run --agent ino           # run an agent (CLI + Discord)
barebone-agent run --agent ino -m "..."  # one-shot

# Knowledge pool curation (EP-00013/EP-00014)
barebone-agent skill {search,pull,list}  # curate AKW skills locally
barebone-agent role  {search,pull,list}  # curate AKW personas locally

# Local-first memory (EP-00015)
barebone-agent prefs list                # active + pending preferences
barebone-agent prefs pull <slug>         # import a pref from AKW
barebone-agent prefs promote <slug>      # move pending → active

# AKW backup (EP-00015)
barebone-agent akw push                  # mirror local artifacts to AKW now
barebone-agent akw status                # diff / dirty count per watched dir

# Status / diagnostics
barebone-agent status [--agent X]        # dashboard
barebone-agent tasks {list,show,...}     # task management
barebone-agent tokens [--by-model]       # token usage
barebone-agent config validate           # config sanity check
```

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.
