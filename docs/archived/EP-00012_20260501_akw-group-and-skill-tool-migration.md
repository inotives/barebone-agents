# EP-00012 — AKW Group & Skill Tool Migration

## Problem / Pain Points

The `agent-knowledge-wikia` MCP server (referred to as `akw` in our agent config) shipped two breaking changes in EP-00008 and EP-00009 that our integration has not yet absorbed:

1. **Sessions are now "groups."** Tools renamed:
   - `session_start` → `group_start`
   - `session_log` → `group_log`
   - `session_end` → `group_end`
   - `session_status` → `group_status`
   Argument shapes changed too:
   - `group_start` no longer takes `session_id` or `type`. Returns `{group_id, segment_start_at, pending, recommended_context}`. A continuation reuses the same `group_id` and opens a new segment automatically.
   - `group_end` takes no args (closes the active segment).
   - `group_log` keys onto the active group, not a passed-in `session_id` — sends `{request, response}` only.
2. **Skills excluded from `memory_search`.** AKW added dedicated discovery tools: `skill_search`, `skill_get`, `agent_search`, `agent_get`. Our `fetch_akw_skills` in `agent_loop.rs` calls `memory_search(tier="skill")` — that now returns nothing because skills moved out of the general index into a separate walker.
3. **Memory tier folder rename.** `drafts/` → `1_drafts/`, `knowledge/` → `2_knowledges/`, `skills/` → `3_intelligences/skills/`. New tier `3_intelligences/agents/` for personas. Our docs and runbook reference the old paths.

Concrete breakage today:
- `src/session.rs:102,147,174,218,296` — calls renamed/removed tools (`mcp_akw__session_start|log|end`). All AKW session bookkeeping is dead on the new server.
- `src/agent_loop.rs:374,382` — `memory_search(tier="skill")` returns empty; `akw_skills_enabled` is effectively a no-op now.
- `config/skills/agent_runbook.md:86-89` — instructs agents to use stale tool names and `knowledge/` paths.
- `docs/SPECS.md:518-573` — narrates the old `_ensure_akw_session` lifecycle.
- `src/tools/delegate.rs:628-640` — block-list test references `mcp_akw__knowledge_get` (never existed) and uses `name.contains("knowledge")` heuristic; the new tool surface (`skill_search`, `agent_search`, `group_log`, `memory_*`) does not match `"knowledge"`, so the spirit of the block rule needs rechecking.

## Suggested Solution

Swap MCP tool names + argument shapes, switch skill discovery to `skill_search`, and refresh docs. Keep our internal Rust naming (`SessionManager`, `session_*`) — that's our domain (a CLI/Discord conversation), and AKW's "group" is just the persistence concept it maps onto. Only the wire-level MCP tool strings change.

### Phase 1 — `session.rs` group lifecycle
- Replace tool names:
  - `mcp_akw__session_start` → `mcp_akw__group_start`
  - `mcp_akw__session_log` → `mcp_akw__group_log`
  - `mcp_akw__session_end` → `mcp_akw__group_end`
- `start_akw_session`:
  - Drop `type` field from args. Keep `agent` and `metadata`.
  - Pass through `metadata.channel`, `metadata.conv_id`, `metadata.project_id` (project_id moves into metadata, no longer top-level).
  - Parse response: extract `group_id` (top-level field), drop the `session.id` nested lookup. `recommended_context` parsing stays the same shape.
- `log_turn`: remove `session_id` arg from MCP call payload; send `{request, response}` only. The MCP keys onto its active group internally.
- `end_akw_session`: send `{}` instead of `{session_id}`.
- `has_akw`: probe `mcp_akw__group_start` instead of `mcp_akw__session_start`.
- `ActiveSession.session_id` → `group_id` (internal field rename for clarity); external API of `SessionManager` unchanged.
- Update tests at `src/session.rs:236-301` that register `mcp_akw__session_start`.

### Phase 2 — `agent_loop.rs` skill discovery
- `fetch_akw_skills`:
  - Probe `mcp_akw__skill_search` (not `mcp_akw__memory_search`) for the enable check.
  - Replace the `memory_search(tier="skill")` call with `skill_search(query=message)` (no `tier` arg needed; `skill_search` is tier-scoped by definition).
  - Keep the `memory_read` follow-up on the returned `path` field — that's the minimal-change route. `SKILL.md` files are still readable via `memory_read`, just at the new `3_intelligences/skills/...` path.
  - Defer `skill_get` (manifest-aware fetch) to a follow-up EP if/when we want to surface bundle resources to the agent.
- Top-3 path selection logic stays.

### Phase 3 — `tools/delegate.rs` block list
Context: `build_restricted_registry` is a strict allow-list (only `web_search, web_fetch, api_request, shell_execute, file_read, file_write` by default). The `name.contains("memory")/("knowledge")` check at `src/tools/delegate.rs:196-197` is belt-and-suspenders for callers who pass a custom `allowed` list that accidentally names AKW tools. No caller does today, so user-facing risk is zero — this is pure hardening.

With the new AKW tool surface, `memory_*` still matches `"memory"`, but `skill_*`, `agent_*`, `group_*`, `project_*`, `maintain_*` slip past both keywords. To keep "AKW is off-limits to sub-agents by default" working, switch to a prefix match:

- `src/tools/delegate.rs:195-198`: replace the two `name.contains("memory")/("knowledge")` clauses with a single `name.starts_with("mcp_akw__")` clause. Blocks every AKW tool name — old and new — in one rule.
- `src/tools/delegate.rs:628-640`: the test uses fictional `mcp_akw__knowledge_get`. Replace with a real tool name like `mcp_akw__skill_search` so the test still demonstrates the prefix rule.
- No allow-list expansion. If a future caller wants AKW access in a sub-agent, they pass it explicitly (and it still has to be in the allow list).

### Phase 4 — Sub-agent role resolution (local-first, AKW fallback)
Currently `delegate.rs:218 load_role_profile` reads `agents/_roles/{role}.md` directly. AKW's new `3_intelligences/agents/` tier holds 59 personas — useful as a long-tail catalog but not appropriate as the *primary* resolver for this harness.

**Why local-first:**
- **Determinism.** BM25 ranking is term-frequency-driven and produces surprises ("researcher" → ux_researcher beats trend_researcher by 0.1). The 9 curated local profiles are exactly the team's taxonomy.
- **Speed.** Local file read is sync I/O. AKW is an MCP roundtrip per delegate call (only mitigated by caching).
- **Offline-safe.** Works without AKW being up.
- **Curation.** The team controls `agents/_roles/`. AKW agents are externally-authored.

Resolution order for a `role` argument passed to `delegate`:
1. **Local** — `agents/_roles/{role}.md`. Read fresh every call (uncached, so dev edits take effect).
2. **AKW (cache check then lookup)** — `mcp_akw__agent_search(query=role)` → top-ranked `path` → `mcp_akw__agent_get(agent_path)` → `content`. Cached process-wide on the `role` key. Triggers when no local file exists.
3. **Default** — `default_role_prompt()`. Triggers when neither local nor AKW resolves.

Failure modes that drop into the next step: missing local file, AKW MCP not registered, `agent_search` returns empty, `agent_get` errors, response parsing fails. Each path logs at `debug` so the operator can see which won.

Implementation:
- `load_role_profile` is `async` and takes `&ToolRegistry` plus `root_dir`. Returns `String`, never errors.
- Process-lifetime cache: `LazyLock<Mutex<HashMap<String, String>>>` keyed on `role`. AKW hits only — local file reads are intentionally uncached.
- Tests cover: local-wins-over-AKW, AKW-fallback-when-no-local, default-when-neither, cache-hit-skips-mcp.

**Out of scope (revisit later):**
- Passing task context into the AKW search query as a BM25 tiebreaker. Today the AKW fallback uses `query=role` only, so the rare delegate that goes to AKW still risks ranking surprises. Address only if real-world misfires show up.
- Allowing `role="<domain>/<slug>"` shorthand to bypass AKW search. Useful if/when ino learns the AKW persona slugs.
- Pruning `agents/_roles/*.md`. The 9 local profiles stay; they're the primary surface now.

### Phase 5 — Docs & runbook
- `config/skills/agent_runbook.md`:
  - Replace `mcp_akw__memory_search(tier="skill")` example with `mcp_akw__skill_search(query="<topic>")`.
  - Mention `mcp_akw__agent_search` / `mcp_akw__agent_get` for persona discovery.
  - Update `memory_create` example path: `knowledge/<topic>.md` → `1_drafts/<sub>/<topic>.md` (writes go to the drafts tier; the curator promotes to `2_knowledges/`).
- `docs/SPECS.md` lines 518-573:
  - Rewrite the `_ensure_akw_session` narrative as `_ensure_akw_group`. Update tool names and the response-parsing pseudocode.
  - Update the folder layout example (lines 46-63) to reflect the numbered tiers.
- `AGENTS.md` — no AKW path references; no change needed.

### Phase 6 — Verify
- `cargo test` — ensure renamed tests pass, in-memory mock tests for `SessionManager` still cover the lifecycle.
- Live smoke: with ino agent connected to AKW, run `barebone-agent run "search for skills about X"` and confirm a skill_search tool call lands in the conversation log.
- Confirm `group_status` is reachable (we don't call it today, but it's the diagnostic for "is my group still alive").

### Out of Scope
- `skill_get` integration in `agent_loop.rs`. Defer until we actually want bundle manifests in the system prompt. (`agent_get` is in scope for Phase 4.)
- Auto-equip skills based on group metadata (mentioned in EP-00009 out-of-scope) — not our problem here.
- Tier-3 agent persona authoring tools or CLI parity for `akw skill search` / `akw agent search` (already shipped in AKW; we just consume).
- Backwards compatibility with the old session_* names. AKW has no production users and has dropped them entirely.
- Removing local `agents/_roles/*.md` files. They stay as the offline fallback for Phase 4.

### Key Decisions
- Keep `SessionManager` and `session_*` Rust symbols. Internal-facing; renaming would touch every channel and add zero clarity.
- `skill_search` + `memory_read` (not `skill_get`) for minimal diff.
- `delegate.rs` block-list switches to `mcp_akw__` prefix match — one-line hardening fix.
- Sub-agent roles: **local `agents/_roles/` first**, AKW `agent_search`/`agent_get` second (long-tail fallback), default prompt last. AKW hits cached process-wide; local file reads always fresh. Local-first chosen for determinism, speed, offline-safety, and team-curated taxonomy — AKW BM25 ranking ("researcher" → ux_researcher) made it unsuitable as primary.

## Implementation Status
- [x] Phase 1: session.rs group lifecycle
- [x] Phase 2: agent_loop.rs skill discovery
- [x] Phase 3: tools/delegate.rs block list
- [x] Phase 4: sub-agent role resolution via AKW
- [x] Phase 5: docs & runbook
- [x] Phase 6: verify (cargo test: 265 passed; live smoke green — group_start/log/end + skill_search end-to-end against AKW)

## Status: DONE
