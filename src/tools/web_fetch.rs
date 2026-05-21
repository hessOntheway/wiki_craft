use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ToolDefinition, ToolExecutor, ToolHandler};

#[derive(Debug, Clone, Deserialize)]
pub struct WebFetchInput {
    pub url: String,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub include_headers: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFetchOutput {
    pub url: String,
    pub final_url: String,
    pub status_code: u16,
    pub status_text: Option<String>,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub truncated: bool,
    pub byte_count: usize,
    pub headers: BTreeMap<String, String>,
    pub text: String,
}

pub fn web_fetch_handler() -> ToolHandler {
    let definition = ToolDefinition {
        name: "web_fetch".to_string(),
        description: "Fetch a public HTTP(S) page, extract readable text, and return response metadata with bounded bytes.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "timeout_seconds": {"type": "integer"},
                "max_bytes": {"type": "integer"},
                "include_headers": {"type": "boolean"}
            },
            "required": ["url"],
            "additionalProperties": false
        }),
    };

    let execute: ToolExecutor = std::sync::Arc::new(move |input_json: &str| {
        let input: WebFetchInput =
            serde_json::from_str(input_json).context("invalid input JSON for web_fetch")?;
        let output = run_web_fetch(&input)?;
        serde_json::to_string_pretty(&output).context("failed to serialize web_fetch output")
    });

    ToolHandler::new(definition, execute)
}

pub fn run_web_fetch(input: &WebFetchInput) -> Result<WebFetchOutput> {
    let parsed_url =
        reqwest::Url::parse(&input.url).with_context(|| format!("invalid url: {}", input.url))?;
    match parsed_url.scheme() {
        "http" | "https" => {}
        other => bail!("unsupported URL scheme: {other}; only http and https are allowed"),
    }

    let timeout_seconds = input.timeout_seconds.unwrap_or(15).clamp(1, 60);
    let max_bytes = input.max_bytes.unwrap_or(200_000).clamp(1, 1_000_000);
    let client = Client::builder()
        .user_agent("wiki-craft-web-fetch/0.1")
        .redirect(Policy::limited(5))
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .context("failed to build web fetch client")?;

    let response = client
        .get(parsed_url.clone())
        .send()
        .with_context(|| format!("failed to fetch url: {}", input.url))?;

    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            Some((
                name.as_str().to_ascii_lowercase(),
                value.to_str().ok()?.to_string(),
            ))
        })
        .collect::<BTreeMap<_, _>>();

    let mut body = Vec::new();
    let mut limited_reader = response.take(max_bytes as u64 + 1);
    limited_reader
        .read_to_end(&mut body)
        .context("failed to read web response body")?;
    let truncated = body.len() > max_bytes;
    if truncated {
        body.truncate(max_bytes);
    }

    let raw_text = String::from_utf8_lossy(&body).to_string();
    let text = decode_response_text(&raw_text, content_type.as_deref());
    let title = extract_title(&raw_text, content_type.as_deref());

    Ok(WebFetchOutput {
        url: input.url.clone(),
        final_url,
        status_code: status.as_u16(),
        status_text: status.canonical_reason().map(ToOwned::to_owned),
        content_type,
        title,
        truncated,
        byte_count: body.len(),
        headers,
        text,
    })
}

pub fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_response_text(raw: &str, content_type: Option<&str>) -> String {
    if content_type
        .map(|value| value.contains("html") || value.contains("xhtml"))
        .unwrap_or(false)
    {
        html_to_text(raw)
    } else {
        normalize_whitespace(raw)
    }
}

fn extract_title(text: &str, content_type: Option<&str>) -> Option<String> {
    if !content_type
        .map(|value| value.contains("html") || value.contains("xhtml"))
        .unwrap_or(false)
    {
        return None;
    }
    let title_re = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok()?;
    let title = title_re.captures(text)?.get(1)?.as_str();
    let cleaned = normalize_whitespace(&strip_tags(title));
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn html_to_text(html: &str) -> String {
    let script_re = Regex::new(r"(?is)<(script|style|noscript)[^>]*>.*?</(script|style|noscript)>")
        .expect("valid regex");
    let tag_re = Regex::new(r"(?is)<[^>]+>").expect("valid regex");
    let without_blocks = script_re.replace_all(html, " ");
    let stripped = tag_re.replace_all(&without_blocks, " ");
    normalize_whitespace(&strip_html_entities(&stripped))
}

fn strip_tags(input: &str) -> String {
    let tag_re = Regex::new(r"(?is)<[^>]+>").expect("valid regex");
    tag_re.replace_all(input, " ").to_string()
}

fn strip_html_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_html_blocks() {
        let text = decode_response_text(
            "<html><head><title>A &amp; B</title><style>x</style></head><body><h1>Hello</h1><script>bad()</script></body></html>",
            Some("text/html"),
        );
        assert!(text.contains("A & B"));
        assert!(text.contains("Hello"));
        assert!(!text.contains("bad()"));
    }
}
