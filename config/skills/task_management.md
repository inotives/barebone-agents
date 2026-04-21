## Task Management

> Foundation skill — applies to ALL tasks you create or receive.

### Creating a Task

Every task you create must include these in the description:

1. **Why** — what triggered this task, what problem it solves
2. **What** — clear deliverable, acceptance criteria, expected output format
3. **Follow-up** — what happens after completion (who reads it, what decision it informs)
4. **Dependencies** — does this need something first? Does it block something else?

```
task_create(
  title="Research auth options for the API",
  description="WHY: Need to decide auth strategy before implementing endpoints.\nWHAT: Compare JWT, OAuth2, API keys. Output a summary with pros/cons and recommendation.\nFOLLOW-UP: Boss will review and decide approach.\nDEPENDS ON: None.",
  priority="high"
)
```

**For role-based assignment** (multi-agent mode):
```
task_create(
  title="Implement JWT middleware",
  agent_class="coder",
  depends_on=["MIS-00001-T00001"]
)
```
This creates the task as `status="blocked"` (has dependencies) and any agent with the matching role will claim it when unblocked.

### Receiving a Task

When you pick up a task:
1. Read the full description — understand the WHY, not just the WHAT
2. Check dependencies — are all `depends_on` tasks done?
3. Update status: `task_update(key="<key>", status="in_progress")`
4. Deliver in the format requested
5. Complete: `task_update(key="<key>", status="done", result="<summary of outcome>")`

### Following Up on Delegated Tasks

If you created a task for another agent, YOU own the follow-up:

**Periodic check:**
```
task_list(status="done", agent_name="all")
```

For each completed task:
1. Read the result via `task_get(key="<key>")`
2. Act on findings (adjust strategy, create follow-up task, report to boss)
3. If the result is insufficient → create a follow-up task with more specific requirements

### Task Lifecycle

```
create → todo/backlog → in_progress → done
               ↑                        ↓
               └──── blocked ←──── (if issues found)
```

- **backlog**: default for manual tasks
- **todo**: ready for execution (scheduled, role-assigned, or heartbeat pickup)
- **blocked**: waiting on `depends_on` tasks to complete
- **in_progress**: actively being worked on
- **done**: completed with result

### Priority Guide

| Priority | Response Time | Examples |
|----------|--------------|---------|
| **critical** | Immediate | System down, security issue, data loss |
| **high** | Within 1 hour | Time-sensitive research, urgent boss request |
| **medium** | Within 1 day | Feature work, routine analysis |
| **low** | When idle | Cleanup, optimization, nice-to-have research |

### Anti-patterns

- **Fire and forget**: creating a task and never checking the result
- **Vague tasks**: "look into auth" — no WHY, no deliverable, no follow-up
- **Duplicate tasks**: check `task_list()` before creating new ones
- **Over-delegation**: if you can do it in 2 minutes, just do it yourself
