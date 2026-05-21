use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::openai::ChatCompletionResult;
use crate::llm::usage::ModelUsage;

#[derive(Debug, Clone)]
pub struct PromptCache {
    dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptCacheEntry {
    created_at_unix_ms: u128,
    message: Value,
    usage: ModelUsage,
}

impl PromptCache {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        create_dir_all(&dir)
            .with_context(|| format!("failed to create prompt cache dir: {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn lookup(&self, key: &str) -> Result<Option<ChatCompletionResult>> {
        let path = self.entry_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let mut contents = String::new();
        File::open(&path)
            .with_context(|| format!("failed to open prompt cache entry: {}", path.display()))?
            .read_to_string(&mut contents)
            .with_context(|| format!("failed to read prompt cache entry: {}", path.display()))?;
        let entry: PromptCacheEntry = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse prompt cache entry: {}", path.display()))?;
        Ok(Some(ChatCompletionResult {
            message: entry.message,
            usage: entry.usage,
            cached: true,
        }))
    }

    pub fn store(&self, key: &str, response: &ChatCompletionResult) -> Result<()> {
        let entry = PromptCacheEntry {
            created_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock error")?
                .as_millis(),
            message: response.message.clone(),
            usage: response.usage.clone(),
        };
        let path = self.entry_path(key);
        let tmp_path = path.with_extension("json.tmp");
        create_dir_all(&self.dir).with_context(|| {
            format!("failed to create prompt cache dir: {}", self.dir.display())
        })?;
        let contents = serde_json::to_string_pretty(&entry)
            .context("failed to serialize prompt cache entry")?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .with_context(|| {
                format!(
                    "failed to create prompt cache temp file: {}",
                    tmp_path.display()
                )
            })?;
        file.write_all(contents.as_bytes()).with_context(|| {
            format!(
                "failed to write prompt cache temp file: {}",
                tmp_path.display()
            )
        })?;
        std::fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to persist prompt cache entry: {}", path.display()))?;
        Ok(())
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prompt_cache_roundtrips_response() {
        let dir = std::env::temp_dir().join(format!(
            "wiki-craft-prompt-cache-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let cache = PromptCache::new(&dir).expect("create cache");
        let response = ChatCompletionResult {
            message: json!({"role": "assistant", "content": "hello"}),
            usage: ModelUsage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
            cached: false,
        };
        cache.store("abc", &response).expect("store");
        let loaded = cache.lookup("abc").expect("lookup").expect("hit");
        assert!(loaded.cached);
        assert_eq!(loaded.message, response.message);
        assert_eq!(loaded.usage.input_tokens, 10);
        let _ = std::fs::remove_dir_all(dir);
    }
}
