//! `awl-ipc` — Agent Work Ledger Tauri 2 IPC commands.
//!
//! Thin handlers over `awl-core` / `awl-db` / `awl-export`. Every
//! `#[tauri::command]` lives here so the binary crate (`awl-desktop`)
//! stays focused on the window / tray lifecycle. Commands stay private
//! (the `tauri::command` proc-macro clashes with `pub fn`); use
//! `register(builder)` to wire them into a Tauri builder from outside.

use awl_db::open_db;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use awl_core::connectors::claude::ClaudeConnector;
use awl_core::connectors::codex::CodexConnector;
use awl_core::connectors::cursor::CursorConnector;
use awl_core::connectors::markdown_summary::MarkdownSummaryConnector;
use awl_core::connectors::opencode::OpenCodeConnector;
use awl_core::connectors::trae::TraeMetadataConnector;
use awl_core::connectors::workbuddy::WorkBuddyConnector;
use awl_core::connectors::{Connector, ScanReport};
use awl_core::error::{AppError, AppResult};
use awl_core::app_settings::{AppSettings, PromptTemplate};
use awl_core::{ai, eisenhower, reports, summary};
use awl_db::retention;

/// How often the in-app background scanner re-runs every supported
/// connector while the app is running. We deliberately keep this off the
/// hot loop: every connector also runs once on dev startup (see
/// `auto_rescan_all`), and a single Codex scan already takes ~2s.
const BACKGROUND_SCAN_INTERVAL_SECS: u64 = 300;
/// Menu item id for "Show window".
const MENU_ID_SHOW: &str = "show";
/// Menu item id for "Scan now" — runs an immediate `auto_rescan_all`.
const MENU_ID_SCAN_NOW: &str = "scan_now";
/// Menu item id for "Quit" — fully tears the app down.
const MENU_ID_QUIT: &str = "quit";


/// Snapshot of the app's health, returned by `get_health`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub db_path: String,
    pub schema_version: i64,
    pub ts: String,
}

/// Summary of a registered source for the Connectors page.
#[derive(Debug, Serialize, Deserialize)]
pub struct SourceRow {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub root_path: String,
    pub status: String,
    pub last_scan_at: Option<String>,
}

/// One row from `raw_events` for the timeline preview.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventRow {
    pub id: String,
    pub source_id: String,
    pub source_kind: String,
    pub source_display_name: String,
    pub event_type: String,
    pub occurred_at: String,
    pub project_key: String,
    pub payload_preview: String,
}

/// One `sessions_summary` partition, joined with the source's display
/// name for the Sessions page. Returned by `list_sessions_summary`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionPartitionRow {
    pub id: String,
    pub source_id: String,
    pub source_display_name: String,
    pub started_at: String,
    pub ended_at: String,
    pub event_count: i64,
    pub project_key: Option<String>,
    /// Pre-LLM one-line hint synthesised by the ETL pass (used as the
    /// card subtitle when no AI summary has been requested yet).
    pub summary: String,
    pub importance: i64,
    pub heuristic_kind: String,
}

/// Shared application state. Wrapped in an `Arc<Mutex>` so the background
/// auto-rescan task on dev startup can hold its own clone alongside the
/// `tauri::State` borrow used by IPC commands — both sides serialise through
/// the same mutex.
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub db_path: PathBuf,
}

/// Resolve the per-user data directory for this app, creating it if missing.
/// `awl_db::resolve_data_dir` owns the canonical 4-line implementation;
/// we re-export it here so IPC commands can stay in the `awl-ipc`
/// namespace without a separate import per call site.
pub use awl_db::resolve_data_dir;

#[tauri::command]
fn get_health(state: State<'_, AppState>) -> AppResult<Health> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let version: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "0".to_string());

    Ok(Health {
        ok: true,
        db_path: state.db_path.display().to_string(),
        schema_version: version.parse().unwrap_or(0),
        ts: awl_core::connectors::rfc3339(Some(awl_core::connectors::now_ms())),
    })
}

/// Return every registered source for the Connectors page.
#[tauri::command]
fn list_sources(state: State<'_, AppState>) -> AppResult<Vec<SourceRow>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let mut stmt = conn.prepare(
        "SELECT id, kind, display_name, root_path, status, last_scan_at FROM sources ORDER BY kind",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SourceRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                display_name: row.get(2)?,
                root_path: row.get(3)?,
                status: row.get(4)?,
                last_scan_at: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// v0.5.5: delete a single source row by id. The schema's FK CASCADE
/// (raw_events / sessions_summary / connector_runs) cleans up child
/// rows automatically; `plan_items.source_session_id` cascades through
/// the deleted sessions_summary. Returns the number of `sources` rows
/// actually deleted (0 if the id was unknown so the UI can show
/// "已删除" / "找不到数据源" distinctly).
///
/// Manual user-driven cleanup only — we never auto-prune empty
/// sources. The Settings page shows every row, dev/test residue
/// included, and lets the user decide. The Connectors scanner never
/// recreates a row whose `id` was user-deleted unless a future scan
/// re-discovers it under the same `kind` + `root_path` fingerprint.
#[tauri::command]
fn delete_source(state: State<'_, AppState>, id: String) -> AppResult<usize> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let deleted = conn.execute("DELETE FROM sources WHERE id = ?1", params![id])?;
    Ok(deleted)
}

/// Most recent events across all sources, ordered by **ingest time**
/// (`created_at DESC`) so the user sees the most recently captured work
/// regardless of when the underlying agent actually emitted it.
///
/// Stage 1-G+1: we balance across sources — each registered connector
/// contributes its most recent `PER_SOURCE_CAP` events, then the merged
/// result is re-sorted by `created_at DESC`. This stops a single chatty
/// source (e.g. OpenCode dumping 3000+ session rows in one scan) from
/// drowning out the others inside the home-screen preview.
///
/// `limit` is clamped between 1 and 1000 by the frontend; we also defend
/// here. Note that with `PER_SOURCE_CAP=20` and 4 active sources the
/// outer `LIMIT ?1` is rarely the binding constraint — it still acts as a
/// hard ceiling for callers who ask for `limit=10` and want a tight
/// preview.
const PER_SOURCE_CAP: i64 = 20;

#[tauri::command]
fn list_recent_events(state: State<'_, AppState>, limit: i64) -> AppResult<Vec<EventRow>> {
    let limit = limit.clamp(1, 1000);
    let conn = state.db.lock().expect("db mutex poisoned");
    let mut stmt = conn.prepare(
        "WITH per_source AS (
             SELECT e.id, e.source_id, s.kind, s.display_name, e.event_type,
                    e.occurred_at, e.project_key, e.payload, e.created_at,
                    ROW_NUMBER() OVER (PARTITION BY s.kind ORDER BY e.created_at DESC) AS rn
             FROM raw_events e JOIN sources s ON s.id = e.source_id
         )
         SELECT id, source_id, kind, display_name, event_type, occurred_at,
                COALESCE(project_key, ''), substr(payload, 1, 240)
         FROM per_source
         WHERE rn <= ?2
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit, PER_SOURCE_CAP], |row| {
            Ok(EventRow {
                id: row.get(0)?,
                source_id: row.get(1)?,
                source_kind: row.get(2)?,
                source_display_name: row.get(3)?,
                event_type: row.get(4)?,
                occurred_at: row.get(5)?,
                project_key: row.get(6)?,
                payload_preview: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Filter for `list_timeline_events`. Any `None` field is treated as "all".
#[derive(Debug, Default, Deserialize)]
pub struct TimelineFilter {
    /// Limit by `kind` values (e.g. `["codex", "claude_code"]`).
    #[serde(default)]
    pub source_kinds: Option<Vec<String>>,
    /// Limit by exact `event_type` values.
    #[serde(default)]
    pub event_types: Option<Vec<String>>,
    /// Match `project_key` containing this substring.
    #[serde(default)]
    pub project_contains: Option<String>,
    /// Only events with `occurred_at >= since_iso`. ISO-8601 string.
    #[serde(default)]
    pub since_iso: Option<String>,
    /// Only events with `occurred_at <= until_iso`. ISO-8601 string.
    #[serde(default)]
    pub until_iso: Option<String>,
    /// Max rows to return. Clamped 1..=2000 by the backend.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Sort direction: `"asc"` or `"desc"`. Defaults to desc (newest first).
    #[serde(default)]
    pub order: Option<String>,
}

#[tauri::command]
fn list_timeline_events(
    state: State<'_, AppState>,
    filter: TimelineFilter,
) -> AppResult<Vec<EventRow>> {
    let limit = filter.limit.unwrap_or(200).clamp(1, 2000);
    let order = filter.order.as_deref().unwrap_or("desc");
    if !matches!(order, "asc" | "desc") {
        return Err(AppError::Connector(
            "timeline.order".into(),
            format!("must be 'asc' or 'desc', got {order:?}"),
        ));
    }
    let conn = state.db.lock().expect("db mutex poisoned");

    // Build SQL dynamically. We always include the JOIN on sources so
    // `source_kind` / `source_display_name` are always populated.
    let mut sql = String::from(
        "SELECT e.id, e.source_id, s.kind, s.display_name, e.event_type, e.occurred_at, COALESCE(e.project_key, ''), substr(e.payload, 1, 240)
         FROM raw_events e JOIN sources s ON s.id = e.source_id WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut next_param = |sql: &mut String, prefix: &str, value: String, args: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        sql.push_str(&format!(" {prefix} ?{}", args.len() + 1));
        args.push(Box::new(value));
    };

    if let Some(ref kinds) = filter.source_kinds {
        if !kinds.is_empty() {
            let placeholders: Vec<String> = (0..kinds.len())
                .map(|i| format!("?{}", args.len() + 1 + i))
                .collect();
            sql.push_str(&format!(" AND s.kind IN ({})", placeholders.join(",")));
            for k in kinds {
                args.push(Box::new(k.clone()));
            }
        }
    }
    if let Some(ref ets) = filter.event_types {
        if !ets.is_empty() {
            let placeholders: Vec<String> = (0..ets.len())
                .map(|i| format!("?{}", args.len() + 1 + i))
                .collect();
            sql.push_str(&format!(" AND e.event_type IN ({})", placeholders.join(",")));
            for e in ets {
                args.push(Box::new(e.clone()));
            }
        }
    }
    if let Some(ref p) = filter.project_contains {
        if !p.is_empty() {
            next_param(&mut sql, "AND e.project_key LIKE", format!("%{p}%"), &mut args);
        }
    }
    if let Some(ref s) = filter.since_iso {
        if !s.is_empty() {
            next_param(&mut sql, "AND e.occurred_at >=", s.clone(), &mut args);
        }
    }
    if let Some(ref u) = filter.until_iso {
        if !u.is_empty() {
            next_param(&mut sql, "AND e.occurred_at <=", u.clone(), &mut args);
        }
    }
    sql.push_str(&format!(" ORDER BY e.occurred_at {} LIMIT ?{}", order.to_uppercase(), args.len() + 1));
    args.push(Box::new(limit));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(EventRow {
                id: row.get(0)?,
                source_id: row.get(1)?,
                source_kind: row.get(2)?,
                source_display_name: row.get(3)?,
                event_type: row.get(4)?,
                occurred_at: row.get(5)?,
                project_key: row.get(6)?,
                payload_preview: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Static metadata describing what the connector registry *plans* to
/// support vs. what's currently wired up. Used by the UI to render a
/// supported / unsupported badge.
#[derive(Debug, Serialize)]
pub struct SupportedConnector {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub status: &'static str, // "supported" | "planned"
    pub notes: &'static str,
}

#[tauri::command]
fn list_supported_connectors() -> AppResult<Vec<SupportedConnector>> {
    Ok(vec![
        SupportedConnector {
            kind: "codex",
            display_name: "OpenAI Codex",
            status: "supported",
            notes: "Reads zstd-compressed JSONL + 6 SQLite buckets from ~/.codex/",
        },
        SupportedConnector {
            kind: "claude_code",
            display_name: "Claude Code",
            status: "supported",
            notes: "Reads plaintext JSONL transcripts from ~/.claude/projects/",
        },
        SupportedConnector {
            kind: "opencode",
            display_name: "OpenCode",
            status: "supported",
            notes: "Read-only SQLite from ~/.local/share/opencode/opencode.db",
        },
        SupportedConnector {
            kind: "workbuddy",
            display_name: "WorkBuddy",
            status: "supported",
            notes: "check-point + plan-task meta.json under ~/Library/Application Support/WorkBuddyExtension/",
        },
        SupportedConnector {
            kind: "doubao",
            display_name: "Doubao (豆包)",
            status: "supported",
            notes: "Chromium IndexedDB across 11 Default/Profile 1-3 roots; emits session_meta per record.",
        },
        SupportedConnector {
            kind: "doubao_work",
            display_name: "Doubao Work (豆包工作)",
            status: "supported",
            notes: "Same Chromium IDB layout as Doubao, separate scope.",
        },
        SupportedConnector {
            kind: "minimax_code",
            display_name: "MiniMax Agent",
            status: "supported",
            notes: "Chromium IndexedDB at app_._0.indexeddb.leveldb; telemetry payloads (FCM/heartbeat/fire-core).",
        },
        SupportedConnector {
            kind: "trae",
            display_name: "Trae",
            status: "supported",
            notes: "Plaintext JSON / state.vscdb ItemTable keys (ChatStore, input-history, memento/icube-ai-agent-storage). Encrypted ai-agent/database.db is left for Stage 6.",
        },
        SupportedConnector {
            kind: "markdown_summary",
            display_name: "Markdown Summary",
            status: "supported",
            notes: "Walks the user-configured summary_dir (Settings → 本地配置) one level deep for .md files, parses YAML frontmatter, emits markdown_summary events.",
        },
        SupportedConnector {
            kind: "codebuddy_cn",
            display_name: "CodeBuddy CN",
            status: "planned",
            notes: "codebuddy-sessions.vscdb (SQLite) exists but ItemTable is empty (0 rows).",
        },
    ])
}

/// Render the supplied event rows as a Markdown timeline and write them
/// to `path`. Returns the absolute path written for confirmation.
#[tauri::command]
fn export_markdown(
    state: State<'_, AppState>,
    filter: TimelineFilter,
    path: String,
) -> AppResult<String> {
    // Reuse `list_timeline_events` so the export mirrors what the user
    // sees on screen — same filter, same ordering, same limit.
    let conn = state.db.lock().expect("db mutex poisoned");
    let rows = list_timeline_events_inner(&conn, &filter)?;
    drop(conn);

    let mut md = String::new();
    md.push_str("# Agent Work Ledger — Timeline\n\n");
    md.push_str(&format!(
        "- generated_at: {}\n- row_count: {}\n",
        awl_core::connectors::rfc3339(Some(awl_core::connectors::now_ms())),
        rows.len()
    ));
    if let Some(ref kinds) = filter.source_kinds {
        if !kinds.is_empty() {
            md.push_str(&format!("- source_kinds: {}\n", kinds.join(", ")));
        }
    }
    if let Some(ref ets) = filter.event_types {
        if !ets.is_empty() {
            md.push_str(&format!("- event_types: {}\n", ets.join(", ")));
        }
    }
    if let Some(ref p) = filter.project_contains {
        if !p.is_empty() {
            md.push_str(&format!("- project_contains: {p}\n"));
        }
    }
    md.push_str("\n");

    let mut last_day = String::new();
    for row in &rows {
        let day = row.occurred_at.get(..10).unwrap_or("").to_string();
        if day != last_day {
            if !last_day.is_empty() {
                md.push_str("\n");
            }
            md.push_str(&format!("## {day}\n\n"));
            last_day = day;
        }
        md.push_str(&format!(
            "### {ts} — {kind} · {etype}\n\n- project: `{project}`\n- source: {display}\n\n```\n{payload}\n```\n\n",
            ts = row.occurred_at,
            kind = row.source_kind,
            etype = row.event_type,
            project = row.project_key,
            display = row.source_display_name,
            payload = row.payload_preview,
        ));
    }

    std::fs::write(&path, md.as_bytes())?;
    Ok(path)
}

/// Internal helper shared by `list_timeline_events` and `export_markdown`.
/// Accepts the same `TimelineFilter` and runs the same SQL. Returns rows
/// without re-locking the connection.
fn list_timeline_events_inner(
    conn: &Connection,
    filter: &TimelineFilter,
) -> AppResult<Vec<EventRow>> {
    let limit = filter.limit.unwrap_or(200).clamp(1, 2000);
    let order = filter.order.as_deref().unwrap_or("desc");
    if !matches!(order, "asc" | "desc") {
        return Err(AppError::Connector(
            "timeline.order".into(),
            format!("must be 'asc' or 'desc', got {order:?}"),
        ));
    }

    let mut sql = String::from(
        "SELECT e.id, e.source_id, s.kind, s.display_name, e.event_type, e.occurred_at, COALESCE(e.project_key, ''), substr(e.payload, 1, 240)
         FROM raw_events e JOIN sources s ON s.id = e.source_id WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut next_param = |sql: &mut String, prefix: &str, value: String, args: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        sql.push_str(&format!(" {prefix} ?{}", args.len() + 1));
        args.push(Box::new(value));
    };

    if let Some(ref kinds) = filter.source_kinds {
        if !kinds.is_empty() {
            let placeholders: Vec<String> = (0..kinds.len())
                .map(|i| format!("?{}", args.len() + 1 + i))
                .collect();
            sql.push_str(&format!(" AND s.kind IN ({})", placeholders.join(",")));
            for k in kinds {
                args.push(Box::new(k.clone()));
            }
        }
    }
    if let Some(ref ets) = filter.event_types {
        if !ets.is_empty() {
            let placeholders: Vec<String> = (0..ets.len())
                .map(|i| format!("?{}", args.len() + 1 + i))
                .collect();
            sql.push_str(&format!(" AND e.event_type IN ({})", placeholders.join(",")));
            for e in ets {
                args.push(Box::new(e.clone()));
            }
        }
    }
    if let Some(ref p) = filter.project_contains {
        if !p.is_empty() {
            next_param(&mut sql, "AND e.project_key LIKE", format!("%{p}%"), &mut args);
        }
    }
    if let Some(ref s) = filter.since_iso {
        if !s.is_empty() {
            next_param(&mut sql, "AND e.occurred_at >=", s.clone(), &mut args);
        }
    }
    if let Some(ref u) = filter.until_iso {
        if !u.is_empty() {
            next_param(&mut sql, "AND e.occurred_at <=", u.clone(), &mut args);
        }
    }
    sql.push_str(&format!(" ORDER BY e.occurred_at {} LIMIT ?{}", order.to_uppercase(), args.len() + 1));
    args.push(Box::new(limit));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(EventRow {
                id: row.get(0)?,
                source_id: row.get(1)?,
                source_kind: row.get(2)?,
                source_display_name: row.get(3)?,
                event_type: row.get(4)?,
                occurred_at: row.get(5)?,
                project_key: row.get(6)?,
                payload_preview: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Wrap a connector scan with the v0.2 ETL pass. The connector writes
/// `raw_events`; we then call `etl::reconcile_sessions_summary` to rebuild
/// that source's `sessions_summary` partitions and stamp the count onto
/// `ScanReport.sessions_added`.
///
/// A failed ETL is logged but does *not* fail the scan — the raw events
/// are already in the DB, so the user can trigger a manual
/// `rebuild_sessions_summary` later.
fn run_connector_then_etl<F>(
    conn: &mut rusqlite::Connection,
    run: F,
) -> AppResult<ScanReport>
where
    F: FnOnce(&mut rusqlite::Connection) -> AppResult<ScanReport>,
{
    let mut report = run(conn)?;
    if !report.source_id.is_empty() && report.status != "failed" {
        match awl_core::etl::reconcile_sessions_summary(conn, &report.source_id) {
            Ok(n) => report.sessions_added = n,
            Err(e) => tracing::warn!(
                source_id = %report.source_id,
                error = %e,
                "etl: reconcile failed; raw events remain but sessions_summary may be stale",
            ),
        }
    }
    Ok(report)
}

/// Top-level ETL pass: rebuild every source's `sessions_summary` partition
/// using whatever `raw_events` are currently in the DB. Returns the
/// number of partitions written (across all sources). Use after a manual
/// bulk import or if you suspect an ETL drift.
#[derive(Debug, Serialize, Deserialize)]
pub struct EtlReport {
    pub partitions_total: i64,
    pub sources_rebuilt: i64,
    pub duration_ms: i64,
}

#[tauri::command]
fn rebuild_sessions_summary(state: State<'_, AppState>) -> AppResult<EtlReport> {
    let started = std::time::Instant::now();
    let conn = state.db.lock().expect("db mutex poisoned");
    let mut sources_rebuilt: i64 = 0;
    let mut partitions_total: i64 = 0;

    let mut stmt = conn.prepare("SELECT id FROM sources ORDER BY id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let source_ids: Vec<String> = rows.filter_map(|r| r.ok()).collect();

    for sid in &source_ids {
        match awl_core::etl::reconcile_sessions_summary(&conn, sid) {
            Ok(n) => {
                partitions_total += n;
                sources_rebuilt += 1;
            }
            Err(e) => tracing::warn!(
                source_id = %sid,
                error = %e,
                "etl: rebuild failed for source"
            ),
        }
    }

    Ok(EtlReport {
        partitions_total,
        sources_rebuilt,
        duration_ms: started.elapsed().as_millis() as i64,
    })
}

/// Most recent ETL partitions for the Sessions page. Joins on
/// `sources.display_name` so the UI can render `Codex / claude_code /
/// minimax` style labels without a second round-trip. Capped at 200 to
/// keep the payload bounded; the page is paginated manually.
#[tauri::command]
fn list_sessions_summary(state: State<'_, AppState>) -> AppResult<Vec<SessionPartitionRow>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    Ok(list_sessions_summary_inner(&conn))
}

/// Inner helper shared between the IPC wrapper and integration tests, so
/// tests don't have to drive the full `AppState` boilerplate.
pub fn list_sessions_summary_inner(conn: &Connection) -> Vec<SessionPartitionRow> {
    let mut stmt = match conn.prepare(
        "SELECT s.id, s.source_id, COALESCE(src.display_name, s.source_id),
                s.started_at, s.ended_at, s.event_count,
                s.project_key, s.summary, s.importance, s.heuristic_kind
         FROM sessions_summary s
         LEFT JOIN sources src ON src.id = s.source_id
         ORDER BY s.started_at DESC
         LIMIT 200",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
            Ok(SessionPartitionRow {
                id: row.get(0)?,
                source_id: row.get(1)?,
                source_display_name: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
                event_count: row.get(5)?,
                project_key: row.get(6)?,
                summary: row.get(7)?,
                importance: row.get(8)?,
                heuristic_kind: row.get(9)?,
            })
        })
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Run a single connector scan synchronously. Returns the `ScanReport` even
/// if the connector reports partial / failed status — only hard errors bubble
/// up to the UI.
///
/// `since_ms` (Stage 4-D) lets the user scope a manual scan to a single
/// point in time — e.g. "scan Codex since yesterday 09:00" without
/// re-ingesting 5 months of history. The launchd background scan and
/// `auto_rescan_all` always pass `None` because they want the full window.
#[tauri::command]
fn scan_codex(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = CodexConnector;
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

#[tauri::command]
fn scan_claude(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = ClaudeConnector;
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

#[tauri::command]
fn scan_opencode(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = OpenCodeConnector;
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

#[tauri::command]
fn scan_workbuddy(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    // Stage 4-D.5a: 王总's "我昨天还在用,怎么没数据" was the Web
    // client (workbuddy.cn IndexedDB in Chrome), not the desktop
    // extension (which stopped writing on 2026-05-15). One IPC now
    // covers both surfaces — the desktop `check-point`/`plan-task`
    // tree AND the Chromium IndexedDB root. We aggregate the two
    // `ScanReport`s so the UI still gets a single row per click.
    let desktop = WorkBuddyConnector;
    let desktop_report = run_connector_then_etl(&mut conn, |c| {
        desktop.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })?;
    let web = awl_core::connectors::workbuddy::WorkBuddyWebConnector::new();
    let web_report = web.scan(awl_core::connectors::ScanCtx {
        conn: &mut conn,
        source_id: "",
        since_ms,
    });
    Ok(merge_workbuddy_reports(desktop_report, web_report))
}

fn merge_workbuddy_reports(
    desktop: ScanReport,
    web: AppResult<ScanReport>,
) -> ScanReport {
    match web {
        Ok(w) => {
            let merged_error = match (desktop.error.as_deref(), w.error.as_deref()) {
                (Some(a), Some(b)) => Some(format!("desktop: {a} | web: {b}")),
                (Some(a), None) => Some(a.to_string()),
                (None, Some(b)) => Some(format!("web: {b}")),
                (None, None) => None,
            };
            let status = if desktop.status == "ok" && w.status == "ok" {
                "ok".to_string()
            } else {
                "partial".to_string()
            };
            ScanReport {
                source_id: desktop.source_id,
                kind: desktop.kind,
                status,
                events_inserted: desktop.events_inserted + w.events_inserted,
                sessions_added: desktop.sessions_added + w.sessions_added,
                files_scanned: desktop.files_scanned + w.files_scanned,
                duration_ms: desktop.duration_ms + w.duration_ms,
                error: merged_error,
            }
        }
        Err(e) => ScanReport {
            error: Some(format!("desktop ok; web failed: {e}")),
            ..desktop
        },
    }
}

#[tauri::command]
fn scan_doubao(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = awl_core::connectors::doubao::DoubaoConnector::consumer();
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

#[tauri::command]
fn scan_doubao_work(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = awl_core::connectors::doubao::DoubaoConnector::work();
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

#[tauri::command]
fn scan_minimax(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = awl_core::connectors::minimax::MiniMaxConnector::new();
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

/// Stage 5-E.1a: scan the Trae IDE metadata roots. Always registered
/// as a single Trae source row; the connector surfaces every plaintext
/// Trae file (sandbox JSON, mcp.json, settings, globalStorage,
/// ttnet, ckg_server, workspaceStorage/{workspace.json, state.vscdb})
/// under that one source.
#[tauri::command]
fn scan_trae(state: State<'_, AppState>, since_ms: Option<i64>) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = TraeMetadataConnector::new();
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

/// Stage 5-E.2b: scan the user-configured `summary_dir` for hand-
/// written `.md` summaries. Emits one `markdown_summary` event per
/// file (YAML frontmatter + body), deduped by payload hash.
#[tauri::command]
fn scan_markdown_summary(
    state: State<'_, AppState>,
    since_ms: Option<i64>,
) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = MarkdownSummaryConnector::new();
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

/// Stage 6 / Track B: scan the TRAE SOLO SQLCipher-encrypted
/// database. The first call per session triggers a macOS
/// keychain GUI prompt via `awl_core::trae_solo::probe_state`.
/// Until the user clicks "Always Allow" on the prompt, the
/// connector returns a `skipped` scan with a `KeychainAccessDenied`
/// error string and zero rows — the Home page card surfaces
/// the same state to the user.
#[tauri::command]
fn scan_trae_solo(
    state: State<'_, AppState>,
    since_ms: Option<i64>,
) -> AppResult<ScanReport> {
    let mut conn = state.db.lock().expect("db mutex poisoned");
    let connector = awl_core::connectors::trae_solo::TraeSolo;
    run_connector_then_etl(&mut conn, |c| {
        connector.scan(awl_core::connectors::ScanCtx {
            conn: c,
            source_id: "",
            since_ms,
        })
    })
}

/// Stage 6 / Track B: surface the TRAE SOLO keychain / decryption
/// state to the Home page card. Returns the same
/// `TraeSoloState` enum that drives the connector's skip
/// behaviour, so the UI can render the right copy: "no
/// keychain entry" / "user denied" / "unlocked (N events)" /
/// "schema mismatch" / "error: …". Pure read — does not write
/// to the ledger, does not call the SQLCipher CLI, and does
/// not need the DB.
#[tauri::command]
fn trae_solo_status() -> awl_core::trae_solo::TraeSoloState {
    awl_core::trae_solo::probe_state()
}

/// v0.5.6: confirm the user clicked "Quit" in the in-app
/// confirm dialog. The flag in
/// `awl_core::lifecycle::QUIT_CONFIRMED` lets the
/// `RunEvent::ExitRequested` arm recognise the next exit as
/// user-confirmed and pass it through. The companion
/// `app.exit(0)` call is dispatched by the **frontend** via
/// `import { exit } from '@tauri-apps/api/app'` immediately
/// after this IPC returns, so the IPC handler itself doesn't
/// need an `AppHandle` (which would force a Tauri runtime
/// feature onto this crate just for one call). Order of
/// operations: `mark_quit_confirmed` → log → IPC returns →
/// frontend `await exit(0)` → Tauri emits ExitRequested →
/// handler sees flag set → log "exit-allowed" → process exits.
/// The lifecycle log therefore contains the full chain
/// (`exit-confirmed` then `exit-allowed`) even when the user
/// takes the explicit-confirm path.
#[tauri::command]
fn confirm_quit() -> AppResult<()> {
    awl_core::lifecycle::mark_quit_confirmed();
    awl_core::lifecycle::log_event("exit-confirmed");
    Ok(())
}

/// Test-only entry point: scan a known connector kind against a
/// caller-supplied connection. Used by integration tests so we can
/// exercise the connector pipeline without spinning up Tauri.
pub fn scan_for_tests(
    conn: &mut rusqlite::Connection,
    kind: &str,
    since_ms: Option<i64>,
) -> AppResult<ScanReport> {
    let ctx = awl_core::connectors::ScanCtx {
        conn,
        source_id: "",
        since_ms,
    };
    match kind {
        "codex" => CodexConnector.scan(ctx),
        "claude_code" => ClaudeConnector.scan(ctx),
        "cursor" => CursorConnector.scan(ctx),
        "opencode" => OpenCodeConnector.scan(ctx),
        "workbuddy" => {
            // Mirror `scan_workbuddy`: both the local check-point /
            // plan-task tree and the Chromium IndexedDB Web root are
            // touched by 王总 on different days, so an `auto_rescan`
            // tick must cover both surfaces.
            let desktop = WorkBuddyConnector.scan(awl_core::connectors::ScanCtx {
                conn: ctx.conn,
                source_id: ctx.source_id,
                since_ms: ctx.since_ms,
            })?;
            let web = awl_core::connectors::workbuddy::WorkBuddyWebConnector::new()
                .scan(awl_core::connectors::ScanCtx {
                    conn: ctx.conn,
                    source_id: ctx.source_id,
                    since_ms: ctx.since_ms,
                });
            Ok(merge_workbuddy_reports(desktop, web))
        }
        "doubao" => awl_core::connectors::doubao::DoubaoConnector::consumer().scan(ctx),
        "doubao_work" => awl_core::connectors::doubao::DoubaoConnector::work().scan(ctx),
        "minimax_code" => awl_core::connectors::minimax::MiniMaxConnector::new().scan(ctx),
        "trae" => TraeMetadataConnector::new().scan(ctx),
        "markdown_summary" => MarkdownSummaryConnector::new().scan(ctx),
        other => Err(awl_core::error::AppError::UnknownConnector(other.into())),
    }
}


/// Stage 1-H: walk every supported connector and re-ingest whatever new
/// files have appeared since the last run. Runs once on dev launch; failures
/// are isolated per-connector and reported via `tracing` instead of bubbling
/// up to the UI (which is not even visible yet at startup).
///
/// This is the "today's new work shows up immediately" fix — without it,
/// any agent activity between two app launches stays invisible until the
/// user manually clicks a Rescan button.
pub fn auto_rescan_all(db: Arc<Mutex<Connection>>) {
    use awl_core::connectors::kinds;
    // Stage 6 (Track B) added `trae_solo`. The first call per
    // session triggers a one-time macOS keychain GUI prompt;
    // if the user denies, the connector returns a `skipped`
    // scan and the rest of auto_rescan continues unaffected.
    let targets: [&'static str; 11] = [
        kinds::CODEX,
        kinds::CLAUDE_CODE,
        kinds::CURSOR,
        kinds::OPENCODE,
        kinds::WORKBUDDY,
        kinds::DOUBAO,
        kinds::DOUBAO_WORK,
        kinds::MINIMAX_CODE,
        kinds::TRAE,
        kinds::TRAE_SOLO,
        kinds::MARKDOWN_SUMMARY,
    ];
    tracing::info!(
        connectors = targets.len(),
        "startup auto-rescan: dispatching background thread"
    );
    let started = std::time::Instant::now();
    let mut total_events: i64 = 0;
    let mut total_sessions: i64 = 0;
    let mut ok_count = 0usize;
    let mut fail_count = 0usize;
    for kind in targets {
        let mut guard = match db.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(kind, error = %e, "auto-rescan: db mutex poisoned, skipping");
                fail_count += 1;
                continue;
            }
        };
        match scan_for_tests(&mut guard, kind, None) {
            Ok(mut report) => {
                total_events += report.events_inserted;
                if !report.source_id.is_empty() && report.status != "failed" {
                    // Inline ETL: each successful scan is followed by a
                    // sessions_summary rebuild for that source. Mirrors
                    // the IPC handler behaviour.
                    match awl_core::etl::reconcile_sessions_summary(
                        &guard,
                        &report.source_id,
                    ) {
                        Ok(n) => {
                            total_sessions += n;
                            report.sessions_added = n;
                        }
                        Err(e) => tracing::warn!(
                            source_id = %report.source_id,
                            error = %e,
                            "auto-rescan etl: reconcile failed; raw events left as-is",
                        ),
                    }
                }
                ok_count += 1;
                tracing::info!(
                    kind = %report.kind,
                    status = %report.status,
                    files = report.files_scanned,
                    events = report.events_inserted,
                    sessions = report.sessions_added,
                    error = ?report.error,
                    duration_ms = report.duration_ms,
                    "auto-rescan connector finished"
                );
            }
            Err(e) => {
                // Stage 6 (Track B / v0.5.1): trae_solo can fail
                // with `connector not registered` when the
                // keychain entry doesn't exist yet. That's
                // not a real failure — just an early-skip.
                // Count it as a no-op rather than a hard
                // fail so the startup summary doesn't lie
                // about health.
                let msg = e.to_string();
                if msg.contains("connector not registered")
                    || msg.contains("no keychain entry")
                {
                    tracing::info!(
                        kind,
                        note = "skipped (keychain-gated, no source row yet)",
                        "auto-rescan: connector not yet registered; skipping"
                    );
                } else {
                    fail_count += 1;
                    tracing::warn!(kind, error = %e, "auto-rescan connector failed");
                }
            }
        }
    }
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        ok = ok_count,
        failed = fail_count,
        total_events,
        total_sessions,
        "startup auto-rescan complete"
    );

    // v0.4.3: after every scan pass, prune raw_events older than the
    // user's configured retention window. Cheap on a 9K-row table;
    // keeps the DB from growing unbounded for the user who never
    // opens the Settings page. Setting `0` disables the prune.
    let retention_days = awl_core::app_settings::load().event_retention_days;
    if let Ok(guard) = db.lock() {
        match retention::prune_events(&guard, retention_days) {
            Ok(report) if report.ran => {
                if report.events_deleted > 0 {
                    tracing::info!(
                        deleted = report.events_deleted,
                        cutoff_iso = %report.cutoff_iso,
                        days = report.days,
                        "retention: pruned raw_events"
                    );
                }
            }
            Ok(_) => {
                // ran=false means days <= 0; no-op, no log.
            }
            Err(e) => {
                tracing::warn!(error = %e, "retention: prune failed; raw events left as-is");
            }
        }
    }
}

/// Stage 1-H-b: background scanner loop. Sleeps `BACKGROUND_SCAN_INTERVAL_SECS`
/// between full sweeps. Each tick delegates to `auto_rescan_all`, which is
/// the same path the startup scan and the "Scan Now" tray menu use, so all
/// three call-sites share one code path and one set of logging fields.
///
/// Exit semantics: there is no clean shutdown signal today — Tauri 2 does not
/// expose a per-thread cancellation handle that wakes a `std::thread::sleep`.
/// When the process exits the OS reclaims the thread. That is acceptable for
/// a local-only tool: at worst we lose one final tick at shutdown.
pub fn background_scan_loop(db: Arc<Mutex<Connection>>) {
    tracing::info!(
        interval_secs = BACKGROUND_SCAN_INTERVAL_SECS,
        "background scanner loop started"
    );
    loop {
        std::thread::sleep(std::time::Duration::from_secs(BACKGROUND_SCAN_INTERVAL_SECS));
        tracing::debug!("background scanner loop: tick");
        auto_rescan_all(db.clone());
    }
}

/// Hard-reset of the events pipeline: delete every row in `raw_events`,
/// `sessions_summary` and `connector_runs`. Sources / privacy rules are
/// kept so the configured connector list survives. The CASCADE FK on
/// `plan_items.source_session_id` and `session_summaries.session_summary_id`
/// automatically wipes any rows that referenced a now-deleted session.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResetReport {
    pub raw_events_deleted: i64,
    pub sessions_summary_deleted: i64,
    pub connector_runs_deleted: i64,
}

#[tauri::command]
fn reset_events(state: State<'_, AppState>) -> AppResult<ResetReport> {
    let conn = state.db.lock().expect("db mutex poisoned");
    // Order matters: FK chains. session_summaries / plan_items reference
    // sessions_summary, so delete sessions_summary first and let the FKs
    // cascade. raw_events must come last if we ever add a FK from
    // sessions_summary.event_count to raw_events (currently no FK, so
    // order is purely operational clarity for the user-visible counts).
    let sessions_summary_deleted = conn.execute("DELETE FROM sessions_summary", [])? as i64;
    let raw_events_deleted = conn.execute("DELETE FROM raw_events", [])? as i64;
    let connector_runs_deleted = conn.execute("DELETE FROM connector_runs", [])? as i64;
    Ok(ResetReport {
        raw_events_deleted,
        sessions_summary_deleted,
        connector_runs_deleted,
    })
}

// ---------------------------------------------------------------------
// Stage 1-H-b2: launchd plist headless scanner.
//
// When the user installs the LaunchAgent, `launchd` will exec this
// binary with `--scan-once` every BACKGROUND_SCAN_INTERVAL_SECS even if
// the app's main window is closed. The function below mirrors the
// in-app startup path (resolve_data_dir -> open_db -> auto_rescan_all)
// but exits with a status code so launchd's logging reflects success
// or failure honestly.
// ---------------------------------------------------------------------
pub fn run_scan_once_mode() {
    let dir = match resolve_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("agent-work-ledger --scan-once: resolve_data_dir failed: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("agent-work-ledger --scan-once: create data dir failed: {e}");
        std::process::exit(2);
    }
    let db_path = dir.join("ledger.db");
    tracing::info!(db_path = %db_path.display(), "scan-once: opening ledger");
    let conn = match open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "scan-once: open_db failed");
            std::process::exit(2);
        }
    };
    let db = Arc::new(Mutex::new(conn));
    auto_rescan_all(db);
    tracing::info!("scan-once: complete, success");
    std::process::exit(0);
}

/// Stage 5-E.3c: headless morning-brief mode for the launchd 8am
/// plist. Computes the daily Eisenhower digest, picks the top-3
/// Q1, persists it into the `daily_briefs` cache, and logs the
/// headline so the plist's `StandardOutPath` keeps an audit
/// trail. The UI also reads the cached row on home-page mount so
/// the morning card is populated even if the user opens the app
/// later in the day.
pub fn run_brief_once_mode() {
    let dir = match resolve_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("agent-work-ledger --brief-once: resolve_data_dir failed: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("agent-work-ledger --brief-once: create data dir failed: {e}");
        std::process::exit(2);
    }
    let db_path = dir.join("ledger.db");
    tracing::info!(db_path = %db_path.display(), "brief-once: opening ledger");
    let conn = match open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "brief-once: open_db failed");
            std::process::exit(2);
        }
    };
    let today = awl_core::brief::today_utc();
    match awl_core::brief::generate(&conn, today) {
        Ok(b) => {
            if let Err(e) = awl_core::brief::save_brief(&conn, &b) {
                tracing::error!(error = %e, "brief-once: save_brief failed");
                std::process::exit(2);
            }
            tracing::info!(
                date = %b.date,
                ai_used = b.ai_used,
                total_q1 = b.total_q1,
                top_n = b.top_q1.len(),
                headline = %b.headline,
                "brief-once: complete"
            );
            // Pretty stdout line so the plist StandardOutPath is
            // human-readable when `tail -F`'d.
            println!(
                "[{}] {}",
                b.date,
                b.headline
            );
            for it in b.top_q1.iter() {
                println!(
                    "  - Q1 imp={}/10 urg={}/10 events={} | {}",
                    it.importance,
                    it.urgency,
                    it.event_count,
                    it.label
                );
            }
            std::process::exit(0);
        }
        Err(e) => {
            tracing::error!(error = %e, "brief-once: generate failed");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------
// Stage 1-H-b2: install / uninstall / inspect the LaunchAgent.
//
// The plist lives in the app resources at
// `app/launchd/com.minimaxcode.agentworkledger.scanner.plist` and is a
// template — we patch it at install time with the *current* binary path
// (which differs between dev mode at target/debug/... and prod mode at
// /Applications/Agent Work Ledger.app/Contents/MacOS/agent-work-ledger).
// The patched copy lands at
// `~/Library/LaunchAgents/com.minimaxcode.agentworkledger.scanner.plist`.
// ---------------------------------------------------------------------
const LAUNCHD_LABEL: &str = "com.minimaxcode.agentworkledger.scanner";
// Stage 5-E.3c: morning-brief LaunchAgent (8 am daily).
const BRIEF_LAUNCHD_LABEL: &str = "com.minimaxcode.agentworkledger.brief";

fn launchd_plist_path_for(label: &str) -> Option<std::path::PathBuf> {
    launchd_user_agents_dir().map(|h| h.join(format!("{label}.plist")))
}

/// Install a single LaunchAgent plist by label. `kind` is the
/// resource kind passed to `bundled_plist_path` (currently
/// "scanner" or "brief"). Returns the resolved target plist
/// path so the caller can keep going on partial success.
fn install_plist_from_template(label: &str, kind: &str) -> AppResult<std::path::PathBuf> {
    let template = bundled_plist_path(kind)
        .ok_or_else(|| AppError::Other(format!("could not locate bundled plist ({kind}) template")))?;
    let target = launchd_plist_path_for(label)
        .ok_or_else(|| AppError::Other("could not resolve ~/Library/LaunchAgents/".into()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Render template: replace __BINARY_PATH__ with the current exe.
    let raw = std::fs::read_to_string(&template)?;
    let binary_path = std::env::current_exe()
        .map_err(|e| AppError::Other(format!("current_exe failed: {e}")))?;
    let rendered = raw.replace("__BINARY_PATH__", &binary_path.to_string_lossy());
    std::fs::write(&target, rendered)?;

    // Always unload first so a re-install doesn't pile up stale copies.
    let _ = launchctl(&["bootout", "gui", label]);
    launchctl(&["bootstrap", "gui", &target.to_string_lossy()])
        .map_err(|e| AppError::Other(e))?;
    Ok(target)
}

fn launchd_user_agents_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join("Library").join("LaunchAgents"))
}

fn launchd_plist_path() -> Option<std::path::PathBuf> {
    launchd_user_agents_dir().map(|h| h.join(format!("{LAUNCHD_LABEL}.plist")))
}

fn bundled_plist_template() -> Option<std::path::PathBuf> {
    // Backwards-compat alias — the scanner install path predates
    // `kind`; defer to the kind-aware loader with "scanner".
    bundled_plist_path("scanner")
}

fn bundled_plist_path(kind: &str) -> Option<std::path::PathBuf> {
    // The plist sits next to the executable so the production .app can
    // ship it inside Contents/Resources. In dev mode cargo puts the
    // binary at target/debug/agent-work-ledger and we expect the plist
    // at ../launchd/ relative to the workspace root. Resolve by walking
    // up from the current_exe until we find a `launchd/` directory that
    // contains our template; fall back to the workspace source path.
    //
    // Note: when the bundle puts a resource straight into
    // `Contents/Resources/` it is renamed `_up_<basename>` to avoid
    // colliding with reserved names like `Info.plist` and `Resources`
    // itself — so we have to look for the file under both shapes.
    //
    // `kind` is "scanner" or "brief" — keeps the loader agnostic
    // to a single global label so adding new launchd agents later
    // is one row, not a refactor.
    let plist_name = match kind {
        "scanner" => format!("{LAUNCHD_LABEL}.plist"),
        "brief" => format!("{BRIEF_LAUNCHD_LABEL}.plist"),
        other => format!("{other}.plist"),
    };
    let candidates = [
        // production .app: <Contents/MacOS/<exe>> -> <Contents/Resources/_up_/launchd/..plist>
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .map(|p| {
                p.join("Resources")
                    .join("_up_")
                    .join("launchd")
                    .join(&plist_name)
            }),
        // production .app: <Contents/MacOS/<exe>> -> <Contents/Resources/launchd/..plist>
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .map(|p| p.join("Resources").join("launchd").join(&plist_name)),
        // workspace dev: <repo>/app/src-tauri/target/debug/<exe> -> <repo>/app/launchd/..plist
        std::env::current_exe()
            .ok()
            .and_then(|p| {
                let mut p = p;
                for _ in 0..6 {
                    if !p.pop() {
                        return None;
                    }
                }
                Some(p)
            })
            .map(|p| p.join("app").join("launchd").join(&plist_name)),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LaunchdStatus {
    pub installed: bool,
    pub plist_path: Option<String>,
    pub binary_path: Option<String>,
    pub last_load_error: Option<String>,
}

fn launchctl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl spawn failed: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        s.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    if !out.status.success() {
        return Err(format!(
            "launchctl {} failed (exit {}): {}",
            args.join(" "),
            out.status,
            s.trim()
        ));
    }
    Ok(s)
}

#[tauri::command]
fn launchd_status() -> AppResult<LaunchdStatus> {
    let plist = launchd_plist_path();
    let installed = plist.as_ref().map(|p| p.is_file()).unwrap_or(false);
    let binary = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let mut last_load_error: Option<String> = None;
    if installed {
        if let Err(e) = launchctl(&["printid", "-gui", LAUNCHD_LABEL]) {
            last_load_error = Some(e);
        }
    }
    Ok(LaunchdStatus {
        installed,
        plist_path: plist.map(|p| p.to_string_lossy().into_owned()),
        binary_path: binary,
        last_load_error,
    })
}

#[tauri::command]
fn install_launchd_scanner() -> AppResult<LaunchdStatus> {
    install_plist_from_template(LAUNCHD_LABEL, "scanner")?;
    let plist_path = launchd_plist_path_for(LAUNCHD_LABEL)
        .ok_or_else(|| AppError::Other("could not resolve ~/Library/LaunchAgents/".into()))?;
    let binary_path = std::env::current_exe()
        .map_err(|e| AppError::Other(format!("current_exe failed: {e}")))?;
    // Stage 5-E.3c: also install the morning-brief LaunchAgent so
    // the home-page card is populated at 8 am. Failure here is
    // non-fatal — the scanner still works without it.
    let _ = install_plist_from_template(BRIEF_LAUNCHD_LABEL, "brief");
    Ok(LaunchdStatus {
        installed: true,
        plist_path: Some(plist_path.to_string_lossy().into_owned()),
        binary_path: Some(binary_path.to_string_lossy().into_owned()),
        last_load_error: None,
    })
}

#[tauri::command]
fn uninstall_launchd_scanner() -> AppResult<LaunchdStatus> {
    let plist = launchd_plist_path().ok_or_else(|| {
        AppError::Other("could not resolve ~/Library/LaunchAgents/".into())
    })?;
    if plist.exists() {
        let _ = launchctl(&["bootout", "gui", LAUNCHD_LABEL]);
        std::fs::remove_file(&plist)?;
    }
    Ok(LaunchdStatus {
        installed: false,
        plist_path: Some(plist.to_string_lossy().into_owned()),
        binary_path: std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()),
        last_load_error: None,
    })
}

// ---------------------------------------------------------------------
// Stage 2: AI semantic layer (Ollama local-first).
//
// All prompts go through the global redactor when `redact_prompts` is
// true (the default). The endpoint is a local URL — we never call a
// cloud provider in this stage.
// ---------------------------------------------------------------------
#[tauri::command]
fn ai_get_settings() -> ai::AiSettings {
    ai::load_settings()
}

#[tauri::command]
fn ai_save_settings(settings: ai::AiSettings) -> AppResult<()> {
    ai::save_settings(&settings).map_err(AppError::Other)
}

#[tauri::command]
fn ai_status() -> ai::AiStatus {
    let s = ai::load_settings();
    ai::status(&s)
}

#[tauri::command]
fn ai_query(prompt: String, json_mode: Option<bool>) -> ai::GenerateReport {
    let s = ai::load_settings();
    ai::generate(&s, &prompt, json_mode.unwrap_or(false))
}

// ---------------------------------------------------------------------
// Stage 5-E.2a: app-level (non-AI) settings + Prompt 库 IPC.
//
// AppSettings lives in `<data_dir>/app_settings.json` (separate from
// ai_settings.json so provider switches don't drag the user's content
// choices around with them). The IPC surface returns the resolved
// summary_dir absolute path so the UI can show tooltips without doing
// shell expansion.
// ---------------------------------------------------------------------
#[tauri::command]
fn app_get_settings() -> AppSettings {
    let mut s = awl_core::app_settings::load();
    s.summary_dir = awl_core::app_settings::expand_tilde(&s.summary_dir);
    s
}

#[tauri::command]
fn app_save_settings(settings: AppSettings) -> AppResult<()> {
    awl_core::app_settings::save(&settings).map_err(AppError::Other)?;
    // Ensure the directory exists so the markdown scanner (Stage
    // 5-E.2b) never crashes on first run. We don't write anything
    // inside it — that's the user's space.
    let expanded = awl_core::app_settings::expand_tilde(&settings.summary_dir);
    if let Err(e) = std::fs::create_dir_all(&expanded) {
        tracing::warn!(
            dir = %expanded,
            error = %e,
            "app_settings: could not create summary_dir; scanner will skip with a clear error"
        );
    }
    Ok(())
}

#[tauri::command]
fn app_list_prompts() -> Vec<PromptTemplate> {
    awl_core::app_settings::builtin_templates()
}

// v0.4.3: manual prune trigger for the Settings page's "立即清理"
// button. We use the *current* saved retention window — passing
// `days` from the frontend would let the user accidentally wipe
// everything by typing 99999. The auto-rescan pass uses the same
// logic, so the manual button is just a synchronous variant.
#[tauri::command]
fn retention_prune_now(
    db: tauri::State<Arc<Mutex<Connection>>>,
) -> AppResult<retention::PruneReport> {
    let days = awl_core::app_settings::load().event_retention_days;
    let guard = db.lock().map_err(|e| AppError::Other(format!("db mutex poisoned: {e}")))?;
    retention::prune_events(&guard, days)
}

// ---------------------------------------------------------------------
// Stage 4: per-session AI summarisation IPC.
//
// The user clicks "Summarise" on a session card in the UI and the
// frontend calls `summarize_session(session_id)`. The handler delegates
// to `summary::summarise_session`, which owns the cache lookup, prompt
// assembly, provider call (cloud force-redacts at the ai layer),
// fallback decision and persistence. Returns the freshly written row
// or the cached hit, plus a `cached` flag and `age_hours` so the UI
// can show provenance.
// ---------------------------------------------------------------------
#[tauri::command]
fn summarize_session(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<summary::SessionSummary> {
    let conn = state.db.lock().expect("db mutex poisoned");
    summary::summarise_session(&conn, &session_id)
}

// ---------------------------------------------------------------------
// Stage 5-E.4a: master-detail UI for the Sessions page.
//
// The left pane lists every `sessions_summary` row and the right pane
// shows the selected one — its metadata, any cached AI summary, and a
// chronological list of every raw_event that fell into its
// `[started_at, ended_at]` window. We surface the full payload for
// each event so the user can drill into what actually happened in that
// bucket without going through an LLM roundtrip. `list_session_events`
// reads directly from `raw_events` (no LIMIT, unlike the summarise
// path) which keeps the implementation honest — the UI is allowed to
// see everything the ETL pipeline already ingested.
// ---------------------------------------------------------------------
#[tauri::command]
fn list_session_events(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<Vec<summary::SessionEventRow>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    summary::fetch_session_events_for_ui(&conn, &session_id)
}

// ---------------------------------------------------------------------
// Stage 5-E.4b: portable Markdown export for one partition.
//
// `render_session_markdown` returns the raw markdown bytes plus a
// filesystem-safe suggested filename. The frontend then asks the user
// where to save via `tauri-plugin-dialog::save` and hands the resulting
// path to `write_text_file`. We do not write to disk during the render
// call itself — keeping it pure (no LLM, no FS side-effects) makes the
// function testable from a plain `cargo test --lib` and lets the
// caller preview / inspect the bytes before they hit the user's disk.
// ---------------------------------------------------------------------
#[tauri::command]
fn render_session_markdown(
    state: State<'_, AppState>,
    session_id: String,
    include_summary: bool,
) -> AppResult<awl_export::MarkdownExport> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let app_version = env!("CARGO_PKG_VERSION");
    awl_export::render_session_markdown(&conn, &session_id, include_summary, app_version)
}

/// Write `content` to a user-selected file. The path is supplied by
/// the frontend after `tauri-plugin-dialog::save` confirms where the
/// user wants it — keeping the path on the JS side means the dialog
/// itself is the authorisation gate. We deliberately do not
/// filesystem-scoped `plugin-fs` so the user can save anywhere their
/// picker lets them.
#[tauri::command]
fn write_text_file(path: String, content: String) -> AppResult<()> {
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, content)?;
    Ok(())
}

/// Write raw binary bytes (base64-encoded) to a user-selected file.
/// Same authorisation model as `write_text_file` — the JS-side
/// `tauri-plugin-dialog::save` is the picker that gates the target
/// path; the Rust side is the dumb disk writer. We cross the IPC
/// boundary as base64 because the Tauri 2 IPC layer is JSON-only.
#[tauri::command]
fn write_binary_file(path: String, content_base64: String) -> AppResult<()> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64.as_bytes())
        .map_err(|e| {
            awl_core::error::AppError::Connector(
                "write_binary_file".into(),
                format!("base64 decode failed: {e}"),
            )
        })?;
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, bytes)?;
    Ok(())
}

// ---------------------------------------------------------------------
// Stage 5.5 / v0.3.4: bulk Sessions export.
//
// Renders every `sessions_summary` row that matches the optional
// source id + optional `since_ms` cutoff as **one** file in either
// JSON (pretty-printed envelope with `app_version`, `exported_utc`,
// `total`, and a `sessions` array) or CSV (RFC 4180; opens cleanly
// in Excel/Numbers/Sheets). Re-uses the same `MarkdownExport` shape
// + `write_text_file` IPC as the single-bucket markdown path so the
// frontend has one flow regardless of shape.
//
// We deliberately do not include raw event payloads — bulk payload
// material gets heavy fast and the per-bucket markdown export is the
// right tool when the user actually needs the timeline.
// ---------------------------------------------------------------------
#[tauri::command]
fn export_sessions_bulk(
    state: State<'_, AppState>,
    format: String,
    source_filter: Option<String>,
    since_ms: Option<i64>,
) -> AppResult<awl_export::MarkdownExport> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let app_version = env!("CARGO_PKG_VERSION");
    let fmt: awl_export::BulkFormat = format
        .parse()
        .map_err(|e: String| awl_core::error::AppError::Connector(
            "export_sessions_bulk".into(),
            e,
        ))?;
    awl_export::render_sessions_bulk(
        &conn,
        fmt,
        source_filter.as_deref(),
        since_ms,
        app_version,
    )
}

// ---------------------------------------------------------------------
// Stage 5.5 / v0.3.5: bulk Sessions export as a multi-file .zip.
//
// Same `source_filter` + `since_ms` shape as the JSON/CSV path, but
// each partition is rendered with `render_session_markdown(include_summary=true)`
// and written as its own `.md` under a top-level `sessions/` prefix.
// A top-level `INDEX.md` acts as a manifest table so the bundle is
// `cat`-friendly and lives as a stable unit when the user shares it
// via Slack/Drive. Binary bytes cross IPC as base64 (JSON-only
// channel); the frontend re-decodes and writes via `write_binary_file`.
// ---------------------------------------------------------------------
#[tauri::command]
fn render_sessions_bulk_zip(
    state: State<'_, AppState>,
    source_filter: Option<String>,
    since_ms: Option<i64>,
) -> AppResult<awl_export::BulkZipExport> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let app_version = env!("CARGO_PKG_VERSION");
    awl_export::render_sessions_bulk_zip(
        &conn,
        source_filter.as_deref(),
        since_ms,
        app_version,
    )
}

// ---------------------------------------------------------------------
// Stage 5.7 / v0.3.7: cross-source search.
//
// One IPC that fans out across sessions_summary / raw_events / plan_items
// / sources, ranks by recency, and returns a unified `Vec<SearchHit>`
// capped at 50. Pure SQL — LIKE-based rather than FTS5 to keep the
// schema migration cost at zero (see `search.rs` for the trade-off).
// ---------------------------------------------------------------------
#[tauri::command]
fn global_search(
    state: State<'_, AppState>,
    query: String,
) -> AppResult<awl_core::search::SearchResult> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::search::global_search(&conn, &query)
}

// ---------------------------------------------------------------------
// Stage 4: window-level AI reports (Daily / Weekly) IPC.
//
// The user clicks "Generate report" on the Reports page, which has
// already computed the calendar window (UTC midnight-to-midnight for
// daily, Mon→Mon for weekly) and passes the RFC3339 start + end
// strings plus a free-form label. The handler delegates to
// `reports::generate_window_report`, which owns the cache lookup and
// the LLM/heuristic split. `list_window_reports` returns the most
// recent cached rows so the Reports page can render a history strip.
// ---------------------------------------------------------------------
#[tauri::command]
fn generate_window_report(
    state: State<'_, AppState>,
    label: String,
    window_start: String,
    window_end: String,
) -> AppResult<reports::WindowReport> {
    let conn = state.db.lock().expect("db mutex poisoned");
    reports::generate_window_report(&conn, &label, &window_start, &window_end)
}

#[tauri::command]
fn list_window_reports(
    state: State<'_, AppState>,
    label: Option<String>,
    limit: i64,
) -> AppResult<Vec<reports::WindowReport>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    reports::list_window_reports(&conn, label.as_deref(), limit)
}

// ---------------------------------------------------------------------
// Stage 3: Eisenhower planner IPC surface.
//
// All commands take &AppState.db so the planner shares the same DB
// mutex as the connectors; the planning tables live in the same
// `ledger.db` file as `raw_events` and `sessions`.
// ---------------------------------------------------------------------
#[tauri::command]
fn list_plan_templates(state: State<'_, AppState>) -> AppResult<Vec<awl_core::planner::PlanTemplate>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::list_templates(&conn)
}

#[tauri::command]
fn generate_eisenhower_digest(
    state: State<'_, AppState>,
    lookback_hours: Option<i64>,
) -> AppResult<eisenhower::EisenhowerDigest> {
    let conn = state.db.lock().expect("db mutex poisoned");
    eisenhower::generate(&conn, lookback_hours.unwrap_or(eisenhower::DEFAULT_LOOKBACK_HOURS))
}

#[tauri::command]
fn promote_eisenhower_q1(
    state: State<'_, AppState>,
    digest: eisenhower::EisenhowerDigest,
) -> AppResult<i64> {
    let conn = state.db.lock().expect("db mutex poisoned");
    eisenhower::promote_q1_to_plan(&conn, &digest)
}

#[tauri::command]
fn generate_daily_brief(state: State<'_, AppState>) -> AppResult<awl_core::brief::DailyBrief> {
    let conn = state.db.lock().expect("db mutex poisoned");
    let b = awl_core::brief::generate(&conn, awl_core::brief::today_utc())?;
    awl_core::brief::save_brief(&conn, &b)?;
    Ok(b)
}

#[tauri::command]
fn latest_daily_brief(state: State<'_, AppState>) -> AppResult<Option<awl_core::brief::DailyBrief>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::brief::latest_cached(&conn)
}

#[tauri::command]
fn create_plan_template(
    state: State<'_, AppState>,
    name: String,
    kind: String,
) -> AppResult<awl_core::planner::PlanTemplate> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::create_template(&conn, &name, &kind)
}

#[tauri::command]
fn delete_plan_template(state: State<'_, AppState>, id: String) -> AppResult<i64> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::delete_template(&conn, &id)
}

#[tauri::command]
fn list_plan_items(state: State<'_, AppState>, template_id: String) -> AppResult<Vec<awl_core::planner::PlanItem>> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::list_items(&conn, &template_id)
}

#[tauri::command]
fn upsert_plan_item(
    state: State<'_, AppState>,
    input: awl_core::planner::UpsertItemInput,
) -> AppResult<awl_core::planner::PlanItem> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::upsert_item(&conn, input)
}

#[tauri::command]
fn delete_plan_item(state: State<'_, AppState>, id: String) -> AppResult<i64> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::delete_item(&conn, &id)
}

#[tauri::command]
fn ai_suggest_quadrant(state: State<'_, AppState>, text: String) -> AppResult<awl_core::planner::QuadrantSuggestion> {
    let conn = state.db.lock().expect("db mutex poisoned");
    awl_core::planner::suggest_quadrant(&conn, &text)
}

/// Wire every `#[tauri::command]` in this crate into the supplied
/// Tauri builder. The commands are private to the crate (the
/// `tauri::command` proc-macro conflicts with `pub fn`), so callers
/// must go through this entry point instead of naming commands
/// directly inside `generate_handler!`.
pub fn register<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        get_health,
        list_sources,
        delete_source,
        confirm_quit,
        list_recent_events,
        list_timeline_events,
        list_supported_connectors,
        export_markdown,
        scan_codex,
        scan_claude,
        scan_opencode,
        scan_workbuddy,
        scan_doubao,
        scan_doubao_work,
        scan_minimax,
        scan_trae,
        scan_trae_solo,
        trae_solo_status,
        scan_markdown_summary,
        rebuild_sessions_summary,
        list_sessions_summary,
        list_session_events,
        render_session_markdown,
        export_sessions_bulk,
        render_sessions_bulk_zip,
        write_text_file,
        write_binary_file,
        global_search,
        reset_events,
        launchd_status,
        install_launchd_scanner,
        uninstall_launchd_scanner,
        ai_get_settings,
        ai_save_settings,
        ai_status,
        ai_query,
        app_get_settings,
        app_save_settings,
        app_list_prompts,
        retention_prune_now,
        list_plan_templates,
        create_plan_template,
        delete_plan_template,
        list_plan_items,
        upsert_plan_item,
        delete_plan_item,
        ai_suggest_quadrant,
        summarize_session,
        generate_window_report,
        list_window_reports,
        generate_eisenhower_digest,
        promote_eisenhower_q1,
        generate_daily_brief,
        latest_daily_brief
    ])
}
