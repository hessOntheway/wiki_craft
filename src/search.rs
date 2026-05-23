use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::knowledge::WorkspacePaths;
use crate::knowledge::{
    VAULT_INDEX_PATH, VAULT_TOPICS_DIR, VaultFrontmatter, extract_wikilinks, parse_vault_markdown,
};
use crate::sources::SourceManifest;
use crate::support::{markdown_heading, sort_dedup_nonempty, truncate_chars};

const DEFAULT_SNIPPET_MAX_CHARS: usize = 1200;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub top_k: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub top_k: usize,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub kind: SearchResultKind,
    pub title: Option<String>,
    pub heading: Option<String>,
    pub score: f64,
    pub line_start: usize,
    pub line_end: usize,
    pub snippet: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub wikilinks: Vec<String>,
    pub source_ids: Vec<String>,
    pub source_urls: Vec<String>,
    pub version_hashes: Vec<String>,
    pub updated_at_run_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchResultKind {
    Index,
    Topic,
    SourceSummary,
}

#[derive(Debug, Clone)]
struct SearchDocument {
    display_path: String,
    kind: SearchResultKind,
    frontmatter: VaultFrontmatter,
    body: String,
    body_start_line: usize,
    wikilinks: Vec<String>,
}

#[derive(Debug, Clone)]
struct SearchChunk {
    display_path: String,
    kind: SearchResultKind,
    frontmatter: VaultFrontmatter,
    wikilinks: Vec<String>,
    heading: Option<String>,
    text: String,
    line_start: usize,
}

#[derive(Debug, Clone)]
struct QueryTerms {
    phrase: String,
    compact_phrase: String,
    words: Vec<String>,
    cjk_chars: Vec<char>,
}

pub fn search_configured(config_path: &Path, options: SearchOptions) -> Result<SearchResponse> {
    if options.query.trim().is_empty() {
        bail!("search query must not be empty");
    }
    let top_k = options.top_k.max(1);
    let config = AppConfig::load_or_default(config_path)?;
    let paths = workspace_paths_for_search(config, config_path);

    let manifest = SourceManifest::load(&paths.manifest_path).unwrap_or_default();
    let documents = collect_documents(&paths, &manifest)?;
    let terms = query_terms(&options.query);
    let mut scored = documents
        .iter()
        .flat_map(split_document)
        .filter_map(|chunk| score_chunk(&chunk, &terms).map(|score| (chunk, score)))
        .collect::<Vec<_>>();

    scored.sort_by(|(left_chunk, left_score), (right_chunk, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| kind_rank(left_chunk.kind).cmp(&kind_rank(right_chunk.kind)))
            .then_with(|| left_chunk.display_path.cmp(&right_chunk.display_path))
            .then_with(|| left_chunk.line_start.cmp(&right_chunk.line_start))
    });

    let results = scored
        .into_iter()
        .take(top_k)
        .map(|(chunk, score)| chunk_to_result(chunk, score, &terms))
        .collect::<Vec<_>>();

    Ok(SearchResponse {
        query: options.query,
        top_k,
        results,
    })
}

pub fn render_text_response(response: &SearchResponse) -> String {
    if response.results.is_empty() {
        return format!("No Wiki Craft results for `{}`.", response.query);
    }
    let mut out = Vec::new();
    out.push(format!("Wiki Craft results for `{}`:", response.query));
    for (idx, result) in response.results.iter().enumerate() {
        let heading = result
            .heading
            .as_ref()
            .map(|heading| format!(" - {heading}"))
            .unwrap_or_default();
        let title = result
            .title
            .as_ref()
            .map(|title| format!(" ({title})"))
            .unwrap_or_default();
        out.push(format!(
            "{}. {}:{}{}{} [{:?}, score {:.2}]",
            idx + 1,
            result.path,
            result.line_start,
            title,
            heading,
            result.kind,
            result.score
        ));
        if !result.source_urls.is_empty() {
            out.push(format!("   sources: {}", result.source_urls.join(", ")));
        }
        out.push(indent_snippet(&result.snippet));
    }
    out.join("\n")
}

fn workspace_paths_for_search(mut config: AppConfig, config_path: &Path) -> WorkspacePaths {
    if Path::new(&config.runtime.root).is_relative() {
        config.runtime.root = config_project_root(config_path)
            .join(&config.runtime.root)
            .to_string_lossy()
            .to_string();
    }
    WorkspacePaths::from_config(&config)
}

fn config_project_root(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn collect_documents(
    paths: &WorkspacePaths,
    manifest: &SourceManifest,
) -> Result<Vec<SearchDocument>> {
    let mut documents = Vec::new();
    let knowledge_root = &paths.knowledge_current;
    let index_path = knowledge_root.join(VAULT_INDEX_PATH);
    if index_path.exists() {
        documents.push(read_vault_document(
            &index_path,
            SearchResultKind::Index,
            None,
            None,
        )?);
    }

    documents.extend(read_vault_markdown_dir(
        &knowledge_root.join(VAULT_TOPICS_DIR),
        SearchResultKind::Topic,
        manifest,
    )?);
    documents.extend(read_vault_markdown_dir(
        &paths.source_summaries_current,
        SearchResultKind::SourceSummary,
        manifest,
    )?);
    documents.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    Ok(documents)
}

fn read_vault_markdown_dir(
    root: &Path,
    kind: SearchResultKind,
    manifest: &SourceManifest,
) -> Result<Vec<SearchDocument>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut documents = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let source_id = path.file_stem().and_then(|stem| stem.to_str());
        let record = source_id.and_then(|source_id| manifest.sources.get(source_id));
        documents.push(read_vault_document(
            path,
            kind,
            record.map(|record| record.url.clone()),
            record.map(|record| record.content_hash.clone()),
        )?);
    }
    Ok(documents)
}

fn read_vault_document(
    path: &Path,
    kind: SearchResultKind,
    manifest_url: Option<String>,
    manifest_hash: Option<String>,
) -> Result<SearchDocument> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed = parse_vault_markdown(&text);
    let mut frontmatter = parsed.frontmatter;
    if let Some(url) = manifest_url {
        frontmatter.source_urls.push(url);
    }
    if let Some(hash) = manifest_hash {
        frontmatter.version_hashes.push(hash);
    }
    frontmatter.source_urls.extend(extract_urls(&parsed.body));
    frontmatter
        .version_hashes
        .extend(extract_hashes(&parsed.body));
    normalize_frontmatter_lists(&mut frontmatter);
    let wikilinks = extract_wikilinks(&parsed.body);

    Ok(SearchDocument {
        display_path: path.display().to_string(),
        kind,
        frontmatter,
        body: parsed.body,
        body_start_line: parsed.body_start_line,
        wikilinks,
    })
}

fn split_document(document: &SearchDocument) -> Vec<SearchChunk> {
    let mut chunks = Vec::new();
    let mut current_heading = None;
    let mut current_start = document.body_start_line;
    let mut current_lines = Vec::<String>::new();

    for (idx, line) in document.body.lines().enumerate() {
        let line_number = document.body_start_line + idx;
        if let Some(heading) = markdown_heading(line) {
            if !current_lines.is_empty() {
                chunks.push(SearchChunk {
                    display_path: document.display_path.clone(),
                    kind: document.kind,
                    frontmatter: document.frontmatter.clone(),
                    wikilinks: document.wikilinks.clone(),
                    heading: current_heading.clone(),
                    text: current_lines.join("\n"),
                    line_start: current_start,
                });
            }
            current_heading = Some(heading);
            current_start = line_number;
            current_lines = vec![line.to_string()];
        } else {
            current_lines.push(line.to_string());
        }
    }

    if !current_lines.is_empty() {
        chunks.push(SearchChunk {
            display_path: document.display_path.clone(),
            kind: document.kind,
            frontmatter: document.frontmatter.clone(),
            wikilinks: document.wikilinks.clone(),
            heading: current_heading,
            text: current_lines.join("\n"),
            line_start: current_start,
        });
    }

    if chunks.is_empty() && !document.body.trim().is_empty() {
        chunks.push(SearchChunk {
            display_path: document.display_path.clone(),
            kind: document.kind,
            frontmatter: document.frontmatter.clone(),
            wikilinks: document.wikilinks.clone(),
            heading: None,
            text: document.body.clone(),
            line_start: document.body_start_line,
        });
    }

    chunks
}

fn query_terms(query: &str) -> QueryTerms {
    let phrase = query.trim().to_lowercase();
    let compact_phrase = phrase.split_whitespace().collect::<String>();
    let mut words = phrase
        .split(|char: char| !(char.is_alphanumeric() || is_cjk(char)))
        .filter(|term| term.chars().count() >= 2)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    words.sort();
    words.dedup();

    let mut cjk_chars = phrase
        .chars()
        .filter(|char| is_cjk(*char))
        .collect::<Vec<_>>();
    cjk_chars.sort();
    cjk_chars.dedup();

    QueryTerms {
        phrase,
        compact_phrase,
        words,
        cjk_chars,
    }
}

fn score_chunk(chunk: &SearchChunk, terms: &QueryTerms) -> Option<f64> {
    let frontmatter = &chunk.frontmatter;
    let title = frontmatter.title.clone().unwrap_or_default();
    let heading = chunk.heading.clone().unwrap_or_default();

    let mut score = 0.0;
    score += score_text(&title, terms, 32.0, 12.0, 2.0, 1.8);
    score += score_text(&frontmatter.aliases.join(" "), terms, 24.0, 9.0, 1.6, 1.4);
    score += score_text(&frontmatter.tags.join(" "), terms, 16.0, 6.0, 1.2, 1.0);
    score += score_text(&chunk.wikilinks.join(" "), terms, 14.0, 5.0, 1.0, 0.9);
    score += score_text(&heading, terms, 20.0, 7.0, 1.5, 1.2);
    score += score_text(&frontmatter.source_ids.join(" "), terms, 8.0, 3.0, 0.6, 0.4);
    score += score_text(
        &frontmatter.source_urls.join(" "),
        terms,
        6.0,
        2.0,
        0.4,
        0.3,
    );
    score += score_text(&chunk.text, terms, 12.0, 2.5, 0.35, 0.35);

    if score <= 0.0 {
        return None;
    }

    let priority_bonus = match chunk.kind {
        SearchResultKind::Topic => 8.0,
        SearchResultKind::Index => 3.0,
        SearchResultKind::SourceSummary => 0.0,
    };
    let length_penalty = 1.0 + (chunk.text.chars().count() as f64 / 2200.0).sqrt();
    Some(round_score((score + priority_bonus) / length_penalty))
}

fn score_text(
    text: &str,
    terms: &QueryTerms,
    phrase_score: f64,
    word_score: f64,
    cjk_score: f64,
    compact_score: f64,
) -> f64 {
    if text.trim().is_empty() {
        return 0.0;
    }
    let lower = text.to_lowercase();
    let compact = lower.split_whitespace().collect::<String>();
    let mut score = 0.0;

    if !terms.phrase.is_empty() && lower.contains(&terms.phrase) {
        score += phrase_score;
    }
    if !terms.compact_phrase.is_empty()
        && terms.compact_phrase != terms.phrase
        && compact.contains(&terms.compact_phrase)
    {
        score += compact_score;
    }
    for word in &terms.words {
        let count = count_occurrences(&lower, word);
        if count > 0 {
            score += word_score * count as f64;
        }
    }
    for cjk in &terms.cjk_chars {
        let count = lower.chars().filter(|char| char == cjk).take(8).count();
        if count > 0 {
            score += cjk_score * count as f64;
        }
    }
    score
}

fn chunk_to_result(chunk: SearchChunk, score: f64, terms: &QueryTerms) -> SearchResult {
    let (snippet, line_start, line_end) = best_snippet(&chunk, terms);
    SearchResult {
        path: chunk.display_path,
        kind: chunk.kind,
        title: chunk.frontmatter.title,
        heading: chunk.heading,
        score,
        line_start,
        line_end,
        snippet,
        aliases: chunk.frontmatter.aliases,
        tags: chunk.frontmatter.tags,
        wikilinks: chunk.wikilinks,
        source_ids: chunk.frontmatter.source_ids,
        source_urls: chunk.frontmatter.source_urls,
        version_hashes: chunk.frontmatter.version_hashes,
        updated_at_run_id: chunk.frontmatter.updated_at_run_id,
    }
}

fn best_snippet(chunk: &SearchChunk, terms: &QueryTerms) -> (String, usize, usize) {
    let lines = chunk.text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return (String::new(), chunk.line_start, chunk.line_start);
    }

    let best = lines
        .iter()
        .position(|line| line_matches(line, terms))
        .unwrap_or(0);
    let start = best.saturating_sub(2);
    let end = (best + 3).min(lines.len().saturating_sub(1));
    let snippet = truncate_chars(&lines[start..=end].join("\n"), DEFAULT_SNIPPET_MAX_CHARS);
    (snippet, chunk.line_start + start, chunk.line_start + end)
}

fn line_matches(line: &str, terms: &QueryTerms) -> bool {
    let lower = line.to_lowercase();
    if !terms.phrase.is_empty() && lower.contains(&terms.phrase) {
        return true;
    }
    if !terms.compact_phrase.is_empty()
        && terms.compact_phrase != terms.phrase
        && lower
            .split_whitespace()
            .collect::<String>()
            .contains(&terms.compact_phrase)
    {
        return true;
    }
    if terms.words.iter().any(|word| lower.contains(word)) {
        return true;
    }
    terms.cjk_chars.iter().any(|char| lower.contains(*char))
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count().min(12)
}

fn extract_urls(text: &str) -> Vec<String> {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let regex = URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s<>)\]"]+"#).unwrap());
    collect_regex_matches(regex, text)
}

fn extract_hashes(text: &str) -> Vec<String> {
    static HASH_RE: OnceLock<Regex> = OnceLock::new();
    let regex = HASH_RE.get_or_init(|| Regex::new(r"\b[a-fA-F0-9]{16,64}\b").unwrap());
    collect_regex_matches(regex, text)
}

fn collect_regex_matches(regex: &Regex, text: &str) -> Vec<String> {
    let mut values = regex
        .find_iter(text)
        .map(|match_| {
            match_
                .as_str()
                .trim_end_matches(['.', ',', ';', ':'])
                .to_string()
        })
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn normalize_frontmatter_lists(frontmatter: &mut VaultFrontmatter) {
    sort_dedup_nonempty(&mut frontmatter.aliases);
    sort_dedup_nonempty(&mut frontmatter.tags);
    sort_dedup_nonempty(&mut frontmatter.source_ids);
    sort_dedup_nonempty(&mut frontmatter.source_urls);
    sort_dedup_nonempty(&mut frontmatter.version_hashes);
}

fn indent_snippet(snippet: &str) -> String {
    snippet
        .lines()
        .map(|line| format!("   {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn round_score(score: f64) -> f64 {
    (score * 100.0).round() / 100.0
}

fn kind_rank(kind: SearchResultKind) -> usize {
    match kind {
        SearchResultKind::Topic => 0,
        SearchResultKind::Index => 1,
        SearchResultKind::SourceSummary => 2,
    }
}

fn is_cjk(char: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&char)
        || ('\u{3400}'..='\u{4dbf}').contains(&char)
        || ('\u{3040}'..='\u{30ff}').contains(&char)
        || ('\u{ac00}'..='\u{d7af}').contains(&char)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn splits_markdown_by_heading_after_frontmatter() {
        let document = SearchDocument {
            display_path: "topics/home.md".to_string(),
            kind: SearchResultKind::Topic,
            frontmatter: VaultFrontmatter {
                title: Some("Home".to_string()),
                ..Default::default()
            },
            body: "# Home\nintro\n\n## Install\ncargo run".to_string(),
            body_start_line: 8,
            wikilinks: Vec::new(),
        };
        let chunks = split_document(&document);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading.as_deref(), Some("Home"));
        assert_eq!(chunks[1].heading.as_deref(), Some("Install"));
        assert_eq!(chunks[1].line_start, 11);
    }

    #[test]
    fn searches_only_approved_current_vault_dirs() {
        let root = unique_temp_dir();
        fs::create_dir_all(root.join(".wiki_craft/knowledge/current/topics")).unwrap();
        fs::create_dir_all(root.join(".wiki_craft/candidates/run_1/knowledge/topics")).unwrap();
        fs::write(
            root.join("wiki_craft.toml"),
            "[runtime]\nroot = \".wiki_craft\"\n",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/knowledge/current/index.md"),
            "# Index\n\n- [[topics/home|Home]]",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/knowledge/current/topics/home.md"),
            "---\ntitle: \"Retrieval\"\naliases: [lookup]\ntags: [memory]\nsource_ids: []\nsource_urls: []\nversion_hashes: []\n---\n\n# Retrieval\nstable llama retrieval",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/candidates/run_1/knowledge/topics/draft.md"),
            "# Candidate\nsecret draft term",
        )
        .unwrap();

        let response = search_configured(
            &root.join("wiki_craft.toml"),
            SearchOptions {
                query: "llama".to_string(),
                top_k: 5,
            },
        )
        .unwrap();
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].kind, SearchResultKind::Topic);
        assert!(
            response.results[0]
                .path
                .ends_with(".wiki_craft/knowledge/current/topics/home.md")
        );

        let draft_response = search_configured(
            &root.join("wiki_craft.toml"),
            SearchOptions {
                query: "secret draft term".to_string(),
                top_k: 5,
            },
        )
        .unwrap();
        assert!(draft_response.results.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn topic_metadata_can_beat_source_summary_body_match() {
        let root = unique_temp_dir();
        fs::create_dir_all(root.join(".wiki_craft/knowledge/current/topics")).unwrap();
        fs::create_dir_all(root.join(".wiki_craft/source_summaries/current")).unwrap();
        fs::write(
            root.join("wiki_craft.toml"),
            "[runtime]\nroot = \".wiki_craft\"\n",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/knowledge/current/index.md"),
            "# Index",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/knowledge/current/topics/search.md"),
            "---\ntitle: \"Search\"\naliases: [retrieval]\ntags: [lookup]\nsource_ids: [s1]\nsource_urls: [https://example.test]\nversion_hashes: [abcdef1234567890]\n---\n\n# Search\nShort note.",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/source_summaries/current/s1.md"),
            "# Source\n\nretrieval retrieval retrieval evidence",
        )
        .unwrap();

        let response = search_configured(
            &root.join("wiki_craft.toml"),
            SearchOptions {
                query: "retrieval".to_string(),
                top_k: 2,
            },
        )
        .unwrap();

        assert_eq!(response.results[0].kind, SearchResultKind::Topic);
        assert_eq!(response.results[0].title.as_deref(), Some("Search"));
        assert!(
            response.results[0]
                .source_urls
                .contains(&"https://example.test".to_string())
        );

        fs::remove_dir_all(root).unwrap();
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wiki_craft_search_test_{nanos}"))
    }
}
