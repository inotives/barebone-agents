---
keywords: [rust, backend, api, server, tokio, async, error]
scope: backend-rust
summary: Rust backend coding style preferences.
---

When writing Rust backend code:
- Prefer `Result<T, String>` for errors at API boundaries; use `thiserror` only when error variants are public.
- Avoid `unwrap()` outside tests.
- Keep async functions small; extract pure helpers for testability.
