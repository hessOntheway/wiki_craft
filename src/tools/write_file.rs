use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use super::{ToolDefinition, ToolExecutor, ToolHandler};

#[derive(Debug, Deserialize)]
struct WriteFileInput {
    path: String,
    content: String,
}

pub fn write_file_handler(workspace_root: PathBuf) -> ToolHandler {
    let definition = ToolDefinition {
        name: "write_file".to_string(),
        description: "Write markdown/text under the workspace root. Parent traversal is rejected."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    };

    let execute: ToolExecutor = std::sync::Arc::new(move |input_json: &str| {
        let input: WriteFileInput =
            serde_json::from_str(input_json).context("invalid input JSON for write_file")?;
        let path = validate_write_path(&workspace_root, &input.path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent dir: {}", parent.display()))?;
        }
        fs::write(&path, input.content)
            .with_context(|| format!("failed to write file {}", path.display()))?;
        serde_json::to_string_pretty(&json!({"ok": true, "path": path.display().to_string()}))
            .context("failed to encode write_file output")
    });

    ToolHandler::new(definition, execute)
}

fn validate_write_path(workspace_root: &Path, path: &str) -> Result<PathBuf> {
    if path.trim().is_empty() {
        bail!("write_file path cannot be empty");
    }
    let workspace_root = workspace_root.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize workspace root: {}",
            workspace_root.display()
        )
    })?;
    let candidate = Path::new(path);
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("'..' path segments are not allowed");
    }
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    };
    if !absolute.starts_with(&workspace_root) {
        bail!("write path escapes workspace: {}", absolute.display());
    }
    Ok(absolute)
}
