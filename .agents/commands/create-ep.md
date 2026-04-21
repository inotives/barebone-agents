Create a new Execution Plan (EP) for any code changes

Arguments: $ARGUMENTS (topic description)

Steps:
1. Find the next EP number by checking existing files in `docs/` and `docs/archived/` for the highest `EP-XXXXX` number, then increment by 1
2. Use today's date in YYYYMMDD format
3. Convert the topic to kebab-case for the filename
4. Create the file at: `docs/EP-XXXXX_<YYYYMMDD>_<topic>.md`

Use this template:
```markdown
# EP-XXXXX — <Title>

## Problem / Pain Points
- (describe the issue or motivation)

## Suggested Solution
- (proposed approach, file changes, architecture decisions)

## Implementation Status
- [ ] Step 1
- [ ] Step 2
- [ ] Step 3

## Status: IN PROGRESS
```

Fill in the Problem and Suggested Solution sections based on the topic provided. Ask the user to review and approve the plan before starting implementation.
