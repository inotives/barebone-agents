Archive completed Execution Plans.

Steps:
1. Scan `docs/` for EP files (`EP-*.md`), excluding `archived/`
2. Check each EP for `## Status: DONE` in the content
3. For each DONE EP:
   - Create `docs/archived/` directory if it doesn't exist
   - Move the file: `docs/EP-XXXXX_*.md` → `docs/archived/EP-XXXXX_*.md`
   - Report: "Archived EP-XXXXX — <title>"
4. If nothing to archive, report "No completed EPs to archive"
5. Show summary: number of EPs archived, remaining active EPs in `docs/`
