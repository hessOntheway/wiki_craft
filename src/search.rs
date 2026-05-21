use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::knowledge::{DEFAULT_SCHEMA_PATH, WorkspacePaths};
use crate::sources::SourceManifest;

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
    pub searched_paths: Vec<String>,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub kind: SearchResultKind,
    pub heading: Option<String>,
    pub score: f64,
    pub line_start: usize,
    pub line_end: usize,
    pub snippet: String,
    pub source_urls: Vec<String>,
    pub version_hashes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchResultKind {
    Schema,
    Knowledge,
    SourceSummary,
}

#[derive(Debug, Clone)]
struct SearchDocument {
    display_path: String,
    kind: SearchResultKind,
    text: String,
    manifest_url: Option<String>,
    manifest_hash: Option<String>,
}

#[derive(Debug, Clone)]
struct SearchChunk {
    display_path: String,
    kind: SearchResultKind,
    heading: Option<String>,
    text: String,
    line_start: usize,
    manifest_url: Option<String>,
    manifest_hash: Option<String>,
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
    let project_root = config_project_root(config_path);
    let paths = workspace_paths_for_search(config, &project_root);
    let schema_path = project_root.join(DEFAULT_SCHEMA_PATH);

    let manifest = SourceManifest::load(&paths.manifest_path).unwrap_or_default();
    let documents = collect_documents(&schema_path, &paths, &manifest)?;
    let searched_paths = documents
        .iter()
        .map(|document| document.display_path.clone())
        .collect::<Vec<_>>();
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
        searched_paths,
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
        out.push(format!(
            "{}. {}:{}{} [score {:.2}]",
            idx + 1,
            result.path,
            result.line_start,
            heading,
            result.score
        ));
        if !result.source_urls.is_empty() {
            out.push(format!("   sources: {}", result.source_urls.join(", ")));
        }
        out.push(indent_snippet(&result.snippet));
    }
    out.join("\n")
}

fn workspace_paths_for_search(mut config: AppConfig, project_root: &Path) -> WorkspacePaths {
    if Path::new(&config.runtime.root).is_relative() {
        config.runtime.root = project_root
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
    schema_path: &Path,
    paths: &WorkspacePaths,
    manifest: &SourceManifest,
) -> Result<Vec<SearchDocument>> {
    let mut documents = Vec::new();
    if schema_path.exists() {
        documents.push(read_document(
            schema_path,
            SearchResultKind::Schema,
            None,
            None,
        )?);
    }
    documents.extend(read_markdown_dir(
        &paths.knowledge_current,
        SearchResultKind::Knowledge,
        manifest,
    )?);
    documents.extend(read_markdown_dir(
        &paths.source_summaries_current,
        SearchResultKind::SourceSummary,
        manifest,
    )?);
    Ok(documents)
}

fn read_markdown_dir(
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
        documents.push(read_document(
            path,
            kind,
            record.map(|record| record.url.clone()),
            record.map(|record| record.content_hash.clone()),
        )?);
    }
    documents.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    Ok(documents)
}

fn read_document(
    path: &Path,
    kind: SearchResultKind,
    manifest_url: Option<String>,
    manifest_hash: Option<String>,
) -> Result<SearchDocument> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(SearchDocument {
        display_path: path.display().to_string(),
        kind,
        text,
        manifest_url,
        manifest_hash,
    })
}

fn split_document(document: &SearchDocument) -> Vec<SearchChunk> {
    let mut chunks = Vec::new();
    let mut current_heading = None;
    let mut current_start = 1usize;
    let mut current_lines = Vec::<String>::new();

    for (idx, line) in document.text.lines().enumerate() {
        let line_number = idx + 1;
        if let Some(heading) = markdown_heading(line) {
            if !current_lines.is_empty() {
                chunks.push(SearchChunk {
                    display_path: document.display_path.clone(),
                    kind: document.kind,
                    heading: current_heading.clone(),
                    text: current_lines.join("\n"),
                    line_start: current_start,
                    manifest_url: document.manifest_url.clone(),
                    manifest_hash: document.manifest_hash.clone(),
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
            heading: current_heading,
            text: current_lines.join("\n"),
            line_start: current_start,
            manifest_url: document.manifest_url.clone(),
            manifest_hash: document.manifest_hash.clone(),
        });
    }

    if chunks.is_empty() && !document.text.trim().is_empty() {
        chunks.push(SearchChunk {
            display_path: document.display_path.clone(),
            kind: document.kind,
            heading: None,
            text: document.text.clone(),
            line_start: 1,
            manifest_url: document.manifest_url.clone(),
            manifest_hash: document.manifest_hash.clone(),
        });
    }

    chunks
}

fn markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|char| *char == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = trimmed.get(level..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    let heading = rest.trim();
    if heading.is_empty() {
        None
    } else {
        Some(heading.to_string())
    }
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
    let text = chunk.text.to_lowercase();
    let compact_text = text.split_whitespace().collect::<String>();
    let heading = chunk.heading.clone().unwrap_or_default().to_lowercase();
    let mut score = 0.0;

    if !terms.phrase.is_empty() && text.contains(&terms.phrase) {
        score += 12.0;
    }
    if !terms.compact_phrase.is_empty()
        && terms.compact_phrase != terms.phrase
        && compact_text.contains(&terms.compact_phrase)
    {
        score += 8.0;
    }

    for word in &terms.words {
        let count = count_occurrences(&text, word);
        if count > 0 {
            score += 2.5 * count as f64;
        }
        if heading.contains(word) {
            score += 4.0;
        }
    }

    for cjk in &terms.cjk_chars {
        let count = text.chars().filter(|char| char == cjk).take(8).count();
        if count > 0 {
            score += 0.35 * count as f64;
        }
        if heading.contains(*cjk) {
            score += 1.0;
        }
    }

    if score <= 0.0 {
        return None;
    }

    let length_penalty = 1.0 + (chunk.text.chars().count() as f64 / 1800.0).sqrt();
    Some(round_score(score / length_penalty))
}

fn chunk_to_result(chunk: SearchChunk, score: f64, terms: &QueryTerms) -> SearchResult {
    let (snippet, line_start, line_end) = best_snippet(&chunk, terms);
    let mut source_urls = extract_urls(&chunk.text);
    if let Some(url) = chunk.manifest_url {
        source_urls.push(url);
    }
    source_urls.sort();
    source_urls.dedup();

    let mut version_hashes = extract_hashes(&chunk.text);
    if let Some(hash) = chunk.manifest_hash {
        version_hashes.push(hash);
    }
    version_hashes.sort();
    version_hashes.dedup();

    SearchResult {
        path: chunk.display_path,
        kind: chunk.kind,
        heading: chunk.heading,
        score,
        line_start,
        line_end,
        snippet,
        source_urls,
        version_hashes,
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

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
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

fn is_cjk(char: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&char)
        || ('\u{3400}'..='\u{4dbf}').contains(&char)
        || ('\u{3040}'..='\u{30ff}').contains(&char)
        || ('\u{ac00}'..='\u{d7af}').contains(&char)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn splits_markdown_by_heading() {
        let document = SearchDocument {
            display_path: "Home.md".to_string(),
            kind: SearchResultKind::Knowledge,
            text: "# Home\nintro\n\n## Install\ncargo run".to_string(),
            manifest_url: None,
            manifest_hash: None,
        };
        let chunks = split_document(&document);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading.as_deref(), Some("Home"));
        assert_eq!(chunks[1].heading.as_deref(), Some("Install"));
        assert_eq!(chunks[1].line_start, 4);
    }

    #[test]
    fn searches_only_approved_current_dirs() {
        let root = unique_temp_dir();
        fs::create_dir_all(root.join(".wiki_craft/knowledge/current")).unwrap();
        fs::create_dir_all(root.join(".wiki_craft/candidates/run_1/knowledge")).unwrap();
        fs::write(
            root.join("wiki_craft.toml"),
            "[runtime]\nroot = \".wiki_craft\"\n",
        )
        .unwrap();
        fs::write(root.join("WIKI_CRAFT.md"), "# Schema\napproved only").unwrap();
        fs::write(
            root.join(".wiki_craft/knowledge/current/Home.md"),
            "# Home\nstable llama retrieval",
        )
        .unwrap();
        fs::write(
            root.join(".wiki_craft/candidates/run_1/knowledge/Home.md"),
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
        assert!(
            response.results[0]
                .path
                .ends_with(".wiki_craft/knowledge/current/Home.md")
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
    fn extracts_urls_and_hashes_from_result() {
        let chunk = SearchChunk {
            display_path: "summary.md".to_string(),
            kind: SearchResultKind::SourceSummary,
            heading: Some("Source".to_string()),
            text: "Source: https://example.com/a.\nVersion hash: abcdef1234567890".to_string(),
            line_start: 1,
            manifest_url: Some("https://example.com/manifest".to_string()),
            manifest_hash: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
        };
        let result = chunk_to_result(chunk, 1.0, &query_terms("source"));
        assert!(
            result
                .source_urls
                .contains(&"https://example.com/a".to_string())
        );
        assert!(
            result
                .source_urls
                .contains(&"https://example.com/manifest".to_string())
        );
        assert!(
            result
                .version_hashes
                .contains(&"abcdef1234567890".to_string())
        );
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wiki_craft_search_test_{nanos}"))
    }
}
