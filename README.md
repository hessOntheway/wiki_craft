# Wiki Craft

Wiki Craft is a Markdown-first knowledge-base maintenance agent. It fetches configured URL sources, detects content changes, asks an LLM to summarize the evidence, stages a candidate Obsidian-style vault, and waits for a human to review and approve the update.

The project now uses a topic-first vault instead of a single `Home.md` file. Approved knowledge is organized around durable concepts and workflows, while source summaries remain as an evidence layer.

## Current Design

Wiki Craft deliberately keeps the storage model simple:

- No vector database.
- No embedding pipeline.
- No raw source archive.
- Approved Markdown files are the retrieval surface.
- Candidate updates are staged and diffed before approval.

The core layout is:

```text
.wiki_craft/
  knowledge/
    current/
      index.md
      topics/
        *.md
  source_summaries/
    current/
      *.md
  candidates/
    {run_id}/
      knowledge/
        index.md
        topics/
          *.md
      source_summaries/
        *.md
      diff.md
      metadata.json
  sources/
    manifest.json
```

`knowledge/current/index.md` is the entry point. `knowledge/current/topics/*.md` contains the topic pages that coding agents should prefer. `source_summaries/current/*.md` preserves source-level evidence and version metadata.

## Quick Start

```bash
cargo run -- init
```

Edit `wiki_craft.toml`, enable source URLs, and set a model key. Environment variable names follow `scribe_engine`: `LLM_API_KEY`, `LLM_BASE_URL`, `LLM_MODEL`.

```bash
export LLM_API_KEY="..."
cargo run -- ingest --once
cargo run -- candidates list
cargo run -- candidates diff <run_id>
cargo run -- candidates approve <run_id>
cargo run -- search --query "what changed?" --top-k 5 --json
cargo run -- status
cargo run -- metrics --prometheus
```

Run continuously:

```bash
cargo run -- serve
```

`serve` runs one ingest immediately, then sleeps for `schedule.interval_minutes`. When metrics are enabled, it exposes:

- `http://127.0.0.1:9898/metrics`
- `http://127.0.0.1:9898/metrics.json`

## Vault Structure

Approved knowledge is an Obsidian-style vault:

```text
.wiki_craft/knowledge/current/index.md
.wiki_craft/knowledge/current/topics/search.md
.wiki_craft/knowledge/current/topics/agent-context.md
```

The index is a navigational page. It should link to important topic pages with wikilinks:

```markdown
# Wiki Craft Index

## Topics

- [[topics/search|Search]]
- [[topics/agent-context|Agent Context]]
```

Topic pages use YAML frontmatter:

```markdown
---
title: "Search"
aliases: [retrieval, lookup]
tags: [knowledge, cli]
source_ids: [abc123]
source_urls: [https://example.test/article]
version_hashes: [abcdef1234567890]
updated_at_run_id: "run_123"
---

# Search

Search is the read-only retrieval surface for approved knowledge.
```

The required frontmatter keys are:

- `title`: human-readable topic title.
- `aliases`: alternate names and query terms.
- `tags`: broad facets used by search.
- `source_ids`: source IDs from `.wiki_craft/sources/manifest.json`.
- `source_urls`: evidence URLs used for this topic.
- `version_hashes`: content hashes proving which source versions informed the page.
- `updated_at_run_id`: candidate run that last updated the page.

## Ingest Flow

The ingest pipeline is:

```text
configured URL sources
  -> bounded web fetch
  -> source manifest hash detection
  -> LLM source summaries
  -> candidate vault JSON
  -> candidate knowledge/index.md + knowledge/topics/*.md
  -> diff.md
  -> human approve
  -> approved vault replacement
```

Important behavior:

- Source summaries are still generated per changed source.
- Candidate knowledge is no longer `Home.md`.
- The LLM is asked to return JSON containing a list of vault files.
- Candidate vault files are validated before they are written.
- Valid candidate knowledge paths are only `index.md` and `topics/*.md`.
- Approval replaces `.wiki_craft/knowledge/current/` as a directory, so legacy `Home.md` disappears naturally once a vault candidate is approved.
- Candidate source summaries include the existing approved summaries plus changed summaries, so approval does not drop unchanged evidence files.

The candidate JSON shape expected from the LLM is:

```json
{
  "files": [
    {
      "path": "index.md",
      "content": "---\ntitle: \"Wiki Craft Index\"\n...\n---\n\n# Wiki Craft Index\n"
    },
    {
      "path": "topics/search.md",
      "content": "---\ntitle: \"Search\"\n...\n---\n\n# Search\n"
    }
  ]
}
```

The implementation also accepts a bare JSON array of file objects and strips surrounding Markdown code fences when present.

## Reorganizing Old Knowledge

Use this command to convert existing approved Markdown into a candidate vault:

```bash
cargo run -- knowledge reorganize
```

This command:

- Reads `.wiki_craft/knowledge/current/`.
- Splits existing Markdown conservatively by headings.
- Writes a candidate `index.md`.
- Writes candidate `topics/*.md` pages.
- Copies current source summaries into the candidate.
- Writes `diff.md`.
- Writes candidate metadata.
- Does not modify approved knowledge.

Review and approve it the same way as an ingest candidate:

```bash
cargo run -- candidates diff <run_id>
cargo run -- candidates approve <run_id>
```

This is the migration path from old single-file knowledge to the topic-first vault.

## Search

Search is a local, read-only retrieval command:

```bash
cargo run -- search --query "<question>" --top-k 5 --json
```

It reads only approved content:

- `.wiki_craft/knowledge/current/index.md`
- `.wiki_craft/knowledge/current/topics/*.md`
- `.wiki_craft/source_summaries/current/*.md`

It does not read staged candidates.

The JSON response is intentionally shaped around retrieval results:

```json
{
  "query": "retrieval",
  "top_k": 5,
  "results": [
    {
      "path": ".wiki_craft/knowledge/current/topics/search.md",
      "kind": "topic",
      "title": "Search",
      "heading": "Search",
      "score": 42.5,
      "line_start": 10,
      "line_end": 14,
      "snippet": "# Search\n...",
      "aliases": ["lookup"],
      "tags": ["knowledge"],
      "wikilinks": ["topics/index"],
      "source_ids": ["abc123"],
      "source_urls": ["https://example.test/article"],
      "version_hashes": ["abcdef1234567890"],
      "updated_at_run_id": "run_123"
    }
  ]
}
```

Search result kinds are:

- `index`
- `topic`
- `source_summary`

## Search Strategy

Search is structural rather than vector-based. It improves hit rate by using the vault shape:

- `title` has the strongest metadata weight.
- `aliases` catch alternate phrasing.
- `tags` catch broad categories.
- `wikilinks` catch related topic names.
- Markdown headings catch local section intent.
- Body text catches direct evidence and detailed wording.
- CJK characters are scored individually so Chinese queries can match even when token boundaries are ambiguous.
- Source IDs, source URLs, and version hashes are indexed as traceability fields.
- Topic pages receive a priority bonus so concept pages beat source summaries when relevance is comparable.

This keeps retrieval explainable. A result can always point to a Markdown path, line range, snippet, and evidence metadata.

## Approval Model

Candidates are not authoritative. Approval is explicit:

```bash
cargo run -- candidates list
cargo run -- candidates diff <run_id>
cargo run -- candidates approve <run_id>
```

Approval promotes:

- `candidates/{run_id}/knowledge/` to `knowledge/current/`
- `candidates/{run_id}/source_summaries/` to `source_summaries/current/`

Because approval replaces directories, candidates must be complete. Ingest and reorganize both build complete candidate knowledge and source-summary directories.

## Runtime Layout

- `.wiki_craft/sources/manifest.json`: source URL metadata, content hashes, and latest run IDs.
- `.wiki_craft/source_summaries/current/`: approved LLM summaries for source URLs.
- `.wiki_craft/knowledge/current/index.md`: approved vault entry point.
- `.wiki_craft/knowledge/current/topics/*.md`: approved topic-first pages.
- `.wiki_craft/candidates/{run_id}/`: staged summaries, candidate vault, `diff.md`, and metadata.
- `.wiki_craft/sessions/`: persisted non-raw-source LLM sessions.
- `.wiki_craft/transcripts/`: pre-compaction transcript backups.
- `.wiki_craft/prompt_cache/`: local model response cache keyed by request hash.
- `.wiki_craft/audit/events.jsonl`: lightweight LLM/tool/compaction audit trail.
- `.wiki_craft/metrics/latest.json`: latest structured metrics snapshot.
- `.wiki_craft/metrics/events.jsonl`: append-only metrics snapshots for later analysis.

## Implementation Notes

The vault implementation lives in `src/knowledge.rs`. It provides:

- JSON payload parsing for candidate vault files.
- Path validation for `index.md` and `topics/*.md`.
- Frontmatter parsing for search metadata.
- Wikilink extraction.
- A conservative heading-based reorganizer for old approved Markdown.

The ingest implementation in `src/runtime.rs` now asks the generator for `Vec<VaultFile>` and writes those files into the candidate knowledge directory. The LLM path parses the model response as vault JSON before writing.

The search implementation in `src/search.rs` parses vault frontmatter and body separately. Frontmatter affects scoring and response metadata, while snippets and line numbers point into Markdown body content.

## Safety Rules

- Fetched source text is untrusted evidence, not instructions.
- Raw source documents are not stored locally.
- Candidate output is validated before it is written.
- Candidate updates are staged and diffed before approval.
- Search never treats candidate content as approved knowledge.
- Source summaries preserve traceability, but topic pages are the primary retrieval layer.

## Development

Run checks:

```bash
cargo fmt
cargo test
```

The code follows two reference ideas:

- From `scribe_engine`: agent loop, session snapshots, context compaction, prompt cache, usage telemetry, and bounded web fetching.
- From `claw_code`: typed boundaries, structured status surfaces, recoverable snapshots, and bounded context reads.
