import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Link, useNavigate } from "react-router-dom";
import { format } from "date-fns";
import { zhCN } from "date-fns/locale";

import { Popover, PopoverContent, PopoverTrigger } from "./components/ui/popover";
import { Calendar } from "./components/ui/calendar";
import { cn } from "./lib/utils";
import type { BriefDelta, DailyBrief } from "./lib/types";

type HealthStatus = {
  ok: boolean;
  db_path: string;
  schema_version: number;
  ts: string;
};

type SourceRow = {
  id: string;
  kind: string;
  display_name: string;
  root_path: string;
  status: string;
  last_scan_at: string | null;
};

type ScanReport = {
  source_id: string;
  kind: string;
  status: string;
  events_inserted: number;
  sessions_added: number;
  files_scanned: number;
  duration_ms: number;
  error: string | null;
};

type ResetReport = {
  raw_events_deleted: number;
  sessions_summary_deleted: number;
  connector_runs_deleted: number;
};

type LaunchdStatus = {
  installed: boolean;
  plist_path: string | null;
  binary_path: string | null;
  last_load_error: string | null;
};

// Local mirror of the Rust AiSettings struct. Keep in lockstep with
// `app/src-tauri/src/ai.rs` and `app/src/lib/types.ts`. The two endpoint
// fields were added in Phase D.2 so the user can plug in OpenAI- or
// Anthropic-compatible upstream proxies (moonshot / deepseek / 自部署 etc).
type AiSettings = {
  provider: "local" | "openai" | "anthropic";
  local_endpoint: string;
  local_model: string;
  openai_endpoint: string;
  openai_model: string;
  anthropic_endpoint: string;
  anthropic_model: string;
  api_key: string;
  redact_prompts: boolean;
  cache_hours: number;
};

type AiStatus = {
  reachable: boolean;
  provider: string;
  endpoint: string;
  model: string;
  latency_ms: number | null;
  installed_models: string[];
  error: string | null;
  has_api_key: boolean;
};

type AiQueryReport = {
  ok: boolean;
  response: string;
  provider: string;
  model: string;
  duration_ms: number;
  redacted_chars: number;
  error: string | null;
};

type EventRow = {
  id: string;
  source_id: string;
  source_kind: string;
  source_display_name: string;
  event_type: string;
  occurred_at: string;
  project_key: string;
  payload_preview: string;
};

// Default starting point for the "since" filter when the user first opens
// the date picker in 指定起始 mode: 00:00 of yesterday. Live mode
// (`liveMode === true`) always overrides this with a rolling
// `Date.now() - 24h` window, so this constant is only a fallback for
// the picker.
function defaultSinceDate(): Date {
  const d = new Date();
  d.setDate(d.getDate() - 1);
  d.setHours(0, 0, 0, 0);
  return d;
}

export default function App() {
  const navigate = useNavigate();
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [events, setEvents] = useState<EventRow[]>([]);
  // Default raised from 25 → 100 in 1-G+1 so all four sources can be
  // visible without paging. The user can keep extending this with the
  // "Load more" button below the list (each click adds +100).
  const [eventsLimit, setEventsLimit] = useState(100);
  const [scan, setScan] = useState<ScanReport | null>(null);
  const [scanError, setScanError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [launchd, setLaunchd] = useState<LaunchdStatus | null>(null);
  const [aiSettings, setAiSettings] = useState<AiSettings | null>(null);
  const [aiStatus, setAiStatus] = useState<AiStatus | null>(null);
  const [aiPrompt, setAiPrompt] = useState("");
  const [aiLastReport, setAiLastReport] = useState<AiQueryReport | null>(null);

  // Stage 5-E.3c: daily morning brief. Cached on home-page mount
  // (from `daily_briefs` table, populated by the 8 am launchd tick)
  // and refreshed on demand via `generate_daily_brief`.
  const [brief, setBrief] = useState<DailyBrief | null>(null);
  const [briefBusy, setBriefBusy] = useState(false);

  // Phase D.3: time-window selector for all 7 scan buttons.
  //   liveMode === true  → since_ms = now - 24h (rolling window)
  //   liveMode === false → since_ms = sinceDate (user-picked calendar date)
  // The effective value is re-computed inside the scan handlers (not at
  // render time) so a long-running App shell doesn't drift the "now"
  // baseline between renders.
  const [liveMode, setLiveMode] = useState(true);
  const [sinceDate, setSinceDate] = useState<Date>(() => defaultSinceDate());

  const refreshHealth = useCallback(async () => {
    try {
      setHealth(await invoke<HealthStatus>("get_health"));
    } catch (e) {
      console.error("get_health failed", e);
    }
  }, []);

  const refreshSources = useCallback(async () => {
    try {
      setSources(await invoke<SourceRow[]>("list_sources"));
    } catch (e) {
      console.error("list_sources failed", e);
    }
  }, []);

  const refreshEvents = useCallback(async () => {
    try {
      setEvents(await invoke<EventRow[]>("list_recent_events", { limit: eventsLimit }));
    } catch (e) {
      console.error("list_recent_events failed", e);
    }
  }, [eventsLimit]);

  const refreshLaunchd = useCallback(async () => {
    try {
      setLaunchd(await invoke<LaunchdStatus>("launchd_status"));
    } catch (e) {
      console.error("launchd_status failed", e);
    }
  }, []);

  const refreshAiSettings = useCallback(async () => {
    try {
      setAiSettings(await invoke<AiSettings>("ai_get_settings"));
    } catch (e) {
      console.error("ai_get_settings failed", e);
    }
  }, []);

  const refreshAiStatus = useCallback(async () => {
    try {
      setAiStatus(await invoke<AiStatus>("ai_status"));
    } catch (e) {
      console.error("ai_status failed", e);
    }
  }, []);

  useEffect(() => {
    refreshHealth();
    refreshSources();
    refreshEvents();
    refreshLaunchd();
    refreshAiSettings();
    refreshAiStatus();
  }, [refreshHealth, refreshSources, refreshEvents, refreshLaunchd, refreshAiSettings, refreshAiStatus]);

  const refreshBrief = useCallback(async () => {
    try {
      setBrief(await invoke<DailyBrief | null>("latest_daily_brief"));
    } catch (e) {
      console.error("latest_daily_brief failed", e);
    }
  }, []);

  const runDailyBrief = useCallback(async () => {
    setBriefBusy(true);
    setScanError(null);
    try {
      const b = await invoke<DailyBrief>("generate_daily_brief");
      setBrief(b);
    } catch (e) {
      setScanError(`generate_daily_brief failed: ${String(e)}`);
    } finally {
      setBriefBusy(false);
    }
  }, []);

  useEffect(() => {
    refreshBrief();
  }, [refreshBrief]);

  // Reads the current scan-window policy at click time. Returns null
  // only as a defensive fallback — the Rust side accepts Option<i64>
  // and treats None as "scan everything". Callers always pass an
  // explicit number so per-connector `since_ms` filtering is in effect.
  const computeSinceMs = useCallback((): number => {
    if (liveMode) {
      return Date.now() - 24 * 60 * 60 * 1000;
    }
    return sinceDate.getTime();
  }, [liveMode, sinceDate]);

  const runAiQuery = async () => {
    if (!aiPrompt.trim()) return;
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<AiQueryReport>("ai_query", { prompt: aiPrompt });
      setAiLastReport(r);
    } catch (e) {
      setScanError(`ai_query failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const runCodexScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_codex", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runClaudeScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_claude", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runOpenCodeScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_opencode", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runWorkBuddyScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_workbuddy", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runDoubaoScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_doubao", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runDoubaoWorkScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_doubao_work", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runMiniMaxScan = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ScanReport>("scan_minimax", {
        sinceMs: computeSinceMs(),
      });
      setScan(r);
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runReset = async () => {
    if (!confirm("Wipe raw_events + sessions + connector_runs? Sources and privacy rules are kept.")) {
      return;
    }
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<ResetReport>("reset_events");
      setScanError(
        `Reset: ${r.raw_events_deleted} events, ${r.sessions_summary_deleted} session summaries, ${r.connector_runs_deleted} runs deleted.`
      );
      await Promise.all([refreshSources(), refreshEvents()]);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runInstallLaunchd = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<LaunchdStatus>("install_launchd_scanner");
      setLaunchd(r);
      setScanError(`LaunchAgent installed at ${r.plist_path ?? "(unknown)"}`);
    } catch (e) {
      setScanError(`install_launchd_scanner failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const runUninstallLaunchd = async () => {
    setBusy(true);
    setScanError(null);
    try {
      const r = await invoke<LaunchdStatus>("uninstall_launchd_scanner");
      setLaunchd(r);
      setScanError("LaunchAgent uninstalled.");
    } catch (e) {
      setScanError(`uninstall_launchd_scanner failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  // Display-only — used in the header strip. Re-evaluates `Date.now()`
  // on every render is fine here: it's just a UX label, not the
  // canonical scan-window the handlers will pass to the backend.
  const displaySinceMs = liveMode ? Date.now() - 24 * 60 * 60 * 1000 : sinceDate.getTime();

  return (
    <div className="min-h-screen p-6">
      <header className="mb-6 flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Agent 工作台账</h1>
          <p className="text-sm text-neutral-600">
            本地优先的跨 agent 工作聚合 + Eisenhower 四象限规划
          </p>
        </div>
        <Link
          to="/search"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
          data-testid="home-search"
        >
          🔍 搜索
        </Link>
        <Link
          to="/sessions"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
        >
          会话总览 →
        </Link>
        <Link
          to="/reports"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
        >
          窗口报告 →
        </Link>
        <Link
          to="/timeline"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
        >
          时间线 →
        </Link>
        <Link
          to="/planner"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
        >
          规划器 →
        </Link>
        <Link
          to="/settings"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
        >
          设置 →
        </Link>
      </header>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {/* Stage 5-E.3c: daily morning brief. Cached row from
            `daily_briefs` is shown on cold start; clicking
            「重新生成」 runs the full Eisenhower pipeline + writes
            a fresh row. */}
        <Card title="今日 Q1 摘要" className="lg:col-span-2">
          {brief ? (
            <div>
              <div className="mb-3 flex flex-wrap items-baseline gap-2">
                <p className="text-sm font-medium text-neutral-900">
                  {brief.headline}
                </p>
                <span className="text-[11px] text-neutral-500">
                  {brief.date} · {brief.ai_used ? "AI" : "启发式"} · {brief.total_q1} 个 Q1 / {brief.total_projects} 项目
                </span>
                {/* v0.4.2: morning-over-morning delta vs the most-
                    recent previous brief. `null` for the first brief
                    the app has ever generated — we show "首次" so
                    the user doesn't wonder if the card is broken. */}
                {brief.delta_vs_prev ? (
                  <DeltaPill delta={brief.delta_vs_prev} />
                ) : (
                  <span
                    className="rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium text-neutral-500"
                    data-testid="brief-delta-first"
                  >
                    首次
                  </span>
                )}
                <button
                  type="button"
                  onClick={runDailyBrief}
                  disabled={briefBusy}
                  className="ml-auto rounded-full border border-neutral-300 bg-white px-3 py-1 text-xs transition hover:bg-neutral-100 disabled:opacity-50"
                >
                  {briefBusy ? "生成中…" : "重新生成"}
                </button>
              </div>
              {/* v0.4.2: per-source Q1 rollup. Each chip is a
                  click-through that opens the Sessions page filtered
                  to that connector. Empty when no Q1 today — we
                  still render the row so the layout doesn't jump
                  between empty/non-empty states.
                  Filter param is `source_display_name` (not
                  `source_id`) because that's what Sessions.tsx's
                  chip group matches on today — switching to id
                  would orphan the existing source-filter UI. We
                  trust that connector display names are unique
                  in practice (each connector registers one
                  canonical name on first scan). */}
              {brief.by_source.length > 0 && (
                <div
                  className="mb-3 flex flex-wrap gap-1"
                  data-testid="brief-by-source"
                >
                  {brief.by_source.map((b) => (
                    <button
                      key={b.source_id}
                      type="button"
                      onClick={() =>
                        navigate(`/sessions?source=${encodeURIComponent(b.source_display_name)}`)
                      }
                      title={`查看 ${b.source_display_name} 的所有 Q1`}
                      className="rounded-full border border-neutral-300 bg-white px-2 py-0.5 text-[10px] font-medium text-neutral-700 transition hover:border-ink-900 hover:bg-neutral-100"
                      data-testid={`brief-by-source-${b.source_id}`}
                    >
                      {b.source_display_name} · {b.count}
                    </button>
                  ))}
                </div>
              )}
              {brief.top_q1.length === 0 ? (
                <p className="text-sm text-neutral-500">
                  最近 24 小时没有 Q1 紧急+重要项,继续推进 Q2 即可。
                </p>
              ) : (
                <ol className="space-y-2">
                  {brief.top_q1.map((it, idx) => (
                    <li
                      key={`${it.project_key}-${idx}`}
                      className="flex items-start gap-3 rounded-md border border-red-200 bg-red-50 px-3 py-2"
                    >
                      <span className="rounded-full bg-red-600 px-2 py-0.5 text-[11px] font-bold text-white">
                        #{idx + 1}
                      </span>
                      <div className="flex-1">
                        <p className="text-sm font-medium text-neutral-900">
                          {it.label || it.project_key}
                        </p>
                        <p className="text-[11px] text-neutral-600">
                          {it.project_key} · {it.source_display_name}
                        </p>
                        <div className="mt-1 flex flex-wrap gap-1 text-[10px]">
                          <span className="rounded bg-white px-1.5 py-0.5 text-neutral-700">
                            imp {it.importance}/10
                          </span>
                          <span className="rounded bg-white px-1.5 py-0.5 text-neutral-700">
                            urg {it.urgency}/10
                          </span>
                          <span className="rounded bg-white px-1.5 py-0.5 text-neutral-700">
                            {it.event_count} events
                          </span>
                        </div>
                      </div>
                    </li>
                  ))}
                </ol>
              )}
            </div>
          ) : (
            <div>
              <p className="mb-3 text-sm text-neutral-500">
                还没有今日摘要。点击「生成」跑一次 Eisenhower 评估,或等早 8 点 launchd 自动跑。
              </p>
              <button
                type="button"
                onClick={runDailyBrief}
                disabled={briefBusy}
                className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
              >
                {briefBusy ? "生成中…" : "生成今日摘要"}
              </button>
            </div>
          )}
        </Card>

        <Card title="Status">
          {health && (
            <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
              <dt className="text-neutral-500">Backend</dt>
              <dd className="font-mono">{health.ok ? "正常" : "异常"}</dd>
              <dt className="text-neutral-500">数据库</dt>
              <dd className="font-mono break-all text-xs">{health.db_path}</dd>
              <dt className="text-neutral-500">Schema</dt>
              <dd className="font-mono">v{health.schema_version}</dd>
              <dt className="text-neutral-500">检测时间</dt>
              <dd className="font-mono text-xs">{health.ts}</dd>
            </dl>
          )}
        </Card>

        <Card title="数据源扫描">
          {/* Phase D.3: scan-window selector. Default live mode
              (rolling now-24h), user can flip to 指定起始 and pick a
              calendar date. The picked window is what the 7 scan
              handlers below pass as `sinceMs` to the Rust IPC. */}
          <div className="mb-3 flex flex-wrap items-center gap-2 text-xs">
            <span className="text-neutral-500">扫描时间范围</span>
            <button
              type="button"
              onClick={() => setLiveMode(true)}
              className={cn(
                "rounded-full border px-3 py-1 font-medium transition",
                liveMode
                  ? "border-ink-900 bg-ink-900 text-white"
                  : "border-neutral-300 bg-white text-neutral-700 hover:bg-neutral-100"
              )}
            >
              实时(过去 24h)
            </button>
            <button
              type="button"
              onClick={() => setLiveMode(false)}
              className={cn(
                "rounded-full border px-3 py-1 font-medium transition",
                !liveMode
                  ? "border-ink-900 bg-ink-900 text-white"
                  : "border-neutral-300 bg-white text-neutral-700 hover:bg-neutral-100"
              )}
            >
              指定起始
            </button>
            {!liveMode && (
              <Popover>
                <PopoverTrigger className="rounded-full border border-neutral-300 bg-white px-3 py-1 font-medium text-neutral-700 transition hover:bg-neutral-100">
                  {sinceDate
                    ? format(sinceDate, "yyyy-MM-dd (EEEEEE)", { locale: zhCN })
                    : "选择日期"}
                </PopoverTrigger>
                <PopoverContent className="w-auto p-0" align="start">
                  <Calendar
                    mode="single"
                    selected={sinceDate}
                    onSelect={(d) => {
                      if (d) setSinceDate(d);
                    }}
                    locale={zhCN}
                    autoFocus
                  />
                </PopoverContent>
              </Popover>
            )}
            <span className="font-mono text-[11px] text-neutral-500">
              since = {format(displaySinceMs, "yyyy-MM-dd HH:mm")}
            </span>
          </div>

          <div className="mb-3 flex flex-wrap items-center gap-2">
            <button
              onClick={runCodexScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描 OpenAI Codex"}
            </button>
            <button
              onClick={runClaudeScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描 Claude Code"}
            </button>
            <button
              onClick={runOpenCodeScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描 OpenCode"}
            </button>
            <button
              onClick={runWorkBuddyScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描 WorkBuddy"}
            </button>
            <button
              onClick={runDoubaoScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描豆包"}
            </button>
            <button
              onClick={runDoubaoWorkScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描豆包工作台"}
            </button>
            <button
              onClick={runMiniMaxScan}
              disabled={busy}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "扫描中…" : "扫描 MiniMax Agent"}
            </button>
            <button
              onClick={() => {
                refreshSources();
                refreshEvents();
              }}
              className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
            >
              刷新
            </button>
            <button
              onClick={runReset}
              disabled={busy}
              className="rounded-full border border-red-200 px-3 py-1.5 text-sm text-red-700 transition hover:bg-red-50 disabled:opacity-50"
            >
              重置全部事件
            </button>
            <span className="text-xs text-neutral-500">
              {sources.length} 个数据源
            </span>
          </div>
          {scanError && (
            <p className="mb-2 text-sm text-red-600">{scanError}</p>
          )}
          {scan && (
            <div className="mb-3 rounded-lg bg-neutral-50 p-3 text-xs leading-relaxed">
              <strong className="font-mono">{scan.kind}</strong> — {scan.status} ·{" "}
              <span className="font-mono">{scan.events_inserted}</span> 个事件写入,{" "}
              <span className="font-mono">{scan.sessions_added}</span> 个会话,{" "}
              <span className="font-mono">{scan.files_scanned}</span> 个文件,{" "}
              <span className="font-mono">{scan.duration_ms} ms</span>
              {scan.error && (
                <span className="ml-2 italic text-neutral-500">{scan.error}</span>
              )}
            </div>
          )}
          {launchd && (
            <div className="mb-3 rounded-lg border border-neutral-200 bg-white p-3 text-xs leading-relaxed">
              <div className="mb-1 font-medium">后台扫描守护进程</div>
              <div className="text-neutral-600">
                状态:{" "}
                <span className="font-mono">
                  {launchd.installed ? "已安装" : "未安装"}
                </span>
                {launchd.last_load_error && (
                  <span className="ml-2 italic text-red-600">{launchd.last_load_error}</span>
                )}
              </div>
              {launchd.plist_path && (
                <div className="font-mono text-[11px] text-neutral-500 break-all">
                  plist: {launchd.plist_path}
                </div>
              )}
              <div className="mt-2 flex gap-2">
                {!launchd.installed ? (
                  <button
                    onClick={runInstallLaunchd}
                    disabled={busy}
                    className="rounded-full border border-neutral-300 px-3 py-1 text-xs transition hover:bg-neutral-100 disabled:opacity-50"
                  >
                    安装扫描守护进程
                  </button>
                ) : (
                  <button
                    onClick={runUninstallLaunchd}
                    disabled={busy}
                    className="rounded-full border border-red-200 px-3 py-1 text-xs text-red-700 transition hover:bg-red-50 disabled:opacity-50"
                  >
                    卸载扫描守护进程
                  </button>
                )}
              </div>
            </div>
          )}
          <ul className="divide-y divide-neutral-100 text-sm">
            {sources.map((s) => (
              <li key={s.id} className="flex items-center justify-between py-2">
                <div>
                  <div className="font-medium">{s.display_name}</div>
                  <div className="font-mono text-[11px] text-neutral-500">
                    {s.root_path || "—"}
                  </div>
                </div>
                <div className="text-right text-xs">
                  <span
                    className={`inline-block rounded-full px-2 py-0.5 ${
                      s.status === "readable"
                        ? "bg-emerald-100 text-emerald-700"
                        : s.status === "discovered"
                          ? "bg-amber-100 text-amber-700"
                          : "bg-neutral-100 text-neutral-700"
                    }`}
                  >
                    {s.status}
                  </span>
                  <div className="mt-1 font-mono text-[10px] text-neutral-400">
                    {s.last_scan_at ?? "never"}
                  </div>
                </div>
              </li>
            ))}
            {sources.length === 0 && (
              <li className="py-3 text-center text-xs text-neutral-400">
                还没有数据源,跑一次扫描以填充。
              </li>
            )}
          </ul>
        </Card>

        <Card title="AI 洞察(本地)">
          <div className="mb-2 flex flex-wrap items-baseline gap-2 text-xs">
            <span
              className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 ${
                aiStatus?.reachable
                  ? "bg-emerald-50 text-emerald-700"
                  : "bg-amber-50 text-amber-700"
              }`}
            >
              Ollama {aiStatus?.reachable ? "已联通" : "未启动"}
              {aiStatus?.latency_ms != null && (
                <span className="ml-1 font-mono text-[10px] text-neutral-500">
                  {aiStatus.latency_ms} ms
                </span>
              )}
            </span>
            {aiStatus?.endpoint && (
              <span className="font-mono text-[11px] text-neutral-500 break-all">
                {aiStatus.endpoint}
              </span>
            )}
            {aiStatus?.installed_models && aiStatus.installed_models.length > 0 && (
              <span className="font-mono text-[11px] text-neutral-500">
                · 模型: {aiStatus.installed_models.slice(0, 3).join(", ")}
                {aiStatus.installed_models.length > 3
                  ? ` 等共 ${aiStatus.installed_models.length} 个`
                  : ""}
              </span>
            )}
          </div>
          {aiStatus?.error && (
            <p className="mb-2 text-xs italic text-amber-700">
              {aiStatus.error}
              <span className="ml-2 text-neutral-500">
                启动 Ollama:<span className="font-mono">ollama serve</span>,
                拉模型:<span className="font-mono">ollama pull gemma4:e4b</span>
              </span>
            </p>
          )}
          <textarea
            value={aiPrompt}
            onChange={(e) => setAiPrompt(e.target.value)}
            placeholder="问点今天的活,比如:总结最近 5 个 Codex 会话"
            className="mb-2 w-full rounded-md border border-neutral-300 bg-white p-2 text-sm focus:border-ink-900 focus:outline-none"
            rows={3}
            disabled={busy || !aiStatus?.reachable}
          />
          <div className="flex flex-wrap items-center gap-2">
            <button
              onClick={runAiQuery}
              disabled={busy || !aiStatus?.reachable || !aiPrompt.trim()}
              className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              {busy ? "查询中…" : "查询本地模型"}
            </button>
            <button
              onClick={refreshAiStatus}
              disabled={busy}
              className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100 disabled:opacity-50"
            >
              刷新 Ollama
            </button>
            <span className="text-xs text-neutral-500">
              {aiSettings?.local_model ?? "(默认)"}
            </span>
          </div>
          {aiLastReport && (
            <div className="mt-3 rounded-md bg-neutral-50 p-3 text-xs leading-relaxed">
              <div className="mb-1 font-mono text-[10px] text-neutral-500">
                {aiLastReport.provider} · {aiLastReport.model} ·{" "}
                {aiLastReport.duration_ms} ms · 已 redact{" "}
                {aiLastReport.redacted_chars} 字符
              </div>
              {aiLastReport.error ? (
                <p className="text-red-700">{aiLastReport.error}</p>
              ) : (
                <pre className="whitespace-pre-wrap break-words font-sans">
                  {aiLastReport.response}
                </pre>
              )}
            </div>
          )}
        </Card>
      </div>

      <Card title="最近事件" className="mt-4">
        {events.length === 0 ? (
          <p className="py-6 text-center text-sm text-neutral-400">
            还没有事件,先在上面跑一次扫描。
          </p>
        ) : (
          <>
            <ul className="divide-y divide-neutral-100 text-xs">
              {events.map((e) => (
                <li key={e.id} className="py-2">
                  <div className="flex items-baseline justify-between gap-3">
                    <span className="font-mono text-[11px] text-neutral-500">
                      {e.occurred_at}
                    </span>
                    <span className="font-medium">{e.event_type}</span>
                    <span className="font-mono text-[11px] text-neutral-400">
                      {e.source_display_name}
                    </span>
                  </div>
                  <div className="mt-0.5 truncate font-mono text-[11px] text-neutral-600">
                    {e.project_key || "—"} · {e.payload_preview}
                  </div>
                </li>
              ))}
            </ul>
            <div className="mt-2 flex items-center justify-between border-t border-neutral-100 pt-2 text-[11px] text-neutral-500">
              <span>
                显示 {events.length} 条 · 上限 {eventsLimit}
              </span>
              <button
                type="button"
                onClick={() => setEventsLimit((n) => Math.min(n + 100, 1000))}
                disabled={eventsLimit >= 1000}
                className="rounded-md border border-neutral-300 px-2 py-1 font-medium text-neutral-700 hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40"
              >
                加载更多 (+100)
              </button>
            </div>
          </>
        )}
      </Card>

      <section className="mt-4 rounded-2xl border border-dashed border-neutral-300 bg-white/50 p-4 text-xs text-neutral-500">
        v0.2 跨 agent 工作台账:7 个 connector(codex / claude / opencode /
        workbuddy / 豆包 / 豆包工作台 / MiniMax Agent),raw events → ETL 30
        分钟分桶 → sessions_summary → AI 摘要(可缓存 24h)。IPC:{" "}
        <code>scan_codex</code>、<code>scan_claude</code>、{" "}
        <code>list_sources</code>、<code>list_recent_events</code>、{" "}
        <code>list_recent_events</code>.
      </section>
    </div>
  );
}

function Card({
  title,
  children,
  className = "",
}: {
  title: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section
      className={`rounded-2xl border border-neutral-200 bg-white p-4 shadow-sm ${className}`}
    >
      <h2 className="mb-3 text-sm font-medium uppercase tracking-wider text-neutral-500">
        {title}
      </h2>
      {children}
    </section>
  );
}

// v0.4.2: morning-over-morning delta pill for the brief card.
// Colour comes from the *direction*, not the magnitude — a Q1 -1
// is good news even when a Q1 +5 isn't strictly worse news. The
// arrow glyph and the chip colour make the verdict scannable at a
// glance; the `±N` numerals give the precise delta for the user
// who actually cares.
function DeltaPill({ delta }: { delta: BriefDelta }) {
  const { q1_delta, verdict } = delta;
  // more urgent → red arrow up; less urgent → green arrow down;
  // same → neutral equal sign. We use verdict (not raw delta sign)
  // because it's the authoritative categorisation the brief
  // computed — keeps UI consistent with future heuristic tweaks.
  const tone =
    verdict === "more urgent"
      ? "border-red-300 bg-red-50 text-red-700"
      : verdict === "less urgent"
      ? "border-emerald-300 bg-emerald-50 text-emerald-700"
      : "border-neutral-300 bg-neutral-100 text-neutral-600";
  const arrow =
    verdict === "more urgent" ? "↑" : verdict === "less urgent" ? "↓" : "=";
  const sign = q1_delta > 0 ? "+" : "";
  return (
    <span
      data-testid="brief-delta"
      data-verdict={verdict}
      className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${tone}`}
    >
      Q1 {sign}
      {q1_delta} {arrow} · {verdict === "more urgent" ? "较昨天更急" : verdict === "less urgent" ? "较昨天缓" : "持平"}
    </span>
  );
}