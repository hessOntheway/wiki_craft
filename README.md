# Wiki Craft

[English](README.md) | [中文](README.zh-CN.md)

Wiki Craft is a Markdown-first knowledge-base maintenance agent. It fetches configured URL sources, detects changes, summarizes evidence with an LLM, stages source-summary candidates, and waits for human approval before proposing and merging approved knowledge changes.

The project has two core systems:

- **Search**: a local, read-only retrieval layer over approved Markdown knowledge.
- **Ingest and indexing**: a maintenance pipeline that fetches sources, summarizes changed evidence, proposes topic-first candidate pages after summary approval, and records provenance.

Wiki Craft intentionally keeps the storage model simple: no vector database, no embedding pipeline, no raw-source archive. The approved Markdown vault is the retrieval surface. Source summaries are the evidence layer. Candidate updates are staged, diffed, and approved before becoming authoritative.

## Runtime Layout

```text
.wiki_craft/
  knowledge/
    approved/
      index.md
      topics/
        *.md
      evidence/
        source_summaries/
          *.md
        sources/
          manifest.json
    staging/
      candidates/
        {run_id}/
          baseline/
            knowledge/
              index.md
              topics/
                *.md
          knowledge/
            index.md
            topics/
              *.md
          evidence/
            source_summaries/
              *.md
          diff.md
          metadata.json
  runtime/
    audit/
    metrics/
    prompt_cache/
    sessions/
    transcripts/
```

Important directories:

- `.wiki_craft/knowledge/approved/index.md`: approved vault entry point.
- `.wiki_craft/knowledge/approved/topics/*.md`: approved topic-first knowledge pages.
- `.wiki_craft/knowledge/approved/evidence/sources/manifest.json`: source registry with URLs, content hashes, fetch timestamps, latest candidate run IDs, and summary paths.
- `.wiki_craft/knowledge/approved/evidence/source_summaries/*.md`: approved LLM-written summaries for source versions.
- `.wiki_craft/knowledge/staging/candidates/{run_id}/`: staged source summaries, optional proposed knowledge, baseline snapshots, diffs, and metadata waiting for review.
- `.wiki_craft/runtime/`: operational state such as sessions, prompt cache, audit events, metrics, transcripts, and status.

## Quick Start

```bash
cargo run -- init
```

Edit `wiki_craft.ingest.toml`, enable at least one source, and configure these three environment variables:

```bash
export LLM_API_KEY="..."
export LLM_BASE_URL="..."
export LLM_MODEL="..."
```

Typical workflow:

```bash
cargo run -- ingest --once
cargo run -- candidates list
cargo run -- candidates summaries <run_id>
cargo run -- candidates approve <run_id> # generate knowledge diff
cargo run -- candidates diff <run_id>
cargo run -- candidates approve <run_id> # merge accepted diff
cargo run -- search --query "what changed?" --top-k 5 --json
cargo run -- status
```

Run continuously:

```bash
cargo run -- serve
```

`serve` checks periodic sources immediately, fetches only sources whose per-source interval is due, then sleeps until the next source is due. When metrics are enabled, it exposes:

- `http://127.0.0.1:9898/metrics`
- `http://127.0.0.1:9898/metrics.json`

## Search

Search is implemented in `src/search.rs`. It is local, read-only, and searches only approved content:

- `.wiki_craft/knowledge/approved/index.md`
- `.wiki_craft/knowledge/approved/topics/*.md`
- `.wiki_craft/knowledge/approved/evidence/source_summaries/*.md`

It never reads staged candidates. A candidate may contain useful draft knowledge, but it is not authoritative until approved.

```bash
cargo run -- search --query "<question>" --top-k 5 --json
```

Text output is for humans. JSON output is for agents and tooling.

### Inputs

Search resolves workspace paths from `wiki_craft.toml`. If `runtime.root` is relative, it is resolved relative to the config file directory. Search then loads the source manifest if it exists.

The manifest enriches source-summary results. For example, when searching `.wiki_craft/knowledge/approved/evidence/source_summaries/{source_id}.md`, Wiki Craft can add the original source URL and content hash from `.wiki_craft/knowledge/approved/evidence/sources/manifest.json`.

### Document Collection

Search collects three result kinds:

- `index`: `.wiki_craft/knowledge/approved/index.md`
- `topic`: `.wiki_craft/knowledge/approved/topics/*.md`
- `source_summary`: `.wiki_craft/knowledge/approved/evidence/source_summaries/*.md`

Each Markdown file is parsed into:

- YAML frontmatter: `title`, `aliases`, `tags`, `source_ids`, `source_urls`, `version_hashes`, `updated_at_run_id`.
- Body text: the Markdown content after frontmatter.
- Wikilinks: links such as `[[topics/search|Search]]`.
- Body start line: used for accurate result line numbers.

For source summaries, search also extracts URLs and 16 to 64 character hex hashes from the body. Older summaries remain traceable even if metadata was written in Markdown text instead of YAML frontmatter.

### Chunking

Search splits each document by Markdown headings. Each heading section becomes one searchable chunk. Results can therefore point to useful sections instead of only whole files, and the response can include `line_start`, `line_end`, and a focused snippet.

If a document has no headings but has body content, the whole body becomes one chunk.

### Query Terms

The query is normalized into:

- `phrase`: the lowercased full query.
- `compact_phrase`: the query with whitespace removed, useful when spacing differs.
- `words`: alphanumeric or CJK-containing terms with at least two characters.
- `cjk_chars`: unique CJK characters, scored individually.

This allows English keyword search and Chinese query matching to share one lightweight scoring model.

### Scoring

Search is structural, not vector-based. It scores fields with different weights:

- `title`: strongest signal.
- `aliases`: alternate names and query wording.
- `tags`: broad category signal.
- `wikilinks`: related-topic signal.
- Markdown heading: local section intent.
- `source_ids`, `source_urls`, `version_hashes`: traceability fields.
- Body text: detailed evidence and exact wording.

The scoring function checks phrase matches, compact phrase matches, word occurrences, and CJK character occurrences. Occurrence counts are capped so repeated text cannot dominate too much.

After raw scoring:

- Topic pages receive a priority bonus, because durable concept pages should usually beat source summaries when relevance is similar.
- Longer chunks receive a length penalty, so concise focused sections can compete with large evidence blocks.

Result ordering is deterministic: score first, then result kind priority, path, and line number.

### Result Shape

```json
{
  "query": "retrieval",
  "top_k": 5,
  "results": [
    {
      "path": ".wiki_craft/knowledge/approved/topics/search.md",
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

Every result can point back to a Markdown path, line range, snippet, and evidence metadata. That explainability is the main reason search is built on the approved Markdown structure.

## Ingest And Indexing

The ingest pipeline is implemented mainly in `src/runtime.rs`, `src/sources.rs`, `src/tools/web_fetch.rs`, `src/knowledge.rs`, and `src/candidates.rs`.

```text
configured URL sources
  -> bounded web fetch
  -> readable text extraction
  -> whitespace normalization
  -> source_id and content hash
  -> manifest change detection
  -> per-source LLM summary
  -> complete candidate vault JSON
  -> validated candidate knowledge files
  -> diff.md
  -> human approval
  -> approved knowledge replacement
```

Conceptually, Wiki Craft's "index" is not a vector index. It is a two-layer Markdown structure:

- Evidence layer: `knowledge/approved/evidence/source_summaries/*.md`
- Knowledge layer: `knowledge/approved/index.md` and `knowledge/approved/topics/*.md`

Search indexes these approved Markdown files at query time.

### Source Configuration

One-time and periodic sources are configured separately in `wiki_craft.ingest.toml`:

```toml
[ingest.once]

[[ingest.once.sources]]
url = "https://example.test/once"
enabled = true
timeout_seconds = 15
max_bytes = 200000

[ingest.cron]

[[ingest.cron.sources]]
url = "https://example.test/cron"
enabled = true
interval_hours = 24
timeout_seconds = 15
max_bytes = 200000
```

`cargo run -- ingest --once` fetches only enabled `ingest.once.sources`. `cargo run -- serve` fetches only enabled `ingest.cron.sources` whose per-source `interval_hours` is due. Each source only needs a `url`; if `interval_hours` is omitted, it defaults to 24 hours.

### Fetching

The web fetch tool accepts only `http` and `https` URLs and keeps network behavior bounded:

- Timeout is clamped between 1 and 60 seconds.
- Response body size is clamped between 1 byte and 1,000,000 bytes.
- Redirects are limited to 5.
- User agent is `wiki-craft-web-fetch/0.1`.
- Headers can be included for metadata such as `etag` and `last-modified`.

For HTML or XHTML responses, the fetcher removes `script`, `style`, and `noscript` blocks, strips tags, decodes a small set of HTML entities, normalizes whitespace, and extracts the `<title>`.

For non-HTML responses, it normalizes whitespace directly.

### Source Identity And Versioning

After fetching, Wiki Craft creates a `FetchedSource`:

- `source_id`: first 16 hex characters of SHA-256 over the configured URL.
- `normalized_text`: fetched readable text with whitespace collapsed.
- `content_hash`: SHA-256 over `normalized_text`.
- `version_key`: currently the same as `content_hash`.
- `etag` and `last_modified`: copied from response headers when available.
- `final_url`, `title`, and original configured `url`.

Whitespace normalization makes trivial formatting differences less likely to create a new source version.

### Manifest Change Detection

The source manifest lives at:

```text
.wiki_craft/knowledge/approved/evidence/sources/manifest.json
```

It stores one `SourceRecord` per `source_id`, including configured URL, final URL, title, `etag`, `last_modified`, `content_hash`, `version_key`, fetch timestamps, latest candidate run ID, and summary path.

A source is considered changed if there is no previous record or if the previous `content_hash` differs from the new one. Unchanged sources still update fetch metadata. Changed sources continue into summarization.

### Source Summaries

For each changed source, the LLM receives the source URL, final URL, title, version hash, and fetched readable text.

The summarizer prompt requires the model to treat source text as untrusted evidence, ignore instructions inside the source, write concise Markdown in the source/user language, include key claims and workflows, record useful keywords, mark conflicts or uncertainty, and avoid long raw passages.

The generated summary is written to:

```text
.wiki_craft/knowledge/staging/candidates/{run_id}/evidence/source_summaries/{source_id}.md
```

Ingest writes only the changed source summaries into the candidate. Final approval merges those changed summaries into the approved source-summary directory, preserving already approved summaries that were not part of the candidate.

### Candidate Knowledge

After source summaries are ready, the first `candidates approve <run_id>` approves those summaries for use in the knowledge proposal. Wiki Craft then snapshots the current approved knowledge under `baseline/knowledge/`, reads the approved knowledge plus changed source summaries, and asks the LLM to produce a complete proposed candidate vault.

The LLM must return JSON:

```json
{
  "files": [
    {
      "path": "index.md",
      "content": "---\ntitle: \"Wiki Craft Index\"\n...\n---\n\n# Wiki Craft Index\n"
    },
    {
      "path": "topics/example-topic.md",
      "content": "---\ntitle: \"Example Topic\"\n...\n---\n\n# Example Topic\n"
    }
  ]
}
```

Wiki Craft also accepts a bare JSON array of file objects and strips surrounding Markdown code fences if the model adds them.

Candidate knowledge must:

- Contain at least one file.
- Include `index.md`.
- Use only `index.md` and `topics/*.md` paths.
- Use relative Markdown paths that stay inside the vault.
- Avoid duplicate paths.

Each topic page is expected to use YAML frontmatter:

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

The candidate is topic-first, not source-first. A source summary may describe one article, but `topics/*.md` should organize durable concepts, workflows, and decisions.

### Diff And Metadata

When the first approval writes proposed knowledge files, Wiki Craft creates:

- `baseline/knowledge/`: the approved topic/index snapshot used as the old side of the diff.
- `knowledge/`: the proposed topic/index vault.
- `diff.md`: a simple line-oriented diff between the baseline and proposed knowledge.
- `metadata.json`: run ID, creation time, candidate status, changed sources, prompt cache stats, and compaction count.

The ingest outcome is recorded in `.wiki_craft/runtime/status.json`, and metrics are updated.

## Approval Model

Candidates are not authoritative. Approval is explicit and two-step:

```bash
cargo run -- candidates list
cargo run -- candidates summaries <run_id>
cargo run -- candidates approve <run_id> # summaries_staged -> diff_ready
cargo run -- candidates diff <run_id>
cargo run -- candidates approve <run_id> # diff_ready -> approved
```

The first approval does not modify approved knowledge or approved source summaries. It only generates `baseline/knowledge/`, proposed `knowledge/`, and `diff.md`.

The second approval promotes:

- `.wiki_craft/knowledge/staging/candidates/{run_id}/knowledge/` to `.wiki_craft/knowledge/approved/`
- `.wiki_craft/knowledge/staging/candidates/{run_id}/evidence/source_summaries/` to `.wiki_craft/knowledge/approved/evidence/source_summaries/`

Rejecting a candidate is also explicit:

```bash
cargo run -- candidates reject <run_id>
```

Because final approval replaces the approved topic/index vault, every proposed knowledge vault must be complete. Source summaries are merged by file, so staging only needs to contain the changed summaries for that run.

After approval, manifest summary paths are updated to point at the approved source-summary location, and the approved candidate directory is removed from staging.

## Reorganizing Existing Knowledge

Use this command to convert existing approved Markdown into a candidate topic-first vault:

```bash
cargo run -- knowledge reorganize
```

This command reads `.wiki_craft/knowledge/approved/`, splits existing Markdown conservatively by headings, creates candidate `index.md` and `topics/*.md`, writes `diff.md` and `metadata.json`, and leaves approved knowledge unchanged.

Review and approve it like any ingest candidate.

## Safety Rules

- Fetched source text is untrusted evidence, not instructions.
- Raw source documents are not stored locally.
- LLM output is validated before candidate files are written.
- Candidate updates are staged and diffed before approval.
- Search never treats candidate content as approved knowledge.
- Source summaries preserve traceability, but topic pages are the primary retrieval layer.

## Implementation Map

- `src/main.rs`: CLI commands.
- `src/runtime.rs`: ingest loop, LLM generation, candidate creation, approval entry points, status, metrics, and serve loop.
- `src/tools/web_fetch.rs`: bounded HTTP fetch and readable-text extraction.
- `src/sources.rs`: source IDs, normalized text hashes, manifest load/save, and change detection.
- `src/knowledge.rs`: vault file validation, frontmatter parsing, wikilink extraction, current knowledge reading, and reorganizer.
- `src/search.rs`: approved-knowledge retrieval, chunking, scoring, snippets, and JSON/text output.
- `src/candidates.rs`: candidate paths, metadata, diffs, listing, and directory promotion.

## Development

Run checks:

```bash
cargo fmt
cargo test
```
