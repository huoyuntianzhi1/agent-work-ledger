//! Connector trait — implemented by every per-agent data source.
//!
//! Each connector owns one row of the `sources` table and walks its own local
//! data shape (jsonl / sqlite / leveldb / etc.) into the shared
//! `raw_events` / `sessions` / `connector_runs` rows.

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod doubao;
pub mod markdown_summary;
pub mod minimax;
pub mod opencode;
pub mod trae;
pub mod trae_solo;
pub mod workbuddy;

#[cfg(test)]
pub(crate) mod test_locks {
    //! Process-wide mutexes used to serialize tests that mutate global
    //! state — most importantly the `HOME` env var that `dirs::home_dir()`
    //! reads on macOS/Linux. Each per-module `static` would be invisible
    //! to the other module's test, so we centralize the lock here so the
    //! trae and markdown_summary tests *actually* serialize when `cargo
    //! test` runs them in parallel.
    use std::sync::Mutex;
    pub(crate) static HOME_LOCK: Mutex<()> = Mutex::new(());
}

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use rusqlite::Connection;
use serde_json::Value as Json;

/// Stable string identifiers used in the `sources.kind` column. Re-using
/// these strings across versions keeps the registry stable.
pub mod kinds {
    pub const CLAUDE_CODE: &str = "claude_code";
    pub const CODEX: &str = "codex";
    pub const CURSOR: &str = "cursor";
    pub const OPENCODE: &str = "opencode";
    pub const WORKBUDDY: &str = "workbuddy";
    pub const CODEBUDDY_CN: &str = "codebuddy_cn";
    pub const DOUBAO: &str = "doubao";
    pub const DOUBAO_WORK: &str = "doubao_work";
    pub const MINIMAX_CODE: &str = "minimax_code";
    pub const TRAE: &str = "trae";
    pub const TRAE_SOLO: &str = "trae_solo";
    pub const MARKDOWN_SUMMARY: &str = "markdown_summary";
}

/// Result envelope returned to the UI after a connector run.
#[derive(Debug, Serialize, Deserialize)]
pub struct ScanReport {
    pub source_id: String,
    pub kind: String,
    pub status: String, // "ok" | "partial" | "failed"
    pub events_inserted: i64,
    /// Number of `sessions_summary` rows newly inserted by ETL on this
    /// run. In v0.2 the connector surface itself does not run ETL — the
    /// SessionsSummary writer hooks into the same scan but counts the
    /// net-new rows it produced; older builds of the ETL wrote 0 here.
    pub sessions_added: i64,
    pub files_scanned: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
}

/// Pure-data input passed to `Connector::scan`. Holding these makes the
/// trait easy to test without dragging in the full Tauri state.
pub struct ScanCtx<'a> {
    pub conn: &'a mut Connection,
    pub source_id: &'a str,
    pub since_ms: Option<i64>,
}

/// Trait every concrete connector must implement.
pub trait Connector: Send + Sync {
    /// Stable kind string, must match a `kinds::*` constant.
    fn kind(&self) -> &'static str;
    /// Human-readable name, shown in the Connectors page.
    fn display_name(&self) -> &'static str;
    /// Resolve one or more candidate roots on disk, return only those that
    /// actually exist. The first valid root becomes `root_path`.
    fn discover_roots(&self) -> Vec<std::path::PathBuf>;
    /// Run a scan, inserting raw_events / sessions / connector_runs rows.
    fn scan(&self, ctx: ScanCtx<'_>) -> AppResult<ScanReport>;
}

/// Helper for connectors: derive a UTC ISO-8601 timestamp at "now" in ms.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Format a millisecond Unix timestamp as RFC3339, or empty string when null.
pub fn rfc3339(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Validate that `v` is one of the recognised agent kinds.
pub fn validate_kind(v: &Json) -> AppResult<()> {
    let s = v
        .as_str()
        .ok_or_else(|| AppError::Connector("kind".into(), "expected string".into()))?;
    if ![
        kinds::CLAUDE_CODE,
        kinds::CODEX,
        kinds::CURSOR,
        kinds::OPENCODE,
        kinds::WORKBUDDY,
        kinds::CODEBUDDY_CN,
        kinds::DOUBAO,
        kinds::DOUBAO_WORK,
        kinds::MINIMAX_CODE,
        kinds::TRAE,
        kinds::TRAE_SOLO,
        kinds::MARKDOWN_SUMMARY,
    ]
    .contains(&s)
    {
        return Err(AppError::UnknownConnector(s.to_string()));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Since-filter helpers (Stage 4-D.1)
//
// Every concrete connector gets a `since_ms: Option<i64>` through
// `ScanCtx`. The scanner-side UI passes it so the user can scope a scan
// to "today's work" without re-ingesting 5 months of history on every
// click. Connectors that have a per-event timestamp (Codex / Claude /
// OpenCode / WorkBuddy) honor the filter at the insert boundary; the
// Chromium IDB connectors (Doubao / MiniMax Agent) don't yet parse V8
// value timestamps, so they accept the parameter for API symmetry and
// emit all rows — the UI never crashes, but they scan the full root
// regardless. Unparseable timestamps fall through (kept) rather than
// being silently dropped, since the alternative is losing data the
// user explicitly asked to ingest.
// ---------------------------------------------------------------------
pub fn should_skip_iso(since_ms: Option<i64>, iso_ts: &str) -> bool {
    let Some(since) = since_ms else {
        return false;
    };
    match iso_to_ms(iso_ts) {
        Some(ts) => ts < since,
        None => false,
    }
}

pub fn should_skip_ms(since_ms: Option<i64>, ts_ms: i64) -> bool {
    matches!(since_ms, Some(s) if ts_ms < s)
}

fn iso_to_ms(iso: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Build the per-event filter for paths shaped like
/// `<root>/sessions/YYYY/MM/DD/...`. Returns `Ok(false)` when the path
/// doesn't match that shape so the connector falls back to per-row
/// timestamp checks. Used by Codex to short-circuit whole day
/// directories without paying the zstd decode + parse cost.
pub fn path_day_dir_before(since_ms: Option<i64>, path: &std::path::Path) -> bool {
    let Some(since) = since_ms else {
        return false;
    };
    // Walk up looking for components that parse as 4-digit / 2-digit /
    // 2-digit numbers, in that order. Matches `sessions/2026/05/12/...`.
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect();
    let mut year_idx: Option<usize> = None;
    for (i, c) in comps.iter().enumerate() {
        if c.len() == 4 && c.chars().all(|ch| ch.is_ascii_digit()) {
            year_idx = Some(i);
            break;
        }
    }
    let Some(yi) = year_idx else {
        return false;
    };
    if yi + 2 >= comps.len() {
        return false;
    }
    let year: i32 = match comps[yi].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let month: u32 = match comps[yi + 1].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let day: u32 = match comps[yi + 2].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let dt = match chrono::NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
    {
        Some(t) => t,
        None => return false,
    };
    let end_of_day_ms = dt.and_utc().timestamp_millis() + 86_400_000; // exclusive of next day
    end_of_day_ms <= since
}
