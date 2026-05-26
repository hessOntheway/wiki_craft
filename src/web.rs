use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::candidates::{CandidateMetadata, CandidateStatus};
use crate::config::{
    AppConfig, DEFAULT_CONFIG_PATH, KnowledgeBaseCreateInput, KnowledgeBaseDeleteInput,
    KnowledgeBaseList, activate_knowledge_base, create_knowledge_base, delete_knowledge_base,
    list_knowledge_bases,
};
use crate::knowledge::WorkspacePaths;
use crate::runtime::{self, ApproveOutcome, StatusSnapshot};
use crate::search::{SearchOptions, SearchResponse, search_configured};
use crate::skill::{
    CreateSkillOptions, CreateSkillOutcome, SkillTarget, create_knowledge_base_skill,
};

const DEFAULT_SEARCH_TOP_K: usize = 5;
const MAX_SEARCH_TOP_K: usize = 20;

#[derive(Debug, Clone)]
pub struct WebState {
    config_path: PathBuf,
    service_log_path: PathBuf,
    jobs: Arc<Mutex<BTreeMap<String, ActionJob>>>,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    knowledge_base: Option<String>,
    query: Option<String>,
    top_k: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateKnowledgeBaseRequest {
    name: String,
    focus: String,
}

#[derive(Debug, Deserialize)]
struct DeleteKnowledgeBaseRequest {
    confirmation_name: String,
}

#[derive(Debug, Deserialize)]
struct CreateSkillRequest {
    target: SkillTarget,
    destination_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CandidateActions {
    pub approve_summaries: bool,
    pub merge_diff: bool,
    pub reject: bool,
}

#[derive(Debug, Serialize)]
pub struct CandidateDetailResponse {
    pub metadata: CandidateMetadata,
    pub actions: CandidateActions,
    pub summaries_markdown: String,
    pub diff_markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionJob {
    pub job_id: String,
    pub run_id: String,
    pub action: String,
    pub status: ActionJobStatus,
    pub message: String,
    pub started_at_unix_ms: u128,
    pub finished_at_unix_ms: Option<u128>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionJobStatus {
    Running,
    Completed,
    Failed,
}

impl CandidateActions {
    fn for_status(status: &CandidateStatus) -> Self {
        Self {
            approve_summaries: matches!(status, CandidateStatus::SummariesStaged),
            merge_diff: matches!(status, CandidateStatus::DiffReady),
            reject: !matches!(status, CandidateStatus::Approved),
        }
    }
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("{error:#}"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiError {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub fn api_router(config_path: impl Into<PathBuf>) -> Router {
    let config_path = config_path.into();
    let service_log_path = service_log_path_for_config(&config_path);
    append_service_log(
        &service_log_path,
        "info",
        "api_router_started",
        "local API router initialized",
        serde_json::json!({ "config_path": &config_path }),
    );
    let state = WebState {
        config_path,
        service_log_path,
        jobs: Arc::new(Mutex::new(BTreeMap::new())),
    };
    api_routes()
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn api_routes() -> Router<WebState> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/knowledge-bases", get(get_knowledge_bases))
        .route("/api/knowledge-bases", post(post_knowledge_base))
        .route(
            "/api/knowledge-bases/{id}/activate",
            post(post_activate_knowledge_base),
        )
        .route(
            "/api/knowledge-bases/{id}",
            delete(delete_knowledge_base_handler),
        )
        .route("/api/knowledge-bases/{id}/skill", post(post_create_skill))
        .route("/api/status", get(get_status))
        .route("/api/search", get(get_search))
        .route("/api/candidates", get(list_candidates))
        .route("/api/candidates/{run_id}", get(get_candidate))
        .route("/api/jobs/{job_id}", get(get_job))
        .route(
            "/api/candidates/{run_id}/summaries",
            get(get_candidate_summaries),
        )
        .route("/api/candidates/{run_id}/diff", get(get_candidate_diff))
        .route(
            "/api/candidates/{run_id}/approve-summaries",
            post(post_approve_summaries),
        )
        .route("/api/candidates/{run_id}/merge", post(post_merge))
        .route("/api/candidates/{run_id}/reject", post(post_reject))
}

pub async fn serve(config_path: impl Into<PathBuf>, host: String, port: u16) -> Result<()> {
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid bind address: {host}:{port}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind web API on http://{host}:{port}"))?;
    serve_listener(config_path, listener).await
}

pub async fn serve_listener(
    config_path: impl Into<PathBuf>,
    listener: tokio::net::TcpListener,
) -> Result<()> {
    let addr = listener
        .local_addr()
        .context("failed to read web API listener address")?;
    println!("Wiki Craft API running at http://{addr}");
    axum::serve(listener, api_router(config_path))
        .await
        .context("web API server exited unexpectedly")
}

pub fn config_path_from_env() -> PathBuf {
    if let Some(path) = std::env::var("WIKI_CRAFT_CONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
    {
        return absolute_config_path(path);
    }

    discover_default_config_path()
        .unwrap_or_else(|| absolute_config_path(PathBuf::from(DEFAULT_CONFIG_PATH)))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn get_knowledge_bases(
    State(state): State<WebState>,
) -> Result<Json<KnowledgeBaseList>, AppError> {
    list_knowledge_bases(&state.config_path)
        .map(Json)
        .map_err(AppError::internal)
}

async fn post_knowledge_base(
    State(state): State<WebState>,
    Json(payload): Json<CreateKnowledgeBaseRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let record = create_knowledge_base(
        &state.config_path,
        KnowledgeBaseCreateInput {
            name: payload.name,
            focus: payload.focus,
        },
    )
    .map_err(|error| AppError::bad_request(format!("{error:#}")))?;
    Ok(Json(serde_json::json!({ "knowledge_base": record })))
}

async fn post_activate_knowledge_base(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let record = activate_knowledge_base(&state.config_path, &id)
        .map_err(|error| AppError::bad_request(format!("{error:#}")))?;
    Ok(Json(serde_json::json!({ "knowledge_base": record })))
}

async fn delete_knowledge_base_handler(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<DeleteKnowledgeBaseRequest>,
) -> Result<Json<KnowledgeBaseList>, AppError> {
    delete_knowledge_base(
        &state.config_path,
        &id,
        KnowledgeBaseDeleteInput {
            confirmation_name: payload.confirmation_name,
        },
    )
    .map(Json)
    .map_err(|error| AppError::bad_request(format!("{error:#}")))
}

async fn post_create_skill(
    State(state): State<WebState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<CreateSkillRequest>,
) -> Result<Json<CreateSkillOutcome>, AppError> {
    let destination_path = payload
        .destination_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    create_knowledge_base_skill(
        &state.config_path,
        CreateSkillOptions {
            knowledge_base_id: id,
            target: payload.target,
            destination_path,
        },
    )
    .map(Json)
    .map_err(|error| AppError::bad_request(format!("{error:#}")))
}

fn discover_default_config_path() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        starts.push(current_dir);
    }
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    for start in starts {
        let mut dir = Some(start.as_path());
        while let Some(current) = dir {
            let candidate = current.join(DEFAULT_CONFIG_PATH);
            if candidate.is_file() {
                return Some(absolute_config_path(candidate));
            }
            dir = current.parent();
        }
    }

    None
}

fn absolute_config_path(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(&path))
            .unwrap_or(path)
    };
    absolute.canonicalize().unwrap_or(absolute)
}

async fn get_status(State(state): State<WebState>) -> Result<Json<StatusSnapshot>, AppError> {
    runtime::status(&state.config_path)
        .map(Json)
        .map_err(AppError::internal)
}

async fn list_candidates(
    State(state): State<WebState>,
) -> Result<Json<Vec<CandidateMetadata>>, AppError> {
    runtime::list(&state.config_path)
        .map(Json)
        .map_err(AppError::internal)
}

async fn get_search(
    State(state): State<WebState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, AppError> {
    let query = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| AppError::bad_request("search query must not be empty"))?;
    let top_k = params
        .top_k
        .unwrap_or(DEFAULT_SEARCH_TOP_K)
        .clamp(1, MAX_SEARCH_TOP_K);

    search_configured(
        &state.config_path,
        SearchOptions {
            knowledge_base_id: params.knowledge_base,
            query: query.to_string(),
            top_k,
        },
    )
    .map(Json)
    .map_err(AppError::internal)
}

async fn get_candidate(
    State(state): State<WebState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<CandidateDetailResponse>, AppError> {
    candidate_detail(&state.config_path, &run_id).map(Json)
}

async fn get_candidate_summaries(
    State(state): State<WebState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<String, AppError> {
    non_empty_run_id(&run_id)?;
    runtime::candidate_summaries(&state.config_path, &run_id).map_err(AppError::internal)
}

async fn get_candidate_diff(
    State(state): State<WebState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<String, AppError> {
    non_empty_run_id(&run_id)?;
    let metadata = find_candidate(&state.config_path, &run_id)?;
    if metadata.status != CandidateStatus::DiffReady {
        return Err(AppError::conflict(format!(
            "candidate {run_id} has no knowledge diff yet; approve staged summaries first"
        )));
    }
    runtime::candidate_diff(&state.config_path, &run_id).map_err(AppError::internal)
}

async fn post_approve_summaries(
    State(state): State<WebState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<ActionJob>, AppError> {
    ensure_status(
        &state.config_path,
        &run_id,
        CandidateStatus::SummariesStaged,
        "candidate summaries can only be approved while summaries are staged",
    )?;
    start_action_job(state, run_id, "approve_summaries", runtime::approve).map(Json)
}

async fn post_merge(
    State(state): State<WebState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<ActionJob>, AppError> {
    ensure_status(
        &state.config_path,
        &run_id,
        CandidateStatus::DiffReady,
        "candidate diff can only be merged after summaries are approved",
    )?;
    start_action_job(state, run_id, "merge_diff", runtime::merge).map(Json)
}

async fn post_reject(
    State(state): State<WebState>,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    append_service_log(
        &state.service_log_path,
        "info",
        "reject_started",
        "rejecting staged candidate",
        serde_json::json!({ "run_id": &run_id }),
    );
    let metadata = find_candidate(&state.config_path, &run_id)?;
    if metadata.status == CandidateStatus::Approved {
        append_service_log(
            &state.service_log_path,
            "warn",
            "reject_blocked",
            "approved candidate cannot be rejected",
            serde_json::json!({ "run_id": &run_id }),
        );
        return Err(AppError::conflict(format!(
            "candidate {run_id} has already been approved and cannot be rejected"
        )));
    }
    if let Err(error) = runtime::reject(&state.config_path, &run_id) {
        append_service_log(
            &state.service_log_path,
            "error",
            "reject_failed",
            "failed to reject staged candidate",
            serde_json::json!({ "run_id": &run_id, "error": format!("{error:#}") }),
        );
        return Err(AppError::internal(error));
    }
    append_service_log(
        &state.service_log_path,
        "info",
        "reject_completed",
        "staged candidate rejected",
        serde_json::json!({ "run_id": &run_id }),
    );
    Ok(Json(serde_json::json!({
        "run_id": run_id,
        "message": format!("rejected {}", metadata.run_id)
    })))
}

async fn get_job(
    State(state): State<WebState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<ActionJob>, AppError> {
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| AppError::internal(anyhow!("failed to lock action jobs")))?;
    jobs.get(&job_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("job not found: {job_id}")))
}

fn start_action_job(
    state: WebState,
    run_id: String,
    action: &'static str,
    operation: fn(&Path, &str) -> Result<ApproveOutcome>,
) -> Result<ActionJob, AppError> {
    let job_id = format!("{action}_{run_id}_{}", unix_ms());
    let job = ActionJob {
        job_id: job_id.clone(),
        run_id: run_id.clone(),
        action: action.to_string(),
        status: ActionJobStatus::Running,
        message: format!("{action} started"),
        started_at_unix_ms: unix_ms(),
        finished_at_unix_ms: None,
    };
    {
        let mut jobs = state
            .jobs
            .lock()
            .map_err(|_| AppError::internal(anyhow!("failed to lock action jobs")))?;
        jobs.insert(job_id.clone(), job.clone());
    }

    let jobs = Arc::clone(&state.jobs);
    let config_path = state.config_path.clone();
    let service_log_path = state.service_log_path.clone();
    append_service_log(
        &service_log_path,
        "info",
        "job_started",
        "candidate action job started",
        serde_json::json!({ "job_id": &job_id, "run_id": &run_id, "action": action }),
    );
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| operation(&config_path, &run_id));
        let (status, message) = match result {
            Ok(Ok(outcome)) => (ActionJobStatus::Completed, outcome.message),
            Ok(Err(error)) => (ActionJobStatus::Failed, format!("{error:#}")),
            Err(_) => (
                ActionJobStatus::Failed,
                format!("{action} panicked while processing {run_id}"),
            ),
        };
        if let Ok(mut jobs) = jobs.lock()
            && let Some(job) = jobs.get_mut(&job_id)
        {
            job.status = status;
            job.message = message.clone();
            job.finished_at_unix_ms = Some(unix_ms());
        }
        append_service_log(
            &service_log_path,
            if status == ActionJobStatus::Completed {
                "info"
            } else {
                "error"
            },
            if status == ActionJobStatus::Completed {
                "job_completed"
            } else {
                "job_failed"
            },
            "candidate action job finished",
            serde_json::json!({
                "job_id": job_id,
                "run_id": run_id,
                "action": action,
                "status": status,
                "message": message,
            }),
        );
    });

    Ok(job)
}

fn service_log_path_for_config(config_path: &Path) -> PathBuf {
    match AppConfig::load_or_default(config_path) {
        Ok(config) => WorkspacePaths::from_config(&config)
            .root
            .join("runtime")
            .join("web")
            .join("events.jsonl"),
        Err(error) => {
            eprintln!("warn: failed to resolve web service log path: {error:#}");
            config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(".wiki_craft")
                .join("runtime")
                .join("web")
                .join("events.jsonl")
        }
    }
}

fn append_service_log(
    path: &Path,
    level: &str,
    event: &str,
    message: &str,
    fields: serde_json::Value,
) {
    let result = (|| -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create web service log dir: {}", parent.display())
            })?;
        }
        let log_event = serde_json::json!({
            "kind": "web_service_event",
            "ts_unix_ms": unix_ms(),
            "level": level,
            "event": event,
            "message": message,
            "fields": fields,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open web service log: {}", path.display()))?;
        writeln!(file, "{log_event}").context("failed to append web service log event")?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("warn: failed to append web service log event: {error:#}");
    }
}

fn candidate_detail(config_path: &Path, run_id: &str) -> Result<CandidateDetailResponse, AppError> {
    non_empty_run_id(run_id)?;
    let metadata = find_candidate(config_path, run_id)?;
    let summaries_markdown =
        runtime::candidate_summaries(config_path, run_id).map_err(AppError::internal)?;
    let diff_markdown = if metadata.status == CandidateStatus::DiffReady {
        Some(runtime::candidate_diff(config_path, run_id).map_err(AppError::internal)?)
    } else {
        None
    };
    let actions = CandidateActions::for_status(&metadata.status);
    Ok(CandidateDetailResponse {
        metadata,
        actions,
        summaries_markdown,
        diff_markdown,
    })
}

fn find_candidate(config_path: &Path, run_id: &str) -> Result<CandidateMetadata, AppError> {
    non_empty_run_id(run_id)?;
    runtime::list(config_path)
        .map_err(AppError::internal)?
        .into_iter()
        .find(|metadata| metadata.run_id == run_id)
        .ok_or_else(|| AppError::conflict(format!("candidate not found: {run_id}")))
}

fn ensure_status(
    config_path: &Path,
    run_id: &str,
    expected: CandidateStatus,
    message: &str,
) -> Result<(), AppError> {
    let metadata = find_candidate(config_path, run_id)?;
    if metadata.status != expected {
        return Err(AppError::conflict(format!("{message}: {run_id}")));
    }
    Ok(())
}

fn non_empty_run_id(run_id: &str) -> Result<(), AppError> {
    if run_id.trim().is_empty() {
        return Err(AppError::bad_request("run_id must not be empty"));
    }
    Ok(())
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::candidates::{CandidateMetadata, CandidatePaths};
    use crate::config::{
        AppConfig, IngestConfig, KNOWLEDGE_BASE_CONFIG_FILE, KNOWLEDGE_BASE_REGISTRY_FILE,
        KNOWLEDGE_BASES_DIR, KnowledgeBaseCreateInput, KnowledgeBaseFileConfig,
        KnowledgeBaseRecord, KnowledgeBaseRegistry, create_knowledge_base,
    };
    use crate::knowledge::WorkspacePaths;
    use crate::search::SearchResponse;
    use crate::sources::{ChangedSource, SourceManifest};
    use crate::support::now_unix_ms;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wiki-craft-web-{name}-{}", now_unix_ms()))
    }

    fn test_config(root: &Path) -> AppConfig {
        let mut config = AppConfig::default();
        config.runtime.root = root.join(".wiki_craft").display().to_string();
        config.metrics.dir = root
            .join(".wiki_craft")
            .join("runtime")
            .join("metrics")
            .display()
            .to_string();
        config.audit.path = root
            .join(".wiki_craft")
            .join("runtime")
            .join("audit")
            .join("events.jsonl")
            .display()
            .to_string();
        config
    }

    fn write_config(root: &Path) -> PathBuf {
        let config = test_config(root);
        let config_path = root.join("wiki_craft.toml");
        fs::create_dir_all(root).expect("root");
        fs::write(&config_path, toml::to_string_pretty(&config).expect("toml")).expect("config");
        write_active_kb_registry(&root.join(".wiki_craft"));
        config_path
    }

    fn write_active_kb_registry(workspace_root: &Path) {
        let registry = KnowledgeBaseRegistry {
            schema_version: 1,
            active_id: Some("test".to_string()),
            knowledge_bases: vec![KnowledgeBaseRecord {
                id: "test".to_string(),
                name: "Test".to_string(),
                focus: "Test focus".to_string(),
                root: workspace_root.display().to_string(),
                created_at_unix_ms: 1,
                updated_at_unix_ms: 1,
            }],
        };
        registry
            .save(
                &workspace_root
                    .join(KNOWLEDGE_BASES_DIR)
                    .join(KNOWLEDGE_BASE_REGISTRY_FILE),
            )
            .expect("registry");
        KnowledgeBaseFileConfig {
            name: "Test".to_string(),
            focus: "Test focus".to_string(),
            ingest: IngestConfig::default(),
        }
        .save(&workspace_root.join(KNOWLEDGE_BASE_CONFIG_FILE))
        .expect("knowledge base config");
    }

    fn changed_source() -> ChangedSource {
        ChangedSource {
            source_id: "source_1".to_string(),
            url: "https://example.test/source".to_string(),
            final_url: Some("https://example.test/source".to_string()),
            title: Some("Source".to_string()),
            etag: None,
            last_modified: None,
            previous_hash: None,
            new_hash: "abc123".to_string(),
            version_key: None,
            summary_path: "evidence/source_summaries/source_1.md".to_string(),
        }
    }

    fn create_candidate(root: &Path, status: CandidateStatus) -> (PathBuf, String) {
        let config_path = write_config(root);
        let config = AppConfig::load(&config_path).expect("load config");
        let paths = WorkspacePaths::from_config(&config);
        paths.ensure_all().expect("paths");
        SourceManifest::default()
            .save(&paths.manifest_path)
            .expect("manifest");
        let run_id = format!("run_{}", now_unix_ms());
        let candidate_paths = CandidatePaths::new(&paths, &run_id);
        candidate_paths.ensure().expect("candidate dirs");
        let changed = changed_source();
        fs::write(
            candidate_paths
                .source_summaries
                .join(format!("{}.md", changed.source_id)),
            "# Summary\n\nStaged summary.\n",
        )
        .expect("summary");
        if status == CandidateStatus::DiffReady {
            fs::create_dir_all(&candidate_paths.knowledge).expect("knowledge");
            fs::create_dir_all(&candidate_paths.baseline_knowledge).expect("baseline");
            fs::write(
                &candidate_paths.diff,
                "# Wiki Craft Candidate Diff\n\n```diff\n+new\n```",
            )
            .expect("diff");
        }
        let metadata = CandidateMetadata {
            schema_version: 1,
            run_id: run_id.clone(),
            created_at_unix_ms: now_unix_ms(),
            status,
            changed_sources: vec![changed],
            prompt_cache_stats: Default::default(),
            compaction_count: 0,
        };
        crate::candidates::write_candidate_metadata(&candidate_paths, &metadata).expect("metadata");
        (config_path, run_id)
    }

    async fn response_status(router: Router, uri: String) -> StatusCode {
        router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    async fn delete_response(
        router: Router,
        uri: String,
        confirmation_name: &str,
    ) -> (StatusCode, String) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "confirmation_name": confirmation_name }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn search_response(router: Router, uri: &str) -> (StatusCode, Option<SearchResponse>) {
        let response = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed = if status == StatusCode::OK {
            Some(serde_json::from_slice::<SearchResponse>(&bytes).unwrap())
        } else {
            None
        };
        (status, parsed)
    }

    fn write_search_fixture(root: &Path) -> PathBuf {
        let config_path = write_config(root);
        let config = AppConfig::load(&config_path).expect("load config");
        let paths = WorkspacePaths::from_config(&config);
        fs::create_dir_all(paths.knowledge_current.join("topics")).expect("approved topics");
        fs::create_dir_all(
            root.join(".wiki_craft")
                .join("knowledge")
                .join("staging")
                .join("candidates")
                .join("run_1")
                .join("knowledge")
                .join("topics"),
        )
        .expect("staged topics");
        fs::write(
            paths.knowledge_current.join("index.md"),
            "# Index\n\n- [[topics/search|Search]]\n",
        )
        .expect("approved index");
        fs::write(
            paths.knowledge_current.join("topics").join("search.md"),
            "---\ntitle: \"Search\"\naliases: [retrieval]\ntags: [knowledge]\nsource_ids: []\nsource_urls: []\nversion_hashes: []\n---\n\n# Search\nApproved retrieval knowledge.\n",
        )
        .expect("approved topic");
        fs::write(
            root.join(".wiki_craft")
                .join("knowledge")
                .join("staging")
                .join("candidates")
                .join("run_1")
                .join("knowledge")
                .join("topics")
                .join("draft.md"),
            "# Draft\n\nsecret draft term\n",
        )
        .expect("staged topic");
        config_path
    }

    #[tokio::test]
    async fn list_and_detail_include_actions_and_summaries() {
        let root = temp_root("detail");
        let (config_path, run_id) = create_candidate(&root, CandidateStatus::SummariesStaged);
        let router = api_router(config_path);
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/api/candidates/{run_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn diff_requires_diff_ready_status() {
        let root = temp_root("diff-409");
        let (config_path, run_id) = create_candidate(&root, CandidateStatus::SummariesStaged);
        let status = response_status(
            api_router(config_path),
            format!("/api/candidates/{run_id}/diff"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn diff_ready_candidate_serves_diff() {
        let root = temp_root("diff-ok");
        let (config_path, run_id) = create_candidate(&root, CandidateStatus::DiffReady);
        let status = response_status(
            api_router(config_path),
            format!("/api/candidates/{run_id}/diff"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn search_returns_matching_approved_topic_results() {
        let root = temp_root("search-ok");
        let config_path = write_search_fixture(&root);
        let (status, response) = search_response(
            api_router(config_path),
            "/api/search?query=retrieval&top_k=5",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let response = response.expect("search response");
        assert_eq!(response.top_k, 5);
        assert!(
            response
                .results
                .iter()
                .any(|result| result.title.as_deref() == Some("Search"))
        );
    }

    #[tokio::test]
    async fn search_rejects_empty_query() {
        let root = temp_root("search-empty");
        let config_path = write_search_fixture(&root);
        let status =
            response_status(api_router(config_path), "/api/search?query=%20".to_string()).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_does_not_return_staged_candidate_content() {
        let root = temp_root("search-staged");
        let config_path = write_search_fixture(&root);
        let (status, response) = search_response(
            api_router(config_path),
            "/api/search?query=secret%20draft%20term&top_k=5",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(response.expect("search response").results.is_empty());
    }

    #[tokio::test]
    async fn create_skill_endpoint_writes_skill_to_custom_destination() {
        let root = temp_root("skill");
        let config_path = write_search_fixture(&root);
        let destination = root.join("generated-skills");
        let body = serde_json::json!({
            "target": "custom",
            "destination_path": destination.display().to_string(),
        })
        .to_string();
        let response = api_router(config_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/knowledge-bases/test/skill")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let outcome = serde_json::from_slice::<CreateSkillOutcome>(&bytes).unwrap();
        let skill_md = fs::read_to_string(Path::new(&outcome.skill_path).join("SKILL.md"))
            .expect("generated skill");
        assert!(skill_md.contains("--knowledge-base 'test'"));
        assert!(skill_md.contains("Focus: Test focus"));
    }

    #[tokio::test]
    async fn delete_knowledge_base_endpoint_returns_updated_list() {
        let root = temp_root("delete-kb");
        fs::create_dir_all(&root).expect("root");
        let config = test_config(&root);
        let config_path = root.join("wiki_craft.toml");
        fs::write(&config_path, toml::to_string_pretty(&config).expect("toml")).expect("config");
        let first = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "First".to_string(),
                focus: "First focus".to_string(),
            },
        )
        .expect("create first");
        let second = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Second".to_string(),
                focus: "Second focus".to_string(),
            },
        )
        .expect("create second");

        let (status, body) = delete_response(
            api_router(config_path),
            format!("/api/knowledge-bases/{}", second.id),
            "Second",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let list = serde_json::from_str::<KnowledgeBaseList>(&body).expect("list");
        assert_eq!(list.active_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(list.knowledge_bases.len(), 1);
        assert_eq!(list.knowledge_bases[0].id, first.id);
        assert!(!Path::new(&second.root).exists());
    }

    #[tokio::test]
    async fn delete_knowledge_base_endpoint_rejects_wrong_confirmation() {
        let root = temp_root("delete-kb-confirm");
        fs::create_dir_all(&root).expect("root");
        let config = test_config(&root);
        let config_path = root.join("wiki_craft.toml");
        fs::write(&config_path, toml::to_string_pretty(&config).expect("toml")).expect("config");
        let record = create_knowledge_base(
            &config_path,
            KnowledgeBaseCreateInput {
                name: "Exact".to_string(),
                focus: "Focus".to_string(),
            },
        )
        .expect("create");

        let (status, _body) = delete_response(
            api_router(config_path),
            format!("/api/knowledge-bases/{}", record.id),
            "exact",
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(Path::new(&record.root).exists());
    }
}
