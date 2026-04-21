# EP-00003 — LLM Client Layer

## Problem / Pain Points
- Agents need to talk to LLMs across multiple providers (Anthropic, Google, OpenAI-compatible)
- Each provider has a different API format for messages, tools, and responses
- Need a unified interface so the agent loop doesn't care which provider is behind a model
- Fallback chains must try models in order on failure without crashing
- Context window management needed to avoid exceeding token limits
- DeepSeek models return `<think>...</think>` blocks that should be stripped

## Suggested Solution

### Phase 1: Core types + LLM trait
- `TokenUsage`, `ToolCall`, `LLMMessage`, `LLMResponse` structs (spec section 4.1)
- `LLMClient` trait with async `chat(messages, system, tools) -> Result<LLMResponse>`
- `estimate_tokens(text) -> u32` — heuristic `len / 4`
- `truncate_history(messages, system, context_window, max_tokens)` — trim oldest, keep latest user message

### Phase 2: OpenAI-compatible client
- Covers: NVIDIA NIM, OpenRouter, OpenAI, Groq, Ollama
- `reqwest` HTTP client, connection-pooled
- `POST {base_url}/chat/completions` with Bearer auth
- Message conversion: system/user/assistant/tool → OpenAI JSON format
- Tool definitions in OpenAI function-calling format
- Response parsing: content, tool_calls (JSON string args → parse), tokens, stop_reason
- Strip `<think>...</think>` blocks from response content
- Error handling: non-200 → error with status + first 500 chars of body

### Phase 3: Anthropic client
- REST client against `https://api.anthropic.com/v1/messages`
- System prompt as separate `system` parameter (not in messages array)
- Assistant messages as content blocks array (text + tool_use)
- Tool results as user messages with `tool_result` content blocks
- Tool definitions converted to `input_schema` format
- Response parsing: iterate content blocks, concatenate text, extract tool_use
- Tool call arguments are already dicts (not JSON strings like OpenAI)

### Phase 4: Google Gemini client
- REST client against `https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`
- System as `system_instruction` config parameter
- Message roles: "user" / "model" (not "assistant")
- Tool results as `function_response` parts
- Strip `default` from tool parameter properties (Gemini rejects it)
- Synthetic tool IDs: `call_{name}_{hash}` (Gemini doesn't generate IDs)
- Response parsing: iterate candidate parts for text + function_call

### Phase 5: Client pool + fallback chain
- `LLMClientPool` — initialized per agent at startup
- For each model in registry: resolve API key from merged env, create client by provider
- Skip models with missing API keys (log warning)
- `chat_with_fallback(chain, messages, system, tools)` — try each model in order
- On success: set `response.model` to the model ID that succeeded
- All fail: return error (never crash)

### Key decisions
- **reqwest** for HTTP (async, connection pooling) — all 3 providers use REST, no SDKs
- **Trait-based** `LLMClient` so providers are swappable
- No streaming (spec section 4.7) — single request/response
- Token estimation is `len/4` heuristic — no tokenizer crate
- Tool definitions stored internally in OpenAI format, converted per-provider at call time

## Implementation Status
- [x] Phase 1: Core types + LLMClient trait + token estimation + truncation + unit tests (8 tests)
- [x] Phase 2: OpenAI-compatible client + message/tool conversion + think-tag stripping + unit tests (10 tests)
- [x] Phase 3: Anthropic client + content block handling + unit tests (7 tests, no live test — no active API key)
- [x] Phase 4: Google Gemini client + synthetic IDs + schema stripping + unit tests (11 tests)
- [x] Phase 5: Client pool + fallback chain + unit tests (5 tests, live test deferred to EP-00005)

## Status: DONE
