use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "wiki_craft.toml";
pub const DEFAULT_RUNTIME_ROOT: &str = ".wiki_craft";
pub const KNOWLEDGE_BASES_DIR: &str = "knowledge_bases";
pub const KNOWLEDGE_BASE_REGISTRY_FILE: &str = "registry.json";
pub const KNOWLEDGE_BASE_CONFIG_FILE: &str = "knowledge_base.toml";
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-flash";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, skip)]
    pub ingest: IngestConfig,
    #[serde(default, skip)]
    pub knowledge_base: Option<ActiveKnowledgeBase>,
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
        let mut config = Self::load_global(path)?;
        config.load_active_knowledge_base()?;
        Ok(config)
    }

    pub fn load_global(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let mut config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        config.resolve_relative_paths(path);
        Ok(config)
    }

    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            let mut config = Self::default();
            config.load_active_knowledge_base()?;
            Ok(config)
        }
    }

    fn load_active_knowledge_base(&mut self) -> Result<()> {
        let registry = KnowledgeBaseRegistry::load(&self.knowledge_base_registry_path())?;
        let Some(active_id) = registry.active_id.as_deref() else {
            self.ingest = IngestConfig::default();
            self.knowledge_base = None;
            return Ok(());
        };
        self.load_knowledge_base_from_registry(&registry, active_id)
    }

    pub fn select_knowledge_base(&mut self, id: &str) -> Result<()> {
        let registry = KnowledgeBaseRegistry::load(&self.knowledge_base_registry_path())?;
        self.load_knowledge_base_from_registry(&registry, id)
    }

    fn load_knowledge_base_from_registry(
        &mut self,
        registry: &KnowledgeBaseRegistry,
        id: &str,
    ) -> Result<()> {
        let record = registry
            .knowledge_bases
            .iter()
            .find(|record| record.id == id)
            .with_context(|| format!("knowledge base not found in registry: {id}"))?;
        let file = KnowledgeBaseFileConfig::load(
            &PathBuf::from(&record.root).join(KNOWLEDGE_BASE_CONFIG_FILE),
        )?;
        self.ingest = file.ingest;
        self.knowledge_base = Some(ActiveKnowledgeBase {
            id: record.id.clone(),
            name: file.name,
            focus: file.focus,
            root: record.root.clone(),
        });
        Ok(())
    }

    pub fn knowledge_bases_root(&self) -> PathBuf {
        PathBuf::from(&self.runtime.root).join(KNOWLEDGE_BASES_DIR)
    }

    pub fn knowledge_base_registry_path(&self) -> PathBuf {
        self.knowledge_bases_root()
            .join(KNOWLEDGE_BASE_REGISTRY_FILE)
    }

    pub fn active_knowledge_base(&self) -> Result<&ActiveKnowledgeBase> {
        self.knowledge_base.as_ref().context(
            "no active knowledge base; create one in the GUI or run `knowledge-base create`",
        )
    }

    fn resolve_relative_paths(&mut self, config_path: &Path) {
        let base = config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        self.runtime.root = resolve_config_relative_path(base, &self.runtime.root);
        self.audit.path = resolve_config_relative_path(base, &self.audit.path);
        self.context_compact.transcript_dir =
            resolve_config_relative_path(base, &self.context_compact.transcript_dir);
        self.prompt_cache.dir = resolve_config_relative_path(base, &self.prompt_cache.dir);
        self.metrics.dir = resolve_config_relative_path(base, &self.metrics.dir);
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
    pub fn is_empty(&self) -> bool {
        self.once.sources.is_empty() && self.cron.sources.is_empty()
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveKnowledgeBase {
    pub id: String,
    pub name: String,
    pub focus: String,
    pub root: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeBaseRegistry {
    pub schema_version: u32,
    #[serde(default)]
    pub active_id: Option<String>,
    #[serde(default)]
    pub knowledge_bases: Vec<KnowledgeBaseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseRecord {
    pub id: String,
    pub name: String,
    pub focus: String,
    pub root: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseFileConfig {
    pub name: String,
    pub focus: String,
    #[serde(default)]
    pub ingest: IngestConfig,
}

impl KnowledgeBaseRegistry {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                schema_version: 1,
                active_id: None,
                knowledge_bases: Vec::new(),
            });
        }
        let content = fs::read_to_string(path).with_context(|| {
            format!("failed to read knowledge base registry: {}", path.display())
        })?;
        let mut registry: Self = serde_json::from_str(&content).with_context(|| {
            format!(
                "failed to parse knowledge base registry: {}",
                path.display()
            )
        })?;
        if registry.schema_version == 0 {
            registry.schema_version = 1;
        }
        Ok(registry)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create knowledge base registry dir: {}",
                    parent.display()
                )
            })?;
        }
        let content = serde_json::to_string_pretty(self)
            .context("failed to serialize knowledge base registry")?;
        fs::write(path, content).with_context(|| {
            format!(
                "failed to write knowledge base registry: {}",
                path.display()
            )
        })
    }
}

impl KnowledgeBaseFileConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read knowledge base config: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("failed to parse knowledge base config: {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create knowledge base config dir: {}",
                    parent.display()
                )
            })?;
        }
        let content =
            toml::to_string_pretty(self).context("failed to serialize knowledge base config")?;
        fs::write(path, content)
            .with_context(|| format!("failed to write knowledge base config: {}", path.display()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseCreateInput {
    pub name: String,
    pub focus: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseDeleteInput {
    pub confirmation_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseList {
    pub active_id: Option<String>,
    pub knowledge_bases: Vec<KnowledgeBaseRecord>,
}

pub fn list_knowledge_bases(config_path: &Path) -> Result<KnowledgeBaseList> {
    let config = AppConfig::load_global(config_path)?;
    let registry = KnowledgeBaseRegistry::load(&config.knowledge_base_registry_path())?;
    Ok(KnowledgeBaseList {
        active_id: registry.active_id,
        knowledge_bases: registry.knowledge_bases,
    })
}

pub fn create_knowledge_base(
    config_path: &Path,
    input: KnowledgeBaseCreateInput,
) -> Result<KnowledgeBaseRecord> {
    let config = AppConfig::load_global(config_path)?;
    let name = non_empty_trimmed("knowledge base name", &input.name)?;
    let focus = non_empty_trimmed("knowledge base focus", &input.focus)?;
    let now = now_unix_ms();
    let id = unique_knowledge_base_id(&name, now);
    let root = config.knowledge_bases_root().join(&id);
    let record = KnowledgeBaseRecord {
        id: id.clone(),
        name: name.clone(),
        focus: focus.clone(),
        root: root.display().to_string(),
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
    };

    fs::create_dir_all(root.join("knowledge").join("approved").join("topics"))
        .with_context(|| format!("failed to create knowledge base root: {}", root.display()))?;
    fs::create_dir_all(
        root.join("knowledge")
            .join("approved")
            .join("evidence")
            .join("source_summaries"),
    )
    .with_context(|| {
        format!(
            "failed to create knowledge base evidence dirs: {}",
            root.display()
        )
    })?;
    fs::create_dir_all(
        root.join("knowledge")
            .join("approved")
            .join("evidence")
            .join("sources"),
    )
    .with_context(|| {
        format!(
            "failed to create knowledge base source dirs: {}",
            root.display()
        )
    })?;
    fs::create_dir_all(root.join("knowledge").join("staging").join("candidates")).with_context(
        || {
            format!(
                "failed to create knowledge base staging dirs: {}",
                root.display()
            )
        },
    )?;
    fs::create_dir_all(root.join("runtime")).with_context(|| {
        format!(
            "failed to create knowledge base runtime dir: {}",
            root.display()
        )
    })?;

    let kb_config = KnowledgeBaseFileConfig {
        name,
        focus: focus.clone(),
        ingest: IngestConfig::default(),
    };
    kb_config.save(&root.join(KNOWLEDGE_BASE_CONFIG_FILE))?;
    let index = format!(
        "---\ntitle: \"{}\"\naliases: []\ntags: [index]\nsource_ids: []\nsource_urls: []\nversion_hashes: []\n---\n\n# {}\n\nFocus: {}\n",
        escape_toml_like(&record.name),
        record.name,
        focus
    );
    fs::write(
        root.join("knowledge").join("approved").join("index.md"),
        index,
    )
    .with_context(|| format!("failed to write knowledge base index: {}", root.display()))?;

    let registry_path = config.knowledge_base_registry_path();
    let mut registry = KnowledgeBaseRegistry::load(&registry_path)?;
    registry.knowledge_bases.push(record.clone());
    registry.active_id = Some(id);
    registry.save(&registry_path)?;
    Ok(record)
}

pub fn activate_knowledge_base(config_path: &Path, id: &str) -> Result<KnowledgeBaseRecord> {
    let config = AppConfig::load_global(config_path)?;
    let trimmed = non_empty_trimmed("knowledge base id", id)?;
    let registry_path = config.knowledge_base_registry_path();
    let mut registry = KnowledgeBaseRegistry::load(&registry_path)?;
    let record = registry
        .knowledge_bases
        .iter()
        .find(|record| record.id == trimmed)
        .cloned()
        .with_context(|| format!("knowledge base not found: {trimmed}"))?;
    registry.active_id = Some(record.id.clone());
    registry.save(&registry_path)?;
    Ok(record)
}

pub fn delete_knowledge_base(
    config_path: &Path,
    id: &str,
    input: KnowledgeBaseDeleteInput,
) -> Result<KnowledgeBaseList> {
    let config = AppConfig::load_global(config_path)?;
    let trimmed = non_empty_trimmed("knowledge base id", id)?;
    let registry_path = config.knowledge_base_registry_path();
    let mut registry = KnowledgeBaseRegistry::load(&registry_path)?;
    let index = registry
        .knowledge_bases
        .iter()
        .position(|record| record.id == trimmed)
        .with_context(|| format!("knowledge base not found: {trimmed}"))?;
    let record = registry.knowledge_bases[index].clone();
    if input.confirmation_name.trim() != record.name {
        anyhow::bail!("knowledge base name confirmation did not match");
    }

    let root = PathBuf::from(&record.root);
    ensure_deletable_knowledge_base_root(&root, &config.knowledge_bases_root())?;
    if root.exists() {
        fs::remove_dir_all(&root).with_context(|| {
            format!(
                "failed to delete knowledge base directory: {}",
                root.display()
            )
        })?;
    }

    registry.knowledge_bases.remove(index);
    if registry.active_id.as_deref() == Some(record.id.as_str()) {
        registry.active_id = registry
            .knowledge_bases
            .first()
            .map(|knowledge_base| knowledge_base.id.clone());
    }
    registry.save(&registry_path)?;

    Ok(KnowledgeBaseList {
        active_id: registry.active_id,
        knowledge_bases: registry.knowledge_bases,
    })
}

fn non_empty_trimmed(label: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{label} must not be empty");
    }
    Ok(trimmed.to_string())
}

fn ensure_deletable_knowledge_base_root(root: &Path, knowledge_bases_root: &Path) -> Result<()> {
    if contains_parent_component(root) || contains_parent_component(knowledge_bases_root) {
        anyhow::bail!("knowledge base root contains unsupported parent path components");
    }

    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let absolute_root = absolutize(&cwd, root);
    let absolute_base = absolutize(&cwd, knowledge_bases_root);
    if absolute_root == absolute_base || !absolute_root.starts_with(&absolute_base) {
        anyhow::bail!(
            "refusing to delete knowledge base outside configured knowledge_bases root: {}",
            root.display()
        );
    }

    if absolute_root.exists() {
        let canonical_root = fs::canonicalize(&absolute_root).with_context(|| {
            format!(
                "failed to resolve knowledge base root: {}",
                absolute_root.display()
            )
        })?;
        let canonical_base = fs::canonicalize(&absolute_base).with_context(|| {
            format!(
                "failed to resolve configured knowledge_bases root: {}",
                absolute_base.display()
            )
        })?;
        if canonical_root == canonical_base || !canonical_root.starts_with(&canonical_base) {
            anyhow::bail!(
                "refusing to delete knowledge base outside configured knowledge_bases root: {}",
                root.display()
            );
        }
    }

    Ok(())
}

fn absolutize(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn contains_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn resolve_config_relative_path(base: &Path, value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return value.to_string();
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return trimmed.to_string();
    }
    base.join(path).display().to_string()
}

fn unique_knowledge_base_id(name: &str, now: u128) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() {
        "knowledge-base"
    } else {
        slug
    };
    format!("{slug}-{now}")
}

fn escape_toml_like(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
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
# Knowledge base sources live in each .wiki_craft/knowledge_bases/{id}/knowledge_base.toml.
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
        let file: KnowledgeBaseFileConfig = toml::from_str(
            r#"
name = "Docs"
focus = "Rust docs"

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
    fn app_config_loads_active_knowledge_base_config() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-config-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        let kb_root = runtime_root.join(KNOWLEDGE_BASES_DIR).join("docs");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");
        let registry = KnowledgeBaseRegistry {
            schema_version: 1,
            active_id: Some("docs".to_string()),
            knowledge_bases: vec![KnowledgeBaseRecord {
                id: "docs".to_string(),
                name: "Docs".to_string(),
                focus: "Rust docs".to_string(),
                root: kb_root.display().to_string(),
                created_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            }],
        };
        registry
            .save(
                &runtime_root
                    .join(KNOWLEDGE_BASES_DIR)
                    .join(KNOWLEDGE_BASE_REGISTRY_FILE),
            )
            .expect("registry");
        KnowledgeBaseFileConfig {
            name: "Docs".to_string(),
            focus: "Rust docs".to_string(),
            ingest: toml::from_str::<KnowledgeBaseFileConfig>(
                "name = \"Docs\"\nfocus = \"Rust docs\"\n[ingest.once]\n\n[[ingest.once.sources]]\nurl = \"https://example.test/once\"\n",
            )
            .expect("kb config")
            .ingest,
        }
        .save(&kb_root.join(KNOWLEDGE_BASE_CONFIG_FILE))
        .expect("knowledge base config");

        let cfg = AppConfig::load(&config_path).expect("config");

        assert_eq!(cfg.ingest.once.sources.len(), 1);
        assert_eq!(cfg.ingest.once.sources[0].url, "https://example.test/once");
        assert_eq!(
            cfg.active_knowledge_base().expect("active").focus,
            "Rust docs"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_and_activate_knowledge_base_roundtrips_registry() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-kb-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");

        let first = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Product Research".to_string(),
                focus: "Pricing and integration decisions".to_string(),
            },
        )
        .expect("create first");
        let second = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Engineering Notes".to_string(),
                focus: "Implementation details".to_string(),
            },
        )
        .expect("create second");

        let listed = list_knowledge_bases(&config_path).expect("list");
        assert_eq!(listed.active_id.as_deref(), Some(second.id.as_str()));
        assert_eq!(listed.knowledge_bases.len(), 2);

        let activated = activate_knowledge_base(&config_path, &first.id).expect("activate");
        assert_eq!(activated.id, first.id);
        let cfg = AppConfig::load(&config_path).expect("config");
        assert_eq!(
            cfg.active_knowledge_base().expect("active").focus,
            "Pricing and integration decisions"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_inactive_knowledge_base_removes_dir_and_keeps_active() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-kb-delete-inactive-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");

        let first = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "First".to_string(),
                focus: "First focus".to_string(),
            },
        )
        .expect("create first");
        let second = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Second".to_string(),
                focus: "Second focus".to_string(),
            },
        )
        .expect("create second");
        let first_root = PathBuf::from(&first.root);

        let listed = delete_knowledge_base(
            &config_path,
            &first.id,
            KnowledgeBaseDeleteInput {
                confirmation_name: "First".to_string(),
            },
        )
        .expect("delete");

        assert!(!first_root.exists());
        assert_eq!(listed.active_id.as_deref(), Some(second.id.as_str()));
        assert_eq!(listed.knowledge_bases.len(), 1);
        assert_eq!(listed.knowledge_bases[0].id, second.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_active_knowledge_base_selects_first_remaining() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-kb-delete-active-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");

        let first = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "First".to_string(),
                focus: "First focus".to_string(),
            },
        )
        .expect("create first");
        let second = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Second".to_string(),
                focus: "Second focus".to_string(),
            },
        )
        .expect("create second");

        let listed = delete_knowledge_base(
            &config_path,
            &second.id,
            KnowledgeBaseDeleteInput {
                confirmation_name: "Second".to_string(),
            },
        )
        .expect("delete active");

        assert_eq!(listed.active_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(listed.knowledge_bases.len(), 1);
        assert_eq!(listed.knowledge_bases[0].id, first.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_last_knowledge_base_clears_active() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-kb-delete-last-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");

        let only = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Only".to_string(),
                focus: "Only focus".to_string(),
            },
        )
        .expect("create only");

        let listed = delete_knowledge_base(
            &config_path,
            &only.id,
            KnowledgeBaseDeleteInput {
                confirmation_name: "Only".to_string(),
            },
        )
        .expect("delete only");

        assert_eq!(listed.active_id, None);
        assert!(listed.knowledge_bases.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_knowledge_base_rejects_mismatched_confirmation() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-kb-delete-confirm-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");

        let only = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Exact Name".to_string(),
                focus: "Focus".to_string(),
            },
        )
        .expect("create only");
        let kb_root = PathBuf::from(&only.root);

        let result = delete_knowledge_base(
            &config_path,
            &only.id,
            KnowledgeBaseDeleteInput {
                confirmation_name: "exact name".to_string(),
            },
        );

        assert!(result.is_err());
        assert!(kb_root.exists());
        let listed = list_knowledge_bases(&config_path).expect("list");
        assert_eq!(listed.active_id.as_deref(), Some(only.id.as_str()));
        assert_eq!(listed.knowledge_bases.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_knowledge_base_cleans_registry_when_dir_missing() {
        let root = std::env::temp_dir().join(format!(
            "wiki-craft-kb-delete-missing-dir-test-{}",
            crate::support::now_unix_ms()
        ));
        fs::create_dir_all(&root).expect("temp dir");
        let config_path = root.join(DEFAULT_CONFIG_PATH);
        let runtime_root = root.join(".wiki_craft");
        fs::write(
            &config_path,
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .expect("main config");

        let only = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Missing Dir".to_string(),
                focus: "Focus".to_string(),
            },
        )
        .expect("create only");
        fs::remove_dir_all(&only.root).expect("manual delete");

        let listed = delete_knowledge_base(
            &config_path,
            &only.id,
            KnowledgeBaseDeleteInput {
                confirmation_name: "Missing Dir".to_string(),
            },
        )
        .expect("delete missing dir");

        assert_eq!(listed.active_id, None);
        assert!(listed.knowledge_bases.is_empty());
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
