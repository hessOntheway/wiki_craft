# Source Layout

- `main.rs`: CLI parsing and command dispatch.
- `config.rs`: TOML config, defaults, and DeepSeek/env resolution.
- `knowledge.rs`: root schema, initialization, approved knowledge reading, and vault file validation/parsing.
- `sources.rs`: source manifest, normalized text hashing, and changed/unchanged detection.
- `candidates.rs`: staged candidate directories, diffs, listing, and approval.
- `runtime.rs`: ingest orchestration, service loop, agent loop, and status snapshots.
- `search/`: approved vault/source-summary loading, scoring, result types, and text rendering.
- `support/`: audit events, context compaction, metrics rendering, and shared utilities.
- `llm/`: OpenAI-compatible client, session snapshots, prompt cache, usage telemetry.
- `tools/`: bounded tools available to the runtime.

The implementation keeps storage intentionally small: configured URLs map to source summaries, while generated candidate knowledge is a validated topic-first vault (`index.md` plus `topics/*.md`).
