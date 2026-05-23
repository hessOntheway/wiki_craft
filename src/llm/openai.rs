use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{ContextCompactConfig, ResolvedLlmConfig};
use crate::llm::cache::PromptCache;
use crate::llm::usage::{ModelUsage, PromptCacheStats};
use crate::support::audit::{append_event, llm_exchange_event};
use crate::tools::ToolDefinition;

#[derive(Clone)]
pub struct OpenAiCompatClient {
    http: Client,
    cfg: ResolvedLlmConfig,
    cache: Option<PromptCache>,
}

#[derive(Debug, Clone)]
pub struct ChatCompletionResult {
    pub message: Value,
    pub usage: ModelUsage,
    pub cached: bool,
    pub finish_reason: Option<String>,
}

struct AuditExchange<'a> {
    request_hash: &'a str,
    cached: bool,
    messages: &'a [Value],
    tools: &'a [ToolDefinition],
    assistant_message: &'a Value,
    usage: &'a ModelUsage,
    audit_log_path_override: Option<&'a str>,
}

impl OpenAiCompatClient {
    pub fn new(cfg: ResolvedLlmConfig) -> Result<Self> {
        let Some(api_key) = cfg.api_key.clone() else {
            bail!("missing model API key; set LLM_API_KEY");
        };
        if api_key.trim().is_empty() {
            bail!("missing model API key; set LLM_API_KEY");
        }

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("wiki-craft/0.1"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let http = Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build llm http client")?;
        let cache = if cfg.prompt_cache_enabled {
            Some(PromptCache::new(&cfg.prompt_cache_dir)?)
        } else {
            None
        };

        Ok(Self { http, cfg, cache })
    }

    pub fn context_compact_config(&self) -> &ContextCompactConfig {
        &self.cfg.context_compact
    }

    pub fn create_chat_completion(
        &self,
        messages: &[Value],
        tools: &[ToolDefinition],
    ) -> Result<ChatCompletionResult> {
        self.create_chat_completion_with_audit_path(messages, tools, None)
    }

    pub fn complete_text(
        &self,
        system: &str,
        user: &str,
        stats: &mut PromptCacheStats,
    ) -> Result<String> {
        let messages = vec![
            json!({"role": "system", "content": system}),
            json!({"role": "user", "content": user}),
        ];
        let result = self.create_chat_completion(&messages, &[])?;
        if result.cached {
            stats.record_local_cache_hit();
        } else {
            stats.record_usage(&result.usage);
        }
        extract_message_text(&result.message)
    }

    pub fn create_chat_completion_with_audit_path(
        &self,
        messages: &[Value],
        tools: &[ToolDefinition],
        audit_log_path_override: Option<&str>,
    ) -> Result<ChatCompletionResult> {
        let openai_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema
                    }
                })
            })
            .collect();
        let url = format!("{}/chat/completions", self.cfg.base_url);
        let mut body = json!({
            "model": self.cfg.model,
            "messages": messages,
            "stream": false,
            "max_tokens": self.cfg.max_tokens
        });
        if !openai_tools.is_empty() {
            body["tools"] = json!(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        let request_hash = request_hash_hex(&body);
        if let Some(cache) = &self.cache
            && let Some(cached) = cache.lookup(&request_hash, self.cfg.max_tokens)?
        {
            eprintln!("info: local prompt cache hit");
            self.write_audit_event(AuditExchange {
                request_hash: &request_hash,
                cached: true,
                messages,
                tools,
                assistant_message: &cached.message,
                usage: &cached.usage,
                audit_log_path_override,
            });
            return Ok(cached);
        }

        let api_key = self
            .cfg
            .api_key
            .as_deref()
            .context("missing model API key; set LLM_API_KEY")?;
        let response = self
            .http
            .post(&url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .with_context(|| {
                format!(
                    "failed to call model api: url={}, model={}",
                    url, self.cfg.model
                )
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_else(|_| "<no body>".to_string());
            bail!("model api error ({status}): {text}");
        }
        let payload: Value = response
            .json()
            .context("failed to decode model api response")?;
        let choice = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .context("model response missing choices[0]")?;
        let message = choice
            .get("message")
            .cloned()
            .context("model response missing choices[0].message")?;
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let usage = payload
            .get("usage")
            .cloned()
            .map(serde_json::from_value::<ModelUsage>)
            .transpose()
            .context("model response usage payload was invalid")?
            .unwrap_or_default();
        let result = ChatCompletionResult {
            message: message.clone(),
            usage: usage.clone(),
            cached: false,
            finish_reason: finish_reason.clone(),
        };
        self.write_audit_event(AuditExchange {
            request_hash: &request_hash,
            cached: false,
            messages,
            tools,
            assistant_message: &message,
            usage: &usage,
            audit_log_path_override,
        });
        if finish_reason.as_deref() == Some("length") {
            bail_truncated_output(self.cfg.max_tokens)?;
        }
        if let Some(cache) = &self.cache
            && let Err(error) = cache.store(&request_hash, &result)
        {
            eprintln!("warn: failed to write prompt cache entry: {error}");
        }
        Ok(result)
    }

    fn write_audit_event(&self, exchange: AuditExchange<'_>) {
        if !self.cfg.write_model_audit_log {
            return;
        }
        let path = exchange
            .audit_log_path_override
            .unwrap_or(&self.cfg.model_audit_log_path);
        let event = llm_exchange_event(
            self.cfg.model.clone(),
            exchange.request_hash.to_string(),
            exchange.cached,
            exchange.messages,
            exchange.tools,
            exchange.assistant_message,
            exchange.usage.clone(),
        );
        if let Err(error) = append_event(path, &event) {
            eprintln!("warn: failed to append llm audit event: {error}");
        }
    }
}

fn bail_truncated_output(max_tokens: u32) -> Result<()> {
    bail!(
        "model output was truncated by llm.max_tokens={} before a complete response was produced; increase [llm].max_tokens or reduce the candidate vault output size",
        max_tokens
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_output_error_mentions_max_tokens() {
        let error = bail_truncated_output(4096)
            .expect_err("length finish reason should fail")
            .to_string();
        assert!(error.contains("model output was truncated by llm.max_tokens=4096"));
    }
}

pub fn extract_message_text(message: &Value) -> Result<String> {
    message
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .context("model response missing text content")
}

fn request_hash_hex(body: &Value) -> String {
    let canonical = canonicalize_json(body);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn canonicalize_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonicalize_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonicalize_json(&map[key])
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", parts.join(","))
        }
    }
}
