import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Link, useNavigate, useSearchParams } from "react-router-dom";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { cn, sourceColorClass } from "../lib/utils";
import type { SearchHit } from "../lib/types";

// Stage 5.7 / v0.3.7: cross-source search. One search box, results
// grouped by `kind` and coloured by source. Clicking a hit jumps
// to the corresponding destination (Sessions page with the
// partition pre-selected for sessions/events; Planner page for
// plans; Settings page for source hits). URL `?q=foo` is hydrated
// on first render so Cmd-K and the home-page button share the
// same input.

const KIND_LABEL: Record<SearchHit["kind"], string> = {
  session: "会话",
  event: "事件",
  plan: "规划",
  source: "数据源",
};

const KIND_TESTID: Record<SearchHit["kind"], string> = {
  session: "search-hit-session",
  event: "search-hit-event",
  plan: "search-hit-plan",
  source: "search-hit-source",
};

/** Where a hit jumps to when the user clicks it. */
function destinationFor(hit: SearchHit): string {
  switch (hit.kind) {
    case "session":
    case "event":
      // Both session summary IDs and raw_event IDs that came back
      // from the search fanout share the same partition identifier
      // (the join is on source_id + content; the result `id` is
      // the partition id for sessions and the event id otherwise).
      // The Sessions page accepts ?partition= to pre-select.
      return `/sessions?partition=${encodeURIComponent(hit.id)}`;
    case "plan":
      return "/planner";
    case "source":
      return "/sessions";
  }
}

export default function SearchPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  const initialQ = searchParams.get("q") ?? "";
  const [query, setQuery] = useState(initialQ);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-focus on mount — the search box is the entire point.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Cmd-K / Ctrl-K from anywhere → focus the search input.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  const runSearch = useCallback(async (q: string) => {
    if (q.trim().length < 2) {
      setHits([]);
      setError(null);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const out = await invoke<SearchHit[]>("global_search", { query: q });
      setHits(out);
    } catch (e) {
      setError(`搜索失败: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  }, []);

  // Debounced live search on every keystroke (150ms).
  useEffect(() => {
    const t = setTimeout(() => {
      runSearch(query);
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (query) next.set("q", query);
          else next.delete("q");
          return next;
        },
        { replace: true }
      );
    }, 150);
    return () => clearTimeout(t);
  }, [query, runSearch, setSearchParams]);

  const grouped = useMemo(() => {
    const m = new Map<SearchHit["kind"], SearchHit[]>();
    for (const h of hits) {
      const arr = m.get(h.kind) ?? [];
      arr.push(h);
      m.set(h.kind, arr);
    }
    return m;
  }, [hits]);

  return (
    <div className="min-h-screen bg-neutral-50">
      <header className="border-b border-neutral-200 bg-white px-6 py-4">
        <div className="mx-auto flex max-w-3xl items-center gap-3">
          <Link
            to="/"
            className="rounded-md border border-neutral-300 px-3 py-1.5 text-sm transition hover:bg-neutral-100"
          >
            ← 返回首页
          </Link>
          <div className="flex-1">
            <input
              ref={inputRef}
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索会话 / 事件 / 规划 / 数据源 (≥ 2 字)"
              className="w-full rounded-md border border-neutral-300 bg-white px-3 py-2 text-sm focus:border-ink-900 focus:outline-none"
              data-testid="search-input"
            />
          </div>
          {busy && (
            <span className="text-xs text-neutral-500" data-testid="search-busy">
              搜索中…
            </span>
          )}
        </div>
        <p className="mx-auto mt-2 max-w-3xl text-[11px] text-neutral-500">
          {query.trim().length < 2
            ? "输入至少 2 个字符开始搜索 · ⌘K / Ctrl-K 聚焦"
            : `${hits.length} 个命中 · 按类型分组`}
        </p>
      </header>

      {error && (
        <div className="border-b border-red-300 bg-red-50 px-6 py-3 text-sm text-red-700">
          {error}
        </div>
      )}

      <div className="mx-auto max-w-3xl px-6 py-4">
        {hits.length === 0 && !busy && query.trim().length >= 2 && (
          <p className="mt-8 text-center text-sm text-neutral-500">
            没有命中。换一个关键词试试。
          </p>
        )}

        {[...grouped.entries()].map(([kind, list]) => (
          <section
            key={kind}
            className="mb-6"
            data-testid={`search-section-${kind}`}
          >
            <h2 className="mb-2 text-xs font-medium uppercase tracking-wide text-neutral-500">
              {KIND_LABEL[kind]} · {list.length}
            </h2>
            <ul className="space-y-1">
              {list.map((hit) => (
                <li key={`${hit.kind}-${hit.id}`}>
                  <button
                    type="button"
                    onClick={() => navigate(destinationFor(hit))}
                    className="flex w-full items-start gap-3 rounded-md border border-neutral-200 bg-white px-3 py-2 text-left transition hover:border-ink-900 hover:bg-neutral-50"
                    data-testid={KIND_TESTID[hit.kind]}
                  >
                    <Badge
                      variant="default"
                      className={cn(
                        "shrink-0",
                        sourceColorClass(hit.source_display_name)
                      )}
                    >
                      {hit.source_display_name}
                    </Badge>
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium text-neutral-900">
                        {hit.title || hit.snippet || hit.id}
                      </p>
                      {hit.snippet && hit.title && (
                        <p className="mt-0.5 line-clamp-2 text-xs text-neutral-600">
                          {hit.snippet}
                        </p>
                      )}
                      <p className="mt-0.5 flex items-center gap-2 text-[10px] text-neutral-400">
                        <span>{KIND_LABEL[hit.kind]}</span>
                        {hit.project_key && <span>· {hit.project_key}</span>}
                        {hit.started_at && <span>· {hit.started_at}</span>}
                      </p>
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>

      <div className="mx-auto max-w-3xl px-6 pb-12 text-center text-[11px] text-neutral-400">
        <Button
          variant="outline"
          size="sm"
          onClick={() => navigate("/")}
        >
          关闭
        </Button>
      </div>
    </div>
  );
}
