# Wiki Craft

Wiki Craft is a Markdown-first knowledge-base maintenance agent. It periodically fetches configured URL sources, detects content changes, asks an LLM to summarize and update a candidate wiki, then waits for a human to review the diff and approve the update.

The first version intentionally avoids MCP, vector search, UI approval flows, and multi-page crawling. Claude Code, Codex, and similar tools read approved knowledge directly from `.wiki_craft/knowledge/current/` plus `WIKI_CRAFT.md`.

## Quick Start

```bash
cargo run -- init
```

Edit `wiki_craft.toml`, enable a source URL, and set a model key. Environment
variable names follow `scribe_engine`: `LLM_API_KEY`, `LLM_BASE_URL`, `LLM_MODEL`.

```bash
export LLM_API_KEY="..."
cargo run -- ingest --once
cargo run -- candidates list
cargo run -- candidates diff <run_id>
cargo run -- candidates approve <run_id>
cargo run -- status
cargo run -- metrics --prometheus
```

Run continuously:

```bash
cargo run -- serve
```

`serve` runs one ingest immediately, then sleeps for `schedule.interval_minutes`.
When metrics are enabled, it also exposes:

- `http://127.0.0.1:9898/metrics`
- `http://127.0.0.1:9898/metrics.json`

## Runtime Layout

- `.wiki_craft/sources/manifest.json`: source URL metadata, content hashes, and latest run ids.
- `.wiki_craft/source_summaries/current/`: approved LLM summaries for source URLs.
- `.wiki_craft/knowledge/current/`: approved Markdown wiki used by coding agents.
- `.wiki_craft/candidates/{run_id}/`: staged summaries, candidate `Home.md`, `diff.md`, and metadata.
- `.wiki_craft/sessions/`: persisted non-raw-source LLM sessions.
- `.wiki_craft/transcripts/`: pre-compaction transcript backups.
- `.wiki_craft/prompt_cache/`: local model response cache keyed by request hash.
- `.wiki_craft/audit/events.jsonl`: lightweight LLM/tool/compaction audit trail.
- `.wiki_craft/metrics/latest.json`: latest structured metrics snapshot.
- `.wiki_craft/metrics/events.jsonl`: append-only metrics snapshots for later analysis.

## Architecture

```text
URL sources
  -> tools::web_fetch
  -> sources manifest + hash detection
  -> LLM source summaries
  -> candidate Home.md
  -> candidate diff
  -> human approve
  -> approved knowledge/current
```

The code follows two reference ideas:

- From `scribe_engine`: agent loop, session snapshots, context compaction, prompt cache, usage telemetry, and bounded web fetching.
- From `claw_code`: typed boundaries, structured status surfaces, recoverable snapshots, and bounded context reads.

## Development Notes

- v1 stores links, version metadata, and summaries, not raw source documents.
- Fetched source text is treated as untrusted evidence and should never become an instruction.
- Candidate knowledge is not authoritative until approved.
- `WIKI_CRAFT.md` is the project memory/schema file that future AI sessions should read first.
- Cache/token telemetry is written as structured metrics first; logs are only a convenience surface.
