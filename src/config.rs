use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "wiki_craft.toml";
pub const DEFAULT_INGEST_CONFIG_PATH: &str = "wiki_craft.ingest.toml";
pub const DEFAULT_RUNTIME_ROOT: &str = ".wiki_craft";
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-flash";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, skip)]
    pub ingest: IngestConfig,
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
        let mut config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        config.ingest = IngestConfig::load_or_default(ingest_config_path_for(path))?;
        Ok(config)
    }

    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            let mut config = Self::default();
            config.ingest = IngestConfig::load_or_default(ingest_config_path_for(path))?;
            Ok(config)
        }
    }

    pub fn enabled_sources(&self) -> Vec<&SourceConfig> {
        self.enabled_once_sources()
    }

    pub fn enabled_once_sources(&self) -> Vec<&SourceConfig> {
        self.ingest
            .once
            .sources
            .iter()
            .filter(|source| source.enabled)
            .collect()
    }

    pub fn enabled_cron_sources(&self) -> Vec<&SourceConfig> {
        self.ingest
            .cron
            .sources
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

impl IngestConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read ingest config: {}", path.display()))?;
        let file: IngestFileConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse ingest config: {}", path.display()))?;
        Ok(file.ingest)
    }

    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IngestFileConfig {
    #[serde(default)]
    ingest: IngestConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestConfig {
    #[serde(default)]
    pub once: OnceIngestConfig,
    #[serde(default)]
    pub cron: CronIngestConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnceIngestConfig {
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronIngestConfig {
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_fetch_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_fetch_bytes")]
    pub max_bytes: usize,
    #[serde(default)]
    pub interval_hours: Option<u64>,
}

impl SourceConfig {
    pub fn cron_interval_hours(&self) -> u64 {
        self.interval_hours
            .unwrap_or_else(default_cron_interval_hours)
            .max(1)
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
# Ingest sources live in wiki_craft.ingest.toml.
# LLM env vars follow scribe_engine: LLM_API_KEY, LLM_BASE_URL, LLM_MODEL.

[llm]
base_url = "https://api.deepseek.com"
model = "deepseek-v4-flash"
max_tokens = 4096

[audit]
enabled = true
path = ".wiki_craft/runtime/audit/events.jsonl"

[runtime]
root = ".wiki_craft"
max_steps = 8

[context_compact]
enabled = true
auto_token_threshold = 50000
auto_preserve_recent_messages = 4
transcript_dir = ".wiki_craft/runtime/transcripts"

[prompt_cache]
enabled = true
dir = ".wiki_craft/runtime/prompt_cache"

[metrics]
enabled = true
dir = ".wiki_craft/runtime/metrics"
http_bind = "127.0.0.1:9898"
"#
}

pub fn default_ingest_config_toml() -> &'static str {
    r#"# Wiki Craft ingest sources.
# `cargo run -- ingest --once` reads ingest.once.sources.
# `cargo run -- serve` reads ingest.cron.sources.

[ingest.once]

[[ingest.once.sources]]
url = "https://example.com/once"
enabled = false
timeout_seconds = 15
max_bytes = 200000

[ingest.cron]

[[ingest.cron.sources]]
url = "https://example.com/cron"
enabled = false
interval_hours = 24
timeout_seconds = 15
max_bytes = 200000
"#
}

pub fn ingest_config_path_for(config_path: &Path) -> std::path::PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join(DEFAULT_INGEST_CONFIG_PATH))
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_INGEST_CONFIG_PATH))
}

fn non_empty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn default_true() -> bool {
    true
}

fn default_cron_interval_hours() -> u64 {
    24
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
    ".wiki_craft/runtime/transcripts".to_string()
}

fn default_prompt_cache_enabled() -> bool {
    true
}

fn default_prompt_cache_dir() -> String {
    ".wiki_craft/runtime/prompt_cache".to_string()
}

fn default_metrics_enabled() -> bool {
    true
}

fn default_metrics_dir() -> String {
    ".wiki_craft/runtime/metrics".to_string()
}

fn default_metrics_http_bind() -> String {
    "127.0.0.1:9898".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_audit_log_path() -> String {
    ".wiki_craft/runtime/audit/events.jsonl".to_string()
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

    #[test]
    fn parses_ingest_source_groups() {
        let file: IngestFileConfig = toml::from_str(
            r#"
[ingest.once]

[[ingest.once.sources]]
url = "https://example.test/once"
enabled = true
timeout_seconds = 5
max_bytes = 1000

[ingest.cron]

[[ingest.cron.sources]]
url = "https://example.test/cron"
enabled = true
interval_hours = 6
timeout_seconds = 5
max_bytes = 1000

[[ingest.cron.sources]]
url = "https://example.test/disabled"
enabled = false
"#,
        )
        .expect("ingest config");

        let cfg = AppConfig {
            ingest: file.ingest,
            ..Default::default()
        };
        assert_eq!(cfg.enabled_once_sources().len(), 1);
        assert_eq!(cfg.enabled_cron_sources().len(), 1);
        assert_eq!(cfg.ingest.cron.sources[0].cron_interval_hours(), 6);
    }

    #[test]
    fn app_config_loads_sibling_ingest_config() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-config-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        fs::write(&config_path, "[runtime]\nroot = \".wiki_craft\"\n").expect("main config");
        fs::write(
            root.join(DEFAULT_INGEST_CONFIG_PATH),
            "[ingest.once]\n\n[[ingest.once.sources]]\nurl = \"https://example.test/once\"\n",
        )
        .expect("ingest config");

        let cfg = AppConfig::load(&config_path).expect("config");

        assert_eq!(cfg.ingest.once.sources.len(), 1);
        assert_eq!(cfg.ingest.once.sources[0].url, "https://example.test/once");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cron_interval_defaults_to_one_day() {
        let source = SourceConfig {
            url: "https://example.test/cron".to_string(),
            enabled: true,
            timeout_seconds: 5,
            max_bytes: 1000,
            interval_hours: None,
        };

        assert_eq!(source.cron_interval_hours(), 24);
    }
}
