import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Link } from "react-router-dom";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "../components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "../components/ui/dialog";
import { Input } from "../components/ui/input";
import { Label } from "../components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui/select";
import { cn } from "../lib/utils";
import { LLM_PRESETS, providerLabel } from "../lib/locale";
import type {
  AiSettings,
  AiStatus,
  AppSettings,
  DeleteSourceReport,
  PruneReport,
  PromptTemplate,
  ResetReport,
  SourceRow,
} from "../lib/types";

// Settings 页三大职责:
//  1) LLM provider / key / 缓存策略 — 通过 `ai_get_settings` /
//     `ai_save_settings` 持久化。云端 API key 在 `ai::redact_outbound_pub`
//     强制 redact,本页之外任何代码读不到明文。
//  2) Provider 实时状态 — 挂载时调 `ai_status`,切 provider 后能立刻
//     看到本地 Ollama 联通性或云端 401。
//  3) 唯一破坏性操作入口:重置所有 raw_events + sessions_summary +
//     connector_runs(走 `reset_events`),Radix Dialog 二次确认兜底。
//  4) v0.5.5: 单个数据源清理(走 `delete_source`),用于清掉 dev/test
//     残留 row(典型是空 root_path / 0 events / 无 last_scan_at 的 stale
//     source)。FK CASCADE 自动级联清 raw_events / sessions_summary /
//     connector_runs / plan_items,所以一行就够。

const PROVIDER_VALUES = ["local", "openai", "anthropic"] as const;
type Provider = (typeof PROVIDER_VALUES)[number];

const DEFAULT_SETTINGS: AiSettings = {
  provider: "local",
  local_endpoint: "http://localhost:11434",
  local_model: "gemma4:e4b",
  cloud_endpoint: "https://api.openai.com/v1",
  cloud_model: "gpt-4o-mini",
  anthropic_endpoint: "https://api.anthropic.com",
  anthropic_model: "claude-3-5-haiku-latest",
  api_key: "",
  redact_prompts: true,
  cache_hours: 24,
};

const DEFAULT_APP_SETTINGS: AppSettings = {
  summary_dir: "~/Documents/agent_summaries",
  event_retention_days: 90,
};

export default function SettingsPage() {
  const [settings, setSettings] = useState<AiSettings>(DEFAULT_SETTINGS);
  const [appSettings, setAppSettings] =
    useState<AppSettings>(DEFAULT_APP_SETTINGS);
  const [prompts, setPrompts] = useState<PromptTemplate[]>([]);
  const [status, setStatus] = useState<AiStatus | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedTick, setSavedTick] = useState<number>(0);
  const [appSaving, setAppSaving] = useState(false);
  const [appSavedTick, setAppSavedTick] = useState<number>(0);
  // v0.4.3: retention prune state — separate from appSaving so the
  // button doesn't have to wait for the settings save round-trip.
  const [pruning, setPruning] = useState(false);
  const [pruneReport, setPruneReport] = useState<PruneReport | null>(null);
  const [resetOpen, setResetOpen] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [lastReset, setLastReset] = useState<ResetReport | null>(null);
  // v0.5.5: per-source cleanup. `pendingDelete` is the row the user
  // has clicked "删除" on but hasn't confirmed yet; `lastDelete`
  // surfaces the most recent DeleteSourceReport in a green banner so
  // the user sees "已删除 N 个事件 (src_…)" feedback without a refetch.
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [pendingDelete, setPendingDelete] = useState<SourceRow | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [lastDelete, setLastDelete] = useState<DeleteSourceReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshAll = useCallback(async () => {
    setError(null);
    try {
      const [s, st, app, ps, srcs] = await Promise.all([
        invoke<AiSettings>("ai_get_settings"),
        invoke<AiStatus>("ai_status"),
        invoke<AppSettings>("app_get_settings"),
        invoke<PromptTemplate[]>("app_list_prompts"),
        invoke<SourceRow[]>("list_sources"),
      ]);
      setSettings(s);
      setStatus(st);
      setAppSettings(app);
      setPrompts(ps);
      setSources(srcs);
    } catch (e) {
      setError(`加载设置失败:${String(e)}`);
    }
  }, []);

  const refreshSources = useCallback(async () => {
    try {
      const srcs = await invoke<SourceRow[]>("list_sources");
      setSources(srcs);
    } catch (e) {
      setError(`加载数据源失败:${String(e)}`);
    }
  }, []);

  useEffect(() => {
    refreshAll();
  }, [refreshAll]);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await invoke<AiStatus>("ai_status"));
    } catch (e) {
      setError(`检查 provider 状态失败:${String(e)}`);
    }
  }, []);

  const save = useCallback(async () => {
    setSaving(true);
    setError(null);
    try {
      await invoke("ai_save_settings", { settings });
      setSavedTick(Date.now());
      await refreshStatus();
    } catch (e) {
      setError(`保存失败:${String(e)}`);
    } finally {
      setSaving(false);
    }
  }, [settings, refreshStatus]);

  const resetAll = useCallback(async () => {
    setResetting(true);
    setError(null);
    try {
      const r = await invoke<ResetReport>("reset_events");
      setLastReset(r);
      setResetOpen(false);
      // Sources themselves aren't deleted by reset_events — only the
      // child rows. Refresh so the source list re-renders without
      // event counts.
      await refreshSources();
    } catch (e) {
      setError(`重置失败:${String(e)}`);
    } finally {
      setResetting(false);
    }
  }, [refreshSources]);

  // v0.5.5: per-source cleanup. The dialog has already confirmed — the
  // user clicked "是的,删除" — so this just hits the IPC, refreshes
  // the list, and shows the result. FK CASCADE handles the child
  // rows so the source count we render next refresh reflects reality.
  const confirmDeleteSource = useCallback(async () => {
    if (!pendingDelete) return;
    setDeleting(true);
    setError(null);
    try {
      const r = await invoke<DeleteSourceReport>("delete_source", {
        id: pendingDelete.id,
      });
      setLastDelete(r);
      setPendingDelete(null);
      await refreshSources();
    } catch (e) {
      setError(`删除数据源失败:${String(e)}`);
    } finally {
      setDeleting(false);
    }
  }, [pendingDelete, refreshSources]);

  const saveAppSettings = useCallback(async () => {
    setAppSaving(true);
    setError(null);
    try {
      await invoke("app_save_settings", { settings: appSettings });
      setAppSavedTick(Date.now());
      // Refresh so the UI shows the server-expanded absolute path.
      const fresh = await invoke<AppSettings>("app_get_settings");
      setAppSettings(fresh);
    } catch (e) {
      setError(`保存 app 设置失败:${String(e)}`);
    } finally {
      setAppSaving(false);
    }
  }, [appSettings]);

  const pickSummaryDir = useCallback(async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: appSettings.summary_dir,
      });
      if (typeof picked === "string") {
        setAppSettings((prev) => ({ ...prev, summary_dir: picked }));
      }
    } catch (e) {
      setError(`选择目录失败:${String(e)}`);
    }
  }, [appSettings.summary_dir]);

  // v0.4.3: manual retention prune. Always uses the *saved* days
  // value (not the in-form state) so the user can't accidentally
  // type a huge number, click the button, and nuke everything. The
  // `app_save_settings` round-trip already wrote the form value to
  // disk by the time this is callable, so this matches user intent.
  const runPrune = useCallback(async () => {
    setPruning(true);
    setError(null);
    try {
      const r = await invoke<PruneReport>("retention_prune_now");
      setPruneReport(r);
    } catch (e) {
      setError(`立即清理失败:${String(e)}`);
    } finally {
      setPruning(false);
    }
  }, []);

  const copyPrompt = useCallback(async (body: string) => {
    try {
      await navigator.clipboard.writeText(body);
    } catch (e) {
      setError(`复制失败:${String(e)}`);
    }
  }, []);

  const setField = <K extends keyof AiSettings>(k: K, v: AiSettings[K]) =>
    setSettings((prev) => ({ ...prev, [k]: v }));

  return (
    <div className="min-h-screen bg-neutral-50 p-6">
      <header className="mb-6 flex items-baseline justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">设置</h1>
          <p className="text-sm text-neutral-600">
            LLM provider、缓存策略、以及唯一的破坏性操作。云端 API key
            在任何出站请求前都会被强制 redact,详见{" "}
            <code>src/ai.rs::redact_outbound_pub</code>。
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
            onClick={() => refreshStatus()}
          >
            重新检查状态
          </Button>
        </div>
      </header>

      {error && (
        <div className="mb-4 rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-700">
          {error}
        </div>
      )}
      {savedTick > 0 && (
        <div className="mb-4 rounded-md border border-emerald-200 bg-emerald-50 p-3 text-xs text-emerald-800">
          已保存于 {new Date(savedTick).toLocaleTimeString()}。
        </div>
      )}
      {lastReset && (
        <div className="mb-4 rounded-md border border-amber-200 bg-amber-50 p-3 text-xs text-amber-800">
          上次重置:清除 {lastReset.raw_events_deleted} 条原始事件、{" "}
          {lastReset.sessions_summary_deleted} 个会话分区、{" "}
          {lastReset.connector_runs_deleted} 条 connector 运行记录。
        </div>
      )}
      {lastDelete && (
        <div className="mb-4 rounded-md border border-emerald-200 bg-emerald-50 p-3 text-xs text-emerald-800">
          {lastDelete.deleted > 0
            ? `已删除数据源 ${lastDelete.source_id} (FK CASCADE 自动清理 raw_events / sessions_summary / connector_runs)。`
            : `未找到数据源 ${lastDelete.source_id} — 可能已经被清理过。`}
        </div>
      )}
      {appSavedTick > 0 && (
        <div className="mb-4 rounded-md border border-emerald-200 bg-emerald-50 p-3 text-xs text-emerald-800">
          app 设置已保存于 {new Date(appSavedTick).toLocaleTimeString()}。
        </div>
      )}

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <Card className="lg:col-span-1">
          <CardHeader>
            <CardTitle>Provider 状态</CardTitle>
            <CardDescription>
              实时联通性检查,挂载时和每次保存后刷新。
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            {status ? (
              <>
                <div className="flex items-center justify-between gap-2">
                  <span className="text-neutral-500">联通</span>
                  <Badge variant={status.reachable ? "success" : "danger"}>
                    {status.reachable ? "是" : "否"}
                  </Badge>
                </div>
                <Row label="Provider" value={providerLabel(status.provider)} />
                <Row label="接入点" value={status.endpoint} mono />
                <Row label="模型" value={status.model} mono />
                <Row
                  label="延迟"
                  value={
                    status.latency_ms == null
                      ? "—"
                      : `${status.latency_ms} ms`
                  }
                />
                <Row
                  label="API key"
                  value={status.has_api_key ? "已配置" : "未配置"}
                />
                {status.installed_models.length > 0 && (
                  <div>
                    <div className="mb-1 text-xs font-medium uppercase tracking-wide text-neutral-500">
                      已安装模型
                    </div>
                    <div className="flex flex-wrap gap-1">
                      {status.installed_models.map((m) => (
                        <Badge key={m} variant="default" className="font-mono text-[10px]">
                          {m}
                        </Badge>
                      ))}
                    </div>
                  </div>
                )}
                {status.error && (
                  <div className="rounded-md border border-red-200 bg-red-50 p-2 text-xs text-red-700">
                    {status.error}
                  </div>
                )}
              </>
            ) : (
              <p className="text-neutral-500">状态加载中…</p>
            )}
          </CardContent>
        </Card>

        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle>LLM 配置</CardTitle>
            <CardDescription>
              provider 相关的接入点和模型名。点保存写入{" "}
              <code>ai_settings.json</code>(位于数据目录)。
            </CardDescription>
          </CardHeader>
          <CardContent className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div className="md:col-span-2 space-y-1.5">
              <Label htmlFor="provider">Provider</Label>
              <Select
                value={settings.provider}
                onValueChange={(v) => setField("provider", v as Provider)}
              >
                <SelectTrigger id="provider">
                  <SelectValue placeholder="选择 provider" />
                </SelectTrigger>
                <SelectContent>
                  {PROVIDER_VALUES.map((p) => (
                    <SelectItem key={p} value={p}>
                      {providerLabel(p)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="local_endpoint">本地接入点</Label>
              <Input
                id="local_endpoint"
                value={settings.local_endpoint}
                onChange={(e) =>
                  setField("local_endpoint", e.target.value)
                }
                placeholder="http://localhost:11434"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="local_model">本地模型</Label>
              <Input
                id="local_model"
                value={settings.local_model}
                onChange={(e) => setField("local_model", e.target.value)}
                placeholder="gemma4:e4b"
              />
            </div>

            <div className="md:col-span-2 space-y-1.5">
              <Label>快速预设 (Cloud OpenAI 兼容)</Label>
              <div className="flex flex-wrap gap-1.5" data-testid="cloud-preset-chips">
                {LLM_PRESETS.map((p) => {
                  const active = settings.cloud_endpoint === p.base_url;
                  return (
                    <button
                      type="button"
                      key={p.id}
                      onClick={() => {
                        setField("cloud_endpoint", p.base_url);
                        setField("cloud_model", p.default_model);
                      }}
                      title={p.hint}
                      data-testid={`cloud-preset-${p.id}`}
                      className={
                        active
                          ? "rounded-full border border-ink-900 bg-ink-900 px-2.5 py-0.5 text-[11px] text-white"
                          : "rounded-full border border-neutral-300 bg-white px-2.5 py-0.5 text-[11px] text-neutral-700 hover:bg-neutral-100"
                      }
                    >
                      {p.label}
                    </button>
                  );
                })}
              </div>
              <p className="text-[10px] leading-relaxed text-neutral-500">
                点一下就填 base_url + 默认 model,后面仍可手动改。覆盖 DeepSeek / Kimi / Qwen
                / Doubao / GLM / MiniMax / OpenAI 官方。
              </p>
            </div>

            <div className="md:col-span-2 space-y-1.5">
              <Label htmlFor="cloud_endpoint">Cloud (OpenAI 兼容) 接入点</Label>
              <Input
                id="cloud_endpoint"
                value={settings.cloud_endpoint}
                onChange={(e) => setField("cloud_endpoint", e.target.value)}
                placeholder="https://api.openai.com/v1  或  https://api.deepseek.com/v1"
                className="font-mono text-[12px]"
                data-testid="cloud-endpoint-input"
              />
              <p className="text-[10px] leading-relaxed text-neutral-500">
                任何 OpenAI 兼容服务都可以接(国内大模型 / 自部署代理 / proxy
                gateway)。会向 <code>&lt;endpoint&gt;/chat/completions</code> POST。
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="cloud_model">Cloud 模型</Label>
              <Input
                id="cloud_model"
                value={settings.cloud_model}
                onChange={(e) => setField("cloud_model", e.target.value)}
                placeholder="gpt-4o-mini"
                data-testid="cloud-model-input"
              />
            </div>

            <div className="md:col-span-2 space-y-1.5">
              <Label htmlFor="anthropic_endpoint">Anthropic 兼容接入点</Label>
              <Input
                id="anthropic_endpoint"
                value={settings.anthropic_endpoint}
                onChange={(e) =>
                  setField("anthropic_endpoint", e.target.value)
                }
                placeholder="https://api.anthropic.com  或自定义代理域名"
                className="font-mono text-[12px]"
              />
              <p className="text-[10px] leading-relaxed text-neutral-500">
                会向 <code>&lt;endpoint&gt;/v1/messages</code> POST。
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="anthropic_model">Anthropic 模型</Label>
              <Input
                id="anthropic_model"
                value={settings.anthropic_model}
                onChange={(e) =>
                  setField("anthropic_model", e.target.value)
                }
                placeholder="claude-3-5-haiku-latest"
              />
            </div>

            <div className="md:col-span-2 space-y-1.5">
              <Label htmlFor="api_key">
                API key(只存本地,任何出站调用前强制 redact)
              </Label>
              <Input
                id="api_key"
                type="password"
                value={settings.api_key}
                onChange={(e) => setField("api_key", e.target.value)}
                placeholder="sk-…  留空表示不修改现有值"
                autoComplete="off"
              />
              <p className="text-[10px] leading-relaxed text-neutral-500">
                多个 provider 共用同一个 key 槽(本地 Ollama 不需要)。换
                provider 前无需清空,云端在任何出站请求前都会把
                <code>sk-… / Bearer … / 32 字符 hex</code> 自动 redact 成
                <code>[REDACTED:…]</code>。
              </p>
            </div>

            <div className="flex items-center gap-2">
              <input
                id="redact"
                type="checkbox"
                className="h-4 w-4 rounded border-neutral-300 text-neutral-900 focus:ring-neutral-400"
                checked={settings.redact_prompts}
                onChange={(e) =>
                  setField("redact_prompts", e.target.checked)
                }
              />
              <Label htmlFor="redact">
                发送 prompt 给 provider 前先 redact
              </Label>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="cache_hours">
                摘要缓存时长{" "}
                <span className="font-mono">{settings.cache_hours}</span> 小时
              </Label>
              <input
                id="cache_hours"
                type="range"
                min={1}
                max={168}
                step={1}
                value={settings.cache_hours}
                onChange={(e) =>
                  setField("cache_hours", Number(e.target.value))
                }
                className="block w-full accent-neutral-900"
              />
            </div>
          </CardContent>
          <CardFooter className="justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => refreshAll()}
            >
              放弃修改
            </Button>
            <Button
              variant="default"
              size="sm"
              disabled={saving}
              onClick={() => save()}
            >
              {saving ? "保存中…" : "保存"}
            </Button>
          </CardFooter>
        </Card>

        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle>本地配置</CardTitle>
            <CardDescription>
              不是 LLM 相关的 app 配置,保存在{" "}
              <code>app_settings.json</code>(与 <code>ai_settings.json</code>{" "}
              分离,切 provider 不会影响)。
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="summary_dir">
                Markdown 摘要目录
                <span className="ml-2 font-mono text-[10px] text-neutral-500">
                  summary_dir
                </span>
              </Label>
              <div className="flex gap-2">
                <Input
                  id="summary_dir"
                  value={appSettings.summary_dir}
                  onChange={(e) =>
                    setAppSettings((prev) => ({
                      ...prev,
                      summary_dir: e.target.value,
                    }))
                  }
                  placeholder="~/Documents/agent_summaries"
                  className="font-mono text-[12px]"
                />
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => pickSummaryDir()}
                >
                  浏览…
                </Button>
              </div>
              <p className="text-[10px] leading-relaxed text-neutral-500">
                Markdown 扫描器(Stage 5-E.2b)会读这个目录下的一层{" "}
                <code>.md</code> 文件,提取 YAML frontmatter + body,
                落 <code>raw_events.event_type=markdown_summary</code>。
                目录不存在会自动创建,但不会写任何文件进去 ——
                这是你手写总结的地方。
              </p>
            </div>
          </CardContent>
          <CardFooter className="justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => refreshAll()}
            >
              放弃修改
            </Button>
            <Button
              variant="default"
              size="sm"
              disabled={appSaving}
              onClick={() => saveAppSettings()}
            >
              {appSaving ? "保存中…" : "保存"}
            </Button>
          </CardFooter>
        </Card>

        {/* v0.5.5: per-source cleanup. Each row is a registered source
            the scanner has seen (some may be 0-event / no-root_path dev
            residue); clicking 删除 opens the confirm dialog and the
            IPC cascades to raw_events / sessions_summary /
            connector_runs via schema FK. We render the full list
            rather than filtering out "empty" rows because hide =
            invisibly broken. */}
        <Card className="lg:col-span-3">
          <CardHeader>
            <CardTitle>数据源管理</CardTitle>
            <CardDescription>
              扫描器已注册的全部 <code>sources</code> 行。点击删除会清掉
              该 source 下所有原始事件与会话分区(走 FK CASCADE),不可撤销。
              通常用于清理 dev/test 留下的空行(没有 <code>root_path</code>{" "}
              且 <code>last_scan_at</code> 为空的 stale row)。
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-2">
            {sources.length === 0 && (
              <p className="text-xs text-neutral-500">加载中…</p>
            )}
            {sources.map((s) => {
              const isStale = !s.root_path && !s.last_scan_at;
              return (
                <div
                  key={s.id}
                  className="flex items-center justify-between gap-3 rounded-md border border-neutral-200 bg-white p-3"
                  data-testid={`source-row-${s.id}`}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-baseline gap-2">
                      <span className="font-mono text-[11px] text-neutral-500">
                        {s.kind}
                      </span>
                      <span className="text-sm font-medium">
                        {s.display_name}
                      </span>
                      {isStale && (
                        <Badge variant="warning" className="text-[10px]">
                          stale
                        </Badge>
                      )}
                    </div>
                    <div className="mt-1 truncate font-mono text-[10px] text-neutral-500">
                      {s.id}
                    </div>
                    <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-[10px] text-neutral-500">
                      <span>root: {s.root_path || "—"}</span>
                      <span>status: {s.status}</span>
                      <span>
                        last_scan: {s.last_scan_at?.slice(0, 19) ?? "—"}
                      </span>
                    </div>
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => setPendingDelete(s)}
                    data-testid={`source-delete-${s.id}`}
                  >
                    删除
                  </Button>
                </div>
              );
            })}
          </CardContent>
          <CardFooter className="justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => refreshSources()}
              data-testid="sources-refresh"
            >
              刷新
            </Button>
          </CardFooter>
        </Card>

        {/* Per-source confirm dialog. One dialog instance reused for
            every row — `pendingDelete` carries the row context, and
            we close it via setPendingDelete(null) on cancel / success. */}
        <Dialog
          open={pendingDelete !== null}
          onOpenChange={(open) => !open && setPendingDelete(null)}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>确认删除该数据源?</DialogTitle>
              <DialogDescription>
                即将永久删除 <code>{pendingDelete?.kind}</code>{" "}
                (<code>{pendingDelete?.id}</code>) 及其下所有{" "}
                <code>raw_events</code> / <code>sessions_summary</code>{" "}
                / <code>connector_runs</code> 行(走 FK CASCADE)。此操作不可撤销。
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPendingDelete(null)}
                disabled={deleting}
              >
                取消
              </Button>
              <Button
                variant="destructive"
                size="sm"
                disabled={deleting}
                onClick={() => confirmDeleteSource()}
                data-testid="source-delete-confirm"
              >
                {deleting ? "删除中…" : "是的,删除"}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        {/* v0.4.3: data retention. `event_retention_days` lives in
            AppSettings (saved above) and is the same knob the
            auto-rescan pass uses. The button triggers an immediate
            prune so the user can see "已清理 N 条" feedback. */}
        <Card className="lg:col-span-1">
          <CardHeader>
            <CardTitle>数据保留</CardTitle>
            <CardDescription>
              raw_events 自动清理:每次 auto-rescan 完成顺手跑一次。
              设置 0 = 永久保留(无 prune)。sessions_summary / 早报
              等聚合不受影响,重新 ETL 即可重建。
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            <div>
              <Label htmlFor="retention_days">保留天数</Label>
              <Input
                id="retention_days"
                type="number"
                min={0}
                step={1}
                value={appSettings.event_retention_days}
                onChange={(e) =>
                  setAppSettings((prev) => ({
                    ...prev,
                    // Coerce to a non-negative int — the backend
                    // treats anything <= 0 as a no-op.
                    event_retention_days: Math.max(
                      0,
                      Number.parseInt(e.target.value || "0", 10) || 0,
                    ),
                  }))
                }
                data-testid="retention-days-input"
                className="mt-1 w-32"
              />
              <p className="mt-1 text-[11px] text-neutral-500">
                默认 90 天。设置后请到上方 app 设置卡「保存」,再点
                「立即清理」可立即触发。
              </p>
            </div>
            {pruneReport && (
              <div
                className="rounded-md border border-neutral-200 bg-neutral-50 p-2 text-[11px] text-neutral-700"
                data-testid="retention-prune-report"
              >
                {pruneReport.ran ? (
                  pruneReport.events_deleted > 0 ? (
                    <>
                      已清理 <strong>{pruneReport.events_deleted}</strong>{" "}
                      条 raw_events,cutoff{" "}
                      {pruneReport.cutoff_iso.slice(0, 10)} (前{" "}
                      {pruneReport.days} 天)。
                    </>
                  ) : (
                    <>没有早于 {pruneReport.days} 天的 raw_events。</>
                  )
                ) : (
                  <>当前设为 0 (永久保留),已跳过清理。</>
                )}
              </div>
            )}
          </CardContent>
          <CardFooter className="flex justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={pruning}
              onClick={() => runPrune()}
              data-testid="retention-prune-now"
            >
              {pruning ? "清理中…" : "立即清理"}
            </Button>
          </CardFooter>
        </Card>

        <Card className="lg:col-span-1">
          <CardHeader>
            <CardTitle>Prompt 库</CardTitle>
            <CardDescription>
              内置 prompt 模板,Stage 5-E.3 Eisenhower 分类器 + Daily
              brief 都会用到。用户自定义模板走 summary_dir 扫描器。
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-2">
            {prompts.length === 0 && (
              <p className="text-xs text-neutral-500">加载中…</p>
            )}
            {prompts.map((p) => (
              <div
                key={p.id}
                className="rounded-md border border-neutral-200 bg-white p-3"
              >
                <div className="mb-1 flex items-baseline justify-between gap-2">
                  <div>
                    <div className="text-sm font-medium">{p.title}</div>
                    <div className="font-mono text-[10px] text-neutral-500">
                      {p.id}
                    </div>
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => copyPrompt(p.body)}
                  >
                    复制
                  </Button>
                </div>
                <p className="text-[11px] leading-relaxed text-neutral-600">
                  {p.description}
                </p>
                <details className="mt-2">
                  <summary className="cursor-pointer text-[10px] text-neutral-500 hover:text-neutral-700">
                    查看 prompt body
                  </summary>
                  <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded bg-neutral-50 p-2 font-mono text-[10px] leading-relaxed text-neutral-700">
                    {p.body}
                  </pre>
                </details>
              </div>
            ))}
          </CardContent>
        </Card>

        <Card className="border-red-200 lg:col-span-3">
          <CardHeader>
            <CardTitle className="text-red-700">危险操作</CardTitle>
            <CardDescription>
              不可撤销。分享 ledger 前或一次失败的实验后使用。
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col items-start justify-between gap-3 md:flex-row md:items-center">
            <p className="text-sm text-neutral-600">
              重置会删除所有 <code>raw_events</code>、所有{" "}
              <code>sessions_summary</code> 分区、以及所有
              <code> connector_runs</code> 行。已生成的 AI
              摘要会随之级联清理,下次扫描会从零开始。
            </p>
            <Dialog open={resetOpen} onOpenChange={setResetOpen}>
              <DialogTrigger asChild>
                <Button variant="destructive" size="sm">
                  重置全部事件
                </Button>
              </DialogTrigger>
              <DialogContent>
                <DialogHeader>
                  <DialogTitle>确认重置所有已采集的事件?</DialogTitle>
                  <DialogDescription>
                    此操作不可撤销。下个扫描周期 launchd 会从零开始。
                  </DialogDescription>
                </DialogHeader>
                <DialogFooter>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setResetOpen(false)}
                    disabled={resetting}
                  >
                    取消
                  </Button>
                  <Button
                    variant="destructive"
                    size="sm"
                    disabled={resetting}
                    onClick={() => resetAll()}
                  >
                    {resetting ? "重置中…" : "是的,全部重置"}
                  </Button>
                </DialogFooter>
              </DialogContent>
            </Dialog>
          </CardContent>
        </Card>
      </div>

      <p className="mt-6 text-xs text-neutral-500">
        云端 provider 在 key 缺失或账号无权访问指定模型时会返回
        401,会在状态卡片上显示为红色 badge。
      </p>
    </div>
  );
}

function Row({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="text-neutral-500">{label}</span>
      <span className={cn(mono && "font-mono text-[11px]")}>{value}</span>
    </div>
  );
}
