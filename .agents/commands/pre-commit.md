Run pre-commit checks before committing changes.

Steps to perform:

1. Detect which parts of the codebase have changes:
   - Python (`src/`, `tests/`): run Python checks
   - Both: run all checks
2. **Python checks** (if `src/` or `tests/` changed):
   - Type check: `uv run python -m py_compile` on changed `.py` files, or `uv run mypy src/` if mypy is available
   - Tests: `uv run pytest tests/ -v`
3. Run file size scan for files over 800 lines using the find command with wc -l, filtering for files >= 800 lines
4. Generate a results table with: check name, status (pass/fail), details
5. Run the pre-commit documentation checklist — verify these files are up to date with current changes: `CLAUDE.md`, `docs/project_overview.md`
6. Check for completed EPs that haven't been archived: scan `docs/` (excluding `archived/`) for files containing `## Status: DONE` — flag them with "Run `/archive-ep` to move completed plans to archive"
7. Report any docs that appear stale or missing updates
