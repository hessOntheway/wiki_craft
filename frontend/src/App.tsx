import { useCallback, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import {
  Check,
  CircleAlert,
  Database,
  FileDiff,
  FileText,
  GitMerge,
  LoaderCircle,
  Plus,
  RefreshCw,
  Save,
  Search,
  Trash2,
} from "lucide-react";

type CandidateStatus = "summaries_staged" | "diff_ready" | "approved";
type TabKey = "summaries" | "diff" | "metadata";
type PageMode = "review" | "search";
type SearchResultKind = "index" | "topic" | "source_summary";
type SkillTarget = "codex" | "claude" | "custom";

interface ChangedSource {
  source_id: string;
  url: string;
  final_url?: string | null;
  title?: string | null;
  previous_hash?: string | null;
  new_hash: string;
  summary_path: string;
}

interface PromptCacheStats {
  requests?: number;
  hits?: number;
  misses?: number;
  writes?: number;
  saved_input_tokens?: number;
}

interface CandidateMetadata {
  schema_version: number;
  run_id: string;
  created_at_unix_ms: number;
  status: CandidateStatus;
  changed_sources: ChangedSource[];
  prompt_cache_stats?: PromptCacheStats;
  compaction_count?: number;
}

interface CandidateActions {
  approve_summaries: boolean;
  merge_diff: boolean;
  reject: boolean;
}

interface CandidateDetail {
  metadata: CandidateMetadata;
  actions: CandidateActions;
  summaries_markdown: string;
  diff_markdown?: string | null;
}

interface KnowledgeBaseRecord {
  id: string;
  name: string;
  focus: string;
  root: string;
  created_at_unix_ms: number;
  updated_at_unix_ms: number;
}

interface KnowledgeBaseListResponse {
  active_id?: string | null;
  knowledge_bases: KnowledgeBaseRecord[];
}

interface KnowledgeBaseCreateResponse {
  knowledge_base: KnowledgeBaseRecord;
}

interface CreateSkillResponse {
  skill_name: string;
  skill_path: string;
  message: string;
}

interface StatusSnapshot {
  pending_candidates: number;
  compaction_count: number;
  updated_at_unix_ms: number;
  last_run?: {
    run_id?: string | null;
    message: string;
    checked_sources: number;
    changed_sources: number;
  } | null;
}

interface ActionOutcome {
  run_id: string;
  status?: CandidateStatus;
  message: string;
  job_id?: string;
  action?: string;
}

interface ActionJob {
  job_id: string;
  run_id: string;
  action: string;
  status: "running" | "completed" | "failed";
  message: string;
}

interface SearchResult {
  path: string;
  kind: SearchResultKind;
  title?: string | null;
  heading?: string | null;
  score: number;
  line_start: number;
  line_end: number;
  snippet: string;
  aliases: string[];
  tags: string[];
  wikilinks: string[];
  source_ids: string[];
  source_urls: string[];
  version_hashes: string[];
  updated_at_run_id?: string | null;
}

interface SearchResponse {
  query: string;
  top_k: number;
  results: SearchResult[];
}

interface DiffBlock {
  kind: "markdown" | "diff";
  lines: string[];
}

type GuiLogLevel = "info" | "warn" | "error";

const tabs: Array<{ key: TabKey; label: string }> = [
  { key: "summaries", label: "Summaries" },
  { key: "diff", label: "Diff" },
  { key: "metadata", label: "Metadata" },
];

export function App() {
  const [pageMode, setPageMode] = useState<PageMode>("review");
  const [apiBaseUrl, setApiBaseUrl] = useState("");
  const [status, setStatus] = useState<StatusSnapshot | null>(null);
  const [knowledgeBases, setKnowledgeBases] = useState<KnowledgeBaseRecord[]>([]);
  const [activeKnowledgeBaseId, setActiveKnowledgeBaseId] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<CandidateMetadata[]>([]);
  const [selectedRunId, setSelectedRunId] = useState("");
  const [detail, setDetail] = useState<CandidateDetail | null>(null);
  const [activeTab, setActiveTab] = useState<TabKey>("summaries");
  const [loading, setLoading] = useState(true);
  const [busyAction, setBusyAction] = useState("");
  const [lastError, setLastError] = useState("");
  const [lastMessage, setLastMessage] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchTopK, setSearchTopK] = useState(5);
  const [searchResponse, setSearchResponse] = useState<SearchResponse | null>(null);
  const [searchLoading, setSearchLoading] = useState(false);
  const [searchError, setSearchError] = useState("");
  const [searchSearched, setSearchSearched] = useState(false);
  const [showKnowledgeBaseForm, setShowKnowledgeBaseForm] = useState(false);
  const [newKnowledgeBaseName, setNewKnowledgeBaseName] = useState("");
  const [newKnowledgeBaseFocus, setNewKnowledgeBaseFocus] = useState("");
  const [knowledgeBaseCreating, setKnowledgeBaseCreating] = useState(false);
  const [showSkillForm, setShowSkillForm] = useState(false);
  const [skillTarget, setSkillTarget] = useState<SkillTarget>("codex");
  const [skillCustomPath, setSkillCustomPath] = useState("");
  const [skillCreating, setSkillCreating] = useState(false);
  const [skillMessage, setSkillMessage] = useState("");
  const [skillError, setSkillError] = useState("");

  const apiUrl = useCallback(
    (path: string) => (apiBaseUrl ? `${apiBaseUrl}${path}` : path),
    [apiBaseUrl],
  );

  const requestJson = useCallback(
    async <T,>(path: string, options: RequestInit = {}): Promise<T> => {
      const url = apiUrl(path);
      let response: Response;
      try {
        response = await fetch(url, {
          headers: {
            "Content-Type": "application/json",
            ...(options.headers || {}),
          },
          ...options,
        });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        await logGuiEvent("error", "local_api_request_failed", { path, url, error: message });
        throw new Error(`Local API request failed: ${message}`);
      }
      const text = await response.text();
      let data: Record<string, unknown>;
      try {
        data = text ? JSON.parse(text) : {};
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        await logGuiEvent("error", "local_api_response_parse_failed", {
          path,
          url,
          status: response.status,
          error: message,
          bodyPreview: text.slice(0, 1000),
        });
        throw new Error(`Invalid local API response: ${message}`);
      }
      if (!response.ok) {
        const message = typeof data.error === "string" ? data.error : `Request failed: ${response.status}`;
        await logGuiEvent("error", "local_api_error_response", {
          path,
          url,
          status: response.status,
          error: message,
        });
        throw new Error(message);
      }
      return data as T;
    },
    [apiUrl],
  );

  const refresh = useCallback(
    async (preferredRunId?: string) => {
      setLastError("");
      const nextKnowledgeBases = await requestJson<KnowledgeBaseListResponse>("/api/knowledge-bases");
      setKnowledgeBases(nextKnowledgeBases.knowledge_bases);
      setActiveKnowledgeBaseId(nextKnowledgeBases.active_id || null);
      if (!nextKnowledgeBases.active_id) {
        setStatus(null);
        setCandidates([]);
        setSelectedRunId("");
        setDetail(null);
        return;
      }
      const [nextStatus, nextCandidates] = await Promise.all([requestJson<StatusSnapshot>("/api/status"), requestJson<CandidateMetadata[]>("/api/candidates")]);
      setStatus(nextStatus);
      setCandidates(nextCandidates);

      const nextSelected =
        preferredRunId && nextCandidates.some((candidate) => candidate.run_id === preferredRunId)
          ? preferredRunId
          : nextCandidates[0]?.run_id || "";
      setSelectedRunId(nextSelected);

      if (nextSelected) {
        const nextDetail = await requestJson<CandidateDetail>(`/api/candidates/${encodeURIComponent(nextSelected)}`);
        setDetail(nextDetail);
        if (activeTab === "diff" && !nextDetail.diff_markdown) {
          setActiveTab("summaries");
        }
      } else {
        setDetail(null);
        setActiveTab("summaries");
      }
    },
    [activeTab, requestJson],
  );

  const activeKnowledgeBase = useMemo(
    () => knowledgeBases.find((knowledgeBase) => knowledgeBase.id === activeKnowledgeBaseId) || null,
    [activeKnowledgeBaseId, knowledgeBases],
  );

  const createKnowledgeBase = async () => {
    const name = newKnowledgeBaseName.trim();
    const focus = newKnowledgeBaseFocus.trim();
    setLastError("");
    setLastMessage("");
    if (!name || !focus) {
      setLastError("Knowledge base name and focus are required.");
      return;
    }
    setKnowledgeBaseCreating(true);
    try {
      const response = await requestJson<KnowledgeBaseCreateResponse>("/api/knowledge-bases", {
        method: "POST",
        body: JSON.stringify({ name, focus }),
      });
      setNewKnowledgeBaseName("");
      setNewKnowledgeBaseFocus("");
      setShowKnowledgeBaseForm(false);
      setSearchResponse(null);
      setSearchSearched(false);
      setLastMessage(`Knowledge base created: ${response.knowledge_base.name}`);
      await refresh();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void logGuiEvent("error", "knowledge_base_create_failed", { error: message });
      setLastError(message);
    } finally {
      setKnowledgeBaseCreating(false);
    }
  };

  const activateKnowledgeBase = async (id: string) => {
    if (!id || id === activeKnowledgeBaseId) {
      return;
    }
    setLastError("");
    setLastMessage("");
    try {
      await requestJson<KnowledgeBaseCreateResponse>(`/api/knowledge-bases/${encodeURIComponent(id)}/activate`, { method: "POST" });
      setSearchResponse(null);
      setSearchSearched(false);
      await refresh();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void logGuiEvent("error", "knowledge_base_activate_failed", { id, error: message });
      setLastError(message);
    }
  };

  const createSkill = async () => {
    if (!activeKnowledgeBase) {
      return;
    }
    setSkillError("");
    setSkillMessage("");
    const customPath = skillCustomPath.trim();
    if (skillTarget === "custom" && !customPath) {
      setSkillError("Custom destination path is required.");
      return;
    }
    setSkillCreating(true);
    try {
      const response = await requestJson<CreateSkillResponse>(
        `/api/knowledge-bases/${encodeURIComponent(activeKnowledgeBase.id)}/skill`,
        {
          method: "POST",
          body: JSON.stringify({
            target: skillTarget,
            destination_path: skillTarget === "custom" ? customPath : undefined,
          }),
        },
      );
      setSkillMessage(`Skill created: ${response.skill_path}`);
      await logGuiEvent("info", "skill_create_completed", {
        knowledgeBaseId: activeKnowledgeBase.id,
        target: skillTarget,
        skillPath: response.skill_path,
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setSkillError(message);
      void logGuiEvent("error", "skill_create_failed", {
        knowledgeBaseId: activeKnowledgeBase.id,
        target: skillTarget,
        error: message,
      });
    } finally {
      setSkillCreating(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    async function boot() {
      setLoading(true);
      try {
        const baseUrl = await resolveApiBaseUrl();
        if (!cancelled) {
          setApiBaseUrl(baseUrl);
        }
      } catch (error) {
        if (!cancelled) {
          const message = error instanceof Error ? error.message : String(error);
          void logGuiEvent("error", "gui_boot_failed", { error: message });
          setLastError(message);
        }
      }
    }
    void boot();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      if (!apiBaseUrl && window.__TAURI_INTERNALS__) {
        return;
      }
      setLoading(true);
      try {
        await waitForHealth(apiUrl("/api/health"));
        if (!cancelled) {
          await refresh(selectedRunId);
        }
      } catch (error) {
        if (!cancelled) {
          const message = error instanceof Error ? error.message : String(error);
          void logGuiEvent("error", "gui_initial_load_failed", { error: message });
          setLastError(message);
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }
    void load();
    return () => {
      cancelled = true;
    };
  }, [apiBaseUrl, apiUrl, refresh, selectedRunId]);

  const selectedCandidate = useMemo(
    () => candidates.find((candidate) => candidate.run_id === selectedRunId) || null,
    [candidates, selectedRunId],
  );

  const onSelectCandidate = async (runId: string) => {
    if (busyAction || runId === selectedRunId) {
      return;
    }
    setSelectedRunId(runId);
    setLastError("");
    try {
      setDetail(await requestJson<CandidateDetail>(`/api/candidates/${encodeURIComponent(runId)}`));
      setActiveTab("summaries");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void logGuiEvent("error", "candidate_select_failed", { runId, error: message });
      setLastError(message);
    }
  };

  const runAction = async (action: "approve" | "merge" | "reject") => {
    if (!detail || busyAction) {
      return;
    }
    if (action === "reject" && !window.confirm(`Reject candidate ${detail.metadata.run_id}?`)) {
      return;
    }
    const endpoint =
      action === "approve"
        ? "approve-summaries"
        : action === "merge"
          ? "merge"
          : "reject";
    setBusyAction(action);
    setLastError("");
    setLastMessage("");
    try {
      await logGuiEvent("info", "candidate_action_started", {
        action,
        runId: detail.metadata.run_id,
      });
      const outcome = await requestJson<ActionOutcome | ActionJob>(
        `/api/candidates/${encodeURIComponent(detail.metadata.run_id)}/${endpoint}`,
        { method: "POST" },
      );
      const finalOutcome = typeof outcome.job_id === "string" ? await waitForJob(outcome.job_id) : outcome;
      setLastMessage(finalOutcome.message);
      const preferredRunId = action === "reject" || action === "merge" ? undefined : finalOutcome.run_id;
      await refresh(preferredRunId);
      if (action === "approve") {
        setActiveTab("diff");
      }
      await logGuiEvent("info", "candidate_action_completed", {
        action,
        runId: detail.metadata.run_id,
        message: finalOutcome.message,
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void logGuiEvent("error", "candidate_action_failed", {
        action,
        runId: detail.metadata.run_id,
        error: message,
      });
      setLastError(message);
    } finally {
      setBusyAction("");
    }
  };

  const waitForJob = async (jobId: string): Promise<ActionJob> => {
    const deadline = Date.now() + 180000;
    while (Date.now() < deadline) {
      const job = await requestJson<ActionJob>(`/api/jobs/${encodeURIComponent(jobId)}`);
      if (job.status === "completed") {
        void logGuiEvent("info", "candidate_action_job_completed", {
          jobId,
          action: job.action,
          runId: job.run_id,
          message: job.message,
        });
        return job;
      }
      if (job.status === "failed") {
        void logGuiEvent("error", "candidate_action_job_failed", {
          jobId,
          action: job.action,
          runId: job.run_id,
          message: job.message,
        });
        throw new Error(job.message);
      }
      setLastMessage(job.message);
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
    void logGuiEvent("warn", "candidate_action_job_timeout", { jobId });
    throw new Error("Action is still running; refresh the candidate list in a moment.");
  };

  const runSearch = async () => {
    const query = searchQuery.trim();
    const topK = clampTopK(searchTopK);
    setSearchTopK(topK);
    setSearchError("");
    setSearchSearched(true);
    if (!query) {
      setSearchResponse(null);
      setSearchError("Search query must not be empty.");
      return;
    }
    setSearchLoading(true);
    try {
      await logGuiEvent("info", "search_started", { query, topK });
      const params = new URLSearchParams({
        query,
        top_k: String(topK),
      });
      const response = await requestJson<SearchResponse>(`/api/search?${params.toString()}`);
      setSearchResponse(response);
      await logGuiEvent("info", "search_completed", {
        query,
        topK,
        resultCount: response.results.length,
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void logGuiEvent("error", "search_failed", { query, topK, error: message });
      setSearchResponse(null);
      setSearchError(message);
    } finally {
      setSearchLoading(false);
    }
  };

  const canApprove = Boolean(detail?.actions.approve_summaries);
  const canMerge = Boolean(detail?.actions.merge_diff);
  const canReject = Boolean(detail?.actions.reject);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <header className="brand">
          <p className="eyebrow">Wiki Craft</p>
          <h1>Staging Review</h1>
        </header>

        <nav className="mode-switch" aria-label="Workspace mode">
          <button className={pageMode === "review" ? "active" : ""} type="button" onClick={() => setPageMode("review")}>
            <FileText size={16} />
            Review
          </button>
          <button className={pageMode === "search" ? "active" : ""} type="button" onClick={() => setPageMode("search")}>
            <Search size={16} />
            Search
          </button>
        </nav>

        <section className="knowledge-base-panel" aria-label="Knowledge bases">
          <div className="knowledge-base-head">
            <span className="section-title">Knowledge Base</span>
            <button className="icon-button" type="button" onClick={() => setShowKnowledgeBaseForm((value) => !value)} title="New knowledge base">
              <Plus size={17} />
            </button>
          </div>
          {knowledgeBases.length > 0 ? (
            <label className="knowledge-base-select">
              <Database size={16} />
              <select value={activeKnowledgeBaseId || ""} onChange={(event) => void activateKnowledgeBase(event.target.value)}>
                {knowledgeBases.map((knowledgeBase) => (
                  <option value={knowledgeBase.id} key={knowledgeBase.id}>
                    {knowledgeBase.name}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <p className="knowledge-base-empty">No knowledge base yet.</p>
          )}
          {activeKnowledgeBase && <p className="knowledge-base-focus">{activeKnowledgeBase.focus}</p>}
          {showKnowledgeBaseForm && (
            <form
              className="knowledge-base-form"
              onSubmit={(event) => {
                event.preventDefault();
                void createKnowledgeBase();
              }}
            >
              <input value={newKnowledgeBaseName} onChange={(event) => setNewKnowledgeBaseName(event.target.value)} placeholder="Name" />
              <textarea value={newKnowledgeBaseFocus} onChange={(event) => setNewKnowledgeBaseFocus(event.target.value)} placeholder="Focus" rows={4} />
              <button className="primary-button" type="submit" disabled={knowledgeBaseCreating}>
                {knowledgeBaseCreating ? <LoaderCircle className="spin" size={17} /> : <Plus size={17} />}
                Create
              </button>
            </form>
          )}
          {activeKnowledgeBase && (
            <div className="skill-panel">
              <button className="secondary-button" type="button" onClick={() => setShowSkillForm((value) => !value)}>
                <Save size={16} />
                Create Skill
              </button>
              {showSkillForm && (
                <form
                  className="skill-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void createSkill();
                  }}
                >
                  <label>
                    <span>Destination</span>
                    <select value={skillTarget} onChange={(event) => setSkillTarget(event.target.value as SkillTarget)}>
                      <option value="codex">Codex default</option>
                      <option value="claude">Claude default</option>
                      <option value="custom">Custom path</option>
                    </select>
                  </label>
                  {skillTarget === "custom" && (
                    <label>
                      <span>Path</span>
                      <input value={skillCustomPath} onChange={(event) => setSkillCustomPath(event.target.value)} placeholder="~/path/to/skills" />
                    </label>
                  )}
                  <button className="primary-button" type="submit" disabled={skillCreating}>
                    {skillCreating ? <LoaderCircle className="spin" size={17} /> : <Save size={17} />}
                    Generate
                  </button>
                  {(skillError || skillMessage) && <p className={`skill-message ${skillError ? "error" : "success"}`}>{skillError || skillMessage}</p>}
                </form>
              )}
            </div>
          )}
        </section>

        <section className="status-strip" aria-label="Runtime status">
          <StatusLine label="Pending" value={String(status?.pending_candidates ?? 0)} />
          <StatusLine label="Candidates" value={String(candidates.length)} />
          <StatusLine label="Updated" value={formatTimestamp(status?.updated_at_unix_ms)} />
        </section>

        <div className="sidebar-head">
          <span className="section-title">Staging</span>
          <button className="icon-button" type="button" onClick={() => void refresh(selectedRunId)} disabled={loading || Boolean(busyAction)} title="Refresh">
            <RefreshCw size={17} />
          </button>
        </div>

        <nav className="candidate-list" aria-label="Candidates">
          {candidates.length === 0 ? (
            <div className="empty-list">
              <FileText size={20} />
              <p>No staged candidates.</p>
            </div>
          ) : (
            candidates.map((candidate) => (
              <button
                className={`candidate-row ${candidate.run_id === selectedRunId ? "active" : ""}`}
                type="button"
                key={candidate.run_id}
                onClick={() => void onSelectCandidate(candidate.run_id)}
                disabled={Boolean(busyAction)}
              >
                <span className="candidate-main">
                  <strong>{candidate.run_id}</strong>
                  <span>{candidate.changed_sources.length} source{candidate.changed_sources.length === 1 ? "" : "s"}</span>
                </span>
                <StatusBadge status={candidate.status} />
              </button>
            ))
          )}
        </nav>

        <section className="last-run" aria-label="Last run">
          <span className="section-title">Last Run</span>
          <p>{status?.last_run?.message || "No runtime status yet."}</p>
        </section>
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div>
            <p className="eyebrow">{activeKnowledgeBase?.name || "No Knowledge Base"}</p>
            <h2>{pageMode === "review" ? selectedCandidate?.run_id || "No candidate selected" : "Search"}</h2>
          </div>
          {pageMode === "review" && (
            <div className="header-actions">
              <button className="primary-button" type="button" onClick={() => void runAction("approve")} disabled={!canApprove || Boolean(busyAction)}>
                {busyAction === "approve" ? <LoaderCircle className="spin" size={17} /> : <Check size={17} />}
                Approve Summaries
              </button>
              <button className="primary-button merge" type="button" onClick={() => void runAction("merge")} disabled={!canMerge || Boolean(busyAction)}>
                {busyAction === "merge" ? <LoaderCircle className="spin" size={17} /> : <GitMerge size={17} />}
                Merge Diff
              </button>
              <button className="danger-button" type="button" onClick={() => void runAction("reject")} disabled={!canReject || Boolean(busyAction)}>
                {busyAction === "reject" ? <LoaderCircle className="spin" size={17} /> : <Trash2 size={17} />}
                Reject
              </button>
            </div>
          )}
        </header>

        {pageMode === "review" && (lastError || lastMessage) && (
          <div className={`notice ${lastError ? "error" : "success"}`} role="status">
            {lastError ? <CircleAlert size={17} /> : <Check size={17} />}
            <span>{lastError || lastMessage}</span>
          </div>
        )}

        {pageMode === "search" ? (
          <SearchSurface
            query={searchQuery}
            topK={searchTopK}
            response={searchResponse}
            loading={searchLoading}
            error={searchError}
            searched={searchSearched}
            onQueryChange={setSearchQuery}
            onTopKChange={setSearchTopK}
            onSubmit={() => void runSearch()}
          />
        ) : loading ? (
          <section className="empty-state">
            <LoaderCircle className="spin" size={28} />
            <p>Loading local staging state.</p>
          </section>
        ) : detail ? (
          <section className="review-surface">
            <div className="candidate-summary">
              <InfoMetric label="Status" value={<StatusBadge status={detail.metadata.status} />} />
              <InfoMetric label="Sources" value={String(detail.metadata.changed_sources.length)} />
              <InfoMetric label="Created" value={formatTimestamp(detail.metadata.created_at_unix_ms)} />
              <InfoMetric label="Compactions" value={String(detail.metadata.compaction_count || 0)} />
            </div>

            <div className="source-list" aria-label="Changed sources">
              {detail.metadata.changed_sources.map((source) => (
                <a href={source.final_url || source.url} target="_blank" rel="noreferrer" className="source-pill" key={source.source_id}>
                  <span>{source.title || source.source_id}</span>
                  <small>{source.new_hash.slice(0, 12)}</small>
                </a>
              ))}
            </div>

            <div className="tab-bar" role="tablist">
              {tabs.map((tab) => (
                <button
                  className={activeTab === tab.key ? "active" : ""}
                  type="button"
                  key={tab.key}
                  onClick={() => setActiveTab(tab.key)}
                  disabled={tab.key === "diff" && !detail.diff_markdown}
                >
                  {tab.key === "diff" ? <FileDiff size={16} /> : <FileText size={16} />}
                  {tab.label}
                </button>
              ))}
            </div>

            <div className="content-pane">
              {activeTab === "summaries" && <MarkdownView content={detail.summaries_markdown} />}
              {activeTab === "diff" && <DiffMarkdownView content={detail.diff_markdown || "No diff is available yet."} />}
              {activeTab === "metadata" && <pre className="metadata-view">{JSON.stringify(detail.metadata, null, 2)}</pre>}
            </div>
          </section>
        ) : (
          <section className="empty-state">
            <FileText size={30} />
            <p>No staged knowledge is waiting for review.</p>
          </section>
        )}
      </main>
    </div>
  );
}

function SearchSurface({
  query,
  topK,
  response,
  loading,
  error,
  searched,
  onQueryChange,
  onTopKChange,
  onSubmit,
}: {
  query: string;
  topK: number;
  response: SearchResponse | null;
  loading: boolean;
  error: string;
  searched: boolean;
  onQueryChange: (query: string) => void;
  onTopKChange: (topK: number) => void;
  onSubmit: () => void;
}) {
  return (
    <section className="search-surface">
      <form
        className="search-form"
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit();
        }}
      >
        <label className="search-field">
          <span>Query</span>
          <input
            type="search"
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder="Search approved knowledge"
          />
        </label>
        <label className="top-k-field">
          <span>Top K</span>
          <input
            type="number"
            min={1}
            max={20}
            value={topK}
            onChange={(event) => onTopKChange(Number(event.target.value))}
          />
        </label>
        <button className="primary-button" type="submit" disabled={loading}>
          {loading ? <LoaderCircle className="spin" size={17} /> : <Search size={17} />}
          Search
        </button>
      </form>

      {error && (
        <div className="notice error" role="status">
          <CircleAlert size={17} />
          <span>{error}</span>
        </div>
      )}

      <div className="search-results" aria-live="polite">
        {loading ? (
          <section className="empty-state">
            <LoaderCircle className="spin" size={28} />
            <p>Searching approved knowledge.</p>
          </section>
        ) : response && response.results.length > 0 ? (
          <>
            <div className="result-count">
              <span>{response.results.length} result{response.results.length === 1 ? "" : "s"}</span>
              <strong>{response.query}</strong>
            </div>
            {response.results.map((result, index) => (
              <SearchResultCard result={result} index={index} key={`${result.path}-${result.line_start}-${index}`} />
            ))}
          </>
        ) : searched && !error ? (
          <section className="empty-state">
            <Search size={30} />
            <p>No approved knowledge matched this query.</p>
          </section>
        ) : (
          <section className="empty-state">
            <Search size={30} />
            <p>Search approved topics, index pages, and source summaries.</p>
          </section>
        )}
      </div>
    </section>
  );
}

function SearchResultCard({ result, index }: { result: SearchResult; index: number }) {
  const title = result.title || result.heading || result.path;
  const metadata = [
    formatSearchKind(result.kind),
    `Score ${result.score.toFixed(2)}`,
    `${result.path}:${result.line_start}${result.line_end !== result.line_start ? `-${result.line_end}` : ""}`,
  ];

  return (
    <article className="search-result-card">
      <header>
        <div>
          <span className="result-rank">{index + 1}</span>
          <h3>{title}</h3>
        </div>
        <span className={`result-kind ${result.kind}`}>{formatSearchKind(result.kind)}</span>
      </header>
      {result.heading && result.heading !== title && <p className="result-heading">{result.heading}</p>}
      <p className="result-meta">{metadata.join(" | ")}</p>
      <pre className="result-snippet">{result.snippet}</pre>
      <ResultPills label="Tags" values={result.tags} />
      <ResultPills label="Wikilinks" values={result.wikilinks} />
      {result.source_urls.length > 0 && (
        <div className="source-links">
          <span>Sources</span>
          {result.source_urls.map((url) => (
            <a href={url} target="_blank" rel="noreferrer" key={url}>
              {url}
            </a>
          ))}
        </div>
      )}
    </article>
  );
}

function ResultPills({ label, values }: { label: string; values: string[] }) {
  if (values.length === 0) {
    return null;
  }
  return (
    <div className="result-pills">
      <span>{label}</span>
      {values.map((value) => (
        <small key={value}>{value}</small>
      ))}
    </div>
  );
}

function StatusLine({ label, value }: { label: string; value: string }) {
  return (
    <div className="status-line">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function InfoMetric({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="info-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function StatusBadge({ status }: { status: CandidateStatus }) {
  return <span className={`status-badge ${status}`}>{formatStatus(status)}</span>;
}

function MarkdownView({ content }: { content: string }) {
  return (
    <div className="markdown-view">
      {content.split("\n").map((line, index) => {
        if (line.startsWith("# ")) {
          return <h3 key={index}>{line.slice(2)}</h3>;
        }
        if (line.startsWith("## ")) {
          return <h4 key={index}>{line.slice(3)}</h4>;
        }
        if (line.trim() === "") {
          return <div className="blank-line" key={index} />;
        }
        return <p key={index}>{line}</p>;
      })}
    </div>
  );
}

function DiffMarkdownView({ content }: { content: string }) {
  return (
    <div className="diff-markdown">
      {parseDiffMarkdown(content).map((block, blockIndex) =>
        block.kind === "diff" ? (
          <pre className="diff-view" key={blockIndex}>
            {block.lines.map((line, lineIndex) => (
              <span className={`diff-line ${diffLineKind(line)}`} key={`${blockIndex}-${lineIndex}`}>
                {line || " "}
              </span>
            ))}
          </pre>
        ) : (
          <MarkdownView content={block.lines.join("\n")} key={blockIndex} />
        ),
      )}
    </div>
  );
}

function parseDiffMarkdown(content: string): DiffBlock[] {
  const blocks: DiffBlock[] = [];
  let current: DiffBlock = { kind: "markdown", lines: [] };
  for (const line of content.split("\n")) {
    if (line.trim() === "```diff") {
      if (current.lines.length > 0) {
        blocks.push(current);
      }
      current = { kind: "diff", lines: [] };
      continue;
    }
    if (current.kind === "diff" && line.trim() === "```") {
      blocks.push(current);
      current = { kind: "markdown", lines: [] };
      continue;
    }
    current.lines.push(line);
  }
  if (current.lines.length > 0) {
    blocks.push(current);
  }
  return blocks;
}

function diffLineKind(line: string) {
  if (line.startsWith("@@")) {
    return "hunk";
  }
  if (line.startsWith("+++") || line.startsWith("---")) {
    return "file";
  }
  if (line.startsWith("+")) {
    return "added";
  }
  if (line.startsWith("-")) {
    return "removed";
  }
  return "context";
}

function formatStatus(status: CandidateStatus) {
  switch (status) {
    case "summaries_staged":
      return "Summaries Staged";
    case "diff_ready":
      return "Diff Ready";
    case "approved":
      return "Approved";
  }
}

function formatTimestamp(value?: number | null) {
  if (!value) {
    return "-";
  }
  return new Date(value).toLocaleString();
}

function formatSearchKind(kind: SearchResultKind) {
  switch (kind) {
    case "index":
      return "Index";
    case "topic":
      return "Topic";
    case "source_summary":
      return "Source Summary";
  }
}

function clampTopK(value: number) {
  if (!Number.isFinite(value)) {
    return 5;
  }
  return Math.min(20, Math.max(1, Math.round(value)));
}

async function resolveApiBaseUrl() {
  const envBase = import.meta.env.VITE_API_BASE_URL;
  if (envBase) {
    return envBase.replace(/\/$/, "");
  }
  if (window.__TAURI_INTERNALS__) {
    const { invoke } = await import("@tauri-apps/api/core");
    return (await invoke<string>("get_api_base_url")).replace(/\/$/, "");
  }
  return "";
}

async function logGuiEvent(
  level: GuiLogLevel,
  message: string,
  context?: Record<string, unknown> | string,
) {
  if (level === "error") {
    console.error(message, context || "");
  } else if (level === "warn") {
    console.warn(message, context || "");
  } else {
    console.info(message, context || "");
  }

  if (!window.__TAURI_INTERNALS__) {
    return;
  }

  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("log_gui_event", {
      level,
      message,
      context: context || null,
    });
  } catch (error) {
    console.warn("failed to write GUI log", error);
  }
}

async function waitForHealth(url: string) {
  const deadline = Date.now() + 8000;
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) {
        return;
      }
      lastError = new Error(`Health check failed: ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 180));
  }
  throw lastError instanceof Error ? lastError : new Error("Local API did not become ready");
}
