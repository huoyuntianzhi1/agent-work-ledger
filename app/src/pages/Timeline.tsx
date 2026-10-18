import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { Link } from "react-router-dom";
import { format } from "date-fns";
import { zhCN } from "date-fns/locale";

import { statusLabel } from "../lib/locale";
import { Popover, PopoverContent, PopoverTrigger } from "../components/ui/popover";
import { Calendar } from "../components/ui/calendar";

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

type SupportedConnector = {
  kind: string;
  display_name: string;
  status: string; // "supported" | "planned"
  notes: string;
};

type TimelineFilter = {
  source_kinds?: string[] | null;
  event_types?: string[] | null;
  project_contains?: string | null;
  since_iso?: string | null;
  until_iso?: string | null;
  limit?: number | null;
  order?: string | null;
};

const KIND_COLOR: Record<string, string> = {
  codex: "bg-sky-100 text-sky-800",
  claude_code: "bg-violet-100 text-violet-800",
  opencode: "bg-emerald-100 text-emerald-800",
  workbuddy: "bg-amber-100 text-amber-800",
};

// Hours 0..23 and minutes 0..59 (1-minute granularity per 王总
// "精确到时/分"). We render minutes as two-digit strings and keep the
// full 0..59 range so the user can pick any wall-clock minute, not
// just multiples of five.
const HOUR_OPTIONS = Array.from({ length: 24 }, (_, h) => h);
const MINUTE_OPTIONS = Array.from({ length: 60 }, (_, m) => m);
const pad2 = (n: number) => String(n).padStart(2, "0");

/**
 * Self-contained date + minute picker for one boundary (start or end).
 *
 * State held inside:
 *   - `open`         : whether the popover is visible
 *   - `pickerDate`   : the calendar/clock face the user is editing
 *                      (does NOT propagate to `value` until "应用" is
 *                      clicked — gives a transactional edit so an
 *                      accidental calendar click doesn't blow away the
 *                      persisted filter).
 *
 * Communication with the parent:
 *   - `value`       : current ISO string (or null/empty = unset)
 *   - `onChange(iso)` on apply; `onChange("")` on clear.
 */
function DateTimePicker({
  value,
  onChange,
  placeholder,
  label,
  testId,
}: {
  value: string | null | undefined;
  onChange: (iso: string) => void;
  placeholder: string;
  label: string;
  testId?: string;
}) {
  const [open, setOpen] = useState(false);
  const parsed = value ? new Date(value) : null;
  const isSet = parsed != null && !Number.isNaN(parsed.getTime());

  const [pickerDate, setPickerDate] = useState<Date>(
    () => new Date(parsed ?? Date.now())
  );

  // Re-sync the local picker face whenever the parent pushes a new
  // `value` (e.g. user clicked "清除", or the filter was reset).
  useEffect(() => {
    if (isSet) {
      setPickerDate(new Date(parsed as Date));
    }
    // We intentionally do not depend on `pickerDate` here — this is a
    // one-way sync from parent value into local state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);

  const apply = () => {
    const d = new Date(pickerDate);
    // Make sure we don't accidentally drop seconds/ms — clone picks
    // whatever the user has on the face (year/month/day/hour/minute).
    onChange(d.toISOString());
    setOpen(false);
  };

  const clear = () => {
    onChange("");
    setOpen(false);
  };

  return (
    <div className="text-xs text-neutral-500">
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          data-testid={testId}
          className="mt-1 flex w-full items-center justify-between rounded border border-neutral-300 bg-white px-2 py-1 text-left text-sm font-normal text-neutral-800 transition hover:border-neutral-400 focus:border-ink-900 focus:outline-none"
        >
          <span className={isSet ? "font-mono text-[12px]" : "text-neutral-400"}>
            {isSet
              ? format(parsed as Date, "yyyy-MM-dd HH:mm", { locale: zhCN })
              : placeholder}
          </span>
          <span className="text-[10px] text-neutral-400">{label}</span>
        </PopoverTrigger>
        <PopoverContent className="w-auto p-0" align="start">
          <div className="p-3">
            <Calendar
              mode="single"
              selected={pickerDate}
              onSelect={(d) => {
                if (!d) return;
                // Preserve the clock-side hours/minutes the user has
                // already picked — only the date portion should move.
                const merged = new Date(d);
                merged.setHours(pickerDate.getHours());
                merged.setMinutes(pickerDate.getMinutes());
                merged.setSeconds(0);
                merged.setMilliseconds(0);
                setPickerDate(merged);
              }}
              locale={zhCN}
              autoFocus
            />
            <div className="mt-3 flex items-center justify-between gap-2 border-t border-neutral-100 pt-3">
              <div className="flex items-center gap-1 font-mono text-sm">
                <select
                  aria-label="小时"
                  value={pickerDate.getHours()}
                  onChange={(e) => {
                    const next = new Date(pickerDate);
                    next.setHours(Number(e.target.value));
                    next.setSeconds(0);
                    next.setMilliseconds(0);
                    setPickerDate(next);
                  }}
                  className="rounded border border-neutral-300 px-1 py-0.5"
                >
                  {HOUR_OPTIONS.map((h) => (
                    <option key={h} value={h}>
                      {pad2(h)}
                    </option>
                  ))}
                </select>
                <span>:</span>
                <select
                  aria-label="分钟"
                  value={pickerDate.getMinutes()}
                  onChange={(e) => {
                    const next = new Date(pickerDate);
                    next.setMinutes(Number(e.target.value));
                    next.setSeconds(0);
                    next.setMilliseconds(0);
                    setPickerDate(next);
                  }}
                  className="rounded border border-neutral-300 px-1 py-0.5"
                >
                  {MINUTE_OPTIONS.map((m) => (
                    <option key={m} value={m}>
                      {pad2(m)}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex items-center gap-1">
                {value && (
                  <button
                    type="button"
                    onClick={clear}
                    className="rounded-full border border-neutral-300 px-2 py-0.5 text-[11px] text-neutral-600 hover:bg-neutral-100"
                  >
                    清除
                  </button>
                )}
                <button
                  type="button"
                  onClick={apply}
                  className="rounded-full bg-ink-900 px-3 py-0.5 text-[11px] font-medium text-white shadow-sm hover:bg-neutral-800"
                >
                  应用
                </button>
              </div>
            </div>
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}

export default function TimelinePage() {
  const [supported, setSupported] = useState<SupportedConnector[]>([]);
  const [events, setEvents] = useState<EventRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [exportMsg, setExportMsg] = useState<string | null>(null);
  const [filter, setFilter] = useState<TimelineFilter>({
    source_kinds: null,
    event_types: null,
    project_contains: "",
    since_iso: "",
    until_iso: "",
    limit: 200,
    order: "desc",
  });

  useEffect(() => {
    invoke<SupportedConnector[]>("list_supported_connectors")
      .then(setSupported)
      .catch((e) => console.error("list_supported_connectors failed", e));
  }, []);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      const rows = await invoke<EventRow[]>("list_timeline_events", { filter });
      setEvents(rows);
    } catch (e) {
      console.error("list_timeline_events failed", e);
    } finally {
      setBusy(false);
    }
  }, [filter]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const toggleKind = (kind: string) => {
    const cur = filter.source_kinds ?? [];
    const next = cur.includes(kind)
      ? cur.filter((k) => k !== kind)
      : [...cur, kind];
    setFilter({ ...filter, source_kinds: next.length ? next : null });
  };

  const onExportMarkdown = async () => {
    setExportMsg(null);
    const target = await save({
      title: "导出时间线为 Markdown",
      defaultPath: "agent-work-ledger.md",
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (!target) return;
    try {
      setBusy(true);
      const path = await invoke<string>("export_markdown", {
        filter,
        path: target,
      });
      setExportMsg(`已写入 ${events.length} 条事件到 ${path}`);
    } catch (e) {
      setExportMsg(`导出失败:${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  // Group events by ISO date for the visual.
  const grouped = useMemo(() => {
    const map = new Map<string, EventRow[]>();
    for (const e of events) {
      const day = e.occurred_at.slice(0, 10) || "未知日期";
      const arr = map.get(day) ?? [];
      arr.push(e);
      map.set(day, arr);
    }
    return Array.from(map.entries()).sort(([a], [b]) =>
      filter.order === "asc" ? a.localeCompare(b) : b.localeCompare(a)
    );
  }, [events, filter.order]);

  return (
    <div className="min-h-screen p-6">
      <header className="mb-4 flex items-baseline justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">时间线</h1>
          <p className="text-sm text-neutral-600">
            跨 agent 时间聚合 · 共 {events.length} 条 · 按{" "}
            <code className="font-mono">{filter.order === "asc" ? "从旧到新" : "从新到旧"}</code>{" "}
            排序
          </p>
        </div>
        <Link to="/" className="text-sm text-neutral-500 hover:text-neutral-700">
          ← 返回首页
        </Link>
      </header>

      <FilterBar
        filter={filter}
        onChange={setFilter}
        supported={supported}
        onToggleKind={toggleKind}
      />

      <section className="mt-4 flex flex-wrap items-center gap-2">
        <button
          onClick={refresh}
          disabled={busy}
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100 disabled:opacity-50"
        >
          {busy ? "刷新中…" : "刷新"}
        </button>
        <button
          onClick={onExportMarkdown}
          disabled={busy || events.length === 0}
          className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
        >
          导出 Markdown
        </button>
        {exportMsg && (
          <span className="text-xs text-neutral-500">{exportMsg}</span>
        )}
      </section>

      <div className="mt-4 space-y-6">
        {grouped.length === 0 && (
          <p className="py-8 text-center text-sm text-neutral-400">
            当前筛选下还没有匹配的事件。先在上面跑一次扫描,或放宽筛选条件试试。
          </p>
        )}
        {grouped.map(([day, rows]) => (
          <section key={day}>
            <h2 className="mb-2 text-xs font-medium uppercase tracking-widest text-neutral-500">
              {day}
            </h2>
            <ul className="divide-y divide-neutral-100 rounded-2xl border border-neutral-200 bg-white">
              {rows.map((e) => (
                <li key={e.id} className="flex flex-col gap-1 px-4 py-3">
                  <div className="flex items-baseline gap-2 text-xs">
                    <span
                      className={`inline-block rounded-full px-2 py-0.5 font-mono ${
                        KIND_COLOR[e.source_kind] ?? "bg-neutral-100 text-neutral-700"
                      }`}
                    >
                      {e.source_kind}
                    </span>
                    <span className="font-medium text-neutral-800">
                      {e.event_type}
                    </span>
                    <span className="font-mono text-[11px] text-neutral-400">
                      {e.occurred_at}
                    </span>
                  </div>
                  <div className="truncate font-mono text-[11px] text-neutral-600">
                    {e.project_key || "—"} · {e.payload_preview}
                  </div>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}

function FilterBar({
  filter,
  onChange,
  supported,
  onToggleKind,
}: {
  filter: TimelineFilter;
  onChange: (f: TimelineFilter) => void;
  supported: SupportedConnector[];
  onToggleKind: (kind: string) => void;
}) {
  return (
    <section className="rounded-2xl border border-neutral-200 bg-white p-4 text-sm shadow-sm">
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <span className="text-xs uppercase tracking-widest text-neutral-500">
          数据源
        </span>
        {supported.map((s) => {
          const active = (filter.source_kinds ?? []).includes(s.kind);
          const enabled = s.status === "supported";
          return (
            <button
              key={s.kind}
              disabled={!enabled}
              onClick={() => enabled && onToggleKind(s.kind)}
              className={`rounded-full border px-3 py-1 text-xs transition ${
                active
                  ? "border-ink-900 bg-ink-900 text-white"
                  : enabled
                    ? "border-neutral-300 hover:bg-neutral-100"
                    : "border-dashed border-neutral-200 text-neutral-400 cursor-not-allowed"
              }`}
              title={s.notes}
            >
              {s.display_name}
              {!enabled && " · 规划中"}
            </button>
          );
        })}
        <span className="text-[10px] text-neutral-400">
          {supported
            .filter((s) => s.status !== "supported")
            .map((s) => `${s.display_name} · ${statusLabel(s.status)}`)
            .join(" / ") || ""}
        </span>
      </div>
      <div className="grid grid-cols-1 gap-3 md:grid-cols-4">
        <label className="text-xs text-neutral-500">
          项目包含
          <input
            type="text"
            value={filter.project_contains ?? ""}
            onChange={(e) =>
              onChange({ ...filter, project_contains: e.target.value })
            }
            placeholder="例如 Desktop"
            className="mt-1 block w-full rounded border border-neutral-300 px-2 py-1 text-sm"
          />
        </label>
        <div>
          <DateTimePicker
            label="起始"
            testId="timeline-since-picker"
            value={filter.since_iso ?? ""}
            onChange={(iso) => onChange({ ...filter, since_iso: iso })}
            placeholder="不限"
          />
        </div>
        <div>
          <DateTimePicker
            label="截止"
            testId="timeline-until-picker"
            value={filter.until_iso ?? ""}
            onChange={(iso) => onChange({ ...filter, until_iso: iso })}
            placeholder="不限"
          />
        </div>
        <label className="text-xs text-neutral-500">
          排序
          <select
            value={filter.order ?? "desc"}
            onChange={(e) => onChange({ ...filter, order: e.target.value })}
            className="mt-1 block w-full rounded border border-neutral-300 px-2 py-1 text-sm"
          >
            <option value="desc">从新到旧</option>
            <option value="asc">从旧到新</option>
          </select>
        </label>
      </div>
    </section>
  );
}