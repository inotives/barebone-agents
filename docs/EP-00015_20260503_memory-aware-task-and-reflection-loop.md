# EP-00015 — Memory-aware task/conversation workflow + reflection loop

## Problem / Pain Points

Today the harness wires AKW only superficially:

- `recommended_context` returned by `mcp_akw__group_start` is fetched and discarded — the 4th arg of `agent_loop.run` is hardcoded `None` (`scheduler.rs:177`). Even if AKW surfaces relevant pages on session start, the LLM never sees them.
- `recommended_context` itself is project-tag-driven (`server.py:154`) — it ignores the task message. It returns standing project skills + top-5 knowledge index entries. It does not answer "have we done this before?"
- Tasks have no message-aware retrieval before execution. Recurring tasks don't see their own previous `result`.
- There's no aggregation across runs — the harness can't notice "I've answered the same question shape five times; let me write that down as a preference."
- Research deliverables are not persisted to AKW. The full body lives only in `tasks.result` (truncated to 2000 chars at `scheduler.rs:187`). Future tasks can't recall it because it was never written to a draft.
- User preferences (e.g. `2_knowledges/preferences/*.md`) exist in AKW but are never injected into agent context. They're only read if the LLM happens to call `memory_search` with the right query. As the preference catalog grows (git, backend, frontend, research, etc.), loading them all wholesale into every task would also waste context — only the relevant subset should reach a given task.

Net effect: the agent answers each task as a cold start, even when relevant prior work, preferences, and patterns are sitting in AKW.

## Suggested Solution

A four-stage pipeline applied symmetrically to tasks and conversations.

**Architectural baseline (universal, not just preferences)**: the local filesystem is canonical for **every artifact agents produce** — preferences, research drafts, session summaries, reflection outputs, ad-hoc notes. AKW (and any future memory-MCP module) is durable backup / share medium, never source of truth. Writes always land locally first; a single, artifact-agnostic background pusher mirrors them out at interval. The agent's hot path never reads from AKW for selection or decisions — it reads only from local files. AKW reads (`recommended_context`, prior-work search) are *enrichment* that augments the prompt when available and degrades silently when not.

**Watched artifact types** (each with a local canonical location, all backed up by the same pusher):
| Artifact | Local canonical path | AKW backup path |
|---|---|---|
| Active preferences | `agents/_preferences/<slug>.md` | `2_knowledges/preferences/<slug>.md` |
| Pending preferences (review-gated) | `data/drafts/2_knowledges/preferences/<slug>.md` | `1_drafts/2_knowledges/preferences/<slug>.md` |
| Research / report drafts (task output) | `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md` | `1_drafts/2_researches/<file>.md` |
| Session summaries | `data/drafts/sessions/<group_first_8>-<segment_compact_iso>.md` | `1_drafts/sessions/<file>.md` |
| Ad-hoc note drafts | `data/drafts/notes/<slug>.md` | `1_drafts/notes/<file>.md` |

Any future artifact type just registers a new (local_path → akw_path) mapping with the pusher. Skills and roles (EP-00013/EP-00014) remain pull-only for now (they're imported curated content, not agent-produced); a future EP can extend the pusher to mirror local edits back if needed.

1. **Local-first preference pool, pushed to AKW** — preferences live in `agents/_preferences/<slug>.md` (parallel to `_skills`/`_roles` from EP-00013/EP-00014). All writes (manual edits, `save as preference` keyword, reflection-driven) land on the local filesystem first. A background pusher (default hourly) mirrors local changes out to AKW `2_knowledges/preferences/`. Per-task selection uses keyword matching (same shape as the equipped-skills selector) so only relevant preferences enter the prompt. Pull from AKW is one-shot, explicit (`barebone-agent prefs pull <slug>`), used to import another agent's contribution or bootstrap a fresh checkout.
2. **Pre-execution prior-work search** — at task start (or first turn of a conversation), run `memory_search` with the task title + description across `knowledge`, `research_draft`, `session_archived` tiers. Inject top hits as a "Prior work" system context block.
3. **Post-execution draft persistence (local-first)** — for tasks with deliverable output (`metadata.persist_as_draft: true`), write the full result to `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md`. For CLI / Discord conversation segments, the harness writes a session-summary draft to `data/drafts/sessions/<group_first_8>-<segment_compact_iso>.md` at segment end. **Per-turn `mcp_akw__group_log` is removed**; turns live in SQLite, AKW receives only the summarized draft. Pusher mirrors all drafts to AKW on its next cycle.
4. **Counter-triggered pattern reflection (local-first)** — per-scope counter (per recurring task key, or per agent for conversation segments). On hitting threshold N (default 5), retrieve the last N artifacts in scope from local sources, run a structured-output LLM call to detect a pattern. On hit, write a **pending** preference to `data/drafts/2_knowledges/preferences/<scope>-<date>.md` (LLM-derived → review-gated; user runs `prefs promote` to move into the active pool). The user keyword trigger ("save as preference", "please remember") is treated as explicit user intent and writes directly into the active local pool, skipping the review gate. Counter resets on hit, on miss, or on user keyword trigger.

## Decisions

### A. Preferences live in a local-first pool, selected per-task
- New folder `agents/_preferences/<slug>.md` mirrors the `_skills` / `_roles` pattern. **The local filesystem is canonical**; AKW is a durable backup of this folder, not its source.
- Two storage tiers, both local:
  - **Active pool** — `agents/_preferences/<slug>.md`. The selector reads only this directory.
  - **Pending pool** — `data/drafts/2_knowledges/preferences/<slug>.md`. Holds reflection-generated preferences awaiting review. Not read by the selector.
- Preference frontmatter must carry `keywords:` (or fall back to `tags:`), `scope:` (e.g. `git`, `backend-rust`, `research-finance`, `global`), and `summary:` — same shape as the equipped-skills frontmatter so we can reuse the existing selector code in `src/skills.rs`.
- Per-segment selection (run once, cached across turns):
  - **Task channel**: selector input is `task.title + " " + (description ?: "")`. Run in `scheduler::execute_task` once per task execution, before `agent_loop.run`. Cache on the session.
  - **Conversation channels (CLI/Discord)**: selector input is the **first user message** of the segment. Run in `SessionManager::ensure_session` (or the equivalent first-turn hook) once per segment. Cache on `ActiveSession` alongside `recommended_context`. Subsequent turns within the same segment reuse the cached selection — keeps the system prompt stable, prompt-cache-friendly, and prevents preference flicker mid-conversation as the topic drifts.
  - Configurable via `PREFS_MIN_MATCH_HITS` and `PREFS_TOKEN_BUDGET` env vars. Preferences with `scope: global` are always included regardless of match (cheap baseline like personal style rules).
- Selected preferences are prepended to the system prompt under `## User Preferences` per Decision J2's pinned order.
- If the selection is empty (no global prefs, no matches, or `agents/_preferences/` is empty), the preference block is omitted — never blocks task execution.
- New CLI verb: `barebone-agent prefs promote <draft-slug>` moves a file from `data/drafts/2_knowledges/preferences/` to `agents/_preferences/`. This is the user-explicit gate for LLM-generated preferences entering the active pool. As part of the same operation it also issues `mcp_akw__memory_delete` on `1_drafts/2_knowledges/preferences/<draft-slug>.md` to remove the now-redundant draft backup from AKW (Q8 resolved). On AKW unavailable at promote time: log `warn!`, continue with the local promotion, accept the orphan; on AKW 404: log `debug!` (the draft was never pushed yet), continue. No retry queue — promotion is rare and AKW-down-during-promote is rarer.

### A2. Generic local-first artifact pusher
A single, artifact-agnostic background pusher mirrors the watched local directories out to AKW. Preferences are one consumer; research drafts, session drafts, and any future artifact register against the same pusher.

- The agent **never reads from AKW in its hot path**. Pushing to AKW is a separate, decoupled concern.
- **Background task**: a tokio task spawned alongside the heartbeat (`main.rs` startup), interval default `AKW_PUSH_INTERVAL=3600` seconds. Configurable per-agent via `agent.yml`.
- **Watched directories** are configured as `(local_path_glob → akw_path_prefix)` mappings. v1 ships with the five mappings listed in the Suggested Solution table. Adding a new artifact type is one config-line change plus authoring the producer. **Override semantics**: if `agent.yml` includes `akw_push.mappings:` (see Phase 7 for full config schema), it fully replaces the defaults (no merge). To extend the defaults, copy the default list into `agent.yml` and add. This avoids ambiguous merge rules when both sides specify the same `label`. Omitting `akw_push.mappings` entirely keeps the defaults from `default_mappings()`.
- **Diff algorithm**: for each mapping, walk local files matching the glob, compute SHA-256 per file, compare to manifest. Files where hash differs or no manifest entry exists are pushed to the corresponding AKW path:
  - **No manifest entry** → call `mcp_akw__memory_create`. Treat as new file in AKW.
  - **Manifest entry exists, hash differs** → call `mcp_akw__memory_update` (or `memory_create` with overwrite-allowed semantics if the AKW server treats it that way; verify behavior before implementation and pick the right verb). Updating without first creating a delete-then-create dance preserves AKW's history table for the path.
  - **`memory_create` returns "path already exists" error** despite no manifest entry → fallback to `memory_update`. This handles the migration case where AKW was populated by direct calls before the manifest existed; without this fallback, the first cycle would fail loudly.
  Manifest is updated on per-file push success regardless of which verb was used.
- **Manifest**: `data/.akw_push_manifest.json` maps `local_path → {sha256, last_pushed_at, akw_path}`. Resilient across restarts. Gitignored (Q7 default — committing it would conflict across machines).
- **Boot behavior**: pusher runs a "catch-up" cycle ~30s after agent start (background, non-blocking) so freshly-edited or never-pushed files get backed up promptly without delaying the heartbeat. Subsequent cycles run on the configured interval.
- **Initial state**: a fresh checkout has no manifest; the first cycle pushes everything in the watched dirs. This is also how a no-AKW user later turns AKW on — first push uploads whatever has accumulated locally.
- **Deletion policy**: local deletions do **not** propagate to AKW (safety — AKW is backup; a stray `rm` shouldn't wipe shared state). To remove from AKW, the user calls `mcp_akw__memory_delete` directly. A future `barebone-agent akw remove --remote <path>` verb is out of scope for v1.
- **Conflict policy**: pushes overwrite the AKW copy at the same path (last-push-wins). Direct AKW-side edits between two pushes are clobbered. Documented behavior, not a bug — AKW edits should go through a curator/PR flow when multiple agents share state.
- **Pull is explicit, on-demand only, per-artifact-kind**:
  - `barebone-agent prefs pull <slug> [--force] [--rename <new_slug>]` — one-shot fetch from AKW `2_knowledges/preferences/<slug>.md` into the active pool. Mirrors the `skill pull` verb from EP-00014; same collision policy (refuse without `--force`/`--rename`).
  - **Manifest update on pull**: after a successful pull, `prefs pull` writes a manifest entry for the new local file with the SHA-256 of the just-pulled content and the AKW source path. This prevents the next pusher cycle from re-pushing the same content back to AKW (the manifest hash matches the local hash → no diff → no push).
  - Other artifacts (drafts, session summaries) have no `pull` verb in v1 — they're agent-produced, not user-imported. If a user wants to inspect AKW-side artifacts, they use AKW tooling directly.
  - There is no auto-pull and no `--all` bulk pull. New work from other agents enters this agent's local state only by deliberate import.
- **Generic CLI verbs**:
  - `barebone-agent akw push` — run a pusher cycle now (useful before shutdown). Reports diffs, successes, and failures.
  - `barebone-agent akw status` — show watched-dir summary: file count, dirty count (changed since last push), never-pushed count.
- **Per-artifact CLI verbs** stay scoped:
  - `barebone-agent prefs list | promote | pull` — preference-specific (Decision A).
  - Future artifact CLIs follow the same per-kind pattern.
- **AKW unavailable**: pusher cycle logs once at `warn!` and skips. Local edits accumulate; next reachable cycle catches up via the manifest. No retry storm.
- **No `local_only` frontmatter flag**. With local-first, every local file is implicitly local-first. If a user wants something that never reaches AKW, place it under a non-watched directory or extend the pusher's ignore globs in agent config. Out of scope for v1 — assume all watched-dir content is shareable.

### B. Prior-work search is harness-driven, single call, scoped tiers
- Lives in `scheduler::execute_task` (between session_start and agent_loop.run) and in the conversation handler's first-turn path.
- Query: `task.title + " " + (description ?: "")`, capped at 200 chars.
- Tiers: union of `knowledge`, `research_draft`, `session_archived`. Excludes `session_draft` (noisy), skills (already injected via the equipped-skills path), and personas.
- **Path exclusion**: filter out `2_knowledges/preferences/**` from results. Active preferences are already injected via the `## User Preferences` block (Decision A); surfacing them again under `## Prior Work` is duplication.
- Top-K = 3 (configurable via `PRIOR_WORK_TOP_K`). Each hit's full content is fetched via `memory_read` and inlined under a `## Prior Work` system block with each hit prefixed by its path.
- Token budget cap: `PRIOR_WORK_TOKEN_BUDGET` (default 4000 chars total across all hits). If hits exceed, truncate longest first.
- LLM still has `mcp_akw__memory_search` / `memory_read` as tools — it can do follow-up retrievals if the harness pick is insufficient.

### C. Recurring tasks see their own previous result
- In `execute_task`, when `task.schedule.is_some()` and `task.result.is_some()`, append a `## Previous Run Result\n{first 1500 chars}` block to the prompt, alongside dependency-result threading.
- This is independent of the prior-work search above (which queries cross-task) and independent of the recommended_context path. It's the cheapest, highest-signal recall channel for recurring work.

### D. Inject `recommended_context` into the prompt
- One-line fix: thread `SessionManager::get_recommended_context(conv_id)` through to `agent_loop.run` as a new `recommended_context` arg, formatted under a `## Project Context` system block.
- This unblocks the latent-bug noted in EP-00015 design discussion. Applies to both tasks and conversations. Independent of B/C — `recommended_context` is project-keyed standing context, not message-keyed retrieval.

### E. Research/report draft persistence: local first
- **Local-first write path**: `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md` on the filesystem. Slug derived from task title (lowercase, alphanumeric+hyphen, max 40 chars). The minute-precision timestamp ensures hourly recurring tasks produce one draft per run rather than overwriting each other; for daily/weekly tasks this is just slight extra filename length. Collision (two runs in the same minute on the same task) gets a `-N` suffix per the same policy as Decisions G/H.
- Title and body authored by the LLM as the task result. The harness post-processes: takes `task.result`, asks the LLM in a follow-up cheap call (`haiku` model) to produce a `{title, summary, tags, body}` JSON, writes the markdown locally.
- Push to AKW happens via the generic pusher (Decision A2). The harness does **not** call `memory_create` directly — that responsibility belongs to the pusher cycle. This decouples task execution latency from AKW availability and ensures consistency with all other artifact types.
- Opt-in per task via metadata: `metadata.persist_as_draft: true`. Default off — most tasks (one-off ops, status checks) don't produce report-shaped output. Explicit opt-in keeps drafts focused on deliverables.

### E2. Session drafts: local first; per-turn `group_log` dropped
Today, session drafts are AKW-managed: per the AKW MCP `group_wrapup` prompt and our session lifecycle (`session.rs:120` `end_session`), the LLM is instructed to call `memory_create` on `1_drafts/sessions/...` at end of segment, and `SessionManager::log_turn` (`session.rs:103`) calls `mcp_akw__group_log` on every turn. With local-first, both flows change.

**Turn handling**:
- **SQLite is the canonical turn store** (already true today via the `conversations` table). Every request/response pair is written there in full as it happens.
- **Per-turn `mcp_akw__group_log` calls are dropped.** `SessionManager::log_turn` becomes a SQLite-only write; the AKW per-turn push is removed entirely. AKW's own `turns` table will stay empty for our agent — that's accepted, since AKW's role is artifact storage, not live trace.
- Trade-off: AKW loses its incremental, mid-segment visibility into in-progress conversations. If the agent crashes mid-segment, AKW sees `group_start` only; full turn detail is recoverable from SQLite. The user explicitly accepted this — full detail lives locally; AKW only needs the summary.

**Session draft**:
- **Producer**: new `src/session_draft.rs::write_session_draft(llm, db, conv_id, group_id, channel_type, segment_started_at, segment_ended_at) -> Result<PathBuf, ...>` called from `SessionManager::end_session` (`session.rs:120`) before issuing AKW `group_end`. The producer reads turns from SQLite by `conv_id` filtered to `[segment_started_at, segment_ended_at]` (`SessionManager` carries `segment_started_at` on `ActiveSession`; `ended_at` = call time). The `llm` parameter performs the summarization call.
- **Channel filter**: session drafts are written **only when `channel_type != "task"`**. Task-channel sessions are single-round-trip and produce trivial summaries; the research draft (Decision E) and `tasks.result` already cover that content. Writing session drafts for task channels would just spam `data/drafts/sessions/` with hundreds of one-liner files per recurring task per week.
- **Content generation**: cheap LLM call summarizing the segment's turns (pulled fresh from SQLite, full content — no longer constrained by the old 500-char truncation that was there only for AKW group_log). Output: markdown body + frontmatter `{agent, group_id, conv_id, segment_started_at, segment_ended_at, channel_type, turn_count}`. The `agent` field is required so reflection's `agent_conv` scope (Decision G) can filter session drafts by agent without having to read all of them.
- **Optional inline turn log**: the session draft body can include the turn log as an appendix (`## Turns` section) if `SESSION_DRAFT_INCLUDE_TURNS=true` (default true). **Per-turn cap**: each request and each response is truncated to 2KB before inclusion in the appendix; turns over the cap get a `[... truncated, see SQLite conv_id={conv_id}]` marker. **Total appendix cap**: 50KB; if exceeded, drop oldest turns first and add a `[... N earlier turns omitted, see SQLite]` marker. The summary itself (above the appendix) is unaffected by either cap.
- **Local path**: `data/drafts/sessions/<group_first_8>-<segment_compact_iso>.md` — same naming convention AKW expects, just on the local filesystem.
- **Push**: pusher mirrors to AKW `1_drafts/sessions/<file>.md`.

**AKW lifecycle calls that remain**: `group_start` and `group_end` still fire to AKW as segment markers (so AKW knows a segment opened/closed). They're cheap and provide a chronological audit trail in AKW's group table. Only `group_log` is removed.

**Skill prompt update**: remove the "write session draft via memory_create" instruction from `agent_runbook.md` and any other prompts that may reference it. The harness owns this now; the LLM should not also do it (would create duplicates). `group_log` was always harness-owned, so no LLM-facing prompt should mention it — verify this during the doc audit.

**No-AKW behavior**: session draft is still written locally. `group_start` / `group_end` short-circuit on absent AKW (existing behavior at `session.rs:160`). Pusher just doesn't run.

### E3. Ad-hoc note drafts (forward-compatible)
Reserved local path `data/drafts/notes/<slug>.md` for arbitrary agent-authored notes that don't fit research or session shapes. v1 doesn't ship a producer that writes here — this is registered with the pusher mainly so future skills/EPs can drop notes locally and get free AKW backup. If empty, the pusher cycle is a no-op for this directory.

### F. Counter scoping: per-recurring-task-key for scheduled, per-agent for conversation segments
- New SQLite table `reflection_counters(scope_type TEXT, scope_key TEXT, agent_name TEXT, count INTEGER, last_reset_at DATETIME, PRIMARY KEY(scope_type, scope_key, agent_name))`.
- `scope_type` ∈ `{"task_key", "agent_conv"}` (the previously listed `agent_task` scope is removed — see Open Question Q9 for context; if reinstated, threshold should be ≥10 to avoid noise).
  - For recurring tasks (`schedule.is_some()`): increment row `("task_key", task.key, agent_name)` on `complete_task` success (not on `blocked`).
  - For conversation segments: increment row `("agent_conv", "_global", agent_name)` on `SessionManager::end_session` (i.e. once per segment, **not** per turn). Tracks segments, not turn counts.
- Threshold default 5, configurable via `REFLECTION_THRESHOLD`. Per-scope override in agent.yml under `reflection: { task_key_threshold, conv_threshold }`.
- Increments use a single SQL `UPDATE ... RETURNING count` (or `INSERT ... ON CONFLICT DO UPDATE`) inside a transaction so two concurrent task completions can't double-fire reflection at the threshold boundary. With rusqlite's connection-mutex this is already serialized in practice; the explicit transaction guards against future connection-pool changes.

### G. Reflection writes locally to the pending pool; user promotes
- On counter hit, the reflection step runs:
  1. Retrieve the last N artifacts for the scope from local sources (no AKW dependency):
     - `task_key` scope — glob `data/drafts/2_researches/<task_key>-*.md`, sort by mtime, take last N. **Requires `metadata.persist_as_draft: true` on the task**: recurring tasks reset `status='todo'` and overwrite `tasks.result` on each completion (`tasks.rs:382`), so SQLite only ever holds the latest run's result — there's no historical trail to fall back to. If a task hasn't opted into draft persistence and the counter hits threshold, reflection logs at `info!` ("no artifact history for task `{key}`; skipping reflection — set `metadata.persist_as_draft: true` to enable"), resets the counter, and exits without an LLM call. A future EP can add a `task_runs` history table if reflection-without-drafts becomes a real ask.
     - `agent_conv` scope — read session drafts from `data/drafts/sessions/`, filter by frontmatter `agent` matching this agent, sort by mtime, take last N. Session drafts are bounded summaries (Decision E2), so the reflection LLM input stays compact even for users with long Discord histories.
  2. Single LLM call (haiku model, low cost), JSON output schema:
     ```json
     {"pattern_found": bool, "scope": str, "preference_body": str, "evidence_paths": [str]}
     ```
  3. If `pattern_found=false`: log, reset counter, no write.
  4. If `pattern_found=true`: write locally to `data/drafts/2_knowledges/preferences/<scope-slug>-<YYYYMMDD>.md`. **On filename collision** (a prior reflection ran for the same scope same day): append `-N` suffix and increment until unique, e.g. `<scope>-<YYYYMMDD>-2.md`. Frontmatter: `{type: preference, scope, evidence_paths, source: reflection}`. `evidence_paths` are recorded as **local paths** (e.g. `data/drafts/2_researches/TSK-00007-202605031400-nvda-snapshot.md`) — once mirrored to AKW these still reference the local origin; a reader on AKW can derive the AKW counterpart from the fixed mapping table. Reset counter.
- **Pending pool, not active pool.** LLM-derived preferences are review-gated. The user runs `barebone-agent prefs promote <slug>` to move into the active pool (Decision A). This is the local equivalent of the curator promotion step from the AKW-first model.
- The pusher backs up the draft to AKW `1_drafts/2_knowledges/preferences/` on its next cycle (so other agents / the human curator can see and discuss it), but the agent itself doesn't read pending drafts in its hot path.
- **Failure detection**: if the reflection LLM call returns a string starting with the harness's known failure prefixes (same set as `scheduler.rs:188` — `"I'm sorry, all models failed"` or `"LLM call failed"`), treat as failure: log at `warn!`, **do not reset the counter** so the next attempt also retries. JSON parse failure of a non-prefix response is treated as `pattern_found=false` (counter resets; over-eager retries on a flaky LLM aren't worth the cost).

### H. User keyword triggers (Discord/CLI) — direct write to active pool
- Regex: `(?i)\b(save as preference|please remember|remember this|save this preference)\b` matched against user message (whole message, word-bounded).
- On match: take the last assistant turn in the conv_id, run a structured-output extraction prompt against just that turn, write **directly to the active pool** at `agents/_preferences/manual-<YYYYMMDDhhmmss>-<slug>.md` (second-precision timestamp). On filename collision (rapid double-trigger) append `-N` until unique. Never silently overwrite.
- **Edge case — no prior assistant turn** (user's first message in a fresh conv contains the keyword): nothing to save. Acknowledge with "Nothing to save yet — this works after the assistant has responded at least once. Try again after the next reply." Counter is not reset (no save occurred).
- **Why active, not pending**: the keyword is explicit user intent — they've seen the assistant's output and want it remembered. Routing through review-gate would be friction without benefit. The user already reviewed it by virtue of asking for it.
- Reset only the matching scope's counter (`agent_conv` for the conversation that triggered it). Cross-scope counters untouched.
- The pusher backs the new pref up to AKW `2_knowledges/preferences/` on its next cycle.
- Acknowledge in the conversation with the actual path written, e.g. "Saved as preference at `agents/_preferences/manual-20260503143015-summary-style.md` (active). Will sync to AKW within the hour. Edit or remove the file directly to revise."

### I. System tasks bypass the counter
- Tasks created internally by the reflection loop (or future system housekeeping) have `metadata.system: true`. These don't increment counters and don't get prior-work injection (avoid feedback loops where the reflection task triggers a reflection).
- Reserved task title prefix `__sys__` as belt-and-braces filter.

### J. Symmetric treatment for tasks and conversations
- Decisions A, B, D, F, G, H apply to both channels. C and E are task-only (recurring runs / deliverable output don't have conversation analogs). E2 (session drafts) is conversation-only.
- The wiring lives mostly in `SessionManager::ensure_session` (preference + recommended_context injection) and in a new `MemoryAwareContext` builder shared by `scheduler::execute_task` and the conversation-loop entry point.

### J2. System prompt block order (pinned)
With multiple new context blocks introduced across phases, the order matters for both consistency and how the LLM weighs each input. Pin this once so phases don't drift:

1. Character sheet (existing)
2. `## User Preferences` (Decision A — selected per task/turn)
3. Core skills (existing — `config/skills/*.md`)
4. Equipped skills / AKW-fallback skills (existing — EP-00013)
5. `## Project Context` (Decision D — `recommended_context`)
6. `## Prior Work` (Decision B)
7. `## Previous Run Result` (Decision C — task channel only)
8. Cross-agent @mention context (existing — `agent_loop.rs:354`)
9. Parent conversation context (existing — `agent_loop.rs:362`)
10. User message

Each block is omitted entirely (header + content) when its content is empty. Empty headers are never emitted — that would just train the LLM to ignore the heading style.

### K. No-AKW fallback: hot path is already local-only; only push and read-from-AKW degrade
With local-first, the agent's hot path (preference selection, reflection write, draft write, manual save) doesn't touch AKW at all. AKW is only involved in two narrow places: (a) the background pusher mirroring local writes outward, and (b) two read-paths that *enrich* prompts (`recommended_context`, prior-work search). Both are non-blocking and degrade cleanly to "skip the enrichment."

| Capability | AKW present | AKW absent / unavailable |
|---|---|---|
| Active preference selection (Decision A) | reads `agents/_preferences/` only | identical |
| Pending preference selection | not selected — review-gated | identical |
| Manual `save as preference` (Decision H) | writes to local active pool | identical |
| Reflection write (Decision G) | writes to local pending pool | identical |
| Research draft write (Decision E) | writes to local `data/drafts/2_researches/` | identical |
| Session draft write (Decision E2) | writes to local `data/drafts/sessions/` | identical |
| Counter table (Decision F) | SQLite-local | identical |
| Previous-run result (Decision C) | SQLite-local | identical |
| Background pusher (Decision A2) | runs at interval, pushes diffs across all watched mappings to AKW | task not spawned at boot. No retry mid-process. Local writes accumulate; user must restart with AKW configured to catch up. |
| `recommended_context` (Decision D) | injected as `## Project Context` | block omitted (existing `has_akw()` short-circuit at `session.rs:160`) |
| Prior-work search (Decision B) | runs `memory_search` + `memory_read`, injects `## Prior Work` | block omitted |
| AKW lifecycle calls (`group_start`, `group_end`) | fire as today (segment markers only — `group_log` is removed per Decision E2) | short-circuited by existing AKW-absence guard |
| Per-turn logging (`SessionManager::log_turn`) | writes to SQLite only — no AKW call | identical |

**Detection contract**: a single helper `registry.has("mcp_akw__memory_search")` (mirroring the existing `SessionManager::has_akw()` at `session.rs:151`) is the gate at boot. The pusher checks once at start; if absent, it exits and the task is not spawned. Per-task code paths never need to check AKW availability — they only touch local state.

**No silent partial states for read-paths**: if a feature starts a multi-step AKW read (e.g. memory_search → memory_read) and any step fails mid-flight, treat the whole feature as absent for that task (skip the block, no partial injection).

**Pusher liveness**: if AKW was up at boot but later goes down, the pusher logs once at `warn!` per cycle and skips. The manifest is unchanged on failure, so the next successful cycle catches up automatically. No back-off table; relying on the configured interval is good enough at this scale.

**Logging discipline**: AKW absence at boot is a single `info!("AKW MCP not configured — running in local-only mode, push disabled")` line. No per-task warnings ever — local-only is a fully supported mode, not a degraded one.

## Implementation Phases

### Phase 1 — Plumbing fixes (no behavior change for users without prefs)
- Thread `recommended_context` from `SessionManager::ensure_session` into `agent_loop.run` (Decision D).
- Add `recommended_context: &[String]` parameter to `agent_loop.run`; format into system prompt under `## Project Context`. Empty slice → block fully omitted (no header, no content).
- Update conversation channels (CLI, Discord) the same way.
- Establish the shared `has_akw()`-style gate helper that subsequent phases reuse (Decision K).
- **Pin the system prompt block order** per Decision J2 in `build_system_prompt`. Add a unit test that asserts the rendered prompt structure matches the pinned order when all blocks are present (this catches drift in later phases).
- Tests: unit test on `build_system_prompt` with non-empty context; unit test that with AKW absent the `## Project Context` block is omitted (no panic, no warning logged per turn); smoke test that a project-tagged task sees its `recommended_context` in the system prompt.

### Phase 2 — Generic local-first artifact pusher + preference pool + per-task selection
This phase delivers two coupled pieces: the artifact-agnostic pusher infrastructure (used by all later phases) and the preference pool / selection / CLI as the first consumer.

- **Generic pusher infrastructure**: new `src/akw_pusher.rs` with:
  - `WatchedMapping { glob: String, akw_path_prefix: String, label: String }` — describes one watched-dir → AKW-path-prefix mapping.
  - `Manifest` struct (de/serializes `data/.akw_push_manifest.json`).
  - `compute_diffs(mappings, manifest) -> Vec<PushOp>` — walks each glob, hashes files, returns ops for new/changed.
  - `push_cycle(akw_client, mappings, manifest) -> PushReport` — runs the diff, calls `memory_create` for new files and `memory_update` for changed files (per the verb-selection rules in Decision A2). Target AKW path = `prefix + relative_path`. Updates manifest on per-file success. Falls back from `memory_create` → `memory_update` on "path exists" errors.
  - Default mapping list lives in `src/akw_pusher.rs::default_mappings()` and matches the table in the Suggested Solution section. Per-agent overrides via `agent.yml`.
- **Pusher background task**: `tokio::spawn` a loop in `main.rs` startup, after the heartbeat spawn. **Only spawned if AKW is configured at boot.** A 30s warm-up sleep before the first cycle. Subsequent cycles on `AKW_PUSH_INTERVAL`. Errors logged at `warn!` once per cycle, never fatal.
- **Generic CLI**: `barebone-agent akw push | status` — wire into `cli.rs` and a new `cmd_akw.rs`. `push` runs one cycle synchronously; `status` reports per-mapping file counts and dirty counts.
- **Preference pool reader**: new `src/preferences.rs` with `load_preference_pool(dir: &Path) -> Vec<Preference>` and `select_preferences(pool, message, min_hits, budget) -> Vec<Preference>`. Reuse the equipped-skills selector internals where possible (extract a shared `select_by_keywords` helper). `scope: global` bypasses keyword matching. Reads `agents/_preferences/` only — pending pool excluded by design.
- **Selection wiring (cached per segment per Decision A)**:
  - Extend `ActiveSession` with a cached `selected_preferences: Vec<Preference>` field, populated once at segment start (alongside the existing `recommended_context` cache).
  - For task channels: `scheduler::execute_task` runs `select_preferences(pool, task.title + description, ...)` after `ensure_session` and stores the result on the active session.
  - For conv channels: the first-turn hook (in `SessionManager::ensure_session` or the conversation handler's first-message path) runs the selector against the user's first message and caches.
  - `agent_loop::build_system_prompt` reads the cached selection (passed as a parameter, same shape as `recommended_context`) and injects under `## User Preferences`. **No per-turn re-selection** — the cache lasts for the segment lifetime.
- **Preference-specific CLI**: `barebone-agent prefs list | pull <slug> | promote <slug>` — `cmd_prefs.rs`. `pull` and `promote` reuse the existing `akw_client.rs` from EP-00014. `promote` performs: read draft → write active → drop draft → drop draft manifest entry → call `mcp_akw__memory_delete` on the draft AKW path (best-effort per Decision A / Q8 resolution).
- **Frontmatter contract**: document required `keywords`, `scope`, `summary` fields. Add a template at `agents/_preferences/.template.md`.
- Tests:
  - Unit on diff/manifest (new file, changed file, unchanged file, manifest survives across runs, multi-mapping correctness).
  - Unit on pusher with mocked AKW client: new file → `memory_create`; changed file (existing manifest entry) → `memory_update`; `memory_create` returning "path exists" → fallback to `memory_update`; success path updates manifest; failure path leaves manifest untouched; per-mapping isolation — failure on mapping A doesn't block mapping B.
  - Unit on preference selection (global always wins, keyword matching, budget capping, pending pool excluded).
  - Integration with live AKW: boot agent with AKW configured, drop a new preference into `agents/_preferences/` and a research draft into `data/drafts/2_researches/`, confirm both land in AKW at the right paths within one cycle. Edit one, confirm only that file is re-pushed. Delete locally, confirm AKW copy persists.
  - Integration `prefs pull <slug>` round-trip.
  - `barebone-agent akw push` and `akw status` smoke tests.

### Phase 3 — Prior-work search + previous-run threading
- New `src/memory_context.rs::build_prior_work_block(registry, query, top_k, token_budget) -> String`.
- Wire into `scheduler::execute_task` between `ensure_session` and `agent_loop.run`. Pass result as another system-context block.
- Append `## Previous Run Result` block when `task.schedule.is_some() && task.result.is_some()` (Decision C).
- Symmetric: conversation handler calls the same builder on the first user turn (detect via `db.get_conversation_turn_count == 0`).
- Tests: unit on the builder (no hits, hits, over-budget truncation), integration with seeded AKW drafts.

### Phase 4 — Research draft persistence + session draft persistence (both local-first)
Two consumers of the generic pusher; can be implemented in parallel since they share no code with each other.

**Research drafts** (Decision E):
- **Schema**: extend `TaskMetadata` (in `src/db/tasks.rs`) with a new optional field `persist_as_draft: Option<bool>`. Defaults to `None` (treated as `false`). Migration is additive — existing rows have `null` for this field, which deserializes to `None`.
- New `src/draft_writer.rs::persist_research_draft(llm, task: &Task) -> Result<PathBuf, ...>`.
- Called from `scheduler::execute_task` after `complete_task` success, gated on `task.metadata.persist_as_draft == Some(true)`.
- Workflow: cheap LLM call to extract `{title, summary, tags, body}`, then write to `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md` (minute-precision timestamp; `-N` suffix on same-minute collision). **No AKW call here** — Phase 2's pusher picks it up via manifest diff.

**Session drafts + turn-handling refactor** (Decision E2):
- **Drop per-turn AKW push**: in `session.rs::log_turn`, remove the `mcp_akw__group_log` call entirely. The function becomes SQLite-only: it writes the full turn to the `conversations` table and returns. No truncation needed (the 500-char limit existed only because AKW group_log was bandwidth-sensitive).
- **New `src/session_draft.rs::write_session_draft(llm, db, conv_id, group_id, channel_type, segment_started_at, segment_ended_at) -> Result<PathBuf, ...>`** — pulls the segment's turns from SQLite by `conv_id` filtered to `[segment_started_at, segment_ended_at]`, runs a cheap LLM summarization, writes to `data/drafts/sessions/<group_first_8>-<segment_compact_iso>.md` when `channel_type != "task"`. Appends a capped `## Turns` section when `SESSION_DRAFT_INCLUDE_TURNS=true` (default; per-turn 2KB cap, 50KB total cap per Decision E2).
- Called from `SessionManager::end_session` (`session.rs:120`) **before** issuing AKW `group_end`. Local-write failure is logged at `warn!` and does not block `group_end`. Task-channel calls return immediately (no draft).
- Update `agent_runbook.md` and any other prompt instructing the LLM to call `memory_create` on session draft paths — the harness owns this now. (No removal needed for `group_log` instructions; that call has always been harness-owned, not LLM-driven.)
- AKW `group_start` and `group_end` calls are unchanged (segment markers only).

Tests:
- Unit on research-draft slug derivation + path generation; on session-draft naming convention.
- Integration that a `persist_as_draft: true` task ends with a research file at the expected path.
- Integration that a CLI conversation that opens and closes a session ends with a session-draft file at the expected path.
- Integration with AKW configured that both files land in AKW (under `1_drafts/2_researches/` and `1_drafts/sessions/` respectively) within one pusher cycle.
- Confirm no duplicate session drafts (the LLM doesn't also write one).

### Phase 5 — Counter table + reflection loop (local write only)
- Migration: add `reflection_counters` table (Decision F). **Fresh-start policy** (Q10 resolved): migration only creates the schema; no row backfill from existing `conversations` or `tasks`. Counters start at zero on first reflection-eligible event after deployment, so users with pre-existing history aren't blasted with a near-empty reflection on their first post-deploy segment.
- New `src/reflection.rs::increment_and_maybe_reflect(scope_type, scope_key, agent_name, threshold) -> Option<ReflectionOutcome>`. Increments inside a transaction; only triggers retrieval+LLM when the post-increment count `>= threshold`.
- Hooked in:
  - `scheduler::execute_task` after `complete_task` success → `("task_key", task.key, agent_name)`. **Skip when `task.metadata.system == true`** (Decision I — system tasks bypass the counter to prevent reflection-on-reflection feedback loops).
  - `SessionManager::end_session` (Decision F update — segment-bounded, **not** per turn) → `("agent_conv", "_global", agent_name)`. Task-channel segments (`channel_type == "task"`) are excluded — that channel uses the `task_key` scope instead.
- On threshold hit, retrieve last N artifacts from **local sources only** (Decision G):
  - `task_key` scope — glob `data/drafts/2_researches/<task_key>-*.md`, sort by mtime, take last N. If fewer than N drafts exist (task not opted into `persist_as_draft`), log at `info!` ("no artifact history for task `{key}`; skipping reflection"), reset counter, exit. **No SQLite fallback** — recurring tasks overwrite `tasks.result` per completion, so there's no historical trail to fall back to.
  - `agent_conv` scope — last N session drafts from `data/drafts/sessions/` filtered by frontmatter `agent` matching this agent, sorted by mtime.
- Run reflection LLM call. On hit, write pending preference to `data/drafts/2_knowledges/preferences/<scope-slug>-<YYYYMMDD>.md` (`-N` suffix on collision per Decision G). Failure-prefix detection per Decision G — counter is **not** reset on prefix-match failure so the next attempt retries.
- Tests:
  - Unit on counter math (increment, threshold hit, reset, transactional atomicity under simulated concurrent calls).
  - Integration: 5 successful runs of a recurring task with `persist_as_draft: true` produces a local pending preference file (stubbed LLM returns `pattern_found=true`).
  - Integration: 5 successful runs of a recurring task **without** `persist_as_draft` → counter increments to 5, reflection skips with the `info!` log, counter resets. No LLM call, no preference file.
  - Integration: 5 conversation segments end → reflection fires once (not 5× on a 5-turn segment).
  - Integration with AKW configured: the pending preference reaches AKW within one pusher cycle.

### Phase 6 — User keyword triggers (write directly to active pool)
- New `src/triggers.rs::detect_save_preference(message: &str) -> bool` with regex per Decision H.
- Wired into Discord and CLI conversation handlers (not the task path — tasks have no live user input).
- On match: extract a preference from the last assistant turn via cheap LLM call, write directly to `agents/_preferences/manual-<YYYYMMDDhhmmss>-<slug>.md` (active pool, second-precision timestamp, `-N` suffix on collision per Decision H). Pusher backs it up to AKW on next cycle.
- Acknowledge in the conversation per Decision H.
- Tests: unit on the regex (positive/negative cases including quoted phrases); unit on collision suffixing (rapid double-trigger produces `-2`); integration that a triggering message in a CLI session produces a file in the active pool; integration with AKW configured that the new pref reaches AKW.

### Phase 7 — Docs + config
- `docs/SPECS.md`: add a `Memory-aware context` subsection covering the four context blocks (preferences, project, prior work, previous run) and their order.
- `config/skills/agent_runbook.md`: update the "Search before acting" guidance to note that prior-work search is now run automatically — the LLM should still do follow-up searches but no longer needs to do an initial "have we seen this?" search itself.
- New env vars in SPECS table: `PRIOR_WORK_TOP_K`, `PRIOR_WORK_TOKEN_BUDGET`, `REFLECTION_THRESHOLD`, `AKW_PUSH_INTERVAL`, `PREFS_MIN_MATCH_HITS`, `PREFS_TOKEN_BUDGET`, `SESSION_DRAFT_INCLUDE_TURNS` (default `true`).
- New per-agent config under `agent.yml`:
  - `reflection: { task_key_threshold, conv_threshold }`
  - `preferences: { min_match_hits, token_budget }`
  - `akw_push: { interval_seconds, mappings: [{glob, akw_path_prefix, label}] }` — the mappings list defaults to the five entries in Decision A2 / the Suggested Solution table; agents can override or extend.
- Document the preference frontmatter contract (`keywords`, `scope`, `summary`).
- Document the **local-first model** as a top-level architectural section in SPECS:
  - The local filesystem is canonical for all agent-produced artifacts (preferences, research drafts, session summaries, pending preferences, future note drafts).
  - AKW is durable backup, mirrored by a single artifact-agnostic pusher.
  - Hot path is local-only; AKW reads (`recommended_context`, prior-work) are enrichment that degrades silently.
  - Manifest at `data/.akw_push_manifest.json` is gitignored, regenerated automatically.
- Document the `prefs promote` workflow for moving reflection-generated drafts into the active pool.
- `.gitignore` entries: `data/.akw_push_manifest.json`, `data/drafts/sessions/`, optionally `data/drafts/2_researches/` (depending on whether the team wants research drafts in git).

### Phase 8 — Live verification
- **AKW-present smoke run**: pick one recurring task (e.g. a "daily NVDA snapshot" research task) with `metadata.persist_as_draft: true`. Seed `agents/_preferences/` by hand with three local preferences (one `scope: global`, one `scope: research-finance`, one `scope: backend-rust`). Boot the agent with AKW configured. Run an interactive CLI conversation (which produces a session draft) plus the recurring task five times. Verify across **all artifact types**:
  - First pusher cycle uploads all three local prefs to AKW `2_knowledges/preferences/`. Manifest is written.
  - System prompt for the NVDA task contains the global pref + the `research-finance` pref, but **not** the `backend-rust` pref.
  - Project context + prior-work blocks appear when AKW is reachable.
  - Recurring runs see their previous run result.
  - Each run produces a research draft at `data/drafts/2_researches/<task_key>-<YYYYMMDDHHMM>-<slug>.md`. Pusher mirrors each to AKW `1_drafts/2_researches/` within one cycle. Confirm five distinct files (no overwrite from minute-precision collision).
  - **No session drafts are produced for task-channel runs** (Decision E2 channel filter). `data/drafts/sessions/` contains only conversation-segment drafts.
  - The CLI conversation produces a session draft at `data/drafts/sessions/<group_first_8>-<segment_iso>.md` with the LLM summary plus (under `SESSION_DRAFT_INCLUDE_TURNS=true`) a capped `## Turns` appendix (per-turn 2KB, total 50KB). Pusher mirrors to AKW `1_drafts/sessions/` within one cycle. Confirm only **one** session draft per segment (no duplicate from LLM).
  - Confirm **no `mcp_akw__group_log` calls fire during the conversation** (per-turn AKW push is removed). AKW's group `turns` table for our agent stays empty; SQLite holds the canonical turn log. `group_start` and `group_end` still fire as segment markers.
  - Run a 10-turn Discord segment, end the session, confirm `agent_conv` counter incremented by exactly **1** (segment-scoped, not turn-scoped per Decision F).
  - On the 5th run of the recurring task, a pending pref appears at `data/drafts/2_knowledges/preferences/<scope>-<date>.md`. Active pool unchanged. Pusher mirrors the pending pref to AKW `1_drafts/2_knowledges/preferences/`.
  - `barebone-agent prefs promote <slug>` moves the pending pref to active **and** deletes the AKW draft at `1_drafts/2_knowledges/preferences/<slug>.md` (Q8 resolution). Verify the AKW draft is gone via `mcp_akw__memory_read` (should 404) and the new active copy lands at AKW `2_knowledges/preferences/<slug>.md` on the next pusher cycle.
  - Manual `save as preference` keyword in a Discord conv writes directly to active pool. Pusher mirrors to AKW.
  - Editing `agents/_preferences/<slug>.md` locally → next pusher cycle re-pushes only that file (manifest diff isolates).
  - Deleting a local pref → AKW copy persists.
  - `barebone-agent akw status` shows accurate dirty/clean counts across all watched dirs.
- **No-AKW smoke run** (Decision K): start an agent with the AKW MCP disabled in `agent.yml`. Seed `agents/_preferences/` with one global + one scoped pref. Run the same task and a CLI conversation. Verify:
  - Boot logs `AKW MCP not configured — running in local-only mode, push disabled` once. No per-task warnings.
  - Pusher task is **not** spawned.
  - All artifact types still produced locally:
    - Research drafts in `data/drafts/2_researches/`.
    - Session drafts in `data/drafts/sessions/`.
    - Pending preference (5th run) in `data/drafts/2_knowledges/preferences/`.
    - Manual save in `agents/_preferences/`.
  - System prompt includes local prefs scoped correctly; `## Project Context` and `## Prior Work` blocks omitted.
  - Recurring run sees previous-run result.
  - `prefs promote` works against the local pending file.
  - No AKW network attempts in any logs.
  - Restart the agent with AKW now configured — first catch-up cycle uploads everything that accumulated locally during the no-AKW window.

## Out of Scope
- **Curator agent / auto-promote drafts → `2_knowledges/`.** Drafts stay drafts; promotion is manual via the existing curator workflow. Separate EP if the draft volume becomes overwhelming.
- **Cross-agent preference sharing.** Each agent maintains its own `agents/_preferences/`. Sharing happens by deliberate import via `prefs pull <slug>`. Auto-pull (e.g. "every hour, also pull anything new from AKW into the active pool") is not in scope — it would re-introduce AKW as part of the hot-path read model, which is exactly what local-first avoids.
- **Curator agent / auto-promote pending pool.** `prefs promote` is manual. A separate EP could add a curator agent that reviews `data/drafts/2_knowledges/preferences/` and proposes promotions, but that's a different concern.
- **AKW-side authoritative edits.** The pusher's last-push-wins policy means direct AKW edits are unsafe between agent pushes. If you need to manage preferences AKW-side as the authority (e.g. centrally administered policy), that's a different model — out of scope.
- **Bidirectional manifest reconciliation.** Manifest only tracks "what we've pushed." There's no mechanism to detect "AKW has a file we don't know about." Adding one would re-introduce AKW reads on the agent side. Use `prefs pull` explicitly when needed.
- **Intercepting LLM-driven `memory_create` calls.** The agent has `mcp_akw__memory_create` available as a tool; an LLM that decides to call it directly bypasses the local-first pipeline (writes go straight to AKW, no local copy, no manifest entry). v1 documents this caveat and updates skill prompts to discourage direct `memory_create` for the artifact types this EP covers. A future EP can wrap the tool to redirect writes locally + push, but that's invasive (touches the tool registry).
- **Pushing skills/roles edits back.** Local edits to `agents/_skills/` and `agents/_roles/` (pulled via EP-00014) are not auto-pushed. Skills/roles are imported curated content; pushing edits would need a clear write-authorization model. Out of scope here.
- **Mirroring SQLite tasks to AKW.** Tasks live in SQLite and don't auto-produce a markdown unless `persist_as_draft: true`. A future EP could add a "task archive" mode that exports completed tasks as markdown into `data/drafts/tasks/` for backup. Not in v1.
- **Preference expiry / staleness detection.** No timestamps-based pruning. Manual cleanup or curator-driven.
- **Vector / semantic search.** Stays on AKW's BM25. The query keywords from the task title/description are usually good enough; if not, that's a curator-side reindexing concern.
- **Reflection-driven schedule tuning.** "I notice this hourly task always returns the same answer; lower the cadence" — not in scope. Reflection only writes preferences, never modifies tasks.
- **Per-turn richer AKW logging** (replacing 500-char-bookend `group_log` with full tool-call traces). Independent EP if needed.

## Open Questions
- **Q1.** Should the prior-work search be skipped on Discord chitchat (turns under, say, 30 chars)? Cheap heuristic to avoid useless searches. Default: run always; add length gate if it proves noisy in practice.
- **Q2.** Reflection LLM model choice — fixed haiku, or follow the agent's configured model? Haiku is cheap and predictable; agent-configured matches existing patterns. Lean haiku for cost.
- **Q3.** Should manual keyword trigger require explicit confirmation ("Save preference: ...? (y/n)")? Default: no, just save and tell the user where (Decision H). Confirmation adds friction.
- **Q4.** When a recurring task's previous result is itself an error/empty, do we still inject it under `## Previous Run Result`? Default: skip when result is empty or starts with the known failure prefixes.
- **Q5.** Push conflict policy when the AKW copy was edited directly between two pushes from this agent. Default: last-push-wins (clobber). Alternative: pre-push HEAD check (skip + log if remote is newer than local manifest entry). Default keeps the pusher simple; the alternative adds a per-file extra read to AKW which doubles the round trips. Lean default for v1.
- **Q6.** Local deletion → AKW propagation. Default: never propagate (Decision A2). A user who wants to remove a pref from AKW does so via direct AKW tooling. Should a future EP add a `prefs remove --remote <slug>` verb, or is "go fix it on AKW directly" enough? Probably defer until usage pressure exists.
- **Q7.** Push manifest location. Default: `data/.akw_push_manifest.json`. Should this be committed to git? Treating it as a build artifact (gitignored) means a fresh checkout will re-push everything on first cycle — safe but wastes round-trips when AKW already has copies. Treating it as committed means agents share manifest state, which is wrong if multiple agents push from different machines. Default: gitignored. Document that fresh checkouts will catch up on first cycle.

- **Q8 (RESOLVED — `prefs promote` deletes the AKW draft).** Promotion issues `mcp_akw__memory_delete` on `1_drafts/2_knowledges/preferences/<slug>.md` as part of the move. This is a justified exception to the "pusher owns AKW writes" rule — promotion is a user-initiated administrative action, parallel to `prefs pull` (which already reads from AKW directly). AKW unavailable at promote time: log `warn!`, accept the orphan; not worth a retry queue.

- **Q9 (RESOLVED — keep `agent_task` scope removed).** Decision F omits the scope. Rationale: mixing 5 unrelated one-time tasks produces mostly null patterns. Re-add later if real signal emerges.

- **Q10 (RESOLVED — fresh-start counter migration).** Phase 5 migration creates the table empty. Counters start at zero on first post-deploy event; users with prior history don't trigger a near-empty reflection on their first segment.

## Implementation Status
- [ ] Phase 1: plumbing fixes (`recommended_context` injection)
- [ ] Phase 2: generic AKW pusher + preference local pool + per-task selection
- [ ] Phase 3: prior-work search + previous-run threading
- [ ] Phase 4: research draft + session draft persistence (both local-first)
- [ ] Phase 5: counter table + reflection loop (local write only)
- [ ] Phase 6: user keyword triggers (write to active pool)
- [ ] Phase 7: docs + config
- [ ] Phase 8: live verification (AKW-present + no-AKW, all artifact types)

## Status: READY
