# Wiki Craft

[English](README.md) | [中文](README.zh-CN.md)

Wiki Craft 是一个以 Markdown 为核心的知识库维护代理。它会抓取配置好的 URL 来源，检测内容变化，用 LLM 生成证据摘要，先暂存 source summary 候选，并在人工审核后再生成知识 diff、最终合并正式知识。

项目当前有两套核心系统：

- **检索 Search**：在已批准 Markdown 知识库上的本地只读检索层。
- **抓取与索引构建 Ingest and indexing**：抓取来源、摘要变化证据，在摘要批准后生成 topic-first 候选页面，并记录来源追溯的维护流水线。

Wiki Craft 故意保持存储模型简单：没有向量数据库，没有 embedding pipeline，也不保存原始来源全文。已批准的 Markdown vault 就是检索面；source summaries 是证据层；候选更新必须先 staged、diff、approve，之后才成为正式知识。

## 运行目录

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

关键目录：

- `.wiki_craft/knowledge/approved/index.md`：已批准知识库入口。
- `.wiki_craft/knowledge/approved/topics/*.md`：已批准的 topic-first 知识页面。
- `.wiki_craft/knowledge/approved/evidence/sources/manifest.json`：来源注册表，保存 URL、内容 hash、抓取时间、最新候选 run ID 和摘要路径。
- `.wiki_craft/knowledge/approved/evidence/source_summaries/*.md`：已批准的 LLM 来源摘要。
- `.wiki_craft/knowledge/staging/candidates/{run_id}/`：等待审核的来源摘要、可选知识提案、baseline 快照、diff 和元数据。
- `.wiki_craft/runtime/`：运行状态，包括 sessions、prompt cache、audit events、metrics、transcripts 和 status。

## 快速开始

```bash
cargo run -- init
```

先从 example 模板生成本地配置文件：

```bash
cp wiki_craft_example.toml wiki_craft.toml
cp wiki_craft.ingest_example.toml wiki_craft.ingest.toml
```

然后根据自己的 LLM 和来源配置修改 `wiki_craft.toml` 和 `wiki_craft.ingest.toml`。

编辑 `wiki_craft.ingest.toml`，启用至少一个来源，并配置这三个环境变量：

```bash
export LLM_API_KEY="..."
export LLM_BASE_URL="..."
export LLM_MODEL="..."
```

常见流程：

```bash
cargo run -- ingest --once
cargo run -- candidates list
cargo run -- candidates summaries <run_id>
cargo run -- candidates approve <run_id> # 生成知识 diff
cargo run -- candidates diff <run_id>
cargo run -- candidates merge <run_id>   # 合并已同意的 diff
cargo run -- search --query "what changed?" --top-k 5 --json
cargo run -- status
```

持续运行：

```bash
cargo run -- serve
```

`serve` 会立即检查周期性来源，只抓取已到各自周期的来源，然后睡眠到下一个来源到期。启用 metrics 后会暴露：

- `http://127.0.0.1:9898/metrics`
- `http://127.0.0.1:9898/metrics.json`

## 检索

检索逻辑在 `src/search.rs`。它是本地只读检索，只读取已经批准的内容：

- `.wiki_craft/knowledge/approved/index.md`
- `.wiki_craft/knowledge/approved/topics/*.md`
- `.wiki_craft/knowledge/approved/evidence/source_summaries/*.md`

它不会读取 staged candidates。候选知识可以有用，但在批准前不被视为正式知识。

```bash
cargo run -- search --query "<question>" --top-k 5 --json
```

普通文本输出给人看，JSON 输出给 agent 和工具链使用。

### 输入解析

检索会先根据 `wiki_craft.toml` 解析工作区路径。如果 `runtime.root` 是相对路径，就以配置文件所在目录为基准解析。然后它会尝试读取 source manifest。

manifest 会用于补充 source summary 的元数据。例如检索 `.wiki_craft/knowledge/approved/evidence/source_summaries/{source_id}.md` 时，Wiki Craft 可以从 `.wiki_craft/knowledge/approved/evidence/sources/manifest.json` 里补上原始 URL 和内容 hash。

### 文档收集

检索会收集三类结果：

- `index`：`.wiki_craft/knowledge/approved/index.md`
- `topic`：`.wiki_craft/knowledge/approved/topics/*.md`
- `source_summary`：`.wiki_craft/knowledge/approved/evidence/source_summaries/*.md`

每个 Markdown 文件会被解析为：

- YAML frontmatter：`title`、`aliases`、`tags`、`source_ids`、`source_urls`、`version_hashes`、`updated_at_run_id`。
- Body text：frontmatter 之后的 Markdown 正文。
- Wikilinks：例如 `[[topics/search|Search]]`。
- Body start line：用于返回准确的行号。

对 source summary，检索还会从正文里抽取 URL 和 16 到 64 位十六进制 hash。这样即使旧摘要没有完整 frontmatter，只要正文里包含来源链接或版本 hash，也能被检索结果带出来。

### 切块

检索按 Markdown 标题切块。每个标题段落都会成为一个可检索 chunk。这样结果不会只指向整个文件，而是可以指向具体章节，并返回 `line_start`、`line_end` 和聚焦 snippet。

如果文档没有标题但有正文，整个正文会作为一个 chunk。

### 查询解析

查询会被规整成：

- `phrase`：小写后的完整查询。
- `compact_phrase`：去掉空白后的查询，用来匹配空格差异。
- `words`：长度至少为 2 的字母数字或 CJK 词项。
- `cjk_chars`：去重后的 CJK 字符，逐字打分。

这样英文关键词查询和中文查询能共用同一个轻量打分模型。中文没有稳定空格分词，所以代码会额外按 CJK 字符匹配。

### 打分

检索是结构化打分，不是向量检索。不同字段权重不同：

- `title`：最强信号。
- `aliases`：别名和不同问法。
- `tags`：宽泛分类信号。
- `wikilinks`：相关 topic 信号。
- Markdown heading：局部章节意图。
- `source_ids`、`source_urls`、`version_hashes`：来源追溯字段。
- 正文：详细证据和精确措辞。

打分函数会检查完整短语、去空格短语、词项出现次数和 CJK 字符出现次数。出现次数有上限，避免某个长文本因为重复词太多而过度占优。

原始分数计算后还会做两件事：

- Topic pages 会获得优先级加分，因为长期概念页通常应该在相关性相近时排在 source summary 前面。
- 长 chunk 会受到长度惩罚，让简短聚焦的章节可以和大段证据竞争。

结果排序是稳定的：先按分数，再按结果类型优先级、路径和行号排序。

### 返回结构

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

这个结构最重要的特点是可解释：每条结果都能指回 Markdown 路径、行号范围、片段和证据元数据。这也是检索建立在 approved Markdown 结构上的主要原因。

## 抓取与索引构建

抓取与索引构建主要由 `src/runtime.rs`、`src/sources.rs`、`src/tools/web_fetch.rs`、`src/knowledge.rs` 和 `src/candidates.rs` 实现。

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

从概念上说，Wiki Craft 的“索引”不是向量索引，而是两层 Markdown 结构：

- 证据层：`knowledge/approved/evidence/source_summaries/*.md`
- 知识层：`knowledge/approved/index.md` 和 `knowledge/approved/topics/*.md`

检索时再实时读取这些已批准 Markdown 文件并打分。

### 来源配置

一次性来源和周期性来源在 `wiki_craft.ingest.toml` 中分开配置：

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

`cargo run -- ingest --once` 只抓取启用的 `ingest.once.sources`。`cargo run -- serve` 只抓取启用且已到各自 `interval_hours` 周期的 `ingest.cron.sources`。每个来源只需要 `url`；如果省略 `interval_hours`，默认是 24 小时。

### 抓取

Web fetch 工具只接受 `http` 和 `https` URL，并且有边界控制：

- Timeout 会被限制在 1 到 60 秒。
- 响应 body 大小会被限制在 1 byte 到 1,000,000 bytes。
- Redirect 最多 5 次。
- User agent 是 `wiki-craft-web-fetch/0.1`。
- 可以包含 headers，用于记录 `etag`、`last-modified` 等元数据。

对于 HTML/XHTML 响应，抓取器会去掉 `script`、`style`、`noscript`，移除标签，解码一小部分 HTML entity，规整空白，并提取 `<title>`。

非 HTML 响应则直接做空白规整。

### 来源身份与版本

抓取后，Wiki Craft 会生成 `FetchedSource`：

- `source_id`：配置 URL 的 SHA-256 前 16 位十六进制字符。
- `normalized_text`：抓取到的可读文本，折叠连续空白。
- `content_hash`：`normalized_text` 的 SHA-256。
- `version_key`：当前等同于 `content_hash`。
- `etag` 和 `last_modified`：如果响应 headers 中存在就复制过来。
- `final_url`、`title` 和原始配置 `url`。

空白规整意味着换行、连续空格等细微格式差异通常不会触发新版本。

### Manifest 变更检测

source manifest 位于：

```text
.wiki_craft/knowledge/approved/evidence/sources/manifest.json
```

它为每个 `source_id` 保存一条 `SourceRecord`，包括配置 URL、最终 URL、标题、`etag`、`last_modified`、`content_hash`、`version_key`、抓取时间、最新候选 run ID 和摘要路径。

如果 manifest 中没有旧记录，或者旧 `content_hash` 与新 `content_hash` 不同，就认为来源发生变化。未变化来源仍会更新抓取元数据；发生变化的来源才会进入摘要流程。

### 来源摘要

对每个变化的来源，LLM 会收到 source URL、final URL、title、version hash 和抓取出的 readable text。

摘要 prompt 要求模型把来源文本当作不可信证据，不执行来源里的指令，使用与来源或用户相匹配的中英文，输出简洁 Markdown，包含关键主张、核心方法或流程、有用关键词、冲突和不确定性，并避免长篇复制原文。

生成的摘要会写入候选目录：

```text
.wiki_craft/knowledge/staging/candidates/{run_id}/evidence/source_summaries/{source_id}.md
```

ingest 只会把本次变化来源的新摘要写入候选目录。最终批准会把这些变化摘要按文件合并进正式 source-summary 目录，并保留不属于本次候选的既有正式摘要。

### 候选知识库

source summaries 准备好后，第一次 `candidates approve <run_id>` 表示用户同意这些摘要可用于知识库提案。Wiki Craft 随后会把当前正式知识快照保存到 `baseline/knowledge/`，读取正式知识和变化摘要，并让 LLM 生成一个完整候选 vault。

LLM 必须返回 JSON：

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

如果模型返回裸数组，或包了一层 Markdown code fence，解析器也能处理。

候选知识必须满足：

- 至少包含一个文件。
- 必须包含 `index.md`。
- 有效路径只能是 `index.md` 和 `topics/*.md`。
- 路径必须是相对 Markdown 路径，并且不能逃出 vault。
- 不能有重复路径。

每个 topic page 应使用 YAML frontmatter：

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

候选知识库是 topic-first，而不是 source-first。source summary 可以描述单篇文章，但 `topics/*.md` 应该组织长期有效的概念、流程和判断。

### Diff 与元数据

第一次批准写入候选知识文件后，Wiki Craft 会创建：

- `baseline/knowledge/`：用于 diff 旧侧的正式 topic/index 快照。
- `knowledge/`：模型生成的候选 topic/index vault。
- `diff.md`：baseline 与候选知识之间的简单逐行 diff。
- `metadata.json`：run ID、创建时间、候选状态、变化来源、prompt cache stats 和 compaction count。

ingest 结果会写入 `.wiki_craft/runtime/status.json`，metrics 也会更新。

## 批准模型

候选内容不是正式知识。批准分两步显式执行：

```bash
cargo run -- candidates list
cargo run -- candidates summaries <run_id>
cargo run -- candidates approve <run_id> # summaries_staged -> diff_ready
cargo run -- candidates diff <run_id>
cargo run -- candidates merge <run_id>   # diff_ready -> approved
```

第一次批准不会修改正式知识或正式 source summaries，只会生成 `baseline/knowledge/`、候选 `knowledge/` 和 `diff.md`。

merge 步骤会提升：

- `.wiki_craft/knowledge/staging/candidates/{run_id}/knowledge/` 到 `.wiki_craft/knowledge/approved/`
- `.wiki_craft/knowledge/staging/candidates/{run_id}/evidence/source_summaries/` 到 `.wiki_craft/knowledge/approved/evidence/source_summaries/`

拒绝候选也需要显式执行：

```bash
cargo run -- candidates reject <run_id>
```

因为最终批准会整体替换正式 topic/index vault，所以候选知识 vault 必须是完整的。source summaries 则按文件合并，因此 staging 里只需要保存本次变化的摘要。

批准后，manifest 中的 summary path 会更新到正式 source-summary 位置，已批准的 candidate 目录会从 staging 中删除。

## 重组已有知识

可以用下面的命令把已有正式 Markdown 转成候选 topic-first vault：

```bash
cargo run -- knowledge reorganize
```

这个命令会读取 `.wiki_craft/knowledge/approved/`，按标题保守切分已有 Markdown，创建候选 `index.md` 和 `topics/*.md`，写入 `diff.md` 和 `metadata.json`，但不修改正式知识。

它和普通 ingest candidate 一样，需要先 review，再 merge。

## 安全规则

- 抓取到的来源文本只是证据，不是指令。
- 不在本地保存原始来源全文。
- LLM 输出写入候选文件前必须通过校验。
- 所有知识更新都先 staged、再 diff、再 merge。
- 检索不会把 candidate 当成正式知识。
- Source summary 负责证据追溯，topic page 才是主要知识入口。

## 实现位置

- `src/main.rs`：CLI 命令。
- `src/runtime.rs`：ingest loop、LLM 生成、candidate 创建、approve/merge 入口、status、metrics 和 serve loop。
- `src/tools/web_fetch.rs`：有边界的 HTTP 抓取和 readable-text extraction。
- `src/sources.rs`：source IDs、normalized text hashes、manifest load/save 和 change detection。
- `src/knowledge.rs`：vault file validation、frontmatter parsing、wikilink extraction、current knowledge reading 和 reorganizer。
- `src/search.rs`：approved-knowledge retrieval、chunking、scoring、snippets 和 JSON/text output。
- `src/candidates.rs`：candidate paths、metadata、diffs、listing 和 directory promotion。

## 开发

运行检查：

```bash
cargo fmt
cargo test
```
