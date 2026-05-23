use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::SourceConfig;
use crate::tools::WebFetchOutput;

const MANIFEST_SCHEMA_VERSION: u32 = 1;

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
    pub title: Option<String>,
    pub previous_hash: Option<String>,
    pub new_hash: String,
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
        self.sources
            .get(&fetched.source_id)
            .map(|record| record.content_hash != fetched.content_hash)
            .unwrap_or(true)
    }

    pub fn previous_hash(&self, source_id: &str) -> Option<String> {
        self.sources
            .get(source_id)
            .map(|record| record.content_hash.clone())
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
            },
        );
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
}
