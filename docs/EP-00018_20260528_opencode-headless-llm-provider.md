# EP-00018 — OpenCode Headless LLM Provider

## Problem / Pain Points
- The current LLM layer assumes providers are direct chat backends behind `LLMClient.chat(messages, system, tools)`.
- OpenCode headless is not just another OpenAI-compatible endpoint; it can run as a CLI command or as a headless server with its own session, agent, tool, and permission model.
- If integrated naively, OpenCode could become a nested agent runtime and conflict with barebone-agent's existing tool loop, message persistence, and fallback behavior.
- Need a clear provider boundary that lets OpenCode be used as a model option while keeping barebone-agent's tools and conversation DB authoritative.

## Suggested Solution
- Add an `opencode` provider to the model registry and LLM client pool.
- Implement OpenCode integration in two slices:
  - Slice 1: CLI spike using `opencode run --format json` to validate output shape, command lifecycle, timeout behavior, and final-text extraction.
  - Slice 2: durable provider using `opencode serve` and its HTTP session/message API.
- Keep barebone-agent as the tool owner:
  - OpenCode should return final assistant content or structured tool-call instructions compatible with `LLMResponse`.
  - The existing `AgentLoop` continues to execute tools through `ToolRegistry`.
  - Do not let OpenCode auto-execute filesystem/shell tools in the provider integration.
- Treat structured tool-call support as optional after the HTTP integration is proven:
  - Initial implementation may be text-only.
  - Add strict JSON parsing/validation only if OpenCode can reliably emit barebone-compatible tool calls.

## Implementation Status
- [ ] Phase 1 — Config and provider registration
  - Add `Provider::Opencode`.
  - Extend `ModelConfig` only if required for OpenCode-specific fields; prefer reusing `model`, `base_url`, and optional env vars first.
  - Update `config/models.yml`, `.env.template`, `docs/SPECS.md`, and `AGENTS.md` if config shape changes.

- [ ] Phase 2 — CLI spike
  - Add a small internal OpenCode runner that shells out to `opencode run --format json`.
  - Build prompt input from `system + messages`.
  - Parse final assistant text from JSON events.
  - Return `LLMResponse` with estimated token usage.
  - Explicitly mark tool calls unsupported in this slice.

- [ ] Phase 3 — Headless server client
  - Implement `OpencodeClient` using `opencode serve` HTTP APIs.
  - Support attach-to-existing server via `base_url`.
  - Create or reuse an OpenCode session per barebone conversation turn as needed.
  - Send messages through the session/message endpoint with selected OpenCode model/agent.
  - Parse returned message parts into `LLMResponse`.

- [ ] Phase 4 — Tool-call compatibility
  - Define a strict response contract for OpenCode-emitted tool calls.
  - Parse only validated JSON tool-call envelopes into `ToolCall`.
  - On invalid tool-call output, treat it as normal assistant text or return a provider error based on tests.
  - Keep tool execution in barebone-agent.

- [ ] Phase 5 — Verification and hardening
  - Unit-test provider parsing and pool registration.
  - Unit-test CLI JSON parsing and HTTP response parsing with fixtures.
  - Add failure tests for missing `opencode`, nonzero exit, timeout, invalid JSON, server auth failure, and unavailable server.
  - Run `cargo test`.
  - Smoke-test one configured OpenCode model in text-only mode.

## Key Decisions
- Prefer `opencode serve` for the real provider implementation.
- Use the CLI path as a feasibility spike, not the primary long-term design.
- Barebone-agent remains responsible for tools, fallback chains, DB persistence, truncation, and final response saving.
- No streaming in the first implementation; match the existing single-response `LLMClient` contract.
- Token usage can use the existing heuristic unless OpenCode exposes reliable usage metadata in the parsed response.

## Assumptions
- OpenCode is installed and configured outside this repo.
- OpenCode authentication and provider setup remain OpenCode-owned.
- The first shippable version may be text-only if structured tool calls are not reliable.

## Status: IN PROGRESS
