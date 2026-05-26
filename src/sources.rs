use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::SourceConfig;
use crate::tools::WebFetchOutput;
use crate::tools::web_fetch::{decode_response_text, normalize_whitespace};

const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const LOCAL_FILE_MAX_BYTES: u64 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub sources: BTreeMap<String, SourceRecord>,
    #[serde(default)]
    pub last_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: String,
    pub url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: String,
    pub version_key: String,
    pub last_fetched_unix_ms: u128,
    pub last_changed_unix_ms: u128,
    #[serde(default)]
    pub latest_candidate_run_id: Option<String>,
    #[serde(default)]
    pub summary_path: Option<String>,
    #[serde(default)]
    pub pending_content_hash: Option<String>,
    #[serde(default)]
    pub pending_summary_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FetchedSource {
    pub source_id: String,
    pub url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub normalized_text: String,
    pub content_hash: String,
    pub version_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedSource {
    pub source_id: String,
    pub url: String,
    #[serde(default)]
    pub final_url: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    pub previous_hash: Option<String>,
    pub new_hash: String,
    #[serde(default)]
    pub version_key: Option<String>,
    pub summary_path: String,
}

impl SourceManifest {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                schema_version: MANIFEST_SCHEMA_VERSION,
                sources: BTreeMap::new(),
                last_run_id: None,
            });
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read source manifest: {}", path.display()))?;
        let mut manifest: SourceManifest = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse source manifest: {}", path.display()))?;
        if manifest.schema_version == 0 {
            manifest.schema_version = MANIFEST_SCHEMA_VERSION;
        }
        Ok(manifest)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create manifest dir: {}", parent.display()))?;
        }
        let content =
            serde_json::to_string_pretty(self).context("failed to serialize source manifest")?;
        fs::write(path, content)
            .with_context(|| format!("failed to write source manifest: {}", path.display()))
    }

    pub fn is_changed(&self, fetched: &FetchedSource) -> bool {
        self.sources.get(&fetched.source_id).map_or(true, |record| {
            record.content_hash != fetched.content_hash
                && record.pending_content_hash.as_deref() != Some(fetched.content_hash.as_str())
        })
    }

    pub fn previous_hash(&self, source_id: &str) -> Option<String> {
        self.sources.get(source_id).and_then(|record| {
            (!record.content_hash.is_empty()).then(|| record.content_hash.clone())
        })
    }

    pub fn upsert_fetched(
        &mut self,
        fetched: &FetchedSource,
        run_id: Option<&str>,
        summary_path: Option<String>,
    ) {
        let now = now_unix_ms();
        let previous = self.sources.get(&fetched.source_id);
        let changed = previous
            .map(|record| record.content_hash != fetched.content_hash)
            .unwrap_or(true);
        let last_changed_unix_ms = if changed {
            now
        } else {
            previous
                .map(|record| record.last_changed_unix_ms)
                .unwrap_or(now)
        };
        self.sources.insert(
            fetched.source_id.clone(),
            SourceRecord {
                id: fetched.source_id.clone(),
                url: fetched.url.clone(),
                final_url: fetched.final_url.clone(),
                title: fetched.title.clone(),
                etag: fetched.etag.clone(),
                last_modified: fetched.last_modified.clone(),
                content_hash: fetched.content_hash.clone(),
                version_key: fetched.version_key.clone(),
                last_fetched_unix_ms: now,
                last_changed_unix_ms,
                latest_candidate_run_id: run_id
                    .map(ToOwned::to_owned)
                    .or_else(|| previous.and_then(|record| record.latest_candidate_run_id.clone())),
                summary_path: summary_path
                    .or_else(|| previous.and_then(|record| record.summary_path.clone())),
                pending_content_hash: previous
                    .and_then(|record| record.pending_content_hash.clone()),
                pending_summary_path: previous
                    .and_then(|record| record.pending_summary_path.clone()),
            },
        );
    }

    pub fn stage_changed(
        &mut self,
        fetched: &FetchedSource,
        run_id: &str,
        pending_summary_path: String,
    ) {
        let now = now_unix_ms();
        let previous = self.sources.get(&fetched.source_id);
        self.sources.insert(
            fetched.source_id.clone(),
            SourceRecord {
                id: fetched.source_id.clone(),
                url: fetched.url.clone(),
                final_url: fetched.final_url.clone(),
                title: fetched.title.clone(),
                etag: fetched.etag.clone(),
                last_modified: fetched.last_modified.clone(),
                content_hash: previous
                    .map(|record| record.content_hash.clone())
                    .unwrap_or_default(),
                version_key: previous
                    .map(|record| record.version_key.clone())
                    .unwrap_or_default(),
                last_fetched_unix_ms: now,
                last_changed_unix_ms: previous
                    .map(|record| record.last_changed_unix_ms)
                    .unwrap_or(now),
                latest_candidate_run_id: Some(run_id.to_string()),
                summary_path: previous.and_then(|record| record.summary_path.clone()),
                pending_content_hash: Some(fetched.content_hash.clone()),
                pending_summary_path: Some(pending_summary_path),
            },
        );
    }

    pub fn upsert_approved_changed(
        &mut self,
        changed: &ChangedSource,
        run_id: &str,
        summary_path: String,
    ) {
        let now = now_unix_ms();
        let previous_last_changed = self
            .sources
            .get(&changed.source_id)
            .filter(|record| record.content_hash == changed.new_hash)
            .map(|record| record.last_changed_unix_ms);
        self.sources.insert(
            changed.source_id.clone(),
            SourceRecord {
                id: changed.source_id.clone(),
                url: changed.url.clone(),
                final_url: changed
                    .final_url
                    .clone()
                    .unwrap_or_else(|| changed.url.clone()),
                title: changed.title.clone(),
                etag: changed.etag.clone(),
                last_modified: changed.last_modified.clone(),
                content_hash: changed.new_hash.clone(),
                version_key: changed
                    .version_key
                    .clone()
                    .unwrap_or_else(|| changed.new_hash.clone()),
                last_fetched_unix_ms: now,
                last_changed_unix_ms: now,
                latest_candidate_run_id: Some(run_id.to_string()),
                summary_path: Some(summary_path),
                pending_content_hash: None,
                pending_summary_path: None,
            },
        );
        if let Some(previous_last_changed) = previous_last_changed
            && let Some(record) = self.sources.get_mut(&changed.source_id)
        {
            record.last_changed_unix_ms = previous_last_changed;
        }
    }

    pub fn clear_pending_candidate(&mut self, changed: &ChangedSource, run_id: &str) {
        let should_remove = self
            .sources
            .get(&changed.source_id)
            .map(|record| {
                record.latest_candidate_run_id.as_deref() == Some(run_id)
                    && (changed.previous_hash.is_none() || record.content_hash.is_empty())
            })
            .unwrap_or(false);
        if should_remove {
            self.sources.remove(&changed.source_id);
            return;
        }
        if let Some(record) = self.sources.get_mut(&changed.source_id)
            && record.latest_candidate_run_id.as_deref() == Some(run_id)
        {
            record.latest_candidate_run_id = None;
            record.pending_content_hash = None;
            record.pending_summary_path = None;
        }
    }
}

pub fn fetched_from_output(config: &SourceConfig, output: WebFetchOutput) -> FetchedSource {
    let normalized_text = normalize_source_text(&output.text);
    let content_hash = sha256_hex(&normalized_text);
    let etag = output.headers.get("etag").cloned();
    let last_modified = output.headers.get("last-modified").cloned();
    let version_key = content_hash.clone();
    let source_id = source_id_for_url(&config.url);
    FetchedSource {
        source_id,
        url: config.url.clone(),
        final_url: output.final_url,
        title: output.title,
        etag,
        last_modified,
        normalized_text,
        content_hash,
        version_key,
    }
}

pub fn fetched_from_local_file(path: &Path) -> Result<FetchedSource> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve local source file: {}", path.display()))?;
    let metadata = fs::metadata(&canonical).with_context(|| {
        format!(
            "failed to read local source metadata: {}",
            canonical.display()
        )
    })?;
    if !metadata.is_file() {
        bail!("local source must be a file: {}", canonical.display());
    }
    if metadata.len() > LOCAL_FILE_MAX_BYTES {
        bail!(
            "local source file is too large: {} bytes (max {})",
            metadata.len(),
            LOCAL_FILE_MAX_BYTES
        );
    }

    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_default();
    if !is_supported_local_text_extension(&extension) {
        bail!(
            "unsupported local source file extension: {}; supported extensions are {}",
            if extension.is_empty() {
                "<none>"
            } else {
                extension.as_str()
            },
            supported_local_text_extensions().join(", ")
        );
    }

    let bytes = fs::read(&canonical)
        .with_context(|| format!("failed to read local source file: {}", canonical.display()))?;
    if bytes.is_empty() {
        bail!("local source file is empty: {}", canonical.display());
    }
    let raw_text = String::from_utf8(bytes).with_context(|| {
        format!(
            "local source file is not valid UTF-8: {}",
            canonical.display()
        )
    })?;
    let text = if matches!(extension.as_str(), "html" | "htm") {
        decode_response_text(&raw_text, Some("text/html"))
    } else {
        normalize_whitespace(&raw_text)
    };
    let normalized_text = normalize_source_text(&text);
    if normalized_text.trim().is_empty() {
        bail!(
            "local source file has no readable text after normalization: {}",
            canonical.display()
        );
    }

    let file_uri = file_uri_for_path(&canonical)?;
    let content_hash = sha256_hex(&normalized_text);
    let title = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned);

    Ok(FetchedSource {
        source_id: source_id_for_url(&file_uri),
        url: file_uri.clone(),
        final_url: file_uri,
        title,
        etag: None,
        last_modified: None,
        normalized_text,
        content_hash: content_hash.clone(),
        version_key: content_hash,
    })
}

pub fn normalize_source_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn source_id_for_url(url: &str) -> String {
    sha256_hex(url).chars().take(16).collect()
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn file_uri_for_path(path: &Path) -> Result<String> {
    reqwest::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| {
            anyhow::anyhow!(
                "failed to convert local path to file URI: {}",
                path.display()
            )
        })
}

fn is_supported_local_text_extension(extension: &str) -> bool {
    supported_local_text_extensions().contains(&extension)
}

fn supported_local_text_extensions() -> &'static [&'static str] {
    &[
        "md", "markdown", "txt", "html", "htm", "json", "csv", "tsv", "toml", "yaml", "yml",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_hash_ignores_whitespace_noise() {
        let left = sha256_hex(&normalize_source_text("hello\n\nworld"));
        let right = sha256_hex(&normalize_source_text("hello world"));
        assert_eq!(left, right);
    }

    #[test]
    fn detects_changed_content_hash() {
        let mut manifest = SourceManifest {
            schema_version: 1,
            sources: BTreeMap::new(),
            last_run_id: None,
        };
        let fetched = FetchedSource {
            source_id: "s1".to_string(),
            url: "https://example.com".to_string(),
            final_url: "https://example.com".to_string(),
            title: None,
            etag: None,
            last_modified: None,
            normalized_text: "one".to_string(),
            content_hash: "hash-one".to_string(),
            version_key: "hash-one".to_string(),
        };
        assert!(manifest.is_changed(&fetched));
        manifest.upsert_fetched(&fetched, Some("run"), None);
        assert!(!manifest.is_changed(&fetched));
    }

    #[test]
    fn local_file_source_uses_file_uri_and_hashes_text() {
        let root = unique_temp_dir("wiki-craft-local-source-test");
        fs::create_dir_all(&root).expect("test root");
        let path = root.join("notes.md");
        fs::write(&path, "# Notes\n\nhello\nworld").expect("source file");

        let fetched = fetched_from_local_file(&path).expect("local source");

        assert!(fetched.url.starts_with("file://"));
        assert_eq!(fetched.url, fetched.final_url);
        assert_eq!(fetched.source_id, source_id_for_url(&fetched.url));
        assert_eq!(fetched.title.as_deref(), Some("notes.md"));
        assert_eq!(fetched.normalized_text, "# Notes hello world");
        assert_eq!(fetched.content_hash, sha256_hex("# Notes hello world"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_file_source_rejects_unsupported_extension() {
        let root = unique_temp_dir("wiki-craft-local-source-extension-test");
        fs::create_dir_all(&root).expect("test root");
        let path = root.join("notes.bin");
        fs::write(&path, "hello").expect("source file");

        let error = fetched_from_local_file(&path).expect_err("unsupported extension");

        assert!(
            error
                .to_string()
                .contains("unsupported local source file extension")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_file_source_rejects_non_utf8() {
        let root = unique_temp_dir("wiki-craft-local-source-utf8-test");
        fs::create_dir_all(&root).expect("test root");
        let path = root.join("notes.txt");
        fs::write(&path, [0xff, 0xfe]).expect("source file");

        let error = fetched_from_local_file(&path).expect_err("non-utf8 source");

        assert!(error.to_string().contains("not valid UTF-8"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_file_source_rejects_empty_text() {
        let root = unique_temp_dir("wiki-craft-local-source-empty-test");
        fs::create_dir_all(&root).expect("test root");
        let path = root.join("notes.txt");
        fs::write(&path, " \n\t ").expect("source file");

        let error = fetched_from_local_file(&path).expect_err("empty source");

        assert!(error.to_string().contains("no readable text"));
        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("{}-{}", prefix, now_unix_ms()))
    }
}
