use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::knowledge::WorkspacePaths;
use crate::llm::usage::PromptCacheStats;
use crate::sources::ChangedSource;

const CANDIDATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMetadata {
    pub schema_version: u32,
    pub run_id: String,
    pub created_at_unix_ms: u128,
    pub status: CandidateStatus,
    pub changed_sources: Vec<ChangedSource>,
    #[serde(default)]
    pub prompt_cache_stats: PromptCacheStats,
    #[serde(default)]
    pub compaction_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Staged,
    Approved,
}

#[derive(Debug, Clone)]
pub struct CandidatePaths {
    pub root: PathBuf,
    pub source_summaries: PathBuf,
    pub knowledge: PathBuf,
    pub diff: PathBuf,
    pub metadata: PathBuf,
}

impl CandidatePaths {
    pub fn new(paths: &WorkspacePaths, run_id: &str) -> Self {
        let root = paths.candidates_dir.join(run_id);
        Self {
            source_summaries: root.join("source_summaries"),
            knowledge: root.join("knowledge"),
            diff: root.join("diff.md"),
            metadata: root.join("metadata.json"),
            root,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [&self.root, &self.source_summaries, &self.knowledge] {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create candidate dir: {}", dir.display()))?;
        }
        Ok(())
    }
}

pub fn new_run_id() -> String {
    let ms = now_unix_ms();
    format!("run_{ms}")
}

pub fn write_candidate_metadata(
    paths: &CandidatePaths,
    metadata: &CandidateMetadata,
) -> Result<()> {
    if let Some(parent) = paths.metadata.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create metadata dir: {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(metadata).context("failed to serialize candidate metadata")?;
    fs::write(&paths.metadata, content).with_context(|| {
        format!(
            "failed to write candidate metadata: {}",
            paths.metadata.display()
        )
    })
}

pub fn load_candidate_metadata(paths: &WorkspacePaths, run_id: &str) -> Result<CandidateMetadata> {
    validate_run_id(run_id)?;
    let candidate = CandidatePaths::new(paths, run_id);
    let content = fs::read_to_string(&candidate.metadata).with_context(|| {
        format!(
            "failed to read candidate metadata: {}",
            candidate.metadata.display()
        )
    })?;
    serde_json::from_str(&content).context("failed to parse candidate metadata")
}

pub fn list_candidates(paths: &WorkspacePaths) -> Result<Vec<CandidateMetadata>> {
    if !paths.candidates_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&paths.candidates_dir)
        .with_context(|| format!("failed to read {}", paths.candidates_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        if let Ok(metadata) = load_candidate_metadata(paths, &run_id) {
            out.push(metadata);
        }
    }
    out.sort_by(|left, right| right.created_at_unix_ms.cmp(&left.created_at_unix_ms));
    Ok(out)
}

pub fn generate_diff(current_dir: &Path, candidate_dir: &Path) -> Result<String> {
    let current_files = collect_files(current_dir)?;
    let candidate_files = collect_files(candidate_dir)?;
    let mut all_files = BTreeSet::new();
    all_files.extend(current_files.iter().cloned());
    all_files.extend(candidate_files.iter().cloned());

    let mut sections = vec!["# Wiki Craft Candidate Diff".to_string()];
    for rel in all_files {
        let current_path = current_dir.join(&rel);
        let candidate_path = candidate_dir.join(&rel);
        let current = fs::read_to_string(&current_path).unwrap_or_default();
        let candidate = fs::read_to_string(&candidate_path).unwrap_or_default();
        if current == candidate {
            continue;
        }
        let kind = if !current_path.exists() {
            "Added"
        } else if !candidate_path.exists() {
            "Removed"
        } else {
            "Modified"
        };
        sections.push(format!("## {kind}: `{}`\n", rel.display()));
        sections.push("```diff".to_string());
        sections.extend(render_simple_diff(&current, &candidate));
        sections.push("```".to_string());
    }
    if sections.len() == 1 {
        sections.push("No changes.".to_string());
    }
    Ok(sections.join("\n"))
}

pub fn write_diff(paths: &CandidatePaths, current_dir: &Path) -> Result<String> {
    let diff = generate_diff(current_dir, &paths.knowledge)?;
    fs::write(&paths.diff, &diff)
        .with_context(|| format!("failed to write candidate diff: {}", paths.diff.display()))?;
    Ok(diff)
}

pub fn read_diff(paths: &WorkspacePaths, run_id: &str) -> Result<String> {
    validate_run_id(run_id)?;
    let candidate = CandidatePaths::new(paths, run_id);
    fs::read_to_string(&candidate.diff).with_context(|| {
        format!(
            "failed to read candidate diff: {}",
            candidate.diff.display()
        )
    })
}

pub fn approve_candidate(paths: &WorkspacePaths, run_id: &str) -> Result<()> {
    validate_run_id(run_id)?;
    let candidate_paths = CandidatePaths::new(paths, run_id);
    if !candidate_paths.knowledge.exists() {
        bail!(
            "candidate knowledge directory not found: {}",
            candidate_paths.knowledge.display()
        );
    }
    replace_dir(&candidate_paths.knowledge, &paths.knowledge_current)?;
    replace_dir(
        &candidate_paths.source_summaries,
        &paths.source_summaries_current,
    )?;

    let mut metadata = load_candidate_metadata(paths, run_id)?;
    metadata.status = CandidateStatus::Approved;
    write_candidate_metadata(&candidate_paths, &metadata)
}

pub fn candidate_metadata(
    run_id: String,
    changed_sources: Vec<ChangedSource>,
    prompt_cache_stats: PromptCacheStats,
    compaction_count: u64,
) -> CandidateMetadata {
    CandidateMetadata {
        schema_version: CANDIDATE_SCHEMA_VERSION,
        run_id,
        created_at_unix_ms: now_unix_ms(),
        status: CandidateStatus::Staged,
        changed_sources,
        prompt_cache_stats,
        compaction_count,
    }
}

fn collect_files(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    if !root.exists() {
        return Ok(files);
    }
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_path_buf();
        files.insert(rel);
    }
    Ok(files)
}

fn render_simple_diff(current: &str, candidate: &str) -> Vec<String> {
    let old_lines = current.lines().collect::<Vec<_>>();
    let new_lines = candidate.lines().collect::<Vec<_>>();
    let max_len = old_lines.len().max(new_lines.len());
    let mut out = Vec::new();
    for idx in 0..max_len {
        match (old_lines.get(idx), new_lines.get(idx)) {
            (Some(old), Some(new)) if old == new => out.push(format!(" {old}")),
            (Some(old), Some(new)) => {
                out.push(format!("-{old}"));
                out.push(format!("+{new}"));
            }
            (Some(old), None) => out.push(format!("-{old}")),
            (None, Some(new)) => out.push(format!("+{new}")),
            (None, None) => {}
        }
    }
    out
}

fn replace_dir(from: &Path, to: &Path) -> Result<()> {
    let tmp = to.with_extension(format!("tmp_{}", now_unix_ms()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)
            .with_context(|| format!("failed to clean tmp dir: {}", tmp.display()))?;
    }
    copy_dir(from, &tmp)?;
    if to.exists() {
        fs::remove_dir_all(to)
            .with_context(|| format!("failed to remove existing dir: {}", to.display()))?;
    }
    fs::rename(&tmp, to)
        .with_context(|| format!("failed to promote {} to {}", tmp.display(), to.display()))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("failed to create dir: {}", to.display()))?;
    for entry in walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let rel = entry.path().strip_prefix(from).unwrap_or(entry.path());
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create dir: {}", target.display()))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create dir: {}", parent.display()))?;
            }
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn validate_run_id(run_id: &str) -> Result<()> {
    if run_id.trim().is_empty() || run_id.contains('/') || run_id.contains("..") {
        bail!("invalid run_id: {run_id}");
    }
    Ok(())
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
    use crate::config::{AppConfig, RuntimeConfig};
    use crate::knowledge::WorkspacePaths;

    #[test]
    fn diff_marks_added_content() {
        let diff = render_simple_diff("", "hello\nworld\n");
        assert_eq!(diff, vec!["+hello".to_string(), "+world".to_string()]);
    }

    #[test]
    fn approve_promotes_candidate_knowledge() {
        let root = std::env::temp_dir().join(format!("wiki-craft-approve-test-{}", now_unix_ms()));
        let config = AppConfig {
            runtime: RuntimeConfig {
                root: root.to_string_lossy().to_string(),
                max_steps: 4,
            },
            ..Default::default()
        };
        let paths = WorkspacePaths::from_config(&config);
        paths.ensure_all().expect("dirs");
        let run_id = "run_test";
        let candidate = CandidatePaths::new(&paths, run_id);
        candidate.ensure().expect("candidate dirs");
        fs::write(candidate.knowledge.join("Home.md"), "# Candidate\n").expect("home");
        fs::write(candidate.source_summaries.join("source.md"), "# Summary\n").expect("summary");
        let metadata = candidate_metadata(
            run_id.to_string(),
            Vec::new(),
            PromptCacheStats::default(),
            0,
        );
        write_candidate_metadata(&candidate, &metadata).expect("metadata");

        approve_candidate(&paths, run_id).expect("approve");

        let current =
            fs::read_to_string(paths.knowledge_current.join("Home.md")).expect("current home");
        assert_eq!(current, "# Candidate\n");
        let _ = fs::remove_dir_all(root);
    }
}
