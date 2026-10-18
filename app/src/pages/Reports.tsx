import { useCallback, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Link } from "react-router-dom";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "../components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { cn } from "../lib/utils";
import { providerLabel, sessionKindLabel } from "../lib/locale";
import type { WindowReport } from "../lib/types";

// Reports 页是 v0.2 给"今天 / 这周我都干了什么"的回答。用户选择
// 日报 / 周报,点生成,系统把对应窗口里的 `sessions_summary` 行送进
// 当前 provider(云端强制 redact)。缓存 24h(AiSettings 默认),
// 点"重新生成"会重跑 LLM。

type WindowKind = "day" | "week";

function isoDayWindow(reference: Date): { start: string; end: string } {
  const start = new Date(
    Date.UTC(
      reference.getUTCFullYear(),
      reference.getUTCMonth(),
      reference.getUTCDate(),
      0,
      0,
      0,
      0
    )
  );
  const end = new Date(start.getTime() + 24 * 60 * 60 * 1000);
  return { start: start.toISOString(), end: end.toISOString() };
}

function isoWeekWindow(reference: Date): { start: string; end: string } {
  const day = reference.getUTCDay(); // 0 = 周日
  const daysSinceMonday = (day + 6) % 7;
  const monday = new Date(
    Date.UTC(
      reference.getUTCFullYear(),
      reference.getUTCMonth(),
      reference.getUTCDate() - daysSinceMonday,
      0,
      0,
      0,
      0
    )
  );
  const nextMonday = new Date(monday.getTime() + 7 * 24 * 60 * 60 * 1000);
  return { start: monday.toISOString(), end: nextMonday.toISOString() };
}

export default function ReportsPage() {
  const [kind, setKind] = useState<WindowKind>("day");
  const [referenceDate, setReferenceDate] = useState<Date>(() => new Date());
  const [report, setReport] = useState<WindowReport | null>(null);
  const [history, setHistory] = useState<WindowReport[]>([]);
  const [generating, setGenerating] = useState(false);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const window = useMemo(() => {
    return kind === "day"
      ? isoDayWindow(referenceDate)
      : isoWeekWindow(referenceDate);
  }, [kind, referenceDate]);

  const loadHistory = useCallback(async (label: WindowKind) => {
    setLoadingHistory(true);
    setError(null);
    try {
      const r = await invoke<WindowReport[]>("list_window_reports", {
        label,
        limit: 10,
      });
      setHistory(r);
    } catch (e) {
      setError(`读取历史报告失败:${String(e)}`);
    } finally {
      setLoadingHistory(false);
    }
  }, []);

  const generate = useCallback(async () => {
    setGenerating(true);
    setError(null);
    try {
      const r = await invoke<WindowReport>("generate_window_report", {
        label: kind,
        windowStart: window.start,
        windowEnd: window.end,
      });
      setReport(r);
      await loadHistory(kind);
    } catch (e) {
      setError(`生成报告失败:${String(e)}`);
    } finally {
      setGenerating(false);
    }
  }, [kind, window, loadHistory]);

  const onTabChange = (next: string) => {
    if (next !== "day" && next !== "week") return;
    setKind(next);
    setReport(null);
    void loadHistory(next);
  };

  return (
    <div className="min-h-screen bg-neutral-50 p-6">
      <header className="mb-6 flex items-baseline justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">窗口报告</h1>
          <p className="text-sm text-neutral-600">
            把 <code>sessions_summary</code> 里所有内容汇总成日报或周报。
            任何出站调用前云端 prompt 都会强制 redact。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Link
            to="/"
            className="rounded-md border border-neutral-300 bg-white px-3 py-1.5 text-sm transition hover:bg-neutral-100"
          >
            ← 返回首页
          </Link>
        </div>
      </header>

      {error && (
        <div className="mb-4 rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-700">
          {error}
        </div>
      )}

      <Tabs
        defaultValue="day"
        value={kind}
        onValueChange={onTabChange}
        className="w-full"
      >
        <TabsList>
          <TabsTrigger value="day">日报</TabsTrigger>
          <TabsTrigger value="week">周报</TabsTrigger>
        </TabsList>
        <TabsContent value="day">
          <ReportControls
            kind="day"
            referenceDate={referenceDate}
            window={window}
            generating={generating}
            report={report}
            onDateChange={setReferenceDate}
            onGenerate={generate}
          />
        </TabsContent>
        <TabsContent value="week">
          <ReportControls
            kind="week"
            referenceDate={referenceDate}
            window={window}
            generating={generating}
            report={report}
            onDateChange={setReferenceDate}
            onGenerate={generate}
          />
        </TabsContent>
      </Tabs>

      <section className="mt-6">
        <h2 className="mb-2 text-sm font-medium uppercase tracking-wide text-neutral-500">
          {kind === "day" ? "最近日报" : "最近周报"}
        </h2>
        <ReportHistory loading={loadingHistory} reports={history} />
      </section>
    </div>
  );
}

function ReportControls({
  kind,
  referenceDate,
  window,
  generating,
  report,
  onDateChange,
  onGenerate,
}: {
  kind: WindowKind;
  referenceDate: Date;
  window: { start: string; end: string };
  generating: boolean;
  report: WindowReport | null;
  onDateChange: (d: Date) => void;
  onGenerate: () => void;
}) {
  const windowLabel =
    kind === "day"
      ? formatDate(window.start)
      : `${formatDate(window.start)} → ${formatDateMinusDay(window.end)}`;
  return (
    <Card>
      <CardHeader>
        <div className="flex flex-wrap items-baseline justify-between gap-3">
          <div>
            <CardTitle>
              {kind === "day" ? "日报" : "周报"}
            </CardTitle>
            <CardDescription>
              窗口:<span className="font-mono">{windowLabel}</span>
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            <input
              type="date"
              value={toDateInputValue(referenceDate)}
              onChange={(e) => {
                const v = e.target.value;
                if (!v) return;
                const d = new Date(`${v}T00:00:00Z`);
                if (!Number.isNaN(d.getTime())) onDateChange(d);
              }}
              className="h-9 rounded-md border border-neutral-300 bg-white px-2 text-sm"
            />
            <Button
              variant="default"
              size="sm"
              disabled={generating}
              onClick={onGenerate}
            >
              {generating
                ? "生成中…"
                : report
                  ? "重新生成"
                  : `生成${sessionKindLabel(kind)}`}
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {report ? (
          <ReportDetail report={report} />
        ) : (
          <p className="text-sm text-neutral-500">
            还没生成报告。点{" "}
            <span className="font-medium">
              生成{sessionKindLabel(kind)}
            </span>{" "}
            让当前 provider(失败时回退到本地启发式)给你总结这个窗口。
          </p>
        )}
      </CardContent>
    </Card>
  );
}

function ReportDetail({ report }: { report: WindowReport }) {
  const isCached = report.cached;
  const providerVariant =
    report.provider === "heuristic"
      ? "warning"
      : report.provider === "openai"
        ? "success"
        : report.provider === "anthropic"
          ? "codex"
          : "default";
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant={providerVariant}>
          {providerLabel(report.provider)}
        </Badge>
        <Badge variant="default">
          {sessionKindLabel(report.window_label)}
        </Badge>
        {isCached && (
          <Badge variant="default">缓存 · {report.age_hours} 小时前</Badge>
        )}
        <span className="text-xs text-neutral-500">
          生成于 {formatRelative(report.created_at)}
        </span>
      </div>
      <p className="text-lg font-medium text-neutral-900">
        {report.headline}
      </p>
      {report.bullets.length > 0 && (
        <ul className="list-disc space-y-1 pl-5 text-sm text-neutral-700">
          {report.bullets.map((b, i) => (
            <li key={i}>{b}</li>
          ))}
        </ul>
      )}
      {report.themes.length > 0 && (
        <div>
          <div className="mb-1 text-xs font-medium uppercase tracking-wide text-neutral-500">
            主题
          </div>
          <div className="flex flex-wrap gap-1">
            {report.themes.map((t) => (
              <Badge key={t} variant="default" className="font-mono text-[10px]">
                {t}
              </Badge>
            ))}
          </div>
        </div>
      )}
      {report.project_breakdown.length > 0 && (
        <div>
          <div className="mb-1 text-xs font-medium uppercase tracking-wide text-neutral-500">
            项目分布
          </div>
          <div className="overflow-hidden rounded-md border border-neutral-200 bg-white">
            <table className="w-full text-sm">
              <thead className="bg-neutral-50 text-neutral-500">
                <tr>
                  <th className="px-3 py-2 text-left text-xs uppercase tracking-wide">
                    项目
                  </th>
                  <th className="px-3 py-2 text-right text-xs uppercase tracking-wide">
                    会话
                  </th>
                  <th className="px-3 py-2 text-right text-xs uppercase tracking-wide">
                    事件
                  </th>
                </tr>
              </thead>
              <tbody>
                {report.project_breakdown.map((p) => (
                  <tr
                    key={p.project_key}
                    className="border-t border-neutral-100"
                  >
                    <td className="px-3 py-1.5 font-mono text-[12px]">
                      {p.project_key}
                    </td>
                    <td className="px-3 py-1.5 text-right tabular-nums">
                      {p.sessions}
                    </td>
                    <td className="px-3 py-1.5 text-right tabular-nums">
                      {p.events}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

function ReportHistory({
  loading,
  reports,
}: {
  loading: boolean;
  reports: WindowReport[];
}) {
  if (loading && reports.length === 0) {
    return <p className="text-sm text-neutral-500">加载中…</p>;
  }
  if (reports.length === 0) {
    return (
      <p className="text-sm text-neutral-500">
        还没有缓存的报告。先生成一份,历史条带就会开始累。
      </p>
    );
  }
  return (
    <div className="grid grid-cols-1 gap-2 md:grid-cols-2 xl:grid-cols-3">
      {reports.map((r) => (
        <Card
          key={r.id}
          className={cn(
            r.provider === "heuristic"
              ? "border-amber-200"
              : "border-emerald-200"
          )}
        >
          <CardHeader>
            <CardTitle className="text-sm">{r.headline}</CardTitle>
            <CardDescription className="text-xs">
              {formatDate(r.window_start)} · {providerLabel(r.provider)}
              {r.cached ? ` · 缓存 ${r.age_hours} 小时` : ""}
            </CardDescription>
          </CardHeader>
        </Card>
      ))}
    </div>
  );
}

function formatDate(iso: string) {
  const d = new Date(iso);
  return d.toLocaleDateString("zh-CN", {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

// window_end 是排他上界,展示时减 1 天。
function formatDateMinusDay(iso: string) {
  const d = new Date(iso);
  d.setUTCDate(d.getUTCDate() - 1);
  return d.toLocaleDateString("zh-CN", {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

function formatRelative(iso: string) {
  const d = new Date(iso);
  return d.toLocaleString("zh-CN");
}

function toDateInputValue(d: Date): string {
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, "0");
  const day = String(d.getUTCDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}
