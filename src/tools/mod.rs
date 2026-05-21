use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

pub mod read_file;
pub mod web_fetch;
pub mod write_file;

pub use read_file::read_file_handler;
pub use web_fetch::{WebFetchInput, WebFetchOutput, run_web_fetch, web_fetch_handler};
pub use write_file::write_file_handler;

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub type ToolExecutor = Arc<dyn Fn(&str) -> Result<String> + Send + Sync>;

#[derive(Clone)]
pub struct ToolHandler {
    definition: ToolDefinition,
    execute: ToolExecutor,
}

impl ToolHandler {
    pub fn new(definition: ToolDefinition, execute: ToolExecutor) -> Self {
        Self {
            definition,
            execute,
        }
    }

    fn run(&self, input_json: &str) -> Result<String> {
        (self.execute)(input_json)
    }

    pub fn name(&self) -> &str {
        &self.definition.name
    }

    pub fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
}

#[derive(Clone)]
pub struct GlobalToolRegistry {
    handlers: HashMap<String, ToolHandler>,
}

impl GlobalToolRegistry {
    pub fn empty() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn builtins(workspace_root: PathBuf) -> Result<Self> {
        let mut registry = Self::empty();
        for tool in [
            web_fetch_handler(),
            read_file_handler(),
            write_file_handler(workspace_root),
        ] {
            registry = registry.with_tool(tool)?;
        }
        Ok(registry)
    }

    pub fn with_tool(mut self, tool: ToolHandler) -> Result<Self> {
        let name = tool.name().to_string();
        if self.handlers.contains_key(&name) {
            bail!("tool name conflicts with existing handler: {name}");
        }
        self.handlers.insert(name, tool);
        Ok(self)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<ToolDefinition> = self
            .handlers
            .values()
            .map(ToolHandler::definition)
            .collect();
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        definitions
    }

    pub fn execute(&self, name: &str, input_json: &str) -> Result<String> {
        let handler = self
            .handlers
            .get(name)
            .with_context(|| format!("unsupported tool: {name}"))?;
        handler.run(input_json)
    }

    pub fn names(&self) -> HashSet<String> {
        self.handlers.keys().cloned().collect()
    }
}
