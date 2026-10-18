import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Link } from "react-router-dom";
import type { EisenhowerDigest, EisenhowerItem } from "../lib/types";
import { cn, sourceColorClass } from "../lib/utils";
import {
  quadrantSubtitle,
  quadrantTitle,
  planStatusLabel,
  sessionKindLabel,
} from "../lib/locale";

type PlanTemplate = {
  id: string;
  name: string;
  kind: string;
  created_at: string;
};

type PlanItem = {
  id: string;
  template_id: string;
  quadrant: number;
  title: string;
  description: string;
  status: string; // "todo" | "in_progress" | "done"
  priority: number;
  source_event_id: string | null;
  ai_reasoning: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
};

type QuadrantSuggestion = {
  quadrant: number;
  reasoning: string;
};

// Stage 5.9 / v0.4.1: re-pull the Eisenhower digest every 5 min
// while the Planner page is mounted. 5 min is the same cadence the
// in-app background scanner uses, so the digest stays within one
// scanner cycle of fresh data.
const EISENHOWER_AUTOREFRESH_MS = 5 * 60 * 1000;

const STATUS_TINT: Record<string, string> = {
  todo: "bg-white",
  in_progress: "bg-yellow-50",
  done: "bg-emerald-50 line-through text-neutral-500",
};

export default function PlannerPage() {
  const [templates, setTemplates] = useState<PlanTemplate[]>([]);
  const [activeTemplateId, setActiveTemplateId] = useState<string | null>(null);
  const [items, setItems] = useState<PlanItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Add-item form
  const [newTitle, setNewTitle] = useState("");
  const [newDescription, setNewDescription] = useState("");
  const [suggested, setSuggested] = useState<QuadrantSuggestion | null>(null);
  const [pendingQuadrant, setPendingQuadrant] = useState<number>(2);

  // Create-template form
  const [showCreateTemplate, setShowCreateTemplate] = useState(false);
  const [newTemplateName, setNewTemplateName] = useState("");
  const [newTemplateKind, setNewTemplateKind] = useState<"day" | "week">("day");

  // Stage 5-E.3b: Eisenhower digest panel state. Runs the LLM (or
  // heuristic fallback) over recent raw_events grouped by project.
  const [digest, setDigest] = useState<EisenhowerDigest | null>(null);
  const [digestBusy, setDigestBusy] = useState(false);
  const [digestLookback, setDigestLookback] = useState<number>(24);
  const [digestPromoted, setDigestPromoted] = useState<number | null>(null);

  const refreshTemplates = useCallback(async () => {
    try {
      const t = await invoke<PlanTemplate[]>("list_plan_templates");
      setTemplates(t);
      if (t.length > 0 && !activeTemplateId) {
        setActiveTemplateId(t[0].id);
      }
    } catch (e) {
      setError(`list_plan_templates failed: ${String(e)}`);
    }
  }, [activeTemplateId]);

  const refreshItems = useCallback(async (templateId: string) => {
    try {
      const list = await invoke<PlanItem[]>("list_plan_items", { templateId });
      setItems(list);
    } catch (e) {
      setError(`list_plan_items failed: ${String(e)}`);
    }
  }, []);

  useEffect(() => {
    refreshTemplates();
  }, [refreshTemplates]);

  useEffect(() => {
    if (activeTemplateId) {
      refreshItems(activeTemplateId);
    } else {
      setItems([]);
    }
  }, [activeTemplateId, refreshItems]);

  // Stage 5.9 / v0.4.1: auto-refresh the Eisenhower digest every 5
  // minutes while the Planner page is open so the 4 quadrants stay
  // current with whatever new events the background scanner just
  // ingested. Cheap: one SQL aggregate + (optionally) one LLM call
  // every 5 min. Only fires when the page is mounted — when the
  // user is on Sessions / Settings / etc. we save the cycles.
  useEffect(() => {
    // initial mount = one shot so the user immediately sees the
    // most recent digest, then poll on the 5 min tick.
    runEisenhower(digestLookback);
    const handle = setInterval(
      () => runEisenhower(digestLookback),
      EISENHOWER_AUTOREFRESH_MS
    );
    return () => clearInterval(handle);
    // runEisenhower is a stable useCallback (deps: []); the
    // lookback state is intentionally pinned at mount to avoid
    // re-creating the interval on every change. Users can still
    // re-trigger manually by picking a new lookback.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const [digestLastUpdated, setDigestLastUpdated] = useState<Date | null>(null);
  useEffect(() => {
    if (digest && !digestBusy) {
      setDigestLastUpdated(new Date());
    }
  }, [digest, digestBusy]);

  // Stage 5-E.3d: drag-and-drop. We track the quadrant the user
  // is currently hovering so the column lights up as a drop target.
  const [dragOverQuadrant, setDragOverQuadrant] = useState<number | null>(null);

  // Stage 5-E.3d: drop a plan item into a new quadrant. Re-uses
  // `upsert_plan_item` with the existing item's id so the row is
  // updated, not duplicated. `priority` follows the same mapping
  // `pendingQuadrant` uses in `submitNewItem` (Q1=3, Q2=2, Q3=1,
  // Q4=0) so the visual P-label stays consistent.
  const moveItemToQuadrant = useCallback(
    async (id: string, newQuadrant: number) => {
      const target = items.find((it) => it.id === id);
      if (!target || target.quadrant === newQuadrant) return;
      setBusy(true);
      setError(null);
      try {
        await invoke<PlanItem>("upsert_plan_item", {
          input: {
            id: target.id,
            template_id: target.template_id,
            quadrant: newQuadrant,
            title: target.title,
            description: target.description,
            status: target.status,
            priority:
              newQuadrant === 1
                ? 3
                : newQuadrant === 2
                  ? 2
                  : newQuadrant === 3
                    ? 1
                    : 0,
            source_event_id: target.source_event_id,
            ai_reasoning: target.ai_reasoning,
          },
        });
        if (activeTemplateId) {
          await refreshItems(activeTemplateId);
        }
      } catch (e: any) {
        setError(
          typeof e === "string" ? e : (e?.message ?? "拖拽更新失败"),
        );
      } finally {
        setBusy(false);
        setDragOverQuadrant(null);
      }
    },
    [activeTemplateId, items, refreshItems],
  );

  // Stage 5-E.3b: drive the Eisenhower digest panel.
  const runEisenhower = useCallback(async (hours: number) => {
    setDigestBusy(true);
    setError(null);
    setDigestPromoted(null);
    try {
      const d = await invoke<EisenhowerDigest>("generate_eisenhower_digest", {
        lookbackHours: hours,
      });
      setDigest(d);
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? "Eisenhower 评估失败"));
    } finally {
      setDigestBusy(false);
    }
  }, []);

  const promoteQ1 = useCallback(async (d: EisenhowerDigest) => {
    setDigestBusy(true);
    setError(null);
    try {
      const n = await invoke<number>("promote_eisenhower_q1", { digest: d });
      setDigestPromoted(n);
      if (activeTemplateId) {
        await refreshItems(activeTemplateId);
      }
      // Refresh templates list too — if no "day" template existed, the
      // backend's promote path auto-creates one ("Daily Stand-up").
      await refreshTemplates();
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? "Promotion 失败"));
    } finally {
      setDigestBusy(false);
    }
  }, [activeTemplateId, refreshItems, refreshTemplates]);

  const groupedItems = useMemo(() => {
    const groups: Record<number, PlanItem[]> = { 1: [], 2: [], 3: [], 4: [] };
    for (const it of items) {
      const q = Math.max(1, Math.min(4, it.quadrant));
      groups[q].push(it);
    }
    return groups;
  }, [items]);

  const runAiSuggest = async () => {
    if (!newTitle.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const s = await invoke<QuadrantSuggestion>("ai_suggest_quadrant", {
        text: newTitle.trim(),
      });
      setSuggested(s);
      setPendingQuadrant(s.quadrant);
    } catch (e) {
      setError(`ai_suggest_quadrant failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const submitNewItem = async () => {
    if (!activeTemplateId) return;
    const title = newTitle.trim();
    if (!title) return;
    setBusy(true);
    setError(null);
    try {
      await invoke("upsert_plan_item", {
        input: {
          id: null,
          template_id: activeTemplateId,
          quadrant: pendingQuadrant,
          title,
          description: newDescription,
          status: "todo",
          priority: pendingQuadrant === 1 ? 3 : pendingQuadrant === 2 ? 2 : 1,
          source_event_id: null,
          ai_reasoning: suggested?.reasoning ?? null,
        },
      });
      setNewTitle("");
      setNewDescription("");
      setSuggested(null);
      setPendingQuadrant(2);
      await refreshItems(activeTemplateId);
    } catch (e) {
      setError(`upsert_plan_item failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const cycleItemStatus = async (item: PlanItem) => {
    setBusy(true);
    try {
      const next =
        item.status === "todo"
          ? "in_progress"
          : item.status === "in_progress"
          ? "done"
          : "todo";
      await invoke("upsert_plan_item", {
        input: {
          id: item.id,
          template_id: item.template_id,
          quadrant: item.quadrant,
          title: item.title,
          description: item.description,
          status: next,
          priority: item.priority,
          source_event_id: item.source_event_id,
          ai_reasoning: item.ai_reasoning,
        },
      });
      if (activeTemplateId) await refreshItems(activeTemplateId);
    } catch (e) {
      setError(`upsert_plan_item failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const removeItem = async (item: PlanItem) => {
    if (!confirm(`确认删除 "${item.title}"?`)) return;
    setBusy(true);
    try {
      await invoke("delete_plan_item", { id: item.id });
      if (activeTemplateId) await refreshItems(activeTemplateId);
    } catch (e) {
      setError(`delete_plan_item failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const submitNewTemplate = async () => {
    const name = newTemplateName.trim();
    if (!name) return;
    setBusy(true);
    setError(null);
    try {
      const t = await invoke<PlanTemplate>("create_plan_template", {
        name,
        kind: newTemplateKind,
      });
      setNewTemplateName("");
      setShowCreateTemplate(false);
      await refreshTemplates();
      setActiveTemplateId(t.id);
    } catch (e) {
      setError(`create_plan_template failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const removeTemplate = async () => {
    if (!activeTemplateId) return;
    const tmpl = templates.find((t) => t.id === activeTemplateId);
    if (!tmpl) return;
    if (!confirm(`确认删除模板 "${tmpl.name}" 以及它下面的所有事项?`)) return;
    setBusy(true);
    try {
      await invoke("delete_plan_template", { id: activeTemplateId });
      setActiveTemplateId(null);
      await refreshTemplates();
    } catch (e) {
      setError(`delete_plan_template failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="min-h-screen p-6">
      <header className="mb-4 flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">
            Eisenhower 规划器
          </h1>
          <p className="text-sm text-neutral-600">
            四象限的日 / 周模板,辅助分类由本地 Ollama 提供。
          </p>
        </div>
        <Link
          to="/"
          className="rounded-full border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
        >
          ← 返回首页
        </Link>
      </header>

      {error && (
        <p className="mb-3 text-sm text-red-600">{error}</p>
      )}

      {/* Stage 5-E.3b: Eisenhower 4-quadrant digest panel. Runs the
          LLM (or fallback heuristic) over the lookback window and
          surfaces the bucket list with a one-click "promote Q1"
          action that copies the urgent+important items into the
          Daily template below. */}
      <section className="mb-4 rounded-lg border border-neutral-200 bg-white p-4">
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <h2 className="text-base font-semibold tracking-tight">
            Eisenhower 评估
          </h2>
          <span className="text-xs text-neutral-500">
            按重要 × 紧急 分四象限,跨 agent 自动汇总
          </span>
          <span className="ml-auto flex items-center gap-2">
            <label className="text-xs text-neutral-500">回看</label>
            <select
              value={digestLookback}
              onChange={(e) => setDigestLookback(Number(e.target.value))}
              className="rounded-md border border-neutral-300 bg-white px-2 py-1 text-xs"
            >
              <option value={12}>12 小时</option>
              <option value={24}>24 小时</option>
              <option value={72}>3 天</option>
              <option value={168}>7 天</option>
            </select>
            <button
              type="button"
              onClick={() => runEisenhower(digestLookback)}
              disabled={digestBusy}
              className="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50"
            >
              {digestBusy ? "评估中…" : "运行评估"}
            </button>
            {digest && digest.items.length > 0 && (
              <button
                type="button"
                onClick={() => promoteQ1(digest)}
                disabled={digestBusy}
                className="rounded-md border border-red-300 bg-white px-3 py-1.5 text-xs font-medium text-red-700 transition hover:bg-red-50 disabled:opacity-50"
                title="把 Q1 (紧急+重要) 推进到当前模板"
              >
                推进 Q1 到计划
              </button>
            )}
          </span>
          <span
            className="text-[10px] text-neutral-500"
            data-testid="digest-last-updated"
          >
            {digestLastUpdated
              ? `最后刷新 ${digestLastUpdated.toLocaleTimeString()} · 5 min 自动`
              : "尚未刷新"}
          </span>
        </div>

        {digestPromoted !== null && (
          <p className="mb-2 text-xs text-emerald-700">
            ✓ 已推进 {digestPromoted} 个 Q1 项目到 Daily 模板
          </p>
        )}

        {!digest && !digestBusy && (
          <p className="text-sm text-neutral-500">
            点击「运行评估」根据最近 raw_events 自动跑 Eisenhower 分类。
            没配 LLM 时会退化到 heuristic,矩阵照样能用。
          </p>
        )}

        {digest && digest.items.length === 0 && (
          <p className="text-sm text-neutral-500">
            回看窗口内没有 project groups。{digest.note ?? ""}
          </p>
        )}

        {digest && digest.items.length > 0 && (
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-4">
            {[1, 2, 3, 4].map((q) => (
              <EisenhowerColumn
                key={q}
                quadrant={q as 1 | 2 | 3 | 4}
                items={digest.items.filter((it) => it.quadrant === q)}
                onPromote={() => promoteQ1(digest)}
                busy={digestBusy}
                aiOverall={digest.ai_used}
              />
            ))}
          </div>
        )}

        {digest?.note && digest.items.length > 0 && (
          <p className="mt-2 text-xs text-neutral-500">{digest.note}</p>
        )}
      </section>

      <section className="mb-4 rounded-lg border border-neutral-200 bg-white p-3">
        <div className="flex flex-wrap items-center gap-2">
          <label className="text-xs text-neutral-500" htmlFor="planner-template-select">
            模板
          </label>
          <select
            id="planner-template-select"
            value={activeTemplateId ?? ""}
            onChange={(e) => {
              const v = e.target.value;
              console.log("[planner] template select changed →", v);
              setActiveTemplateId(v || null);
            }}
            className="rounded-md border border-neutral-300 bg-white px-2 py-1 text-sm"
          >
            {templates.length === 0 && <option value="">（还没有模板）</option>}
            {templates.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name} · {sessionKindLabel(t.kind)}
              </option>
            ))}
          </select>
          <span className="text-xs text-neutral-500" data-testid="planner-active-template">
            {(() => {
              const t = templates.find((x) => x.id === activeTemplateId);
              if (!t) return "未选模板";
              return `当前: ${t.name} (${sessionKindLabel(t.kind)})`;
            })()}
          </span>
          {activeTemplateId && (
            <button
              onClick={removeTemplate}
              disabled={busy}
              className="rounded-full border border-red-200 px-3 py-1 text-xs text-red-700 transition hover:bg-red-50 disabled:opacity-50"
            >
              删除模板
            </button>
          )}
          <button
            onClick={() => setShowCreateTemplate((v) => !v)}
            className="rounded-full border border-neutral-300 px-3 py-1 text-xs transition hover:bg-neutral-100"
          >
            {showCreateTemplate ? "取消" : "+ 新建模板"}
          </button>
        </div>
        {showCreateTemplate && (
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <input
              value={newTemplateName}
              onChange={(e) => setNewTemplateName(e.target.value)}
              placeholder="模板名称"
              className="rounded-md border border-neutral-300 bg-white px-2 py-1 text-sm"
            />
            <select
              value={newTemplateKind}
              onChange={(e) =>
                setNewTemplateKind(e.target.value as "day" | "week")
              }
              className="rounded-md border border-neutral-300 bg-white px-2 py-1 text-sm"
            >
              <option value="day">日报</option>
              <option value="week">周报</option>
            </select>
            <button
              onClick={submitNewTemplate}
              disabled={busy || !newTemplateName.trim()}
              className="rounded-full bg-ink-900 px-3 py-1 text-xs font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
            >
              创建
            </button>
          </div>
        )}
      </section>

      <section className="mb-4 rounded-lg border border-neutral-200 bg-white p-3">
        <div className="mb-2 text-sm font-medium">添加事项</div>
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          placeholder="标题（例如：排查 Codex scan 偶发失败）"
          className="mb-2 w-full rounded-md border border-neutral-300 bg-white p-2 text-sm focus:border-ink-900 focus:outline-none"
          disabled={!activeTemplateId}
        />
        <textarea
          value={newDescription}
          onChange={(e) => setNewDescription(e.target.value)}
          placeholder="补充说明（可选）"
          rows={2}
          className="mb-2 w-full rounded-md border border-neutral-300 bg-white p-2 text-sm focus:border-ink-900 focus:outline-none"
          disabled={!activeTemplateId}
        />
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={runAiSuggest}
            disabled={busy || !newTitle.trim()}
            className="rounded-full border border-neutral-300 px-3 py-1 text-xs transition hover:bg-neutral-100 disabled:opacity-50"
          >
            让 Ollama 建议象限
          </button>
          <label className="text-xs text-neutral-500">象限</label>
          <select
            value={pendingQuadrant}
            onChange={(e) => setPendingQuadrant(Number(e.target.value))}
            className="rounded-md border border-neutral-300 bg-white px-2 py-1 text-sm"
          >
            {[1, 2, 3, 4].map((q) => (
              <option key={q} value={q}>
                Q{q} · {quadrantTitle(q)}
              </option>
            ))}
          </select>
          <button
            onClick={submitNewItem}
            disabled={busy || !activeTemplateId || !newTitle.trim()}
            className="rounded-full bg-ink-900 px-4 py-1.5 text-sm font-medium text-white shadow-sm transition hover:bg-neutral-800 disabled:opacity-50"
          >
            添加
          </button>
        </div>
        {suggested && (
          <div className="mt-2 rounded-md bg-emerald-50 p-2 text-xs leading-relaxed text-emerald-900">
            <strong className="font-mono">Q{suggested.quadrant}</strong> —{" "}
            {suggested.reasoning}
          </div>
        )}
      </section>

      <div className="mb-2 flex items-baseline justify-between">
        <h2 className="text-sm font-semibold text-neutral-700">
          {(() => {
            const t = templates.find((x) => x.id === activeTemplateId);
            return t
              ? `${t.name} · ${sessionKindLabel(t.kind)} 计划项`
              : "请先选模板";
          })()}
        </h2>
        <span className="text-xs text-neutral-500" data-testid="planner-item-count">
          {items.length === 0
            ? "0 个 item"
            : `共 ${items.length} 个 · ${(groupedItems[1] || []).length} Q1 / ${(groupedItems[2] || []).length} Q2 / ${(groupedItems[3] || []).length} Q3 / ${(groupedItems[4] || []).length} Q4`}
        </span>
      </div>

      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        {[1, 2, 3, 4].map((q) => {
          const list = groupedItems[q] ?? [];
          const tint = q === 1
            ? "bg-red-50 border-red-300"
            : q === 2
              ? "bg-amber-50 border-amber-300"
              : q === 3
                ? "bg-sky-50 border-sky-300"
                : "bg-neutral-50 border-neutral-300";
          const isDropTarget = dragOverQuadrant === q;
          return (
            <div
              key={q}
              data-testid={`planner-quadrant-${q}`}
              data-droppable={list.length === 0 ? "true" : "true"}
              onDragOver={(e) => {
                // Without preventDefault the browser refuses to
                // fire `drop`. We also don't care about the
                // dataTransfer types here — every drag we care
                // about carries a plan_item id under `text/plain`.
                e.preventDefault();
                if (dragOverQuadrant !== q) {
                  setDragOverQuadrant(q);
                }
              }}
              onDragLeave={(e) => {
                // Only clear when the cursor actually leaves the
                // column; inner element drags bubble dragleave too.
                const related = e.relatedTarget as Node | null;
                if (related && (e.currentTarget as Node).contains(related)) {
                  return;
                }
                setDragOverQuadrant((cur) => (cur === q ? null : cur));
              }}
              onDrop={(e) => {
                e.preventDefault();
                const id = e.dataTransfer.getData("text/plain");
                if (id) {
                  void moveItemToQuadrant(id, q);
                } else {
                  setDragOverQuadrant(null);
                }
              }}
              className={`rounded-lg border p-3 transition ${tint} ${
                isDropTarget ? "ring-2 ring-offset-1 ring-neutral-900/40 scale-[1.01]" : ""
              }`}
            >
              <div className="mb-2 flex items-baseline justify-between">
                <div>
                  <div className="text-sm font-semibold">
                    Q{q} · {quadrantTitle(q)}
                  </div>
                  <div className="text-xs text-neutral-500">{quadrantSubtitle(q)}</div>
                </div>
                <span className="text-xs text-neutral-500">
                  {list.length} 项
                </span>
              </div>
              {list.length === 0 ? (
                <p className="text-xs italic text-neutral-400">（空）</p>
              ) : (
                <ul className="space-y-2">
                  {list.map((item) => (
                    <li
                      key={item.id}
                      data-testid={`planner-item-${item.id}`}
                      draggable={true}
                      onDragStart={(e) => {
                        // Stage 5-E.3d: HTML5 native DnD. The drop
                        // handler reads this id back out of
                        // text/plain — keeping it tiny on purpose so
                        // `getData("text/plain")` is unambiguous.
                        e.dataTransfer.setData("text/plain", item.id);
                        e.dataTransfer.effectAllowed = "move";
                      }}
                      onDragEnd={() => {
                        // Browser may cancel the drag (drop on
                        // non-droppable, ESC, etc.) without firing
                        // onDragLeave on the original column, so
                        // clear the ring here as a safety net.
                        setDragOverQuadrant((cur) => (cur ? null : cur));
                      }}
                      className={`cursor-grab rounded-md border border-neutral-200 p-2 text-xs leading-relaxed transition active:cursor-grabbing hover:shadow-sm ${
                        STATUS_TINT[item.status] ?? "bg-white"
                      }`}
                    >
                      <div className="flex items-start justify-between gap-2">
                        <div className="font-medium">{item.title}</div>
                        <span className="font-mono text-[10px] text-neutral-400">
                          P{item.priority}
                        </span>
                      </div>
                      {item.description && (
                        <p className="mt-1 text-[11px] text-neutral-600">
                          {item.description}
                        </p>
                      )}
                      {item.ai_reasoning && (
                        <p className="mt-1 text-[11px] italic text-neutral-500">
                          AI：{item.ai_reasoning}
                        </p>
                      )}
                      <div className="mt-1 flex items-center justify-between">
                        <button
                          onClick={() => cycleItemStatus(item)}
                          disabled={busy}
                          className="rounded-full border border-neutral-300 px-2 py-0.5 text-[10px] transition hover:bg-neutral-100 disabled:opacity-50"
                        >
                          {item.status === "todo"
                            ? "开始"
                            : item.status === "in_progress"
                            ? "完成"
                            : "重新打开"}
                        </button>
                        <span className="text-[10px] text-neutral-400">
                          {planStatusLabel(item.status)}
                        </span>
                        <button
                          onClick={() => removeItem(item)}
                          disabled={busy}
                          className="text-[10px] text-red-700 transition hover:underline disabled:opacity-50"
                        >
                          删除
                        </button>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// Stage 5-E.3b: one column of the Eisenhower matrix. Color-coded by
// quadrant and a single accent border on Q1 to keep the "Do first"
// affordance visually loud.
function EisenhowerColumn({
  quadrant,
  items,
  onPromote,
  busy,
  aiOverall,
}: {
  quadrant: 1 | 2 | 3 | 4;
  items: EisenhowerItem[];
  onPromote: () => void;
  busy: boolean;
  aiOverall: boolean;
}) {
  const style = EISENHOWER_COL_STYLE[quadrant];
  return (
    <div className={`rounded-md border ${style.border} ${style.bg} p-2`}>
      <header className="mb-2 flex items-baseline justify-between">
        <div>
          <p className="text-sm font-semibold" style={{ color: style.title }}>
            Q{quadrant} · {quadrantTitle(quadrant)}
          </p>
          <p className="text-[11px] text-neutral-600">
            {quadrantSubtitle(quadrant)}
          </p>
        </div>
        <span className="rounded-full bg-white/70 px-2 py-0.5 text-[11px] text-neutral-700">
          {items.length}
        </span>
      </header>
      <ul className="space-y-2">
        {items.length === 0 && (
          <li className="rounded border border-dashed border-neutral-300 bg-white/60 px-2 py-3 text-center text-[11px] text-neutral-500">
            这一象限本周没项目
          </li>
        )}
        {items.map((it) => (
          <li
            key={`${it.project_key}-${it.source_display_name}`}
            className="rounded border border-neutral-200 bg-white px-2 py-1.5"
          >
            <p className="text-xs font-medium text-neutral-900">
              {it.label || it.project_key}
            </p>
            <p className="mt-0.5 truncate text-[11px] text-neutral-500">
              {it.project_key}
            </p>
            <div className="mt-1 flex flex-wrap items-center gap-1 text-[10px]">
              <span
                className={cn(
                  "rounded border px-1.5 py-0.5",
                  sourceColorClass(it.source_display_name)
                )}
                data-testid={`planner-source-${it.project_key}-${it.source_display_name}`}
              >
                {it.source_display_name}
              </span>
              <span className="rounded bg-neutral-100 px-1.5 py-0.5 text-neutral-700">
                imp {it.importance}/10
              </span>
              <span className="rounded bg-neutral-100 px-1.5 py-0.5 text-neutral-700">
                urg {it.urgency}/10
              </span>
              <span className="rounded bg-neutral-100 px-1.5 py-0.5 text-neutral-700">
                {it.event_count} events
              </span>
              {it.ai_used ? (
                <span className="rounded bg-emerald-100 px-1.5 py-0.5 text-emerald-800">
                  AI
                </span>
              ) : (
                <span className="rounded bg-neutral-100 px-1.5 py-0.5 text-neutral-500">
                  启发式
                </span>
              )}
            </div>
            {it.reasoning && (
              <p className="mt-1 text-[11px] text-neutral-600">
                {it.reasoning}
              </p>
            )}
          </li>
        ))}
      </ul>
      {quadrant === 1 && items.length > 0 && (
        <button
          type="button"
          onClick={onPromote}
          disabled={busy || !aiOverall}
          className="mt-2 w-full rounded-md bg-red-600 px-2 py-1 text-xs font-medium text-white transition hover:bg-red-700 disabled:opacity-50"
          title="一键把这一象限的所有项目推进到 Daily 模板"
        >
          {busy ? "推进中…" : "推进全部 Q1"}
        </button>
      )}
    </div>
  );
}

const EISENHOWER_COL_STYLE: Record<
  1 | 2 | 3 | 4,
  { bg: string; border: string; title: string }
> = {
  1: { bg: "bg-red-50", border: "border-red-300", title: "#b91c1c" },
  2: { bg: "bg-blue-50", border: "border-blue-300", title: "#1d4ed8" },
  3: { bg: "bg-amber-50", border: "border-amber-300", title: "#b45309" },
  4: { bg: "bg-neutral-50", border: "border-neutral-300", title: "#525252" },
};
