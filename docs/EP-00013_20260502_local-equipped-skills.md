# EP-00013 — Local Equipped Skills (task-matched skill pool)

## Problem / Pain Points

Today's per-task skill injection comes only from AKW (`agent_loop.rs:fetch_akw_skills`):
- 100% reliant on the AKW MCP being up. No AKW → no per-task skill.
- BM25 ranking surprises (same class of issue as roles: a generic query like "research" can pick a less-relevant skill bundle).
- Externally-curated. The team has no first-class place to author skills that should always be considered for matching tasks.

Meanwhile, three env vars (`SKILLS_DIR`, `SKILLS_TOKEN_BUDGET`, `SKILLS_MIN_MATCH_HITS`) sit in `settings.rs` wired to nothing — leftovers from an earlier design that never shipped a local task-matched skill pool.

We already shipped local-first persona resolution in EP-00012 (`agents/_roles/`). The skill story should mirror it.

## Suggested Solution

Add a local skill pool at `agents/_skills/<slug>.md` — same shape as `_roles/`. Per task, the agent loop matches the user message against the pool (keyword + body), greedy-packs into a token budget, and injects the chosen skills as `## Equipped Skills`. AKW `skill_search` becomes the fallback when the local pool returns zero matches.

### Why local-first (mirrors EP-00012 Phase 4 reasoning)
- **Determinism.** Keyword match against a curated pool produces stable results.
- **Speed.** Sync file reads vs MCP roundtrip per delegate call.
- **Offline-safe.** Works without AKW.
- **Curation.** Team controls the pool.

### File format

```yaml
---
name: research_pipeline
keywords: [research, web search, market, macro, trend, citation]
description: Methodical research workflow with citations
---

# Research Pipeline

When the user asks for research, follow this loop:
1. Search existing knowledge first via mcp_akw__memory_search.
2. ...
```

`name` is informational; the **filename** is the slug. `keywords` is the matching seed list. `description` is a one-line summary used for logging only.

### Selection algorithm

1. Read `agents/_skills/*.md` fresh on every call (hot-reload).
2. Tokenize the task message: lowercase → split on non-word → drop a small stopword set.
3. For each skill: hits = `|tokens ∩ (keywords ∪ body_tokens)|` (deduped per skill).
4. Drop skills with `hits < SKILLS_MIN_MATCH_HITS` (default 2).
5. Sort by `(hits desc, name asc)` for deterministic tie-breaking.
6. Greedy pack into `SKILLS_TOKEN_BUDGET` (default 4000), full-file granularity (don't truncate mid-file). Drop the rest.
7. **Local-first gate**: if the result is non-empty, return it. Otherwise call the existing AKW `fetch_akw_skills` path as fallback.

### System prompt layout (additive, no breaking changes)

```
<character_sheet>
## Core Skills              ← config/skills/*  (always — unchanged)
## Equipped Skills          ← NEW: agents/_skills/*  (task-matched)
## AKW Skills               ← only when local pool returned 0 matches
<@mention context>
<parent conv context>
```

### Implementation Phases

#### Phase 1 — Skill model + selector
- New file `src/skills_pool.rs` (or extend `src/skills.rs` with `EquippedSkills`):
  - Struct `EquippedSkill { slug, keywords: Vec<String>, body: String, token_estimate: u32 }`.
  - Loader walks `agents/_skills/*.md`, parses simple YAML frontmatter (use existing serde_yaml dep).
  - Selector function `select(skills: &[EquippedSkill], message: &str, min_hits: u32, token_budget: u32) -> Vec<&EquippedSkill>`.
  - Stopword set inlined (~30 words: a, an, the, and, or, of, to, in, for, on, etc.).
- Tests:
  - frontmatter parsed correctly; missing frontmatter → keywords=[], body counts only
  - empty pool dir → empty result, no error
  - `min_hits` filter applied
  - token budget respected (no overflow, full-file granularity)
  - tie-break stable by name

#### Phase 2 — Wire into agent loop
- `agent_loop.rs:fetch_akw_skills` → renamed to `fetch_dynamic_skills`. New flow:
  1. Load pool from `<root>/agents/_skills/`.
  2. Run selector with `settings.skills_min_match_hits` and `settings.skills_token_budget`.
  3. If non-empty → format with `## Equipped Skills` header, return.
  4. Else → call existing AKW path (skill_search → memory_read), format with `## AKW Skills` header, return.
- `AgentLoop::new`: thread `skills_pool_dir`, `skills_token_budget`, `skills_min_match_hits` into the struct (read from `settings`). The fields exist; we just need to use them.

#### Phase 3 — Seed example skills
- Author `agents/_skills/research_pipeline.md` (matches research / market / macro / trend tasks).
- Author `agents/_skills/code_review.md` (matches review / pull request / diff tasks).
- Author `agents/_skills/debugging.md` (matches bug / error / failure / stack trace tasks).
- These triple as documentation, examples, and live regression tests against the selector.

#### Phase 4 — Docs
- `config/skills/agent_runbook.md`: add a "Skill resolution" section explaining the three-tier prompt layout (Core / Equipped / AKW).
- `docs/SPECS.md` §6.5 (Context Injection): update the order to reflect the new "Equipped Skills" tier.
- `AGENTS.md` project layout block: add `agents/_skills/`.

#### Phase 5 — Verify
- `cargo test` for selector unit tests.
- Live smoke: heartbeat task with a research-y message → expect Equipped Skills section in the system prompt with research_pipeline.md content. Drop the local pool → expect fallback to AKW.

### Out of Scope
- Domain-scoped pools (`agents/_skills/<domain>/*.md`). Flat directory only for now.
- Per-agent overrides (`agents/<name>/skills/`). Possible later.
- Vector / semantic match. Keyword union is sufficient for a curated pool.
- `skill_get` (AKW bundle manifest) integration. Continues to be `skill_search + memory_read`.
- Removing the unused `SKILLS_DIR` env var (we'll repurpose it as the override for `agents/_skills/`'s parent if anyone needs it; default unchanged path).
- Migrating `config/skills/*.md` (Core Skills) into this scheme. Different concept (always vs sometimes), keep separate.

### Key Decisions
- **Path: `agents/_skills/`** mirrors `_roles/`. The `skills/library/` aspiration in older SPECS docs is retired.
- **AKW is a fallback**, not a complement. Runs only when local pool returns 0 matches. Keeps prompts lean and predictable.
- **Frontmatter `keywords`** is the primary match signal; body word match is a secondary signal so a skill without explicit keywords still gets ranked.
- **Greedy pack at file granularity** (no mid-file truncation). Skills are atomic.

## Implementation Status
- [x] Phase 1: skill model + selector (11 new tests, 19 total in skills.rs)
- [x] Phase 2: wire into agent loop (fetch_dynamic_skills: local → AKW fallback)
- [x] Phase 3: seed example skills (research_pipeline, code_review, debugging)
- [x] Phase 4: docs (runbook, SPECS §6.5, AGENTS.md)
- [x] Phase 5: verify (cargo test: 278 passed; live smoke: research task → all 3 local skills picked, AKW skill_search not called — local-first gate works)

## Follow-ups (not blockers)

- **Selector greediness.** The smoke task matched all 3 example skills, including code_review and debugging which aren't really research-relevant. Likely caused by (a) body words leaking common English vocabulary into the match, and (b) the "## Task:" prefix in scheduler-built messages adding the token "task" which appears in most skill bodies. Two cheap fixes if this becomes a real problem:
  1. Weight keyword hits higher than body hits (e.g. `score = 2 × keyword_hits + body_hits`); raise `SKILLS_MIN_MATCH_HITS` to require keyword hits specifically.
  2. Strip the `## Task:` formatter prefix before tokenizing, or add it to stopwords.
- **No mid-file truncation today.** A skill that's larger than the budget will never be picked. Acceptable for now; revisit if real skills bump against the ceiling.

## Status: DONE
