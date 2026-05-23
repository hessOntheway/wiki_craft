use std::fs::{File, create_dir_all};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config::ContextCompactConfig;
use crate::llm::openai::{OpenAiCompatClient, extract_message_text};
use crate::llm::usage::PromptCacheStats;
use crate::tools::ToolDefinition;

const CONTINUATION_PREFIX: &str = "[Context compacted]";
const CONTINUATION_SUFFIX: &str = "Continue from this summary and the recent messages.";
const MAX_SUMMARY_SOURCE_CHARS: usize = 60_000;

#[derive(Debug, Clone)]
pub struct AutoCompactEvent {
    pub removed_messages: usize,
    pub estimated_tokens_before: usize,
    pub transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactPlan {
    compacted_prefix_len: usize,
    keep_from: usize,
    removed_messages: usize,
    estimated_tokens_before: usize,
}

pub fn estimate_messages_tokens(messages: &[Value]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

pub fn auto_compact_if_needed(
    messages: &mut Vec<Value>,
    cfg: &ContextCompactConfig,
    llm: &OpenAiCompatClient,
    audit_log_path_override: Option<&str>,
    tool_definitions: &[ToolDefinition],
    prompt_cache_stats: Option<&mut PromptCacheStats>,
) -> Result<Option<AutoCompactEvent>> {
    if !cfg.enabled || cfg.auto_token_threshold == 0 {
        return Ok(None);
    }
    let Some(plan) = plan_auto_compact(messages, cfg) else {
        return Ok(None);
    };

    let transcript_path = match backup_transcript(messages, &cfg.transcript_dir) {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("warn: failed to save compact transcript: {error}");
            None
        }
    };
    let removed = messages[plan.compacted_prefix_len..plan.keep_from].to_vec();
    let summary_source = build_compact_summary_source(&removed);
    let new_summary = match generate_compact_summary(
        llm,
        messages,
        &summary_source,
        tool_definitions,
        audit_log_path_override,
        prompt_cache_stats,
    ) {
        Ok(summary) => summary,
        Err(error) => {
            eprintln!("warn: failed to generate compact summary: {error}");
            return Ok(None);
        }
    };

    let existing_summary = messages
        .first()
        .and_then(extract_existing_compacted_summary);
    let summary = merge_compact_summaries(existing_summary.as_deref(), &new_summary);
    let preserved = messages.get(plan.keep_from..).unwrap_or_default();
    let mut next_messages = Vec::with_capacity(preserved.len() + 1);
    next_messages.push(json!({
        "role": "system",
        "content": format!("{CONTINUATION_PREFIX}\n\n{summary}\n\n{CONTINUATION_SUFFIX}")
    }));
    next_messages.extend_from_slice(preserved);
    *messages = next_messages;

    Ok(Some(AutoCompactEvent {
        removed_messages: plan.removed_messages,
        estimated_tokens_before: plan.estimated_tokens_before,
        transcript_path,
    }))
}

pub fn remove_orphan_tool_messages(messages: &mut Vec<Value>) -> usize {
    let mut cleaned = Vec::with_capacity(messages.len());
    let mut pending_tool_call_ids = Vec::<String>::new();
    let mut removed = 0usize;

    for message in messages.drain(..) {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "tool" {
            let tool_call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(position) = pending_tool_call_ids
                .iter()
                .position(|id| id.as_str() == tool_call_id)
            {
                pending_tool_call_ids.remove(position);
                cleaned.push(message);
            } else {
                removed += 1;
            }
            continue;
        }

        if !pending_tool_call_ids.is_empty() {
            if let Some(previous) = cleaned.last_mut()
                && previous.get("role").and_then(Value::as_str) == Some("assistant")
                && previous.get("tool_calls").is_some()
                && let Some(obj) = previous.as_object_mut()
            {
                obj.remove("tool_calls");
            }
            pending_tool_call_ids.clear();
        }

        pending_tool_call_ids = assistant_tool_call_ids(&message);
        cleaned.push(message);
    }

    if !pending_tool_call_ids.is_empty()
        && let Some(previous) = cleaned.last_mut()
        && previous.get("role").and_then(Value::as_str) == Some("assistant")
        && previous.get("tool_calls").is_some()
        && let Some(obj) = previous.as_object_mut()
    {
        obj.remove("tool_calls");
    }

    *messages = cleaned;
    removed
}

fn plan_auto_compact(messages: &[Value], cfg: &ContextCompactConfig) -> Option<CompactPlan> {
    let compacted_prefix_len = compacted_summary_prefix_len(messages);
    let compactable = messages.get(compacted_prefix_len..)?;
    if compactable.len() <= cfg.auto_preserve_recent_messages {
        return None;
    }
    let compactable_tokens = estimate_messages_tokens(compactable);
    if compactable_tokens < cfg.auto_token_threshold {
        return None;
    }
    let keep_from = messages
        .len()
        .saturating_sub(cfg.auto_preserve_recent_messages);
    let keep_from = adjust_keep_from_to_tool_boundary(messages, keep_from);
    if keep_from <= compacted_prefix_len {
        return None;
    }
    Some(CompactPlan {
        compacted_prefix_len,
        keep_from,
        removed_messages: keep_from.saturating_sub(compacted_prefix_len),
        estimated_tokens_before: estimate_messages_tokens(messages),
    })
}

fn generate_compact_summary(
    llm: &OpenAiCompatClient,
    history_messages: &[Value],
    summary_source: &str,
    tool_definitions: &[ToolDefinition],
    audit_log_path_override: Option<&str>,
    prompt_cache_stats: Option<&mut PromptCacheStats>,
) -> Result<String> {
    let mut messages = history_messages.to_vec();
    messages.push(json!({
        "role": "user",
        "content": format!(
            "Compress the conversation above into a short summary preserving current goal, completed work, pending work, important file paths, tool usage, cache/compaction state, and unresolved decisions. Output the summary plainly.\n\nTRANSCRIPT START\n{summary_source}\nTRANSCRIPT END"
        )
    }));
    let response = llm.create_chat_completion_with_audit_path(
        &messages,
        tool_definitions,
        audit_log_path_override,
    )?;
    if let Some(stats) = prompt_cache_stats {
        if response.cached {
            stats.record_local_cache_hit();
        } else {
            stats.record_usage(&response.usage);
        }
    }
    extract_message_text(&response.message)
}

fn merge_compact_summaries(existing_summary: Option<&str>, new_summary: &str) -> String {
    let Some(existing_summary) = existing_summary
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return compress_summary_text(new_summary);
    };
    compress_summary_text(&format!(
        "Conversation summary:\n- Previously compacted context:\n{}\n- Newly compacted context:\n{}",
        indent_summary(existing_summary),
        indent_summary(new_summary)
    ))
}

fn indent_summary(summary: &str) -> String {
    summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn compress_summary_text(summary: &str) -> String {
    let mut lines = Vec::new();
    let mut char_count = 0usize;
    for line in summary
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
    {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let truncated = truncate_chars(&normalized, 180);
        let next_count = char_count + truncated.chars().count() + 1;
        if lines.len() >= 24 || next_count > 1_400 {
            lines.push("- ... additional compacted context omitted.".to_string());
            break;
        }
        char_count = next_count;
        lines.push(truncated);
    }
    lines.join("\n")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn adjust_keep_from_to_tool_boundary(messages: &[Value], mut keep_from: usize) -> usize {
    while keep_from > 0
        && messages
            .get(keep_from)
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("tool")
    {
        keep_from -= 1;
    }
    keep_from
}

fn compacted_summary_prefix_len(messages: &[Value]) -> usize {
    usize::from(
        messages
            .first()
            .and_then(extract_existing_compacted_summary)
            .is_some(),
    )
}

fn extract_existing_compacted_summary(message: &Value) -> Option<String> {
    if message.get("role").and_then(Value::as_str) != Some("system") {
        return None;
    }
    let content = message.get("content").and_then(Value::as_str)?;
    let summary = content.strip_prefix(CONTINUATION_PREFIX)?;
    let summary = summary.trim_start_matches('\n');
    let summary = summary
        .split_once(&format!("\n\n{CONTINUATION_SUFFIX}"))
        .map_or(summary, |(value, _)| value);
    Some(summary.trim().to_string())
}

fn assistant_tool_call_ids(message: &Value) -> Vec<String> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    call.get("id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn backup_transcript(messages: &[Value], dir: &str) -> Result<PathBuf> {
    let dir_path = Path::new(dir);
    create_dir_all(dir_path)
        .with_context(|| format!("failed to create transcript dir: {}", dir_path.display()))?;
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock error")?
        .as_millis();
    let path = dir_path.join(format!("transcript_{ts_ms}.jsonl"));
    let mut file = File::create(&path)
        .with_context(|| format!("failed to create transcript file: {}", path.display()))?;
    for message in messages {
        let line = serde_json::to_string(message).context("failed to encode transcript line")?;
        writeln!(file, "{line}").context("failed to write transcript line")?;
    }
    Ok(path)
}

fn build_compact_summary_source(removed: &[Value]) -> String {
    let mut out = String::new();
    for (index, message) in removed.iter().enumerate() {
        let line = serde_json::to_string(message)
            .unwrap_or_else(|_| "{\"error\":\"failed to serialize message\"}".to_string());
        let next_len = out.len().saturating_add(line.len()).saturating_add(1);
        if next_len > MAX_SUMMARY_SOURCE_CHARS {
            out.push_str(&format!(
                "\n[... truncated after {index} removed messages due to summary source limit ...]"
            ));
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
    }
    out
}

fn estimate_message_tokens(message: &Value) -> usize {
    let serialized = serde_json::to_string(message).unwrap_or_default();
    (serialized.chars().count() / 4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_orphan_tool_messages_and_clears_unanswered_tool_calls() {
        let mut messages = vec![
            json!({"role": "assistant", "tool_calls": [{"id": "call_1"}]}),
            json!({"role": "user", "content": "next"}),
            json!({"role": "tool", "tool_call_id": "missing", "content": "late"}),
        ];
        let removed = remove_orphan_tool_messages(&mut messages);
        assert_eq!(removed, 1);
        assert!(messages[0].get("tool_calls").is_none());
    }
}
