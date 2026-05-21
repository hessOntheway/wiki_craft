# Source Layout

- `main.rs`: CLI parsing and command dispatch.
- `config.rs`: TOML config, defaults, and DeepSeek/env resolution.
- `knowledge.rs`: root schema, initialization, and approved knowledge reading.
- `sources.rs`: source manifest, normalized text hashing, and changed/unchanged detection.
- `candidates.rs`: staged candidate directories, diffs, listing, and approval.
- `runtime.rs`: ingest orchestration, service loop, agent loop, and status snapshots.
- `compact.rs`: context compaction and tool-call boundary cleanup.
- `metrics.rs`: structured metrics snapshots, JSONL events, and Prometheus text rendering.
- `audit.rs`: lightweight JSONL audit events for LLM exchanges, tool calls, tool results, and compaction.
- `llm/`: OpenAI-compatible client, session snapshots, prompt cache, usage telemetry.
- `tools/`: bounded tools available to the runtime.

The implementation keeps the first version intentionally small: one configured URL equals one source; generated candidate knowledge is a single `Home.md`.
