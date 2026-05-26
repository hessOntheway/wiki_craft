use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::knowledge::{VAULT_INDEX_PATH, VAULT_TOPICS_DIR, WorkspacePaths, parse_vault_markdown};
use crate::support::{markdown_heading, truncate_chars};

const DEFAULT_TOP_K: usize = 5;
const MAX_DESCRIPTION_CHARS: usize = 900;
const MAX_FOCUS_CHARS: usize = 260;
const MAX_SIGNAL_CHARS: usize = 360;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillTarget {
    Codex,
    Claude,
    Custom,
}

#[derive(Debug, Clone)]
pub struct CreateSkillOptions {
    pub knowledge_base_id: String,
    pub target: SkillTarget,
    pub destination_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSkillOutcome {
    pub skill_name: String,
    pub skill_path: String,
    pub message: String,
}

#[derive(Debug, Clone)]
struct KnowledgeBaseSkillContext {
    id: String,
    name: String,
    focus: String,
    root: PathBuf,
    signals: Vec<String>,
}

pub fn create_knowledge_base_skill(
    config_path: &Path,
    options: CreateSkillOptions,
) -> Result<CreateSkillOutcome> {
    let destination = resolve_destination(options.target, options.destination_path.as_deref())?;
    fs::create_dir_all(&destination).with_context(|| {
        format!(
            "failed to create skill destination directory: {}",
            destination.display()
        )
    })?;

    let context = load_skill_context(config_path, &options.knowledge_base_id)?;
    let config_path = absolute_path(config_path);
    let manifest_path = absolute_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    let skill_name = skill_slug(&context.name, &context.id);
    let skill_dir = destination.join(&skill_name);
    fs::create_dir_all(&skill_dir)
        .with_context(|| format!("failed to create skill directory: {}", skill_dir.display()))?;

    let skill_md = render_skill_md(&skill_name, &context, &config_path, &manifest_path);
    let skill_md_path = skill_dir.join("SKILL.md");
    fs::write(&skill_md_path, skill_md)
        .with_context(|| format!("failed to write {}", skill_md_path.display()))?;

    Ok(CreateSkillOutcome {
        skill_name,
        skill_path: skill_dir.display().to_string(),
        message: format!("created skill at {}", skill_dir.display()),
    })
}

fn load_skill_context(
    config_path: &Path,
    knowledge_base_id: &str,
) -> Result<KnowledgeBaseSkillContext> {
    let id = knowledge_base_id.trim();
    if id.is_empty() {
        bail!("knowledge base id must not be empty");
    }
    let mut config = AppConfig::load_or_default(config_path)?;
    config.select_knowledge_base(id)?;
    let active = config.active_knowledge_base()?.clone();
    let paths = WorkspacePaths::from_config(&config);
    let signals = collect_skill_signals(&paths)?;
    Ok(KnowledgeBaseSkillContext {
        id: active.id,
        name: active.name,
        focus: active.focus,
        root: paths.root,
        signals,
    })
}

fn collect_skill_signals(paths: &WorkspacePaths) -> Result<Vec<String>> {
    let mut signals = BTreeSet::new();
    collect_markdown_signals(
        &paths.knowledge_current.join(VAULT_INDEX_PATH),
        &mut signals,
    )?;
    let topics_dir = paths.knowledge_current.join(VAULT_TOPICS_DIR);
    if topics_dir.exists() {
        let mut topic_paths = walkdir::WalkDir::new(&topics_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
            .collect::<Vec<_>>();
        topic_paths.sort();
        for path in topic_paths {
            collect_markdown_signals(&path, &mut signals)?;
            if signals.len() >= 18 {
                break;
            }
        }
    }
    Ok(signals.into_iter().take(12).collect())
}

fn collect_markdown_signals(path: &Path, signals: &mut BTreeSet<String>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed = parse_vault_markdown(&content);
    if let Some(title) = parsed.frontmatter.title {
        insert_signal(signals, &title);
    }
    for value in parsed
        .frontmatter
        .aliases
        .iter()
        .chain(parsed.frontmatter.tags.iter())
    {
        insert_signal(signals, value);
    }
    for line in parsed.body.lines() {
        if let Some(heading) = markdown_heading(line) {
            insert_signal(signals, &heading);
        }
        if signals.len() >= 24 {
            break;
        }
    }
    Ok(())
}

fn insert_signal(signals: &mut BTreeSet<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.chars().count() >= 2 {
        signals.insert(trimmed.to_string());
    }
}

fn render_skill_md(
    skill_name: &str,
    context: &KnowledgeBaseSkillContext,
    config_path: &Path,
    manifest_path: &Path,
) -> String {
    let description = skill_description(context);
    let cargo_command = format!(
        "cargo run --manifest-path {} -- --config {} search --knowledge-base {} --query \"<query>\" --top-k {DEFAULT_TOP_K} --json",
        shell_quote(&manifest_path.display().to_string()),
        shell_quote(&config_path.display().to_string()),
        shell_quote(&context.id),
    );
    let binary_command = format!(
        "wiki_craft --config {} search --knowledge-base {} --query \"<query>\" --top-k {DEFAULT_TOP_K} --json",
        shell_quote(&config_path.display().to_string()),
        shell_quote(&context.id),
    );
    let signal_text = if context.signals.is_empty() {
        "No approved topic headings were available when this skill was generated.".to_string()
    } else {
        context.signals.join(", ")
    };

    format!(
        r#"---
name: {skill_name}
description: {description_yaml}
metadata:
  wiki_craft:
    knowledge_base_id: {knowledge_base_id_yaml}
    knowledge_base_name: {knowledge_base_name_yaml}
    knowledge_base_root: {knowledge_base_root_yaml}
---

# {title}

Use this skill when the user is asking about, designing, comparing, or validating work related to this Wiki Craft knowledge base.

## Knowledge Base

- Name: {knowledge_base_name}
- ID: `{knowledge_base_id}`
- Focus: {focus}
- Approved index/topic signals: {signal_text}

## Search Workflow

Search this exact knowledge base before answering when the conversation overlaps the focus or signals above. Prefer approved `topic` and `index` results as durable knowledge. Use `source_summary` results as evidence and cite returned source URLs when available.

Use the installed binary when available:

```bash
{binary_command}
```

Fallback command from the Wiki Craft source checkout:

```bash
{cargo_command}
```

Replace `<query>` with a concise natural-language query. For design questions, search the key concept first, then run one or two follow-up searches for architecture, tradeoffs, failure modes, or terminology found in the first results.

Only treat returned approved knowledge as authoritative. Do not rely on staged candidates or unapproved drafts.
"#,
        description_yaml = yaml_scalar(&description),
        knowledge_base_id_yaml = yaml_scalar(&context.id),
        knowledge_base_name_yaml = yaml_scalar(&context.name),
        knowledge_base_root_yaml = yaml_scalar(&context.root.display().to_string()),
        title = &context.name,
        knowledge_base_name = &context.name,
        knowledge_base_id = &context.id,
        focus = &context.focus,
    )
}

fn skill_description(context: &KnowledgeBaseSkillContext) -> String {
    let focus = truncate_chars(&context.focus, MAX_FOCUS_CHARS);
    let signals = context.signals.join(", ");
    let signal_text = if signals.is_empty() {
        String::new()
    } else {
        format!(
            " Approved index/topic signals: {}.",
            truncate_chars(&signals, MAX_SIGNAL_CHARS)
        )
    };
    truncate_chars(
        &format!(
            "Use when discussion may benefit from the Wiki Craft knowledge base \"{}\". Focus: {}.{} Search this specific knowledge base with Wiki Craft before answering.",
            context.name, focus, signal_text
        ),
        MAX_DESCRIPTION_CHARS,
    )
}

fn resolve_destination(target: SkillTarget, custom: Option<&Path>) -> Result<PathBuf> {
    match target {
        SkillTarget::Codex => Ok(env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".codex")))
            .context("cannot resolve Codex skills directory; set CODEX_HOME or HOME")?
            .join("skills")),
        SkillTarget::Claude => Ok(env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".claude")))
            .context("cannot resolve Claude skills directory; set CLAUDE_CONFIG_DIR or HOME")?
            .join("skills")),
        SkillTarget::Custom => custom
            .map(expand_home)
            .context("destination_path is required when target is custom"),
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn expand_home(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if text == "~" {
        return home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| path.to_path_buf());
    }
    path.to_path_buf()
}

fn absolute_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    absolute.canonicalize().unwrap_or(absolute)
}

fn skill_slug(name: &str, fallback_id: &str) -> String {
    let primary = ascii_slug(name);
    let base = if primary.is_empty() {
        ascii_slug(fallback_id)
    } else {
        primary
    };
    let slug = if base.is_empty() || base.starts_with("wiki-craft") {
        base
    } else {
        format!("wiki-craft-{base}")
    };
    if slug.is_empty() {
        "wiki-craft-knowledge-base".to_string()
    } else {
        truncate_slug(&slug, 80)
    }
}

fn ascii_slug(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn truncate_slug(value: &str, max_chars: usize) -> String {
    let mut out = value.chars().take(max_chars).collect::<String>();
    out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "wiki-craft-knowledge-base".to_string()
    } else {
        out
    }
}

fn yaml_scalar(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::config::{
        IngestConfig, KNOWLEDGE_BASE_CONFIG_FILE, KNOWLEDGE_BASE_REGISTRY_FILE,
        KNOWLEDGE_BASES_DIR, KnowledgeBaseFileConfig, KnowledgeBaseRecord, KnowledgeBaseRegistry,
    };

    #[test]
    fn creates_skill_with_focus_signals_and_fixed_search_command() {
        let root = unique_temp_dir();
        let runtime_root = root.join(".wiki_craft");
        let destination = root.join("skills");
        fs::create_dir_all(runtime_root.join("knowledge/approved/topics")).unwrap();
        fs::write(
            root.join("wiki_craft.toml"),
            format!("[runtime]\nroot = \"{}\"\n", runtime_root.display()),
        )
        .unwrap();
        write_registry(
            &runtime_root,
            "agent-memory",
            "Agent Memory",
            "Agent memory research",
        );
        fs::write(
            runtime_root.join("knowledge/approved/index.md"),
            "---\ntitle: \"Agent Memory\"\ntags: [memory, agents]\n---\n\n# Agent Memory\n\n## Retrieval Patterns\n",
        )
        .unwrap();
        fs::write(
            runtime_root.join("knowledge/approved/topics/episodic.md"),
            "---\ntitle: \"Episodic Memory\"\naliases: [experience replay]\ntags: [retrieval]\n---\n\n# Episodic Memory\n",
        )
        .unwrap();

        let outcome = create_knowledge_base_skill(
            &root.join("wiki_craft.toml"),
            CreateSkillOptions {
                knowledge_base_id: "agent-memory".to_string(),
                target: SkillTarget::Custom,
                destination_path: Some(destination.clone()),
            },
        )
        .unwrap();
        let skill_md = fs::read_to_string(Path::new(&outcome.skill_path).join("SKILL.md")).unwrap();

        assert!(skill_md.contains("Focus: Agent memory research"));
        assert!(skill_md.contains("Retrieval Patterns"));
        assert!(skill_md.contains("--knowledge-base 'agent-memory'"));
        assert!(skill_md.contains("--json"));
        assert_eq!(outcome.skill_name, "wiki-craft-agent-memory");

        fs::remove_dir_all(root).unwrap();
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("wiki_craft_skill_test_{nanos}"))
    }

    fn write_registry(runtime_root: &Path, id: &str, name: &str, focus: &str) {
        KnowledgeBaseRegistry {
            schema_version: 1,
            active_id: Some(id.to_string()),
            knowledge_bases: vec![KnowledgeBaseRecord {
                id: id.to_string(),
                name: name.to_string(),
                focus: focus.to_string(),
                root: runtime_root.display().to_string(),
                created_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            }],
        }
        .save(
            &runtime_root
                .join(KNOWLEDGE_BASES_DIR)
                .join(KNOWLEDGE_BASE_REGISTRY_FILE),
        )
        .unwrap();
        KnowledgeBaseFileConfig {
            name: name.to_string(),
            focus: focus.to_string(),
            ingest: IngestConfig::default(),
        }
        .save(&runtime_root.join(KNOWLEDGE_BASE_CONFIG_FILE))
        .unwrap();
    }
}
