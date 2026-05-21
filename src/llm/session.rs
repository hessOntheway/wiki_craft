use std::fs::{create_dir_all, read_to_string, write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::llm::usage::PromptCacheStats;

const SESSION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSessionSnapshot {
    pub schema_version: u32,
    pub session_id: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    #[serde(default)]
    pub prompt_history: Vec<String>,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub prompt_cache_stats: PromptCacheStats,
    #[serde(default)]
    pub compaction_count: u64,
}

pub struct ConversationSession {
    snapshot: ConversationSessionSnapshot,
    session_path: PathBuf,
}

impl ConversationSession {
    pub fn new_with_messages(
        session_id: String,
        messages: Vec<Value>,
        prompt_history: Vec<String>,
        sessions_dir: &Path,
    ) -> Result<Self> {
        let now = now_unix_ms()?;
        let snapshot = ConversationSessionSnapshot {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: session_id.clone(),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            prompt_history,
            messages,
            prompt_cache_stats: PromptCacheStats::default(),
            compaction_count: 0,
        };
        Ok(Self {
            snapshot,
            session_path: sessions_dir.join(format!("{session_id}.json")),
        })
    }

    pub fn simple(
        session_id: String,
        system: &str,
        user: &str,
        sessions_dir: &Path,
    ) -> Result<Self> {
        Self::new_with_messages(
            session_id,
            vec![
                json!({"role": "system", "content": system}),
                json!({"role": "user", "content": user}),
            ],
            vec![user.to_string()],
            sessions_dir,
        )
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let session_path = path.as_ref().to_path_buf();
        let contents = read_to_string(&session_path)
            .with_context(|| format!("failed to read session file: {}", session_path.display()))?;
        let snapshot: ConversationSessionSnapshot = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse session file: {}", session_path.display()))?;
        Ok(Self {
            snapshot,
            session_path,
        })
    }

    pub fn messages_and_prompt_cache_stats_mut(
        &mut self,
    ) -> (&mut Vec<Value>, &mut PromptCacheStats) {
        let snapshot = &mut self.snapshot;
        (&mut snapshot.messages, &mut snapshot.prompt_cache_stats)
    }

    pub fn record_compaction(&mut self) {
        self.snapshot.compaction_count += 1;
        self.snapshot.updated_at_unix_ms =
            now_unix_ms().unwrap_or(self.snapshot.updated_at_unix_ms);
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.session_path.parent()
            && !parent.as_os_str().is_empty()
        {
            create_dir_all(parent)
                .with_context(|| format!("failed to create session dir: {}", parent.display()))?;
        }
        let payload =
            serde_json::to_string_pretty(&self.snapshot).context("failed to serialize session")?;
        write(&self.session_path, payload)
            .with_context(|| format!("failed to write session: {}", self.session_path.display()))
    }

    pub fn snapshot(&self) -> &ConversationSessionSnapshot {
        &self.snapshot
    }
}

fn now_unix_ms() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock error")?
        .as_millis())
}
