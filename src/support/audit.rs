use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::usage::ModelUsage;
use crate::tools::ToolDefinition;

const DEFAULT_PREVIEW_CHARS: usize = 1_200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    LlmExchange {
        ts_unix_ms: u128,
        model: String,
        request_hash: String,
        cached: bool,
        message_count: usize,
        tool_names: Vec<String>,
        last_user_preview: Option<String>,
        assistant_preview: Option<String>,
        usage: ModelUsage,
    },
    ToolCall {
        ts_unix_ms: u128,
        tool_call_id: String,
        name: String,
        arguments_preview: String,
    },
    ToolResult {
        ts_unix_ms: u128,
        tool_call_id: String,
        name: String,
        result_preview: String,
        is_error: bool,
    },
    Compaction {
        ts_unix_ms: u128,
        removed_messages: usize,
        estimated_tokens_before: usize,
        transcript_path: Option<String>,
    },
    CandidateError {
        ts_unix_ms: u128,
        run_id: String,
        stage: String,
        error: String,
    },
}

pub fn append_event(path: &str, event: &AuditEvent) -> Result<()> {
    let path = Path::new(path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create audit dir: {}", parent.display()))?;
    }
    let line = serde_json::to_string(event).context("failed to serialize audit event")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open audit log: {}", path.display()))?;
    writeln!(file, "{line}").context("failed to append audit event")
}

pub fn llm_exchange_event(
    model: String,
    request_hash: String,
    cached: bool,
    messages: &[Value],
    tools: &[ToolDefinition],
    assistant_message: &Value,
    usage: ModelUsage,
) -> AuditEvent {
    AuditEvent::LlmExchange {
        ts_unix_ms: now_unix_ms(),
        model,
        request_hash,
        cached,
        message_count: messages.len(),
        tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
        last_user_preview: last_user_preview(messages),
        assistant_preview: message_text_preview(assistant_message, DEFAULT_PREVIEW_CHARS),
        usage,
    }
}

pub fn tool_call_event(tool_call_id: String, name: String, arguments: String) -> AuditEvent {
    AuditEvent::ToolCall {
        ts_unix_ms: now_unix_ms(),
        tool_call_id,
        name,
        arguments_preview: truncate_chars(&arguments, DEFAULT_PREVIEW_CHARS),
    }
}

pub fn tool_result_event(
    tool_call_id: String,
    name: String,
    result: String,
    is_error: bool,
) -> AuditEvent {
    AuditEvent::ToolResult {
        ts_unix_ms: now_unix_ms(),
        tool_call_id,
        name,
        result_preview: truncate_chars(&result, DEFAULT_PREVIEW_CHARS),
        is_error,
    }
}

pub fn compaction_event(
    removed_messages: usize,
    estimated_tokens_before: usize,
    transcript_path: Option<String>,
) -> AuditEvent {
    AuditEvent::Compaction {
        ts_unix_ms: now_unix_ms(),
        removed_messages,
        estimated_tokens_before,
        transcript_path,
    }
}

pub fn candidate_error_event(run_id: String, stage: String, error: String) -> AuditEvent {
    AuditEvent::CandidateError {
        ts_unix_ms: now_unix_ms(),
        run_id,
        stage,
        error,
    }
}

fn last_user_preview(messages: &[Value]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message_text_preview(message, DEFAULT_PREVIEW_CHARS))
}

fn message_text_preview(message: &Value, max_chars: usize) -> Option<String> {
    let content = message.get("content")?;
    let text = match content {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .filter_map(|value| {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    if text.trim().is_empty() {
        None
    } else {
        Some(truncate_chars(&text, max_chars))
    }
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

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn llm_audit_event_is_compact() {
        let event = llm_exchange_event(
            "model".to_string(),
            "hash".to_string(),
            false,
            &[json!({"role": "user", "content": "hello"})],
            &[],
            &json!({"role": "assistant", "content": "world"}),
            ModelUsage::default(),
        );
        let encoded = serde_json::to_string(&event).expect("encode");
        assert!(encoded.contains("\"kind\":\"llm_exchange\""));
        assert!(encoded.contains("hello"));
        assert!(encoded.contains("world"));
    }

    #[test]
    fn appends_jsonl_events() {
        let path =
            std::env::temp_dir().join(format!("wiki-craft-audit-test-{}.jsonl", now_unix_ms()));
        let event = tool_call_event(
            "call_1".to_string(),
            "web_fetch".to_string(),
            "{\"url\":\"https://example.com\"}".to_string(),
        );
        append_event(path.to_str().expect("utf8 path"), &event).expect("append");
        let contents = std::fs::read_to_string(&path).expect("read audit");
        assert!(contents.contains("\"kind\":\"tool_call\""));
        assert!(contents.ends_with('\n'));
        let _ = std::fs::remove_file(path);
    }
}
