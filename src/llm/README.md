# LLM Layer

This directory contains the model boundary.

- `openai.rs`: DeepSeek/OpenAI-compatible chat completions, local request-hash cache, and lightweight audit events.
- `cache.rs`: local prompt cache entries under `.wiki_craft/prompt_cache/`.
- `usage.rs`: token and cache-hit telemetry, including provider cache tokens and local cache hits.
- `session.rs`: persisted conversation snapshots for non-raw-source wiki generation sessions.

Source summarization sends fetched text to the model but does not persist raw source text in session files.
Runtime metrics derived from this layer are written to `.wiki_craft/metrics/latest.json`,
`.wiki_craft/metrics/events.jsonl`, and can be rendered with `wiki_craft metrics --prometheus`.

Environment variable names follow `scribe_engine`: `LLM_API_KEY`, `LLM_BASE_URL`, and `LLM_MODEL`.
