// Type shapes returned by the Rust backend. Keep in lockstep with
// `app/src-tauri/src/lib.rs` (the IPC surface) — `serde` uses snake_case
// field names and serde renames nothing on the way out, so the TS keys
// mirror the Rust struct fields verbatim.

export type HealthStatus = {
  ok: boolean;
  db_path: string;
  schema_version: number;
  ts: string;
};

export type SourceRow = {
  id: string;
  kind: string;
  display_name: string;
  root_path: string;
  status: string;
  last_scan_at: string | null;
};

export type ScanReport = {
  ok: boolean;
  source_id: string;
  events_inserted: number;
  sessions_added: number;
  files_scanned: number;
  duration_ms: number;
  error: string | null;
};

export type ResetReport = {
  raw_events_deleted: number;
  sessions_summary_deleted: number;
  connector_runs_deleted: number;
};

export type LaunchdStatus = {
  installed: boolean;
  plist_path: string | null;
  binary_path: string | null;
  last_load_error: string | null;
};

export type AiSettings = {
  provider: "local" | "openai" | "anthropic";
  local_endpoint: string;
  local_model: string;
  // v0.4.0 rename: was `openai_endpoint` / `openai_model`. The Rust
  // side uses `#[serde(alias = "openai_endpoint")]` to keep old
  // settings.json files loadable; the new wire shape is the field
  // names below. Use any OpenAI-compatible endpoint — DeepSeek,
  // Moonshot Kimi, Qwen DashScope, Doubao Ark, GLM, MiniMax,
  // OpenAI itself, or a self-hosted proxy.
  cloud_endpoint: string;
  cloud_model: string;
  anthropic_endpoint: string;
  anthropic_model: string;
  api_key: string;
  redact_prompts: boolean;
  cache_hours: number;
};

export type AiStatus = {
  reachable: boolean;
  provider: string;
  endpoint: string;
  model: string;
  latency_ms: number | null;
  installed_models: string[];
  error: string | null;
  has_api_key: boolean;
};

export type AiQueryReport = {
  ok: boolean;
  response: string;
  provider: string;
  model: string;
  duration_ms: number;
  redacted_chars: number;
  error: string | null;
};

export type EventRow = {
  id: string;
  source_id: string;
  source_kind: string;
  source_display_name: string;
  event_type: string;
  occurred_at: string;
  project_key: string;
  payload_preview: string;
};

// One row from `sessions_summary`. Reflected from lib.rs and used as the
// raw ETL partition the Sessions page renders before the user clicks
// "Summarise" to ask the LLM layer.
export type SessionPartition = {
  id: string;
  source_id: string;
  source_display_name: string;
  started_at: string;
  ended_at: string;
  event_count: number;
  project_key: string | null;
  summary: string;
  importance: number;
  heuristic_kind: string;
};

// Returned by the per-session AI enrichment IPC. `provider === "heuristic"`
// means the LLM wasn't reached (no provider configured or parse error) and
// the headline / body / urgency come from the local ETL fallback.
export type SessionSummary = {
  id: string;
  session_summary_id: string;
  headline: string;
  body: string;
  importance: number;
  urgency: number;
  reasoning: string;
  provider: string;
  model: string;
  expires_at: string;
  created_at: string;
  cached: boolean;
  age_hours: number;
};

// Stage 5-E.4a: master-detail event list. Reflected from
// `summary::SessionEventRow`. The Sessions page right pane renders
// every raw event that fell inside the partition's time window — full
// payload, no truncation, ordered ascending. The user can read what
// actually happened in that bucket without an LLM roundtrip.
export type SessionEvent = {
  id: string;
  event_type: string;
  occurred_at: string;
  project_key: string | null;
  payload: string;
};

// Stage 5-E.4b: portable Markdown bundle returned by
// `render_session_markdown`. `content` is the full file body the
// frontend writes through `write_text_file`; `suggested_filename` is
// the default stem the Save dialog opens with (the dialog may
// append `.md` if the user doesn't).
export type MarkdownExport = {
  content: string;
  suggested_filename: string;
};

// Stage 5.5: accepted values for the `format` arg of
// `export_sessions_bulk`. Kept as a literal union so the frontend
// stays in sync with the Rust `FromStr for BulkFormat` without an
// extra shared type.
export type BulkExportFormat = "json" | "csv";

// Stage 5.5 / v0.3.5: payload of `render_sessions_bulk_zip` IPC. The
// zip bytes cross the IPC boundary as base64 (the Tauri 2 IPC layer
// is JSON-only) and are decoded by `write_binary_file` on the way
// out. `count` is the number of partitions that ended up inside
// the archive so the UI can render a summary post-save.
export type BulkZipExport = {
  content_base64: string;
  suggested_filename: string;
  count: number;
};

// Stage 5.7 / v0.3.7: a single cross-source search hit. `kind` is
// one of `"session"` / `"event"` / `"plan"` / `"source"` and
// determines the right "open" destination in the frontend (the
// click-through handler dispatches on `kind`).
//
// v0.5.4 polish: `snippet` may include `<mark>...</mark>` markers
// around the matching positions. The renderer treats the snippet
// as HTML; everything outside the markers is HTML-escaped by
// the backend so the snippet is safe to render via
// `dangerouslySetInnerHTML`.
export type SearchHit = {
  kind: "session" | "event" | "plan" | "source";
  id: string;
  source_id: string;
  source_display_name: string;
  title: string;
  snippet: string;
  project_key: string | null;
  started_at: string | null;
};

// v0.5.4: per-source match counts BEFORE the per-source LIMIT cap
// is applied. Lets the UI render "会话 · 12 / 事件 · 47 / …" so
// the user can see at a glance which source owns the result.
export type SearchCounts = {
  sessions: number;
  events: number;
  plans: number;
  sources: number;
  total: number;
};

// v0.5.4: `global_search` now returns the hit array + counts +
// elapsed_ms. The frontend reads `counts` to render the per-source
// header and `elapsed_ms` to render the "took N ms" footer.
export type SearchResult = {
  hits: SearchHit[];
  counts: SearchCounts;
  elapsed_ms: number;
};

// Returned by `rebuild_sessions_summary` — a manual bulk re-ETL pass.
export type EtlReport = {
  partitions_total: number;
  sources_rebuilt: number;
  duration_ms: number;
};

// One row from `window_reports` — a daily or weekly digest the user
// requested. Headline / bullets / themes / project_breakdown come out
// of the LLM layer (or the heuristic fallback) and are persisted as a
// JSON blob on `window_reports.response`.
export type WindowReport = {
  id: string;
  window_label: "day" | "week" | string;
  window_start: string;
  window_end: string;
  headline: string;
  bullets: string[];
  themes: string[];
  project_breakdown: ProjectBreakdown[];
  provider: string;
  model: string;
  expires_at: string;
  created_at: string;
  cached: boolean;
  age_hours: number;
};

export type ProjectBreakdown = {
  project_key: string;
  sessions: number;
  events: number;
};

// Stage 5-E.2a: app-level (non-AI) settings. Kept separate from
// `AiSettings` because switching the LLM provider shouldn't drag the
// user's content choices (summary dir, future theme, …) around.
export type AppSettings = {
  // Absolute path to the directory the user keeps their hand-written
  // markdown summaries in. The scanner (Stage 5-E.2b) walks one level
  // deep for `.md` files. Backend expands `~/` before returning.
  summary_dir: string;
  // v0.4.3: how many days of `raw_events` to keep on disk before the
  // auto-rescan pass deletes them. `0` = 永久保留 (prune becomes a
  // no-op). Default 90.
  event_retention_days: number;
};

// v0.4.3: outcome of a prune pass. Returned to the Settings page so
// the user sees "已清理 N 条 / 90 天前的" without a second query.
export type PruneReport = {
  events_deleted: number;
  cutoff_iso: string;
  days: number;
  ran: boolean;
};

// Stage 5-E.2a: built-in prompt template (Eisenhower classifier,
// daily-brief, session-summary). The Settings UI lists these so the
// user can preview / copy them. User-defined templates come from the
// summary_dir scanner (Stage 5-E.2b).
export type PromptTemplate = {
  id: string;
  title: string;
  description: string;
  body: string;
};

// Stage 5-E.3a: Eisenhower digest — projects grouped by quadrant after
// the LLM (or fallback heuristic) scores them. Used by the Planner
// page's matrix panel and the upcoming daily-brief card.
export type EisenhowerItem = {
  project_key: string;
  // v0.4.2: backfilled from `ProjectGroup.source_id` so the daily
  // brief can roll Q1 up per source. Empty for the brief sliver
  // between LLM parse and heuristic merge.
  source_id: string;
  source_display_name: string;
  quadrant: 1 | 2 | 3 | 4;
  importance: number;
  urgency: number;
  label: string;
  reasoning: string;
  event_count: number;
  ai_used: boolean;
};

export type EisenhowerDigest = {
  lookback_hours: number;
  generated_at: string;
  items: EisenhowerItem[];
  ai_used: boolean;
  note: string | null;
};

// v0.4.2: per-source Q1 rollup for the brief card. `count` is the
// number of Q1 items the source produced in the brief's 24 h
// window. Clicking through filters the Sessions page by
// `source_id`.
export type SourceBucket = {
  source_id: string;
  source_display_name: string;
  count: number;
};

// v0.4.2: morning-over-morning delta vs the most-recent previous
// brief. `null` for the first brief the app has ever generated.
export type BriefDelta = {
  q1_delta: number;
  projects_delta: number;
  // "more urgent" / "less urgent" / "same" — derived from
  // `q1_delta` so the UI can colour without re-computing.
  verdict: string;
};

// Stage 5-E.3c: morning brief — top-3 Q1 items + 1-line headline
// for the 8am launchd tick (or any user-driven rerun). Surfaced on
// the home page so the user opens the app and immediately sees
// what to do today.
export type DailyBrief = {
  date: string;
  generated_at: string;
  top_q1: EisenhowerItem[];
  headline: string;
  total_projects: number;
  total_q1: number;
  // v0.4.2: per-source Q1 rollup + delta vs the previous brief.
  by_source: SourceBucket[];
  delta_vs_prev: BriefDelta | null;
  ai_used: boolean;
  note: string | null;
};
