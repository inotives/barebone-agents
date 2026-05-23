# EP-00016 — Remove AKW Dependency

## Problem / Pain Points
- The default `agents/ino/agent.yml` includes an AKW MCP server with a hardcoded machine-specific path, which causes startup warnings and failed MCP connection attempts on fresh installs.
- `install.sh` and README setup instructions still treat AKW as an optional integration, but the default agent config makes the harness try to spawn it anyway.
- AKW-specific CLI commands, background pusher behavior, and prior-work MCP lookup increase setup complexity for a harness that should run cleanly as a local-first standalone binary.
- Local memory, preferences, session summaries, and drafts should remain usable without depending on an external AKW server.

## Suggested Solution
- Remove the default AKW MCP server entry from `agents/ino/agent.yml` so a fresh install does not try to spawn AKW.
- Remove AKW setup, warning, and backup guidance from `install.sh`, README, and project docs.
- Remove or de-scope AKW-specific runtime wiring: background pusher spawn, AKW client/pusher modules, and AKW-only CLI subcommands.
- Preserve local-first behavior for preferences, drafts, reflection, session summaries, and local equipped skills.
- Rename local draft directories from AKW-shaped names to harness-native names:
  - `data/drafts/2_researches/` -> `data/drafts/researches/`
  - `data/drafts/2_knowledges/preferences/` -> `data/drafts/knowledges/preferences/`
- Keep generic MCP support intact for non-AKW MCP servers.

## Implementation Status
- [ ] Step 1 — Remove default AKW configuration and installer/docs setup references.
- [ ] Step 2 — Remove AKW runtime pusher and AKW-specific CLI command surface while keeping local-only preferences/drafts.
- [ ] Step 3 — Rename local draft paths from `2_*` AKW naming to `researches` and `knowledges`, including docs and migration behavior.
- [ ] Step 4 — Remove unused AKW code/modules and update specs/project structure.
- [ ] Step 5 — Verify with `cargo test`, `config validate`, and one-shot smoke test.

## Status: IN PROGRESS
