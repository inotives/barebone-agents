---
name: code_review
keywords:
  - review
  - reviewing
  - diff
  - patch
  - pull
  - request
  - pr
  - mr
  - merge
  - changes
  - changeset
description: Pull request review checklist focused on correctness, scope, and maintainability
---

# Code Review

When asked to review code, a diff, or a pull request, work through this checklist in order. Skip a section only if it's clearly N/A — and say so out loud.

## 1. Understand the intent

Before reading code, read the description. State what the change is supposed to do in one sentence. If the description is missing or vague, that's the first comment to leave.

## 2. Scope

- Does the diff match the description? Flag unrelated changes ("drive-by refactor", formatting churn, unrelated rename).
- Is anything obviously missing — tests, doc updates, migration steps, feature-flag cleanup?

## 3. Correctness

For each non-trivial chunk:
- Trace one happy path mentally — does it produce the expected output?
- Trace one failure path — what happens on bad input, network error, empty list, nil pointer, race?
- Check off-by-one / boundary conditions explicitly.
- Look for silent error swallowing (`unwrap_or_default`, `catch {}`, ignored Results).

## 4. Tests

- New behaviour → new test? If not, why not?
- Tests assert observable behaviour, not implementation detail.
- No mocks where an integration test would catch the real bug.
- Failure modes covered, not only the happy path.

## 5. Maintainability

- Names: do they describe intent or implementation?
- Functions doing too much: count distinct concerns; if >2, suggest split.
- Comments: explain *why* a non-obvious choice was made; don't restate the code.
- Premature abstraction: is the second use case real, or speculative?

## 6. Security / safety

- Untrusted input crossing a boundary (HTTP body, env var, file content) — is it validated before use?
- Any new place that constructs SQL, shell commands, or paths from user input?
- Logging or error messages that could leak secrets (tokens, keys, PII)?

## 7. Observability

- New failure modes: is something logged when they happen?
- Errors carry enough context to debug from a single log line?

## How to write the review

- Lead with the verdict: **approve / approve-with-comments / request-changes / blocked**.
- Group comments by severity (blocker / nit / question / suggestion). Don't bury blockers in a wall of nits.
- Quote the line you're commenting on. Future readers shouldn't have to guess.
- Suggest a fix when the comment is a blocker. "This is wrong" without an alternative is half a review.
