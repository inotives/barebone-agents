# Scratch Notes

Ideas and deferred features for future EPs.

---

## Task Verification System

**Problem:** Delegation and heartbeat task execution return results based on LLM judgment. For deterministic tasks (coding, data transforms, etc.), we need verifiable outcomes — "tests pass", "compiles", "output matches expected".

**Proposed approach:** Optional `verify` parameter on delegate and task execution:

```
delegate(
  task: "Write a function that parses CSV files",
  role: "coder",
  verify: {
    command: "cargo test test_csv_parser",
    expect_exit_code: 0,
    max_retries: 3
  }
)
```

Flow:
1. Sub-agent does the work
2. Runs verify command
3. If fail → reads error, attempts fix, re-verifies
4. Loops up to max_retries
5. Returns result with pass/fail status

Can apply at two levels:
- **delegate verify** — inline during conversation
- **task verify** — during heartbeat execution, task not marked `done` until verification passes

---

## Deployment Config

**Problem:** Binary currently expects to run from the project directory. For production deployment, need configurable root path.

**Proposed approach:** `--root-dir` CLI flag + `BAREBONE_HOME` env var, defaulting to current directory.

---

## RSS Fetch Tool

**Problem:** RSS/Atom feeds are a good information source but currently require `api_request` + LLM XML parsing.

**Proposed approach:** Built-in `rss_fetch` tool using `feed-rs` crate. Parses RSS 1.0/2.0/Atom/JSON Feed into clean text (title, link, date, summary per entry). Saves tokens vs raw XML.

---

## Per-Request Token Override

**Problem:** `max_tokens` is fixed per model in `models.yml`. Some tasks need shorter responses (simple Q&A) or longer (code generation).

**Proposed approach:** Optional `max_tokens` override parameter on `LLMClient::chat()`, allowing the agent loop or tools to adjust dynamically per request.
