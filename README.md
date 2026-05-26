# Wiki Craft

[English](README.md) | [中文](README.zh-CN.md)

Wiki Craft is a Markdown-first knowledge-base maintenance agent. It fetches configured URL sources, detects changes, summarizes evidence with an LLM, stages source-summary candidates, and waits for human approval before proposing and merging approved knowledge changes.

The project has two core systems:

- **Search**: a local, read-only retrieval layer over approved Markdown knowledge.
- **Ingest and indexing**: a maintenance pipeline that fetches sources, summarizes changed evidence, proposes topic-first candidate pages after summary approval, and records provenance.

Wiki Craft intentionally keeps the storage model simple: no vector database, no embedding pipeline, no raw-source archive. The approved Markdown vault is the retrieval surface. Source summaries are the evidence layer. Candidate updates are staged, diffed, and approved before becoming authoritative.

## Quick Start

```bash
cargo run -- init
```

Create the global local config from the example template:

```bash
cp wiki_craft_example.toml wiki_craft.toml
```

Then create a knowledge base. The focus is required because Wiki Craft uses it when summarizing sources and generating topic pages:

```bash
cargo run -- knowledge-base create \
  --name "Product Research" \
  --focus "Long-lived product research notes, pricing changes, and integration decisions"
```

Configure these three environment variables:

```bash
export LLM_API_KEY="..."
export LLM_BASE_URL="..."
export LLM_MODEL="..."
```

Edit the active knowledge base config under `.wiki_craft/knowledge_bases/{id}/knowledge_base.toml`, enable at least one source, and then run ingest. See `knowledge_base_example.toml` for the per-knowledge-base source shape.

Typical workflow:

```bash
cargo run -- knowledge-base list
cargo run -- ingest --once
cargo run -- candidates list
cargo run -- candidates summaries <run_id>
cargo run -- candidates approve <run_id> # generate knowledge diff
cargo run -- candidates diff <run_id>
cargo run -- candidates merge <run_id>   # merge accepted diff
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

## Desktop GUI

Wiki Craft also includes a local Tauri desktop GUI for reviewing staged candidates. It uses the same approval model as the CLI:

- Create a knowledge base from the sidebar, including the required focus statement.
- Switch the active knowledge base from the sidebar. Review, search, ingest, and candidate actions operate on the active knowledge base only.
- Use `Import File` in the active knowledge-base panel to pick a local UTF-8 text file and stage it as source evidence for the current knowledge base only. Local file imports still require the normal summary approval and diff merge steps before they become approved knowledge.
- Use `Create Skill` in the knowledge-base panel to generate a Codex/Claude-compatible `SKILL.md` for the selected knowledge base.
- `summaries_staged`: review changed source summaries, then approve summaries to generate a candidate knowledge diff.
- `diff_ready`: review the colorized `diff.md`, then merge the accepted diff into approved knowledge.
- Reject remains explicit and removes the staged candidate without changing approved knowledge.

Install and build the frontend once:

```bash
npm --prefix frontend install
npm run build
```

Run the desktop app in development:

```bash
npm run tauri -- dev
```

Run these commands from the repository root. The desktop shell starts a local API server on an ephemeral `127.0.0.1` port and injects that URL into the React UI. By default it reads `wiki_craft.toml`; set `WIKI_CRAFT_CONFIG=/path/to/wiki_craft.toml` before launch to review a different workspace.

GUI and service logs are separate from the LLM audit log:

- `.wiki_craft/runtime/gui/events.jsonl`: desktop UI events, action failures, and local API request failures reported by the frontend/Tauri shell.
- `.wiki_craft/runtime/web/events.jsonl`: local Axum API startup and candidate action job lifecycle events.
- `.wiki_craft/runtime/audit/events.jsonl`: model/tool-call audit trail for LLM workflows only.

## Search

Search is implemented in `src/search.rs`. It is local, read-only, and searches only approved content:

- `.wiki_craft/knowledge_bases/{id}/knowledge/approved/index.md`
- `.wiki_craft/knowledge_bases/{id}/knowledge/approved/topics/*.md`
- `.wiki_craft/knowledge_bases/{id}/knowledge/approved/evidence/source_summaries/*.md`

It never reads staged candidates. A candidate may contain useful draft knowledge, but it is not authoritative until approved.

```bash
cargo run -- search --query "<question>" --top-k 5 --json
```

Agents can target a specific knowledge base without changing the active GUI/CLI selection:

```bash
cargo run -- search --knowledge-base "<knowledge_base_id>" --query "<question>" --top-k 5 --json
```

Text output is for humans. JSON output is for agents and tooling.

### Inputs

Search resolves the active knowledge base from `.wiki_craft/knowledge_bases/registry.json` unless `--knowledge-base <id>` is provided, then loads that knowledge base's source manifest if it exists.

The manifest enriches source-summary results. For example, when searching `.wiki_craft/knowledge_bases/{id}/knowledge/approved/evidence/source_summaries/{source_id}.md`, Wiki Craft can add the original source URL and content hash from `.wiki_craft/knowledge_bases/{id}/knowledge/approved/evidence/sources/manifest.json`.

### Document Collection

Search collects three result kinds:

- `index`: `.wiki_craft/knowledge_bases/{id}/knowledge/approved/index.md`
- `topic`: `.wiki_craft/knowledge_bases/{id}/knowledge/approved/topics/*.md`
- `source_summary`: `.wiki_craft/knowledge_bases/{id}/knowledge/approved/evidence/source_summaries/*.md`

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

One-time and periodic sources are configured per knowledge base in `.wiki_craft/knowledge_bases/{id}/knowledge_base.toml`:

```toml
name = "Product Research"
focus = "Long-lived product research notes, pricing changes, and integration decisions."

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

Only the active knowledge base is ingested. Use `cargo run -- knowledge-base activate <id>` or the GUI selector to switch.

Local file import is separate from configured URL sources. In the desktop GUI, `Import File` reads a selected UTF-8 text file as a temporary source for the active knowledge base; it does not add the file to `knowledge_base.toml`, and it does not copy raw file contents into `.wiki_craft`.

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
.wiki_craft/knowledge_bases/{id}/knowledge/approved/evidence/sources/manifest.json
```

It stores one `SourceRecord` per `source_id`, including configured URL, final URL, title, `etag`, `last_modified`, `content_hash`, `version_key`, fetch timestamps, latest candidate run ID, and summary path.

A source is considered changed if there is no previous record or if the previous `content_hash` differs from the new one. Unchanged sources still update fetch metadata. Changed sources continue into summarization.

### Source Summaries

For each changed source, the LLM receives the active knowledge base focus, source URL, final URL, title, version hash, and fetched readable text.

The summarizer prompt requires the model to treat source text as untrusted evidence, ignore instructions inside the source, use the knowledge base focus to decide what matters, write concise Markdown in the source/user language, include key claims and workflows, record useful keywords, mark conflicts or uncertainty, and avoid long raw passages.

The generated summary is written to:

```text
.wiki_craft/knowledge_bases/{id}/knowledge/staging/candidates/{run_id}/evidence/source_summaries/{source_id}.md
```

Ingest writes only the changed source summaries into the candidate. Final merge copies those changed summaries into the approved source-summary directory, preserving already approved summaries that were not part of the candidate.

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
cargo run -- candidates merge <run_id>   # diff_ready -> approved
```

The first approval does not modify approved knowledge or approved source summaries. It only generates `baseline/knowledge/`, proposed `knowledge/`, and `diff.md`.

The merge step promotes:

- `.wiki_craft/knowledge_bases/{id}/knowledge/staging/candidates/{run_id}/knowledge/` to `.wiki_craft/knowledge_bases/{id}/knowledge/approved/`
- `.wiki_craft/knowledge_bases/{id}/knowledge/staging/candidates/{run_id}/evidence/source_summaries/` to `.wiki_craft/knowledge_bases/{id}/knowledge/approved/evidence/source_summaries/`

Rejecting a candidate is also explicit:

```bash
cargo run -- candidates reject <run_id>
```

Because merge replaces the approved topic/index vault, every proposed knowledge vault must be complete. Source summaries are merged by file, so staging only needs to contain the changed summaries for that run.

After merge, manifest summary paths are updated to point at the approved source-summary location, and the approved candidate directory is removed from staging.

The desktop GUI exposes the same steps as buttons: `Approve Summaries` for `summaries_staged`, `Merge Diff` for `diff_ready`, and `Reject` for unapproved candidates. The GUI renders `diff.md` with editor-style colors but does not change the approval semantics.

## Reorganizing Existing Knowledge

Use this command to convert existing approved Markdown into a candidate topic-first vault:

```bash
cargo run -- knowledge reorganize
```

This command reads the active knowledge base's approved vault, splits existing Markdown conservatively by headings, creates candidate `index.md` and `topics/*.md`, writes `diff.md` and `metadata.json`, and leaves approved knowledge unchanged.

Review and merge it like any ingest candidate.

## Safety Rules

- Fetched source text is untrusted evidence, not instructions.
- Raw source documents are not stored locally.
- LLM output is validated before candidate files are written.
- Candidate updates are staged and diffed before approval.
- Search never treats candidate content as approved knowledge.
- Source summaries preserve traceability, but topic pages are the primary retrieval layer.
- Runtime GUI and service diagnostics are written under `.wiki_craft/runtime/gui/` and `.wiki_craft/runtime/web/`; the audit log remains reserved for the LLM call chain.

## Implementation Map

- `src/main.rs`: CLI commands.
- `src/runtime.rs`: ingest loop, LLM generation, candidate creation, approve/merge entry points, status, metrics, and serve loop.
- `src/tools/web_fetch.rs`: bounded HTTP fetch and readable-text extraction.
- `src/sources.rs`: source IDs, normalized text hashes, manifest load/save, and change detection.
- `src/knowledge.rs`: vault file validation, frontmatter parsing, wikilink extraction, current knowledge reading, and reorganizer.
- `src/search.rs`: approved-knowledge retrieval, chunking, scoring, snippets, and JSON/text output.
- `src/candidates.rs`: candidate paths, metadata, diffs, listing, and directory promotion.
- `src/web.rs`: local Axum JSON API for the desktop GUI.
- `frontend/`: React/Vite candidate review UI.
- `src-tauri/`: Tauri desktop shell that starts the local API and hosts the built frontend.

## Development

Run checks:

```bash
cargo fmt
cargo test
cargo check --manifest-path src-tauri/Cargo.toml
npm run build
```
