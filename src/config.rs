use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "wiki_craft.toml";
pub const DEFAULT_RUNTIME_ROOT: &str = ".wiki_craft";
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-flash";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    #[serde(default)]
    pub llm: LlmSettings,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub context_compact: ContextCompactConfig,
    #[serde(default)]
    pub prompt_cache: PromptCacheConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("failed to parse config: {}", path.display()))
    }

    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn enabled_sources(&self) -> Vec<&SourceConfig> {
        self.sources
            .iter()
            .filter(|source| source.enabled)
            .collect()
    }

    pub fn resolve_llm(&self) -> ResolvedLlmConfig {
        self.resolve_llm_with(|key| std::env::var(key).ok())
    }

    pub fn resolve_llm_with<F>(&self, env: F) -> ResolvedLlmConfig
    where
        F: Fn(&str) -> Option<String>,
    {
        let api_key = self
            .llm
            .api_key
            .clone()
            .filter(|value| non_empty(value))
            .or_else(|| env("LLM_API_KEY").filter(|value| non_empty(value)))
            .or_else(|| env("DEEPSEEK_API_KEY").filter(|value| non_empty(value)));
        let base_url = self
            .llm
            .base_url
            .clone()
            .filter(|value| non_empty(value))
            .or_else(|| env("LLM_BASE_URL").filter(|value| non_empty(value)))
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string());
        let model = self
            .llm
            .model
            .clone()
            .filter(|value| non_empty(value))
            .or_else(|| env("LLM_MODEL").filter(|value| non_empty(value)))
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_MODEL.to_string());

        ResolvedLlmConfig {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            max_tokens: self.llm.max_tokens,
            write_model_audit_log: self.audit.enabled || self.llm.write_model_audit_log,
            model_audit_log_path: if self.audit.enabled {
                self.audit.path.clone()
            } else {
                self.llm.model_audit_log_path.clone()
            },
            prompt_cache_enabled: self.prompt_cache.enabled,
            prompt_cache_dir: self.prompt_cache.dir.clone(),
            context_compact: self.context_compact.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_fetch_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_fetch_bytes")]
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u64,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            interval_minutes: default_interval_minutes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettings {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub write_model_audit_log: bool,
    #[serde(default = "default_audit_log_path")]
    pub model_audit_log_path: String,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: Some(DEFAULT_DEEPSEEK_BASE_URL.to_string()),
            model: Some(DEFAULT_DEEPSEEK_MODEL.to_string()),
            max_tokens: default_max_tokens(),
            write_model_audit_log: false,
            model_audit_log_path: default_audit_log_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    #[serde(default = "default_audit_log_path")]
    pub path: String,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            path: default_audit_log_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_runtime_root")]
    pub root: String,
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            root: default_runtime_root(),
            max_steps: default_max_steps(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompactConfig {
    #[serde(default = "default_context_compact_enabled")]
    pub enabled: bool,
    #[serde(default = "default_auto_compact_token_threshold")]
    pub auto_token_threshold: usize,
    #[serde(default = "default_auto_compact_preserve_recent_messages")]
    pub auto_preserve_recent_messages: usize,
    #[serde(default = "default_transcript_dir")]
    pub transcript_dir: String,
}

impl Default for ContextCompactConfig {
    fn default() -> Self {
        Self {
            enabled: default_context_compact_enabled(),
            auto_token_threshold: default_auto_compact_token_threshold(),
            auto_preserve_recent_messages: default_auto_compact_preserve_recent_messages(),
            transcript_dir: default_transcript_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptCacheConfig {
    #[serde(default = "default_prompt_cache_enabled")]
    pub enabled: bool,
    #[serde(default = "default_prompt_cache_dir")]
    pub dir: String,
}

impl Default for PromptCacheConfig {
    fn default() -> Self {
        Self {
            enabled: default_prompt_cache_enabled(),
            dir: default_prompt_cache_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    #[serde(default = "default_metrics_enabled")]
    pub enabled: bool,
    #[serde(default = "default_metrics_dir")]
    pub dir: String,
    #[serde(default = "default_metrics_http_bind")]
    pub http_bind: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: default_metrics_enabled(),
            dir: default_metrics_dir(),
            http_bind: default_metrics_http_bind(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedLlmConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub write_model_audit_log: bool,
    pub model_audit_log_path: String,
    pub prompt_cache_enabled: bool,
    pub prompt_cache_dir: String,
    pub context_compact: ContextCompactConfig,
}

pub fn default_config_toml() -> &'static str {
    r#"# Wiki Craft configuration.
# Add one or more enabled sources. v1 fetches only these entry URLs.
# LLM env vars follow scribe_engine: LLM_API_KEY, LLM_BASE_URL, LLM_MODEL.

[[sources]]
name = "example"
url = "https://example.com"
enabled = false
timeout_seconds = 15
max_bytes = 200000

[schedule]
interval_minutes = 60

[llm]
base_url = "https://api.deepseek.com"
model = "deepseek-v4-flash"
max_tokens = 4096

[audit]
enabled = true
path = ".wiki_craft/audit/events.jsonl"

[runtime]
root = ".wiki_craft"
max_steps = 8

[context_compact]
enabled = true
auto_token_threshold = 50000
auto_preserve_recent_messages = 4
transcript_dir = ".wiki_craft/transcripts"

[prompt_cache]
enabled = true
dir = ".wiki_craft/prompt_cache"

[metrics]
enabled = true
dir = ".wiki_craft/metrics"
http_bind = "127.0.0.1:9898"
"#
}

fn non_empty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn default_true() -> bool {
    true
}

fn default_interval_minutes() -> u64 {
    60
}

fn default_fetch_timeout_seconds() -> u64 {
    15
}

fn default_max_fetch_bytes() -> usize {
    200_000
}

fn default_runtime_root() -> String {
    DEFAULT_RUNTIME_ROOT.to_string()
}

fn default_max_steps() -> usize {
    8
}

fn default_context_compact_enabled() -> bool {
    true
}

fn default_auto_compact_token_threshold() -> usize {
    50_000
}

fn default_auto_compact_preserve_recent_messages() -> usize {
    4
}

fn default_transcript_dir() -> String {
    ".wiki_craft/transcripts".to_string()
}

fn default_prompt_cache_enabled() -> bool {
    true
}

fn default_prompt_cache_dir() -> String {
    ".wiki_craft/prompt_cache".to_string()
}

fn default_metrics_enabled() -> bool {
    true
}

fn default_metrics_dir() -> String {
    ".wiki_craft/metrics".to_string()
}

fn default_metrics_http_bind() -> String {
    "127.0.0.1:9898".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_audit_log_path() -> String {
    ".wiki_craft/audit/events.jsonl".to_string()
}

fn default_audit_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn deepseek_defaults_are_used() {
        let cfg = AppConfig::default();
        let resolved = cfg.resolve_llm_with(|_| None);

        assert_eq!(resolved.base_url, DEFAULT_DEEPSEEK_BASE_URL);
        assert_eq!(resolved.model, DEFAULT_DEEPSEEK_MODEL);
        assert!(resolved.api_key.is_none());
    }

    #[test]
    fn llm_key_wins_over_deepseek_compat_key() {
        let cfg = AppConfig::default();
        let mut env = BTreeMap::new();
        env.insert("DEEPSEEK_API_KEY".to_string(), "deepseek-key".to_string());
        env.insert("LLM_API_KEY".to_string(), "llm-key".to_string());

        let resolved = cfg.resolve_llm_with(|key| env.get(key).cloned());

        assert_eq!(resolved.api_key.as_deref(), Some("llm-key"));
    }
}
