import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatRelativeTime(iso: string): string {
  const ms = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(ms)) return iso;
  const min = 60_000;
  const hour = 60 * min;
  const day = 24 * hour;
  if (ms < min) return "just now";
  if (ms < hour) return `${Math.round(ms / min)}m ago`;
  if (ms < day) return `${Math.round(ms / hour)}h ago`;
  if (ms < 7 * day) return `${Math.round(ms / day)}d ago`;
  return new Date(iso).toLocaleDateString();
}

export function formatDurationMs(ms: number): string {
  if (ms < 60_000) return `${Math.round(ms / 1000)}s`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m`;
  return `${(ms / 3_600_000).toFixed(1)}h`;
}

export function importanceClass(importance: number): string {
  if (importance >= 8) return "bg-red-100 text-red-800 border-red-300";
  if (importance >= 5) return "bg-amber-100 text-amber-800 border-amber-300";
  if (importance >= 3) return "bg-sky-100 text-sky-800 border-sky-300";
  return "bg-neutral-100 text-neutral-600 border-neutral-300";
}

export function importanceLabel(importance: number): string {
  if (importance >= 8) return "Critical";
  if (importance >= 5) return "High";
  if (importance >= 3) return "Medium";
  return "Low";
}

// Stage 5.6b / v0.3.6: per-source badge colour. We hash the source
// label into one of eight tailwind hue families so every connector
// the user has installed shows up in a distinct, stable colour the
// moment they appear. `sourceLabel` is whatever string the connector
// surfaced in `source_display_name` (e.g. "Codex", "OpenCode", a
// user-supplied name for a custom path, etc.). The hard-coded map
// gives known connectors a colour that matches the brand feel;
// unknown names fall back to a deterministic hash.
const KNOWN_SOURCE_BG: Record<string, string> = {
  codex: "bg-indigo-100 text-indigo-800 border-indigo-300",
  opencode: "bg-emerald-100 text-emerald-800 border-emerald-300",
  claude_code: "bg-orange-100 text-orange-800 border-orange-300",
  workbuddy: "bg-pink-100 text-pink-800 border-pink-300",
  doubao: "bg-sky-100 text-sky-800 border-sky-300",
  doubao_work: "bg-sky-100 text-sky-800 border-sky-300",
  trae: "bg-cyan-100 text-cyan-800 border-cyan-300",
  minimax_code: "bg-violet-100 text-violet-800 border-violet-300",
  mavis: "bg-violet-100 text-violet-800 border-violet-300",
  markdown_summary: "bg-slate-100 text-slate-700 border-slate-300",
};

const HASH_FALLBACK_HUES = [
  "bg-indigo-100 text-indigo-800 border-indigo-300",
  "bg-emerald-100 text-emerald-800 border-emerald-300",
  "bg-orange-100 text-orange-800 border-orange-300",
  "bg-pink-100 text-pink-800 border-pink-300",
  "bg-sky-100 text-sky-800 border-sky-300",
  "bg-cyan-100 text-cyan-800 border-cyan-300",
  "bg-violet-100 text-violet-800 border-violet-300",
  "bg-amber-100 text-amber-800 border-amber-300",
];

/** djb2 — small, fast, stable across sessions. */
function djb2(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  }
  return h;
}

/** Returns the tailwind class set for a source label. */
export function sourceColorClass(sourceLabel: string): string {
  const key = sourceLabel.trim().toLowerCase();
  if (!key) return HASH_FALLBACK_HUES[0];
  const exact = KNOWN_SOURCE_BG[key];
  if (exact) return exact;
  // Allow loose prefix matches so e.g. "Codex (Remote)" still gets
  // a sensible brand colour.
  for (const [name, cls] of Object.entries(KNOWN_SOURCE_BG)) {
    if (key.startsWith(name) || name.startsWith(key)) return cls;
  }
  return HASH_FALLBACK_HUES[djb2(key) % HASH_FALLBACK_HUES.length];
}