# EP-00017 — Session Mission Conversation Model

## Problem / Pain Points
- The current harness uses `conversation_id` as both the durable conversation/thread id and the active session key.
- Missions and tasks already exist, but their execution logs are not grouped under a durable session-level record.
- Scheduled task runs create independent task conversation ids, so mission-level history is scattered across task conversations.
- Session summaries are currently generated per active conversation segment, not across a mission/session that may contain many task conversations.
- This makes it harder to inspect a mission as one coherent work session with multiple task-level conversations.

## Suggested Solution
- Introduce a durable local session concept where one session can contain many conversations.
- Map mission-level work to sessions:
  - `session_id` represents the broader mission/work context.
  - `conversation_id` represents an individual chat thread or task execution log.
  - mission tasks write their task execution logs as conversations under the mission/session.
- Add schema support for session grouping, starting with a `session_id` on conversation rows and then, if needed, a first-class `sessions` table.
- Preserve current task and mission tables initially; do not collapse `missions` into `sessions` in the first implementation slice.
- Update session summaries in stages:
  - First slice: keep existing per-conversation summaries but record their session grouping.
  - Later slice: generate session-level summaries across all conversations in a session.
- Keep CLI/ad-hoc conversations working by assigning them generated local session ids.

## Implementation Status
- [x] Step 1 — Add `session_id` to conversation persistence and backfill existing rows.
- [x] Step 2 — Set task-run `session_id` from the task's `mission_key`, or a generated standalone-task session when no mission exists.
- [x] Step 3 — Update conversation listing/show commands to expose session grouping without breaking existing output.
- [x] Step 4 — Add a first-class `sessions` table if the initial column-based model proves too limited.
- [x] Step 5 — Update session draft generation to support mission/session-level summaries across many conversations.
- [x] Step 6 — Update README, `docs/SPECS.md`, and `AGENTS.md` project structure/domain wording.
- [x] Step 7 — Verify with schema tests, task execution tests, conversation command tests, `cargo test`, and one-shot smoke test.

## Status: DONE

## Implementation Notes

- `session_id` is a generated UUID, not a mission key.
- `session_name` is optional; mission task runs store the mission key there and reuse the first matching session UUID for later task conversations in that mission.
- CLI `/continue` starts a new conversation under the same session UUID; CLI `/new` starts a fresh session UUID.
- A first-class `sessions` table is deferred; the column model is sufficient for the current slice.
- Per-conversation session drafts now include `session_id` and optional `session_name` in frontmatter.
- Mission task completion refreshes a session-level summary at `data/drafts/sessions/by-session/<session_id>.md`, aggregating final user/assistant turns across conversations in that session.
