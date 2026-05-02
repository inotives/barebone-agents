---
name: research_pipeline
keywords:
  - research
  - investigate
  - analyze
  - market
  - macro
  - trend
  - economy
  - report
  - findings
  - sources
  - citation
description: Methodical research workflow with citations and structured findings
---

# Research Pipeline

When the user asks you to research a topic, follow this loop. Don't skip steps even if the topic feels obvious — provenance is the value.

## 1. Frame the question

Before any tool calls, restate the question in one sentence and list the sub-questions you'll need to answer. Surface ambiguity early — ask the user to disambiguate scope, time window, or geography if any of those are unclear.

## 2. Check existing knowledge first

If the AKW MCP is available, call `mcp_akw__memory_search(query="<topic>")` before reaching for the open web. Past sessions, curated notes, and prior reports often answer the question or constrain the search.

## 3. Plan sources, then fetch

For each sub-question, name 1–3 source types you'll consult (official statistics, primary reporting, recognised analysts). Prefer primary over secondary; prefer dated and attributable over anonymous summaries. Use `web_search` to find candidates, then `web_fetch` to read the substantive ones — search snippets are not findings.

## 4. Triangulate

Don't synthesize from a single source. Cross-check key numbers and claims across at least two independent sources. Flag any number that has only one source as "single-source" in the final report.

## 5. Draft findings, structured

Use this skeleton:

```
# <Topic>

## TL;DR
<3–5 bullets, the answer if the reader stops here>

## Key findings
1. <Finding> — <one-sentence rationale> [<source ref>]
2. ...

## What's contested or unclear
- <Disagreement between sources, single-source claims, gaps>

## Sources
- [Title](url) — <date> — <one-line description>
```

## 6. Save provenance

If AKW is available, write the draft via `mcp_akw__memory_create(path="1_drafts/2_knowledges/<topic>.md", ...)` so future tasks can find it. Otherwise save under `workspace/research/`.

## Rules

- **Cite every non-obvious claim.** Numbers, dates, named events, attributed opinions all need a source ref.
- **Date everything.** "Recent" is meaningless — always include the time window.
- **Distinguish data from interpretation.** A chart shows a number; a recession is an interpretation.
- **Stop when you have enough, not when you run out of time.** Three triangulated findings beat ten single-source claims.
