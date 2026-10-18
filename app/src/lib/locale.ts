// Display-name mapping for opaque enum-like strings that come from
// the Rust backend. We translate only the user-facing labels; the
// underlying identifiers (used in API calls, badges, log lines) stay
// in English to keep the round-trip lossless.

const PROVIDER_LABEL: Record<string, string> = {
  local: "本地 Ollama",
  openai: "OpenAI",
  anthropic: "Anthropic",
  heuristic: "本地启发式",
};

const SESSION_KIND_LABEL: Record<string, string> = {
  day: "日报",
  week: "周报",
};

const IMPORTANCE_LABEL: Record<string, string> = {
  Critical: "关键",
  High: "高",
  Medium: "中",
  Low: "低",
};

const STATUS_LABEL: Record<string, string> = {
  // 通用 UI 状态
  installed: "已安装",
  ok: "正常",
  failed: "失败",
  "not installed": "未安装",
  "partially failed": "部分失败",
  // source 可读性
  readable: "可读",
  discovered: "已发现",
  unreachable: "不可达",
  // plan item 状态
  todo: "待办",
  in_progress: "进行中",
  done: "已完成",
  // connector 状态
  supported: "已支持",
  planned: "规划中",
};

const QUADRANT_LABEL: Record<number, { title: string; subtitle: string }> = {
  1: { title: "立即去做", subtitle: "重要 · 紧急" },
  2: { title: "安排计划", subtitle: "重要 · 不紧急" },
  3: { title: "授权 / 速胜", subtitle: "紧急 · 不重要" },
  4: { title: "舍弃 / 暂存", subtitle: "既不紧急也不重要" },
};

export function providerLabel(id: string): string {
  return PROVIDER_LABEL[id] ?? id;
}

// Stage 5.8 / v0.4.0: quick presets for the OpenAI-compatible
// `cloud_endpoint` field. The Settings page renders these as
// one-click chips; selecting a chip overwrites the cloud_endpoint
// and cloud_model fields with the values below. All presets use
// the standard `/v1/chat/completions` shape (every entry is
// OpenAI-compatible), so the existing `Provider::OpenaiCompat`
// request path works unchanged.
//
// To add a new preset: append one entry here. The Settings UI
// picks it up automatically; no Rust change required.
export type LlmPreset = {
  id: string;
  label: string;
  base_url: string;
  default_model: string;
  hint: string;
};

export const LLM_PRESETS: LlmPreset[] = [
  {
    id: "deepseek",
    label: "DeepSeek",
    base_url: "https://api.deepseek.com/v1",
    default_model: "deepseek-chat",
    hint: "深度求索 — 中文强 / 价格低 / 推理模型 deepseek-reasoner 也可",
  },
  {
    id: "moonshot",
    label: "Moonshot Kimi",
    base_url: "https://api.moonshot.cn/v1",
    default_model: "moonshot-v1-128k",
    hint: "月之暗面 Kimi — 长上下文(128K 起)",
  },
  {
    id: "qwen",
    label: "Qwen DashScope",
    base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    default_model: "qwen-plus",
    hint: "阿里通义千问 — OpenAI 兼容模式",
  },
  {
    id: "doubao",
    label: "Doubao Ark",
    base_url: "https://ark.cn-beijing.volces.com/api/v3",
    default_model: "doubao-pro-32k",
    hint: "字节豆包 Ark — 注意路径是 /api/v3,模型用 endpoint 分配的 ID",
  },
  {
    id: "glm",
    label: "GLM 智谱",
    base_url: "https://open.bigmodel.cn/api/paas/v4",
    default_model: "glm-4-plus",
    hint: "智谱 GLM-4 — 路径 /api/paas/v4",
  },
  {
    id: "MiniMax",
    label: "MiniMax",
    base_url: "https://api.MiniMax.chat/v1",
    default_model: "MiniMax-Text-01",
    hint: "MiniMax — 国内版 endpoint;海外用 api.MiniMax.com",
  },
  {
    id: "openai",
    label: "OpenAI (官方)",
    base_url: "https://api.openai.com/v1",
    default_model: "gpt-4o-mini",
    hint: "OpenAI 官方 API",
  },
];

export function sessionKindLabel(id: string): string {
  return SESSION_KIND_LABEL[id] ?? id;
}

export function importanceLabelEN(id: string): string {
  return IMPORTANCE_LABEL[id] ?? id;
}

export function statusLabel(id: string): string {
  return STATUS_LABEL[id] ?? id;
}

export function quadrantTitle(q: number): string {
  return QUADRANT_LABEL[q]?.title ?? `Q${q}`;
}

export function quadrantSubtitle(q: number): string {
  return QUADRANT_LABEL[q]?.subtitle ?? "";
}

export function planStatusLabel(status: string): string {
  return STATUS_LABEL[status] ?? status;
}
