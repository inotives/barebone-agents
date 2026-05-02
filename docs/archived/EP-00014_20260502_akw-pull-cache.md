# EP-00014 — AKW pull cache (skills + personas)

## Problem / Pain Points

After EP-00013, the agent loop resolves task-relevant skills from a local pool first, AKW only as a fallback. Personas (EP-00012 Phase 4) follow the same pattern. Both local-first paths work — but they only see what's already authored in `agents/_skills/` and `agents/_roles/`.

The team's curated taxonomy is small (3 example skills, 9 roles). AKW has a richer catalog (~216 skills, 59 agents). When the team finds an AKW skill or persona that's been useful, there's no easy way to "pull this into our local set so it's part of the curated taxonomy." Today the only options are:
- Read it via MCP, copy-paste manually into a file — slow, error-prone, no record.
- Live with the AKW fallback every time — works, but pays the MCP roundtrip on every match and isn't reproducible offline.

## Suggested Solution

CLI verbs to pull individual AKW entries into the local pool:

```bash
barebone-agent skill search <query>           # list top 5 matches from AKW (no write)
barebone-agent skill pull <slug>              # write SKILL.md content to agents/_skills/<slug>.md
barebone-agent skill list                     # list local pool

barebone-agent role search <query>            # list top 5 matches from AKW
barebone-agent role pull <slug>               # write persona to agents/_roles/<slug>.md
barebone-agent role list                      # list local _roles/
```

Pulled files become first-class members of the local pool — picked up by the existing selector with zero plumbing changes. No runtime auto-promote (would need usage tracking + writes to the repo from a long-lived process — both invasive). Curation stays explicit.

## Decisions

### A. CLI-driven, not runtime auto-promote
Explicit `pull` is reviewable, version-controllable, easy to undo (`rm agents/_skills/<slug>.md`). Auto-promote based on usage counts adds a usage table, write paths from the agent loop into the repo, and silent commits — too invasive for the gain.

### B. AKW MCP started just for the CLI command
The pull commands need MCP access. Reuse the existing `tools::mcp` connection code, start the AKW server from an agent's `agent.yml`, call the relevant tool, shut down. Cold start is ~1–2s on a warm `uv` cache — acceptable for an opt-in command.

The AKW MCP server config lives per-agent today. We'll resolve it by:
1. If `--agent <name>` is passed, read `agents/<name>/agent.yml`.
2. Otherwise, scan `agents/*/agent.yml` and pick the first with an `mcp_servers` entry named `akw`.
3. Start that one server, ignoring the rest of the agent runtime (no LLM pool, no DB).

The CLI prints the resolved source on every invocation (e.g. `Using akw config from agents/ino/agent.yml`) so the implicit pick is never silent.

If no agent declares an `akw` MCP server (or the named agent has none), the pull commands print "AKW MCP not configured" and exit 1.

If the MCP server fails to spawn or initialize, capture the last ~10 lines of its stderr and surface them in the exit-1 error so the user can diagnose `uv` / path issues without re-running with extra flags.

### C. Single-by-name, optional --query for discovery
- `pull <slug>` — exact pull. Fails if AKW has no matching agent_get / skill_get for that slug-or-path.
- `search <query>` — read-only list of top 5 AKW results with paths. User decides what to pull next.
- No `--all` / bulk pull. The whole point is curation; bulk-import would bloat the local pool with noise.

### D. Pass-through frontmatter; synthesize `keywords:` if missing
AKW SKILL.md frontmatter typically has `name`, `domain`, `title`, `trigger_tags` (not `tags`). Our local pool selector reads `keywords` (frontmatter) + body words. To make pulled skills useful immediately:
- Copy AKW frontmatter through as-is (line-based; we don't round-trip through `serde_yaml`, which would reorder keys and drop comments).
- If `keywords:` is missing, synthesize one from the first available source: `tags` → `trigger_tags`. Insert it as a new top-level YAML block before the closing `---` fence.
- If none of `keywords` / `tags` / `trigger_tags` exist, leave frontmatter as-is. Body-word matching still works.

### E. Filename collision policy
- Default: refuse to overwrite an existing local file. Print the existing path and a one-liner: `File exists at <path>. Use --force to overwrite or --rename <new_slug> to write under a different name.` Exit 1.
- `--force` flag overwrites.
- `--rename <new_slug>` writes under a different filename (also resolves cross-domain slug collisions).
- This applies to both `skill pull` and `role pull`.

### G. Re-pull / refresh story
There is no separate `refresh` verb. To refresh a pulled skill/role from AKW:
- `pull --force <slug>` overwrites the local file with the latest AKW version. Local edits are lost.
- Or `rm agents/_skills/<slug>.md && pull <slug>` for a clean refetch.

Pull is one-way (AKW → local). Once pulled, the local file is the source of truth — drift is the user's responsibility.

### H. CLI verb naming: singular
`skill` / `role` (singular), not `skills` / `roles`. Reads naturally as a verb chain (`skill pull <slug>`). The plural resource managers (`tasks`, `missions`) are a different shape — list/show/create/delete a stored entity — so the inconsistency is justifiable.

### F. Where pulled files land
- Skills: `agents/_skills/<slug>.md`. The slug is the path-tail of the AKW SKILL.md (e.g. `3_intelligences/skills/workflow/incident_commander/SKILL.md` → `incident_commander.md`). Domain (`workflow`) is dropped from the local filename (the local pool is flat).
- Roles: `agents/_roles/<slug>.md`. Same pattern (`3_intelligences/agents/engineering/sre.md` → `sre.md`).

This means a domain collision (two AKW skills with the same slug across different domains) is possible. Refuse on collision; user uses `--rename <new_slug>` to disambiguate.

## Implementation Phases

### Phase 1 — AKW MCP standalone client
- New `src/tools/akw_client.rs`: thin wrapper over `tools::mcp::McpConnection` that resolves the AKW server config (per Decision B), starts it, exposes typed methods, then shuts down on drop.
- Methods and return shapes:
  - `skill_search(query: &str, limit: usize) -> Result<Vec<SearchHit>, AkwError>`
  - `skill_get(slug_or_path: &str) -> Result<FetchedDoc, AkwError>`
  - `agent_search(query: &str, limit: usize) -> Result<Vec<SearchHit>, AkwError>`
  - `agent_get(slug_or_path: &str) -> Result<FetchedDoc, AkwError>`
- Types:
  - `SearchHit { slug: String, path: String, description: Option<String>, score: f32 }` — `path` is the AKW memory path (e.g. `3_intelligences/skills/workflow/incident_commander/SKILL.md`), `slug` is the path-tail derived per Decision F.
  - `FetchedDoc { slug: String, path: String, frontmatter: serde_yaml::Value, body: String, raw: String }` — `raw` is the full source for byte-identical write; `frontmatter`/`body` are parsed for normalization (Decision D).
  - `AkwError` carries an `stderr_tail: Option<String>` for spawn failures (Decision B's friendly-exit path).
- No new MCP transport code — just composition of `McpConnection::connect` + `call_tool`.

### Phase 2 — Pull/search/list logic
- `src/cmd_pull.rs` (or extend `cmd_agents`) — three private functions per kind:
  - `search(query, kind) -> Vec<SearchHit>` — calls the appropriate AKW tool, returns top 5.
  - `pull(slug_or_path, kind, force, rename) -> Result<PulledFile, PullError>` — fetches via AKW, writes to the right local dir, handles collisions.
  - `list(kind) -> Vec<LocalEntry>` — walks `agents/_skills/` or `agents/_roles/`, returns slugs + descriptions.
- Frontmatter normalization helper: parse YAML, copy fields through, synthesize `keywords:` from `tags:` if needed.

### Phase 3 — CLI wiring
- `src/cli.rs`: add `Skill(SkillCommand)` and `Role(RoleCommand)` enum variants (singular per Decision H).
- Each subcommand has `Search { query, --agent }`, `Pull { slug, --force, --rename, --agent }`, `List`.
- `--agent <name>` is shared by `search` and `pull` and selects which `agents/<name>/agent.yml` provides the AKW MCP config (Decision B).
- Wire `main.rs` to dispatch to `cmd_pull::run_skill` / `cmd_pull::run_role`.

### Phase 4 — Tests
- Unit: frontmatter normalization (with/without `keywords`, with/without `tags`), slug derivation from AKW path, collision policy with/without `--force`/`--rename`.
- Integration: stub the MCP layer, exercise pull→write→list round-trip.
- Live smoke: `barebone-agent skill search "incident"`, `barebone-agent skill pull <slug>`, restart agent, verify the pulled skill participates in the local-first selection (file appears in `equipped skills pool loaded count=N+1`).

### Phase 5 — Docs
- `config/skills/agent_runbook.md`: add a "Pulling from AKW" section describing the CLI verbs and when to use them (vs. authoring locally from scratch).
- `AGENTS.md`: list the new subcommands in the Slash Commands / CLI table (or wherever the CLI is enumerated).

## Out of Scope
- **Auto-promote based on usage counts.** Could revisit if explicit curation feels too high-friction.
- **Bulk pull / mirror-the-whole-catalog.** Defeats curation. Use `search` then individual `pull` instead.
- **Bidirectional sync.** Pull is one-way (AKW → local). If a pulled skill drifts (you edit the local file), you keep your local version; later AKW updates don't merge in.
- **Pulling skill bundle resources.** AKW skills can have `resources/`, `scripts/`, `tests/` companions (per EP-00009). For the local pool we only fetch `SKILL.md`. If an agent ever needs the resources, it can still call `mcp_akw__memory_read` against the original path.
- **Selector tuning** (the EP-00013 follow-up about greedy matching). Separate concern.

## Open Questions
_(Resolved in Decisions B and H during review.)_

## Implementation Status
- [x] Phase 1: AKW MCP standalone client
- [x] Phase 2: pull/search/list logic
- [x] Phase 3: CLI wiring
- [x] Phase 4: tests
- [x] Phase 5: docs

## Status: DONE
