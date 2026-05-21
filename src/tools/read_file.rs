use std::fs::read_to_string;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ToolDefinition, ToolExecutor, ToolHandler};

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ReadFileLine {
    line: usize,
    text: String,
}

#[derive(Debug, Serialize)]
struct ReadFileOutput {
    path: String,
    resolved_path: String,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    lines: Vec<ReadFileLine>,
}

pub fn read_file_handler() -> ToolHandler {
    let definition = ToolDefinition {
        name: "read_file".to_string(),
        description: "Read a workspace file with optional 1-based line bounds.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer"},
                "end_line": {"type": "integer"}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    };

    let execute: ToolExecutor = std::sync::Arc::new(move |input_json: &str| {
        let input: ReadFileInput =
            serde_json::from_str(input_json).context("invalid input JSON for read_file")?;
        let output = run_read_file(&input)?;
        serde_json::to_string_pretty(&output).context("failed to serialize read_file output")
    });

    ToolHandler::new(definition, execute)
}

fn run_read_file(input: &ReadFileInput) -> Result<ReadFileOutput> {
    let workspace_root = std::env::current_dir()
        .context("failed to resolve workspace root")?
        .canonicalize()
        .context("failed to canonicalize workspace root")?;
    let resolved_path = resolve_file_path(&workspace_root, &input.path)?;
    let content = read_to_string(&resolved_path)
        .with_context(|| format!("failed to read file: {}", resolved_path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let start_line = input.start_line.unwrap_or(1);
    let end_line = input.end_line.unwrap_or(total_lines.max(start_line));

    if start_line == 0 || end_line == 0 {
        bail!("start_line and end_line must be 1-based positive integers");
    }
    if start_line > end_line {
        bail!("start_line must be less than or equal to end_line");
    }
    if total_lines == 0 {
        return Ok(ReadFileOutput {
            path: input.path.clone(),
            resolved_path: resolved_path.display().to_string(),
            start_line,
            end_line,
            total_lines,
            lines: Vec::new(),
        });
    }
    if start_line > total_lines {
        bail!("start_line {start_line} is beyond file length {total_lines}");
    }
    let clamped_end_line = end_line.min(total_lines);
    let selected_lines = lines[(start_line - 1)..clamped_end_line]
        .iter()
        .enumerate()
        .map(|(idx, text)| ReadFileLine {
            line: start_line + idx,
            text: (*text).to_string(),
        })
        .collect();
    Ok(ReadFileOutput {
        path: input.path.clone(),
        resolved_path: resolved_path.display().to_string(),
        start_line,
        end_line: clamped_end_line,
        total_lines,
        lines: selected_lines,
    })
}

fn resolve_file_path(workspace_root: &Path, path: &str) -> Result<PathBuf> {
    let candidate = Path::new(path);
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("'..' path segments are not allowed");
    }
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    };
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("failed to resolve path: {}", joined.display()))?;
    if !canonical.starts_with(workspace_root) {
        bail!("path escapes workspace: {}", canonical.display());
    }
    if !canonical.is_file() {
        bail!(
            "path does not point to a regular file: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}
