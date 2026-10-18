import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import { save as openSaveDialog } from "@tauri-apps/plugin-dialog";
import { Link } from "react-router-dom";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { cn, formatRelativeTime, importanceClass, sourceColorClass } from "../lib/utils";
import { providerLabel } from "../lib/locale";
import type {
  BulkExportFormat,
  BulkZipExport,
  EtlReport,
  MarkdownExport,
  SessionEvent,
  SessionPartition,
  SessionSummary,
} from "../lib/types";

// Stage 5-E.4a: master-detail layout for browsing `sessions_summary`.
//
// Left pane lists every 30-min ETL partition for the current source
// filter — selecting one populates the right pane with the partition
// metadata, any AI summary that has already been generated, and a
// chronological timeline of every raw event that fell inside its
// `[started_at, ended_at]` window (fetched via the new
// `list_session_events` IPC; the user reads what actually happened
// without an LLM roundtrip).
//
// Visual: neutral palette, ink-900 active row marker so the user
// always sees which partition they are inspecting.

type SourceFilter = "all" | string;

export default function SessionsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  // v0.4.2: brief card's per-source chip navigates here with
  // `?source=<display_name>`. We treat the URL as the source of
  // truth — toggling the chip group below writes back to it so
  // the user can deep-link / share / refresh.
  const urlSource = searchParams.get("source") ?? "all";
  const [partitions, setPartitions] = useState<SessionPartition[]>([]);
  const [summaries, setSummaries] = useState<Record<string, SessionSummary>>({});
  const [events, setEvents] = useState<Record<string, SessionEvent[]>>({});
  const [search, setSearch] = useState("");
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>(urlSource);
  // Sync the filter UI → URL when the chip group changes (e.g. the
  // user clicks "All"). `replace: true` so the chip toggle doesn't
  // flood the browser back-stack.
  const setSourceFilterAndUrl = useCallback(
    (next: SourceFilter) => {
      setSourceFilter(next);
      if (next === "all") {
        setSearchParams((prev) => {
          const np = new URLSearchParams(prev);
          np.delete("source");
          return np;
        });
      } else {
        setSearchParams(
          (prev) => {
            const np = new URLSearchParams(prev);
            np.set("source", next);
            return np;
          },
          { replace: true }
        );
      }
    },
    [setSearchParams]
  );
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const [loadingPartitions, setLoadingPartitions] = useState(false);
  const [summarisingId, setSummarisingId] = useState<string | null>(null);
  const [exportingId, setExportingId] = useState<string | null>(null);
  const [exportingBulk, setExportingBulk] = useState<BulkExportFormat | null>(null);
  const [exportingBulkZip, setExportingBulkZip] = useState(false);
  const [loadingEvents, setLoadingEvents] = useState(false);
  const [rebuildRunning, setRebuildRunning] = useState(false);
  const [lastRebuild, setLastRebuild] = useState<EtlReport | null>(null);
  const [lastExportPath, setLastExportPath] = useState<string | null>(null);
  const [expandedEventId, setExpandedEventId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshPartitions = useCallback(async () => {
    setLoadingPartitions(true);
    setError(null);
    try {
      const r = await invoke<SessionPartition[]>("list_sessions_summary");
      setPartitions(r);
      setSelectedId((prev) => {
        if (prev && r.some((p) => p.id === prev)) return prev;
        return r[0]?.id ?? null;
      });
    } catch (e) {
      setError(`读取会话列表失败:${String(e)}`);
    } finally {
      setLoadingPartitions(false);
    }
  }, []);

  useEffect(() => {
    refreshPartitions();
  }, [refreshPartitions]);

  // Stage 5-E.4a: pull the per-partition event list whenever the user
  // selects a different row. Cached in `events` keyed by partition id
  // so a second click on the same row is instant — this matters on the
  // Codex / OpenCode partitions that can have thousands of events.
  useEffect(() => {
    if (!selectedId) {
      return;
    }
    if (events[selectedId]) {
      return;
    }
    let cancelled = false;
    (async () => {
      setLoadingEvents(true);
      try {
        const r = await invoke<SessionEvent[]>("list_session_events", {
          sessionId: selectedId,
        });
        if (!cancelled) {
          setEvents((prev) => ({ ...prev, [selectedId]: r }));
        }
      } catch (e) {
        if (!cancelled) {
          setError(`读取事件失败(${selectedId}):${String(e)}`);
        }
      } finally {
        if (!cancelled) {
          setLoadingEvents(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selectedId, events]);

  const summariseOne = useCallback(
    async (partitionId: string) => {
      setSummarisingId(partitionId);
      setError(null);
      try {
        const r = await invoke<SessionSummary>("summarize_session", {
          sessionId: partitionId,
        });
        setSummaries((prev) => ({ ...prev, [partitionId]: r }));
      } catch (e) {
        setError(`生成摘要失败(${partitionId}):${String(e)}`);
      } finally {
        setSummarisingId(null);
      }
    },
    []
  );

  const rebuildAll = useCallback(async () => {
    setRebuildRunning(true);
    setError(null);
    try {
      const r = await invoke<EtlReport>("rebuild_sessions_summary");
      setLastRebuild(r);
      setEvents({});
      setSummaries({});
      await refreshPartitions();
    } catch (e) {
      setError(`全量重建失败:${String(e)}`);
    } finally {
      setRebuildRunning(false);
    }
  }, [refreshPartitions]);

  // Stage 5-E.4b: portable Markdown export. We render the bundle on
  // the Rust side (no LLM, pure SQL), ask the user where to save via
  // the platform Save dialog, then write the bytes through the
  // `write_text_file` IPC so we keep the path on the *user's* side
  // of authorisation (the dialog itself is the consent gate).
  const exportOne = useCallback(async (partitionId: string) => {
    setExportingId(partitionId);
    setError(null);
    try {
      const bundle = await invoke<MarkdownExport>("render_session_markdown", {
        sessionId: partitionId,
        includeSummary: true,
      });
      const path = await openSaveDialog({
        defaultPath: bundle.suggested_filename,
        filters: [{ name: "Markdown", extensions: ["md"] }],
        title: "导出会话为 Markdown",
      });
      if (!path) {
        // User dismissed the dialog — silent cancel, no error toast.
        return;
      }
      await invoke<void>("write_text_file", {
        path,
        content: bundle.content,
      });
      setLastExportPath(path);
    } catch (e) {
      setError(`导出失败(${partitionId}):${String(e)}`);
    } finally {
      setExportingId(null);
    }
  }, []);

  // Stage 5.5: bulk Sessions export. Honours the current source
  // filter (so the user gets exactly the row subset they see on
  // the left); `since_ms` is intentionally left as `null` until the
  // UI grows a date range picker. Same flow as the per-bucket
  // export — `write_text_file` is the platform-authorised write.
  const exportBulk = useCallback(
    async (format: BulkExportFormat) => {
      setExportingBulk(format);
      setError(null);
      try {
        const bundle = await invoke<MarkdownExport>("export_sessions_bulk", {
          format,
          sourceFilter: sourceFilter === "all" ? null : sourceFilter,
          sinceMs: null,
        });
        const path = await openSaveDialog({
          defaultPath: bundle.suggested_filename,
          filters: [
            {
              name: format.toUpperCase(),
              extensions: [format],
            },
          ],
          title: `批量导出会话为 ${format.toUpperCase()}`,
        });
        if (!path) {
          return;
        }
        await invoke<void>("write_text_file", {
          path,
          content: bundle.content,
        });
        setLastExportPath(path);
      } catch (e) {
        setError(`批量导出失败: ${String(e)}`);
      } finally {
        setExportingBulk(null);
      }
    },
    [sourceFilter]
  );

  // Stage 5.5 / v0.3.5: bulk Sessions export as a multi-file .md
  // zip. Same filter semantics as the JSON/CSV path, but writes
  // one markdown per partition + a top-level `INDEX.md` manifest.
  // Bytes cross IPC as base64 (Tauri 2 IPC is JSON-only) and are
  // decoded by `write_binary_file` before they hit the user's disk.
  const exportBulkZip = useCallback(async () => {
    setExportingBulkZip(true);
    setError(null);
    try {
      const bundle = await invoke<BulkZipExport>("render_sessions_bulk_zip", {
        sourceFilter: sourceFilter === "all" ? null : sourceFilter,
        sinceMs: null,
      });
      const path = await openSaveDialog({
        defaultPath: bundle.suggested_filename,
        filters: [{ name: "ZIP 压缩包", extensions: ["zip"] }],
        title: "批量导出会话为 .md 压缩包",
      });
      if (!path) {
        return;
      }
      await invoke<void>("write_binary_file", {
        path,
        contentBase64: bundle.content_base64,
      });
      setLastExportPath(path);
    } catch (e) {
      setError(`批量 .zip 导出失败: ${String(e)}`);
    } finally {
      setExportingBulkZip(false);
    }
  }, [sourceFilter]);

  const sourceOptions = useMemo(() => {
    const names = Array.from(
      new Set(partitions.map((p) => p.source_display_name))
    ).sort();
    return ["all", ...names];
  }, [partitions]);

  // Apply search + source filter at render time so the user's last
  // input doesn't reset on refresh. Sorted newest-first so the most
  // recent activity is at the top.
  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return partitions
      .filter((p) => {
        if (sourceFilter !== "all" && p.source_display_name !== sourceFilter)
          return false;
        if (!q) return true;
        return (
          (p.project_key ?? "").toLowerCase().includes(q) ||
          (p.summary ?? "").toLowerCase().includes(q) ||
          p.source_display_name.toLowerCase().includes(q)
        );
      })
      .slice()
      .sort((a, b) => (a.started_at < b.started_at ? 1 : -1));
  }, [partitions, search, sourceFilter]);

  const selected = useMemo(
    () => partitions.find((p) => p.id === selectedId) ?? null,
    [partitions, selectedId]
  );

  return (
    <div className="flex h-screen flex-col bg-neutral-50">
      <header className="flex items-baseline justify-between gap-4 border-b border-neutral-200 bg-white px-6 py-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">会话总览</h1>
          <p className="text-sm text-neutral-600">
            按 30 分钟空闲分桶,挑一个看 AI 摘要和当时发生了啥。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Link
            to="/"
            className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm transition hover:bg-neutral-100"
          >
            ← 返回首页
          </Link>
          <Button
            variant="outline"
            size="sm"
            disabled={exportingBulk !== null}
            onClick={() => exportBulk("json")}
            data-testid="sessions-bulk-export-json"
            title={
              sourceFilter === "all"
                ? "导出全部 sessions 为 JSON"
                : `导出 ${sourceFilter} 的 sessions 为 JSON`
            }
          >
            {exportingBulk === "json" ? "导出中…" : "导出 .json"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={exportingBulk !== null}
            onClick={() => exportBulk("csv")}
            data-testid="sessions-bulk-export-csv"
            title={
              sourceFilter === "all"
                ? "导出全部 sessions 为 CSV (Excel 友好)"
                : `导出 ${sourceFilter} 的 sessions 为 CSV`
            }
          >
            {exportingBulk === "csv" ? "导出中…" : "导出 .csv"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={exportingBulk !== null || exportingBulkZip}
            onClick={() => exportBulkZip()}
            data-testid="sessions-bulk-export-zip"
            title={
              sourceFilter === "all"
                ? "导出全部 sessions 为多文件 .md zip + INDEX.md"
                : `导出 ${sourceFilter} 的 sessions 为多文件 .md zip`
            }
          >
            {exportingBulkZip ? "打包中…" : "导出 .md 集 (zip)"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={loadingPartitions}
            onClick={() => refreshPartitions()}
          >
            {loadingPartitions ? "刷新中…" : "刷新"}
          </Button>
          <Button
            variant="default"
            size="sm"
            disabled={rebuildRunning}
            onClick={() => rebuildAll()}
          >
            {rebuildRunning ? "重建中…" : "全量重建 ETL"}
          </Button>
        </div>
      </header>

      {error && (
        <div className="border-b border-red-300 bg-red-50 px-6 py-3 text-sm text-red-700">
          {error}
        </div>
      )}
      {lastRebuild && (
        <div className="border-b border-emerald-200 bg-emerald-50 px-6 py-2 text-xs text-emerald-800">
          上次 ETL 重建:共 {lastRebuild.partitions_total} 个分区,跨{" "}
          {lastRebuild.sources_rebuilt} 个数据源,耗时{" "}
          {Math.max(1, Math.round(lastRebuild.duration_ms / 1000))} 秒。
        </div>
      )}

      {lastExportPath && (
        <div className="border-b border-sky-200 bg-sky-50 px-6 py-2 text-xs text-sky-800">
          已导出 Markdown 到{" "}
          <span className="font-mono">{lastExportPath}</span>
          <button
            className="ml-3 rounded-full border border-sky-300 px-2 py-0.5 text-[10px] hover:bg-sky-100"
            onClick={() => setLastExportPath(null)}
          >
            关闭
          </button>
        </div>
      )}

      <div className="flex flex-1 overflow-hidden">
        {/* LEFT — partition list */}
        <aside className="flex w-[360px] flex-col border-r border-neutral-200 bg-white">
          <div className="border-b border-neutral-200 p-3">
            <input
              type="search"
              placeholder="搜索项目路径 / 摘要 / 数据源"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              className="mb-2 w-full rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-sm focus:border-ink-900 focus:outline-none"
              data-testid="sessions-search"
            />
            <div className="flex flex-wrap gap-1">
              {sourceOptions.map((name) => (
                <button
                  key={name}
                  onClick={() => setSourceFilterAndUrl(name)}
                  className={cn(
                    "flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] transition",
                    sourceFilter === name
                      ? "border-ink-900 bg-ink-900 text-white"
                      : "border-neutral-300 bg-white text-neutral-700 hover:bg-neutral-100"
                  )}
                  data-testid={`sessions-source-${name}`}
                >
                  {name !== "all" && (
                    <span
                      className={cn(
                        "inline-block h-2 w-2 rounded-full border",
                        // Pick the matching hue from the same family as
                        // the badge colour; the dot itself is solid so
                        // it stands out even on the dark "selected" pill.
                        sourceColorClass(name)
                          .split(" ")
                          .filter((c) => c.startsWith("bg-"))
                          .map((c) => c.replace("-100", "-400"))
                          .join(" ")
                      )}
                    />
                  )}
                  {name === "all" ? "全部" : name}
                </button>
              ))}
            </div>
            <div className="mt-2 text-[11px] text-neutral-500">
              {filtered.length} / {partitions.length} 个 bucket
            </div>
          </div>
          <div className="flex-1 overflow-y-auto">
            {partitions.length === 0 ? (
              <div className="p-4 text-xs text-neutral-500">
                还没有数据。回到首页跑一次任意 connector 扫描,ETL 会自动
                聚类。
              </div>
            ) : filtered.length === 0 ? (
              <div className="p-4 text-xs italic text-neutral-400">
                没有匹配的 bucket
              </div>
            ) : (
              filtered.map((p) => (
                <PartitionRow
                  key={p.id}
                  partition={p}
                  active={p.id === selectedId}
                  onSelect={() => setSelectedId(p.id)}
                />
              ))
            )}
          </div>
        </aside>

        {/* RIGHT — detail pane */}
        <main className="flex-1 overflow-y-auto bg-neutral-50">
          {!selected ? (
            <div className="flex h-full items-center justify-center text-sm text-neutral-500">
              选左边一个 bucket 看详情
            </div>
          ) : (
            <PartitionDetail
              partition={selected}
              summary={summaries[selected.id]}
              events={events[selected.id]}
              eventsLoading={loadingEvents && !events[selected.id]}
              onSummarise={() => summariseOne(selected.id)}
              summariseBusy={summarisingId === selected.id}
              onExport={() => exportOne(selected.id)}
              exportBusy={exportingId === selected.id}
              expandedEventId={expandedEventId}
              onToggleEvent={(id) =>
                setExpandedEventId((cur) => (cur === id ? null : id))
              }
            />
          )}
        </main>
      </div>
    </div>
  );
}

// One row in the left pane. Kept as a separate component so its state
// (hover, etc.) doesn't dirty the whole list on every parent render.
function PartitionRow({
  partition,
  active,
  onSelect,
}: {
  partition: SessionPartition;
  active: boolean;
  onSelect: () => void;
}) {
  const range = `${formatRelativeTime(partition.started_at)} → ${formatRelativeTime(
    partition.ended_at
  )}`;
  const isLive = active;
  return (
    <button
      type="button"
      onClick={onSelect}
      data-testid={`sessions-row-${partition.id}`}
      className={cn(
        "block w-full border-b border-neutral-100 px-3 py-2 text-left text-xs transition",
        isLive
          ? "border-l-2 border-l-ink-900 bg-ink-900/5"
          : "border-l-2 border-l-transparent hover:bg-neutral-50"
      )}
    >
      <div className="mb-1 flex items-center gap-2">
        <span className="rounded-full border border-neutral-300 bg-white px-1.5 py-0.5 text-[10px] font-mono text-neutral-700">
          {partition.source_display_name}
        </span>
        <span className="text-[10px] text-neutral-500">
          {partition.event_count} 事件
        </span>
        <ImportanceDot value={partition.importance} />
      </div>
      <div className="font-mono text-[11px] text-neutral-700">{range}</div>
      <div className="mt-0.5 truncate text-[11px] text-neutral-500">
        {partition.project_key ?? "(未知项目)"}
      </div>
      <div className="mt-1 line-clamp-2 text-[11px] text-neutral-600">
        {partition.summary}
      </div>
    </button>
  );
}

function ImportanceDot({ value }: { value: number }) {
  const tone =
    value >= 8
      ? "bg-red-500"
      : value >= 5
        ? "bg-amber-500"
        : value >= 3
          ? "bg-sky-500"
          : "bg-neutral-300";
  return (
    <span
      className={cn("inline-block h-2 w-2 rounded-full", tone)}
      title={`重要性 ${value}/10`}
    />
  );
}

function PartitionDetail({
  partition,
  summary,
  events,
  eventsLoading,
  onSummarise,
  summariseBusy,
  onExport,
  exportBusy,
  expandedEventId,
  onToggleEvent,
}: {
  partition: SessionPartition;
  summary: SessionSummary | undefined;
  events: SessionEvent[] | undefined;
  eventsLoading: boolean;
  onSummarise: () => void;
  summariseBusy: boolean;
  onExport: () => void;
  exportBusy: boolean;
  expandedEventId: string | null;
  onToggleEvent: (id: string) => void;
}) {
  const range = `${formatRelativeTime(partition.started_at)} → ${formatRelativeTime(
    partition.ended_at
  )}`;
  return (
    <div className="mx-auto max-w-3xl p-6">
      <div className="mb-4 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="mb-2 flex items-center gap-2 text-xs">
            <Badge
              variant="default"
              className={sourceColorClass(partition.source_display_name)}
            >
              {partition.source_display_name}
            </Badge>
            <span className="text-neutral-500">
              {partition.event_count} 个事件
            </span>
            <Badge
              variant={
                partition.importance >= 8
                  ? "danger"
                  : partition.importance >= 5
                    ? "warning"
                    : "default"
              }
              className={cn(importanceClass(partition.importance))}
            >
              重要性 {partition.importance}/10
            </Badge>
            <span className="text-neutral-500">{partition.heuristic_kind}</span>
          </div>
          <h2 className="break-all font-mono text-sm text-neutral-900">
            {range}
          </h2>
          <p className="mt-1 font-mono text-xs text-neutral-600">
            {partition.project_key ?? "(未知项目)"}
          </p>
        </div>
        <div className="flex shrink-0 gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={exportBusy}
            onClick={onExport}
            data-testid={`sessions-export-${partition.id}`}
          >
            {exportBusy ? "导出中…" : "导出单 bucket .md"}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={summariseBusy}
            onClick={onSummarise}
          >
            {summariseBusy
              ? "摘要中…"
              : summary
                ? "重新摘要"
                : "生成 AI 摘要"}
          </Button>
        </div>
      </div>

      <section className="mb-6 rounded-lg border border-neutral-200 bg-white p-4">
        {summary ? (
          <SummaryDetail summary={summary} />
        ) : (
          <div className="text-sm text-neutral-600">
            <p className="mb-2">{partition.summary}</p>
            <p className="text-xs italic text-neutral-500">
              (heuristic 一行 hint,点右上「生成 AI 摘要」出 LLM 版)
            </p>
          </div>
        )}
      </section>

      <section className="rounded-lg border border-neutral-200 bg-white p-4">
        <header className="mb-3 flex items-baseline justify-between">
          <h3 className="text-sm font-semibold text-neutral-900">
            事件时间线
          </h3>
          <span className="text-[11px] text-neutral-500">
            {events
              ? `${events.length} 条`
              : eventsLoading
                ? "加载中…"
                : ""}
          </span>
        </header>
        <EventList
          events={events}
          loading={eventsLoading}
          expandedEventId={expandedEventId}
          onToggleEvent={onToggleEvent}
        />
      </section>
    </div>
  );
}

function SummaryDetail({ summary }: { summary: SessionSummary }) {
  const isCached = summary.cached;
  const providerVariant =
    summary.provider === "heuristic"
      ? "warning"
      : summary.provider === "openai"
        ? "success"
        : summary.provider === "anthropic"
          ? "codex"
          : "default";
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant={providerVariant}>
          {providerLabel(summary.provider)}
        </Badge>
        {isCached && (
          <Badge variant="default">缓存 · {summary.age_hours} 小时前</Badge>
        )}
      </div>
      <p className="font-medium text-neutral-900">{summary.headline}</p>
      <p className="text-neutral-700">{summary.body}</p>
      <p className="text-xs text-neutral-500">{summary.reasoning}</p>
    </div>
  );
}

function EventList({
  events,
  loading,
  expandedEventId,
  onToggleEvent,
}: {
  events: SessionEvent[] | undefined;
  loading: boolean;
  expandedEventId: string | null;
  onToggleEvent: (id: string) => void;
}) {
  if (loading) {
    return <p className="text-xs text-neutral-500">加载事件中…</p>;
  }
  if (!events || events.length === 0) {
    return (
      <p className="text-xs italic text-neutral-500">
        这个 bucket 没有 raw event 落在时间窗内(ETL 重建后偶尔会出现
        这种空桶)。
      </p>
    );
  }
  return (
    <ul className="divide-y divide-neutral-100" data-testid="session-events">
      {events.map((e) => {
        const isOpen = expandedEventId === e.id;
        const preview = e.payload.replace(/\s+/g, " ").slice(0, 120);
        return (
          <li key={e.id} className="py-2">
            <button
              type="button"
              onClick={() => onToggleEvent(e.id)}
              data-testid={`session-event-${e.id}`}
              className="block w-full text-left"
            >
              <div className="flex items-baseline justify-between gap-2">
                <span className="font-mono text-[11px] text-neutral-500">
                  {formatRelativeTime(e.occurred_at)}
                </span>
                <span className="rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-mono text-neutral-700">
                  {e.event_type}
                </span>
              </div>
              <p className="mt-1 font-mono text-[12px] text-neutral-800">
                {preview || <span className="italic text-neutral-400">(空 payload)</span>}
              </p>
            </button>
            {isOpen && (
              <pre className="mt-2 max-h-72 overflow-auto rounded-md bg-neutral-900 px-3 py-2 text-[11px] leading-relaxed text-neutral-100">
                {e.payload}
              </pre>
            )}
          </li>
        );
      })}
    </ul>
  );
}
