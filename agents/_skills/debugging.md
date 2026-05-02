---
name: debugging
keywords:
  - bug
  - debug
  - debugging
  - error
  - failure
  - failing
  - panic
  - crash
  - stack
  - trace
  - traceback
  - exception
  - broken
  - fix
description: Root-cause-driven debugging workflow with reproduction, isolation, and verification
---

# Debugging

When the user reports a bug or asks you to debug something, follow this loop. The goal is to find the root cause, not the smallest patch that makes the symptom go away.

## 1. Reproduce

Before reading code, get a reliable repro. If the user gave you a stack trace, the repro is the input that produced it. If they gave you a vague "it's broken", ask for:
- The exact command / request that fails.
- The full error message and stack trace, not a paraphrase.
- The expected behaviour vs actual.
- When it last worked, if known.

If you can run the repro yourself, do so. Confirmed-by-me beats reported-by-user every time.

## 2. Read the trace top-down

Start at the top frame (closest to the failure). Note the file, line, and the exception type. Don't jump into the code yet — predict what state would have produced this error, then go check.

## 3. Bisect, don't speculate

Form one hypothesis, test it, move on. Common bisection moves:
- **Where:** `git log` / `git blame` on the failing line — when did this code last change?
- **What:** add a log/print at the boundary you suspect — does the bad value appear there?
- **When:** does the bug reproduce on a clean checkout of the prior commit? `git bisect` if the regression range is wide.

Avoid the trap of reading code top-to-bottom hoping the bug jumps out. It won't.

## 4. Find the root, not a symptom

Once a hypothesis matches the evidence, ask: *why does this happen, not just what fails*. A null pointer is a symptom; the question is which contract was violated upstream that allowed a null to get there.

## 5. Fix, then verify

- Fix the root cause. If you only have time for a workaround, leave a comment with `WORKAROUND:` and a link to a follow-up issue.
- Add a regression test that reproduces the original failure and passes after the fix. A bug without a test will return.
- Re-run the original repro — does the symptom actually disappear?
- Run adjacent tests — did the fix break anything close by?

## 6. Report

- **Root cause:** one paragraph. What was wrong, why it produced this symptom.
- **Fix:** one paragraph. What changed and why.
- **Verification:** what you ran to confirm. Test name, command output, screenshot — whatever the change requires.
- **Follow-ups:** anything you noticed but didn't fix, with the reasoning for why now isn't the right time.

## Anti-patterns

- "Add a try/except" without understanding what's being caught.
- Adding nullability checks until the crash stops, without asking why null got there.
- "Bumped the timeout" without measuring why the call is slow.
- Reverting a regression without root-causing it. The next person to make that change will hit the same bug.
