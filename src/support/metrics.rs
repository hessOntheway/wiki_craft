use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::knowledge::WorkspacePaths;
use crate::llm::usage::PromptCacheStats;

const METRICS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub schema_version: u32,
    pub updated_at_unix_ms: u128,
    pub pending_candidates: usize,
    pub last_run_kind: Option<String>,
    pub last_run_checked_sources: usize,
    pub last_run_changed_sources: usize,
    pub llm: LlmMetrics,
    pub cache: CacheMetrics,
    pub compaction: CompactionMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmMetrics {
    pub requests_total: u64,
    pub input_tokens_total: u64,
    pub output_tokens_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheMetrics {
    pub local_hits_total: u64,
    pub local_misses_total: u64,
    pub provider_hit_tokens_total: u64,
    pub provider_miss_tokens_total: u64,
    pub provider_hit_rate: Option<f64>,
    pub cache_creation_input_tokens_total: u64,
    pub cache_read_input_tokens_total: u64,
    pub prompt_cache_hit_tokens_total: u64,
    pub prompt_cache_miss_tokens_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompactionMetrics {
    pub compactions_total: u64,
}

#[derive(Debug, Clone)]
pub struct MetricsInput {
    pub pending_candidates: usize,
    pub last_run_kind: Option<String>,
    pub last_run_checked_sources: usize,
    pub last_run_changed_sources: usize,
    pub prompt_cache_stats: PromptCacheStats,
    pub compaction_count: u64,
}

impl MetricsSnapshot {
    pub fn from_input(input: MetricsInput) -> Self {
        let provider_hit_tokens = input.prompt_cache_stats.total_hit_tokens();
        let provider_miss_tokens = input.prompt_cache_stats.total_miss_tokens();
        Self {
            schema_version: METRICS_SCHEMA_VERSION,
            updated_at_unix_ms: now_unix_ms(),
            pending_candidates: input.pending_candidates,
            last_run_kind: input.last_run_kind,
            last_run_checked_sources: input.last_run_checked_sources,
            last_run_changed_sources: input.last_run_changed_sources,
            llm: LlmMetrics {
                requests_total: input.prompt_cache_stats.total_model_calls,
                input_tokens_total: input.prompt_cache_stats.total_input_tokens,
                output_tokens_total: input.prompt_cache_stats.total_output_tokens,
            },
            cache: CacheMetrics {
                local_hits_total: input.prompt_cache_stats.total_local_cache_hits,
                local_misses_total: input.prompt_cache_stats.total_model_calls,
                provider_hit_tokens_total: provider_hit_tokens,
                provider_miss_tokens_total: provider_miss_tokens,
                provider_hit_rate: ratio(provider_hit_tokens, provider_miss_tokens),
                cache_creation_input_tokens_total: input
                    .prompt_cache_stats
                    .total_cache_creation_input_tokens,
                cache_read_input_tokens_total: input
                    .prompt_cache_stats
                    .total_cache_read_input_tokens,
                prompt_cache_hit_tokens_total: input
                    .prompt_cache_stats
                    .total_prompt_cache_hit_tokens,
                prompt_cache_miss_tokens_total: input
                    .prompt_cache_stats
                    .total_prompt_cache_miss_tokens,
            },
            compaction: CompactionMetrics {
                compactions_total: input.compaction_count,
            },
        }
    }
}

pub fn write_metrics(paths: &WorkspacePaths, snapshot: &MetricsSnapshot) -> Result<()> {
    fs::create_dir_all(&paths.metrics_dir).with_context(|| {
        format!(
            "failed to create metrics dir: {}",
            paths.metrics_dir.display()
        )
    })?;
    let json =
        serde_json::to_vec_pretty(snapshot).context("failed to serialize metrics snapshot")?;
    fs::write(&paths.metrics_latest_path, json).with_context(|| {
        format!(
            "failed to write metrics snapshot: {}",
            paths.metrics_latest_path.display()
        )
    })?;

    let line = serde_json::to_string(snapshot).context("failed to encode metrics event")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.metrics_events_path)
        .with_context(|| {
            format!(
                "failed to open metrics event log: {}",
                paths.metrics_events_path.display()
            )
        })?;
    writeln!(file, "{line}").context("failed to append metrics event")?;
    Ok(())
}

pub fn read_metrics(path: &Path) -> Result<Option<MetricsSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read metrics snapshot: {}", path.display()))?;
    serde_json::from_str(&content)
        .map(Some)
        .with_context(|| format!("failed to parse metrics snapshot: {}", path.display()))
}

pub fn render_prometheus(snapshot: &MetricsSnapshot) -> String {
    let mut lines = Vec::new();
    push_help(
        &mut lines,
        "wiki_craft_llm_requests_total",
        "Total model requests sent to the provider.",
        "counter",
        snapshot.llm.requests_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_llm_input_tokens_total",
        snapshot.llm.input_tokens_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_llm_output_tokens_total",
        snapshot.llm.output_tokens_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_prompt_cache_local_hits_total",
        snapshot.cache.local_hits_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_prompt_cache_local_misses_total",
        snapshot.cache.local_misses_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_prompt_cache_provider_hit_tokens_total",
        snapshot.cache.provider_hit_tokens_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_prompt_cache_provider_miss_tokens_total",
        snapshot.cache.provider_miss_tokens_total,
    );
    lines.push(
        "# HELP wiki_craft_prompt_cache_provider_hit_rate Provider cache hit token ratio."
            .to_string(),
    );
    lines.push("# TYPE wiki_craft_prompt_cache_provider_hit_rate gauge".to_string());
    lines.push(format!(
        "wiki_craft_prompt_cache_provider_hit_rate {}",
        snapshot.cache.provider_hit_rate.unwrap_or(0.0)
    ));
    push_metric(
        &mut lines,
        "wiki_craft_prompt_cache_creation_input_tokens_total",
        snapshot.cache.cache_creation_input_tokens_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_prompt_cache_read_input_tokens_total",
        snapshot.cache.cache_read_input_tokens_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_compactions_total",
        snapshot.compaction.compactions_total,
    );
    push_metric(
        &mut lines,
        "wiki_craft_pending_candidates",
        snapshot.pending_candidates as u64,
    );
    push_metric(
        &mut lines,
        "wiki_craft_last_run_checked_sources",
        snapshot.last_run_checked_sources as u64,
    );
    push_metric(
        &mut lines,
        "wiki_craft_last_run_changed_sources",
        snapshot.last_run_changed_sources as u64,
    );
    lines.push(String::new());
    lines.join("\n")
}

fn push_help(lines: &mut Vec<String>, name: &str, help: &str, kind: &str, value: u64) {
    lines.push(format!("# HELP {name} {help}"));
    lines.push(format!("# TYPE {name} {kind}"));
    lines.push(format!("{name} {value}"));
}

fn push_metric(lines: &mut Vec<String>, name: &str, value: u64) {
    lines.push(format!("{name} {value}"));
}

fn ratio(hit_tokens: u64, miss_tokens: u64) -> Option<f64> {
    let total = hit_tokens + miss_tokens;
    if total == 0 {
        None
    } else {
        Some(hit_tokens as f64 / total as f64)
    }
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
    fn snapshot_computes_cache_rate() {
        let stats = PromptCacheStats {
            total_cache_read_input_tokens: 80,
            total_cache_creation_input_tokens: 20,
            ..Default::default()
        };
        let snapshot = MetricsSnapshot::from_input(MetricsInput {
            pending_candidates: 1,
            last_run_kind: Some("candidate_created".to_string()),
            last_run_checked_sources: 2,
            last_run_changed_sources: 1,
            prompt_cache_stats: stats,
            compaction_count: 3,
        });
        assert_eq!(snapshot.cache.provider_hit_rate, Some(0.8));
    }

    #[test]
    fn prometheus_render_contains_core_metrics() {
        let snapshot = MetricsSnapshot::from_input(MetricsInput {
            pending_candidates: 0,
            last_run_kind: None,
            last_run_checked_sources: 0,
            last_run_changed_sources: 0,
            prompt_cache_stats: PromptCacheStats::default(),
            compaction_count: 0,
        });
        let rendered = render_prometheus(&snapshot);
        assert!(rendered.contains("wiki_craft_llm_requests_total"));
        assert!(rendered.contains("wiki_craft_prompt_cache_provider_hit_rate"));
    }
}
