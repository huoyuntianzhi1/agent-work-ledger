//! v0.3.3 / Stage 5-E.4b: serialize a `sessions_summary` partition into
//! a portable Markdown file the user can share with non-Agent-Work-Ledger
//! readers (Notion, Obsidian, GitHub, email).
//!
//! Pure assembly — no LLM calls, no disk writes. The frontend asks the
//! user where to save via `@tauri-apps/plugin-dialog::save`, then hands
//! the resulting file path to `write_text_file` in `lib.rs`.
//!
//! The output is **stable byte-for-byte** for the same DB state, which
//! gives us a place to anchor a clipboard diff test later.

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::error::AppResult;
use crate::summary::{fetch_session_events_for_ui, load_session_row, SessionEventRow, SessionRow};
// bulk export wants to format "now" as RFC3339 for both the
// suggested filename stamp and the JSON `exported_utc` header.
use chrono::Utc;
// Stage 5.5: zip-bundle export. `base64` is needed because the
// Tauri 2 IPC layer is JSON-only; binary bytes cross the boundary
// as base64-encoded strings.
use base64::Engine as _;
use std::io::Write as _;

/// One rendered export bundle. `content` is the full markdown bytes;
/// `suggested_filename` is the default path stem the dialog opens with
/// (caller appends the user's `.md` if missing).
#[derive(Debug, Clone, Serialize)]
pub struct MarkdownExport {
    pub content: String,
    pub suggested_filename: String,
}

/// Assemble the markdown for a single `sessions_summary` partition.
///
/// `include_summary` only inserts the AI summary block when there is an
/// **uncached** summary already on disk — we never call the LLM during
/// an export; users go to the Sessions page to generate a summary first,
/// then come back here.
pub fn render_session_markdown(
    conn: &Connection,
    partition_id: &str,
    include_summary: bool,
    app_version: &str,
) -> AppResult<MarkdownExport> {
    let session = load_session_row(conn, partition_id)?;
    let events = fetch_session_events_for_ui(conn, partition_id)?;
    let summary = if include_summary {
        load_cached_summary(conn, partition_id)?
    } else {
        None
    };
    let source_display = lookup_source_display_name(conn, &session.source_id)
        .unwrap_or_else(|_| session.source_id.clone());

    let content = render_markdown(
        &session,
        &events,
        summary.as_ref(),
        partition_id,
        &source_display,
        app_version,
    );
    let suggested_filename = suggested_filename(&session);
    Ok(MarkdownExport {
        content,
        suggested_filename,
    })
}

#[derive(Debug)]
struct CachedSummary {
    headline: String,
    body: String,
    reasoning: String,
    provider: String,
    model: String,
    importance: i64,
    urgency: i64,
    age_hours: i64,
}

/// Return the most recent unexpired row from `session_summaries` for
/// this partition, or `None` when the user has never summarised this
/// bucket (or the row expired past `now`). The Sessions page is the
/// place users trigger a summarise; once it's cached we surface it on
/// export, otherwise the heuristic one-liner stays the only summary
/// in the file.
fn load_cached_summary(
    conn: &Connection,
    partition_id: &str,
) -> AppResult<Option<CachedSummary>> {
    let now_iso = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    let mut stmt = conn.prepare(
        "SELECT headline, body, reasoning, provider, model, importance,
                urgency, created_at
         FROM session_summaries
         WHERE session_summary_id = ?1 AND expires_at > ?2
         ORDER BY created_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![partition_id, now_iso])?;
    let row = match rows.next()? {
        Some(r) => r,
        None => return Ok(None),
    };
    let headline: String = row.get(0)?;
    let body: String = row.get(1)?;
    let reasoning: String = row.get(2)?;
    let provider: String = row.get(3)?;
    let model: String = row.get(4)?;
    let importance: i64 = row.get(5)?;
    let urgency: i64 = row.get(6)?;
    let created_at: String = row.get(7)?;
    Ok(Some(CachedSummary {
        headline,
        body,
        reasoning,
        provider,
        model,
        importance,
        urgency,
        age_hours: age_hours_from_iso(&created_at, &now_iso),
    }))
}

fn lookup_source_display_name(conn: &Connection, source_id: &str) -> AppResult<String> {
    let v: String = conn.query_row(
        "SELECT display_name FROM sources WHERE id = ?1",
        params![source_id],
        |row| row.get(0),
    )?;
    Ok(v)
}

fn age_hours_from_iso(then_iso: &str, now_iso: &str) -> i64 {
    let parse = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.timestamp_millis())
    };
    match (parse(then_iso), parse(now_iso)) {
        (Some(t0), Some(t1)) => ((t1 - t0).max(0) / 3_600_000),
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_markdown(
    session: &SessionRow,
    events: &[SessionEventRow],
    summary: Option<&CachedSummary>,
    partition_id: &str,
    source_display: &str,
    app_version: &str,
) -> String {
    let started_at = &session.started_at;
    let ended_at = &session.ended_at;
    let event_count = events.len() as i64;
    let importance = session.heuristic_importance;
    let project_key = session
        .project_key
        .as_deref()
        .unwrap_or("(未知项目)");
    let export_utc = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));

    let mut s = String::new();
    s.push_str("---\n");
    s.push_str("app: Agent Work Ledger\n");
    s.push_str(&format!("app_version: {app_version}\n"));
    s.push_str(&format!("session_id: {partition_id}\n"));
    s.push_str(&format!("exported_utc: {export_utc}\n"));
    s.push_str(&format!("source_id: {}\n", session.source_id));
    s.push_str(&format!("source_display_name: {source_display}\n"));
    s.push_str(&format!("started_at: {started_at}\n"));
    s.push_str(&format!("ended_at: {ended_at}\n"));
    s.push_str(&format!("event_count: {event_count}\n"));
    s.push_str(&format!("importance: {importance}/10\n"));
    // Frontmatter value is quoted so project paths with `/` or `:` survive.
    s.push_str(&format!(
        "project_key: \"{}\"\n",
        project_key.replace('"', "\\\"")
    ));
    s.push_str("---\n\n");

    s.push_str(&format!(
        "# {started_at} → {ended_at} · {source_display} · {project_key}\n\n"
    ));

    s.push_str("## 元数据\n\n");
    s.push_str(&format!("- **数据源**: {source_display}\n"));
    s.push_str(&format!(
        "- **时间窗**: {started_at} → {ended_at}\n"
    ));
    s.push_str(&format!("- **项目**: `{project_key}`\n"));
    s.push_str(&format!("- **事件数**: {event_count}\n"));
    s.push_str(&format!("- **重要性**: {importance}/10\n"));
    s.push_str("- **启发式类型**: auto (ETL)\n");
    s.push_str(&format!("- **Session ID**: `{partition_id}`\n"));
    s.push('\n');

    s.push_str("## heuristic 摘要\n\n");
    s.push_str(&session.heuristic_summary);
    s.push_str("\n\n");

    if let Some(sm) = summary {
        s.push_str("## AI 摘要\n\n");
        s.push_str(&format!(
            "> provider=`{}` model=`{}` importance={}/10 urgency={}/10 cache_age={}h\n\n",
            sm.provider, sm.model, sm.importance, sm.urgency, sm.age_hours
        ));
        s.push_str(&format!("### {}\n\n", sm.headline));
        s.push_str(&sm.body);
        s.push_str("\n\n");
        s.push_str(&format!("_reasoning: {}_\n\n", sm.reasoning));
    }

    s.push_str("## 事件时间线\n\n");
    if events.is_empty() {
        s.push_str("(empty partition — no raw events in window)\n");
        return s;
    }
    for e in events {
        let preview: String = e
            .payload
            .chars()
            .map(|c| if c == '\n' { ' ' } else { c })
            .take(160)
            .collect();
        s.push_str(&format!(
            "- **{}** `{}` — {}\n",
            e.occurred_at, e.event_type, preview
        ));
    }
    s.push('\n');

    s.push_str("## 完整 Payload\n\n");
    for e in events {
        s.push_str(&format!(
            "### `{}` @ {} ({})\n\n",
            e.id, e.occurred_at, e.event_type
        ));
        s.push_str("```json\n");
        // Markdown code-fence-safe: replace any triple backticks in the
        // payload with U+02BC modifier letters so they can't terminate
        // the outer ````json fence.
        let body = e.payload.replace("```", "ʼʼʼ");
        s.push_str(&body);
        if !body.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("```\n\n");
    }

    s
}

/// Format: `{source_basename}_{YYYYMMDD-HHMMSS}_{project_basename}.md`.
/// Falls back to "unknown-project" when no project key was set.
fn suggested_filename(session: &SessionRow) -> String {
    let source = sanitize_filename_segment(&session.source_id);
    let started_compact = started_at_filename_compact(&session.started_at);
    let project = session
        .project_key
        .as_deref()
        .map(sanitize_filename_segment)
        .unwrap_or_else(|| "unknown-project".to_string());
    format!("{source}_{started_compact}_{project}.md")
}

fn started_at_filename_compact(iso: &str) -> String {
    // Strip down to ASCII digits + `Z`, then dash-separate date/time
    // to keep the stem readable (e.g. `20261016-100000Z`). A trailing
    // `Z` is preserved when present.
    let mut out = String::with_capacity(iso.len());
    for c in iso.chars() {
        if c.is_ascii_digit() || c == 'Z' {
            out.push(c);
        }
    }
    if out.len() >= 9 {
        out.insert(8, '-');
    }
    out
}

/// Strip every character that is not `[A-Za-z0-9._-]` from a string
/// to make it safe as a path segment. Capped at 64 chars to keep the
/// dialog header readable.
fn sanitize_filename_segment(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.len() > 64 {
        out.truncate(64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_segment_strips_path_separators() {
        // Leading `/` becomes `_` so we never end up with a hidden
        // leading dot in the file system. Spaces and `#` collapse to
        // `_`; the remaining alphanumerics stay intact.
        assert_eq!(
            sanitize_filename_segment("/Users/me/Project #2"),
            "_Users_me_Project__2"
        );
    }

    #[test]
    fn sanitize_filename_segment_caps_at_64() {
        let s = "x".repeat(120);
        let r = sanitize_filename_segment(&s);
        assert_eq!(r.len(), 64);
    }

    #[test]
    fn started_at_filename_compact_normalises_iso() {
        assert_eq!(
            started_at_filename_compact("2026-10-16T10:00:00Z"),
            "20261016-100000Z"
        );
    }

    #[test]
    fn render_markdown_includes_frontmatter_and_event_count() {
        let session = SessionRow {
            id: "part".into(),
            source_id: "codex".into(),
            started_at: "2026-10-16T10:00:00Z".into(),
            ended_at: "2026-10-16T10:30:00Z".into(),
            event_count: 2,
            project_key: Some("/Users/me/proj".into()),
            heuristic_summary: "2 raw events (heuristic)".into(),
            heuristic_importance: 7,
        };
        let events = vec![
            SessionEventRow {
                id: "evt_001".into(),
                event_type: "user_input".into(),
                occurred_at: "2026-10-16T10:01:00Z".into(),
                project_key: Some("/Users/me/proj".into()),
                payload: r#"{"text":"hi"}"#.into(),
            },
            SessionEventRow {
                id: "evt_002".into(),
                event_type: "assistant_message".into(),
                occurred_at: "2026-10-16T10:05:00Z".into(),
                project_key: Some("/Users/me/proj".into()),
                payload: "你好".into(),
            },
        ];
        let md = render_markdown(
            &session,
            &events,
            None,
            "part",
            "Codex",
            "0.3.3",
        );
        assert!(md.starts_with("---\n"));
        assert!(md.contains("app_version: 0.3.3"));
        assert!(md.contains("session_id: part"));
        assert!(md.contains("event_count: 2"));
        assert!(md.contains("# 2026-10-16T10:00:00Z → 2026-10-16T10:30:00Z"));
        assert!(md.contains("## 元数据"));
        assert!(md.contains("## heuristic 摘要"));
        assert!(md.contains("2 raw events (heuristic)"));
        assert!(md.contains("## 事件时间线"));
        assert!(md.contains("## 完整 Payload"));
        assert!(md.contains("### `evt_001`"));
        assert!(md.contains("```json"));
        assert!(md.contains(r#"{"text":"hi"}"#));
        // Project paths containing `/` survive frontmatter safely.
        assert!(md.contains(r#"project_key: "/Users/me/proj""#));
        // Source display name (looked-up separately, not the id) is in
        // the body title and metadata block.
        assert!(md.contains("· Codex ·"));
    }

    #[test]
    fn render_markdown_omits_summary_block_when_summary_is_none() {
        let session = SessionRow {
            id: "p".into(),
            source_id: "s".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: "2026-01-01T00:10:00Z".into(),
            event_count: 0,
            project_key: None,
            heuristic_summary: "0 raw events (heuristic)".into(),
            heuristic_importance: 1,
        };
        let md = render_markdown(&session, &[], None, "p", "s", "0.3.3");
        assert!(!md.contains("## AI 摘要"));
        assert!(md.contains("(empty partition"));
    }

    #[test]
    fn render_markdown_escapes_triple_backtick_payload() {
        let session = SessionRow {
            id: "p".into(),
            source_id: "s".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: "2026-01-01T00:10:00Z".into(),
            event_count: 1,
            project_key: None,
            heuristic_summary: "1 raw events (heuristic)".into(),
            heuristic_importance: 1,
        };
        let events = vec![SessionEventRow {
            id: "e".into(),
            event_type: "user_input".into(),
            occurred_at: "2026-01-01T00:00:30Z".into(),
            project_key: None,
            payload: "code:\n```\nconsole.log(\"hi\")\n```".into(),
        }];
        let md = render_markdown(&session, &events, None, "p", "s", "0.3.3");

        // 1. Triple backticks inside payloads are neutralised before
        //    they can close the outer ```json fence.
        assert!(md.contains("ʼʼʼ"));
        assert!(
            !md.contains("code:\n```\nconsole"),
            "raw triple backticks must NOT survive the export"
        );

        // 2. The payload block opens and closes cleanly. We use a
        //    robust end-of-document check rather than a substring
        //    count that would also pick up appearances elsewhere.
        assert!(
            md.trim_end().ends_with("```"),
            "markdown must end on a closed fence, got: ...{}",
            &md[md.len().saturating_sub(40)..]
        );

        // 3. The event section heading must reference the event id so
        //    downstream markdown readers can locate every block.
        assert!(md.contains("### `e` @ 2026-01-01T00:00:30Z (user_input)"));
    }
}

// =====================================================================
// Stage 5.5 / v0.3.5 — bulk Sessions export (multi-file zip)
//
// The single-bucket markdown export (E.4b) is great for one specific
// session, but the user's mental model of "share a week of work" sits
// at the source-filter level. This module emits every `sessions_summary`
// row that matches the requested filter as **one** zip containing a
// per-partition `.md` (re-using `render_session_markdown`) plus a
// top-level `INDEX.md` so the recipient can browse the contents
// before opening individual files.
// =====================================================================

/// Same shape as `MarkdownExport` but the payload is a base64-encoded
/// zip blob. Tauri's IPC layer is JSON-only, so binary bytes cross the
/// boundary as base64 (the frontend re-decodes and writes them via
/// `write_binary_file`).
#[derive(Debug, Clone, Serialize)]
pub struct BulkZipExport {
    /// Base64-encoded zip bytes.
    pub content_base64: String,
    pub suggested_filename: String,
    pub count: usize,
}

/// Build a multi-file zip containing one markdown per partition + a
/// top-level `INDEX.md`. Honours the same `source_filter` + `since_ms`
/// shape as the JSON/CSV bulk path; empty result sets still produce
/// a valid zip with a stub INDEX.md so the user doesn't see a
/// confusing "no rows" write.
pub fn render_sessions_bulk_zip(
    conn: &Connection,
    source_filter: Option<&str>,
    since_ms: Option<i64>,
    app_version: &str,
) -> AppResult<BulkZipExport> {
    let rows = fetch_bulk_rows(conn, source_filter, since_ms)?;
    let exported_utc = Utc::now().to_rfc3339();
    let source_label = source_filter.unwrap_or("all");

    let buf: Vec<u8> = Vec::new();
    let cursor = std::io::Cursor::new(buf);
    let mut zip = zip::ZipWriter::new(cursor);
    let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // Top-level INDEX.md — `cat`-friendly manifest of every included
    // partition. Uses the same field shape as the JSON/CSV bulk
    // export so a downstream consumer can parse it back into the
    // same struct family.
    let mut index = String::new();
    index.push_str(&format!(
        "# Sessions bulk export — {source_label}\n\n\
         - app: Agent Work Ledger\n\
         - app_version: {app_version}\n\
         - exported_utc: {exported_utc}\n\
         - source_filter: {source_label}\n\
         - session_count: {}\n\n",
        rows.len()
    ));
    if rows.is_empty() {
        index.push_str("(no sessions matched the current filter)\n");
    } else {
        index.push_str("| # | started_at | source | project | events | importance | file |\n");
        index.push_str("|---|------------|--------|---------|--------|------------|------|\n");
    }

    for (i, row) in rows.iter().enumerate() {
        // Reuse the single-bucket renderer so the per-file contents
        // match exactly what the in-app "导出单 bucket .md" button
        // would have written.
        let md = render_session_markdown(conn, &row.id, true, app_version)?;
        let inner_path = format!(
            "sessions/{stem}",
            stem = zip_member_name(row)
        );
        zip.start_file(inner_path.clone(), options)?;
        zip.write_all(md.content.as_bytes())?;

        index.push_str(&format!(
            "| {n} | {started} | {src} | {proj} | {n_events} | {imp} | `{inner}` |\n",
            n = i + 1,
            started = row.started_at,
            src = row.source_display_name,
            proj = row.project_key.as_deref().unwrap_or("(未知)"),
            n_events = row.event_count,
            imp = row.importance,
            inner = inner_path,
        ));
    }

    zip.start_file("INDEX.md", options)?;
    zip.write_all(index.as_bytes())?;

    let cursor = zip.finish()?;
    let bytes = cursor.into_inner();

    let stamp = started_at_filename_compact(&exported_utc);
    let suggested_filename = format!("sessions_{source_label}_{stamp}.zip");

    Ok(BulkZipExport {
        content_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        suggested_filename,
        count: rows.len(),
    })
}

/// Filename stem used inside the zip for one partition's `.md`.
/// Mirrors `suggested_filename` from the single-bucket export so the
/// user can re-extract a single file and have the same name they
/// would have seen from the in-app button.
fn zip_member_name(row: &BulkRow) -> String {
    let source = sanitize_filename_segment(&row.source_display_name);
    let project = row
        .project_key
        .as_deref()
        .map(sanitize_filename_segment)
        .unwrap_or_else(|| "unknown-project".to_string());
    let started = started_at_filename_compact(&row.started_at);
    format!("{source}_{started}_{project}.md")
}
//
// The single-bucket markdown export (E.4b) is great for one specific
// session, but the user's mental model of "share a week of work" sits
// at the source-filter level. This module emits every `sessions_summary`
// row that matches the requested filter as **one** file the user can
// drop into a spreadsheet (CSV) or pretty-print elsewhere (JSON).
//
// We deliberately don't dump raw event payloads here — bulk payload
// material gets heavy fast and most consumers want the metadata layer
// for triage. Single-bucket markdown export remains the path for the
// full event timeline.
// =====================================================================

/// Output format for the bulk export IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkFormat {
    Json,
    Csv,
}

impl BulkFormat {
    fn as_str(&self) -> &'static str {
        match self {
            BulkFormat::Json => "json",
            BulkFormat::Csv => "csv",
        }
    }
    fn extension(&self) -> &'static str {
        match self {
            BulkFormat::Json => "json",
            BulkFormat::Csv => "csv",
        }
    }
}

impl std::str::FromStr for BulkFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "json" => Ok(BulkFormat::Json),
            "csv" => Ok(BulkFormat::Csv),
            other => Err(format!("unknown bulk format: {other}")),
        }
    }
}

/// One row in the bulk export, regardless of format. Mirrors the
/// `sessions_summary` columns + the joined `sources.display_name` so
/// downstream consumers don't have to round-trip back to the DB.
#[derive(Debug, Serialize)]
struct BulkRow {
    id: String,
    source_id: String,
    source_display_name: String,
    started_at: String,
    ended_at: String,
    event_count: i64,
    project_key: Option<String>,
    importance: i64,
    summary: String,
}

#[derive(Debug, Serialize)]
struct BulkPayload<'a> {
    app: &'a str,
    app_version: &'a str,
    exported_utc: String,
    format: &'a str,
    total: usize,
    sessions: Vec<BulkRow>,
}

/// Render every `sessions_summary` row matching `source_filter` (or
/// all sources when `None`) and `since_ms` (or all time when `None`),
/// in `format`. Returns the same `MarkdownExport` envelope the single-
/// bucket IPC uses so the frontend only needs one path for the Save
/// dialog + write. Format is the only thing that varies.
pub fn render_sessions_bulk(
    conn: &Connection,
    format: BulkFormat,
    source_filter: Option<&str>,
    since_ms: Option<i64>,
    app_version: &str,
) -> AppResult<MarkdownExport> {
    let rows = fetch_bulk_rows(conn, source_filter, since_ms)?;
    let exported_utc = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    let payload = BulkPayload {
        app: "Agent Work Ledger",
        app_version,
        exported_utc: exported_utc.clone(),
        format: format.as_str(),
        total: rows.len(),
        sessions: rows,
    };
    let content = match format {
        BulkFormat::Json => render_bulk_json(&payload)?,
        BulkFormat::Csv => render_bulk_csv(&payload),
    };
    let stamp = started_at_filename_compact(
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            crate::connectors::now_ms(),
        )
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
        .as_str(),
    );
    let suggested_filename = match source_filter {
        Some(s) => format!(
            "sessions_{}_{}.{}",
            sanitize_filename_segment(s),
            stamp,
            format.extension()
        ),
        None => format!("sessions_all_{stamp}.{}", format.extension()),
    };
    Ok(MarkdownExport {
        content,
        suggested_filename,
    })
}

fn fetch_bulk_rows(
    conn: &Connection,
    source_filter: Option<&str>,
    since_ms: Option<i64>,
) -> AppResult<Vec<BulkRow>> {
    // Build the WHERE clause incrementally so a None filter doesn't
    // leak stray `1=1` into the SQL the user might one day inspect.
    let mut sql = String::from(
        "SELECT s.id, s.source_id, src.display_name, s.started_at,
                s.ended_at, s.event_count, s.project_key, s.summary,
                s.importance
         FROM sessions_summary s
         JOIN sources src ON src.id = s.source_id
         WHERE 1=1",
    );
    // `since_ms` is a millisecond timestamp from the UI. The DB stores
    // `started_at` as RFC3339 text, so we materialise the cutoff as a
    // string before binding — otherwise SQLite coerces the comparison
    // and the cutoff silently rounds to numeric junk (e.g. ~1.7e12).
    let since_iso: Option<String> = since_ms.map(|ms| crate::connectors::rfc3339(Some(ms)));
    if source_filter.is_some() {
        sql.push_str(" AND s.source_id = ?1");
    }
    if let Some(iso) = since_iso.as_deref() {
        if source_filter.is_some() {
            sql.push_str(" AND s.started_at >= ?2");
        } else {
            sql.push_str(" AND s.started_at >= ?1");
        }
    }
    sql.push_str(" ORDER BY s.started_at DESC");

    let mut stmt = conn.prepare(&sql)?;
    let mut out: Vec<BulkRow> = Vec::new();
    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<BulkRow> {
        Ok(BulkRow {
            id: row.get(0)?,
            source_id: row.get(1)?,
            source_display_name: row.get(2)?,
            started_at: row.get(3)?,
            ended_at: row.get(4)?,
            event_count: row.get(5)?,
            project_key: row.get(6)?,
            importance: row.get(8)?,
            summary: row.get(7)?,
        })
    };
    let rows = match (source_filter, since_iso.as_deref()) {
        (Some(sid), Some(iso)) => stmt.query_map(params![sid, iso], map_row)?,
        (Some(sid), None) => stmt.query_map(params![sid], map_row)?,
        (None, Some(iso)) => stmt.query_map(params![iso], map_row)?,
        (None, None) => stmt.query_map([], map_row)?,
    };
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn render_bulk_json(payload: &BulkPayload<'_>) -> AppResult<String> {
    Ok(serde_json::to_string_pretty(payload)?)
}

fn render_bulk_csv(payload: &BulkPayload<'_>) -> String {
    let mut s = String::new();
    // Header: keep snake_case + the same order as BulkRow so the file
    // round-trips back into `serde_json::from_str::<BulkRow>` cleanly.
    s.push_str("id,source_id,source_display_name,started_at,ended_at,event_count,project_key,importance,summary\n");
    for r in &payload.sessions {
        s.push_str(&csv_field(&r.id));
        s.push(',');
        s.push_str(&csv_field(&r.source_id));
        s.push(',');
        s.push_str(&csv_field(&r.source_display_name));
        s.push(',');
        s.push_str(&csv_field(&r.started_at));
        s.push(',');
        s.push_str(&csv_field(&r.ended_at));
        s.push(',');
        s.push_str(&r.event_count.to_string());
        s.push(',');
        s.push_str(&csv_field(r.project_key.as_deref().unwrap_or("")));
        s.push(',');
        s.push_str(&r.importance.to_string());
        s.push(',');
        s.push_str(&csv_field(&r.summary));
        s.push('\n');
    }
    s
}

/// RFC 4180 — wrap in double-quotes when the field contains `,` `"`
/// or a newline; escape embedded `"` as `""`. We use the spec's
/// CRLF line terminator so the file opens cleanly in Excel on Windows.
fn csv_field(v: &str) -> String {
    let needs_quote = v.contains(',') || v.contains('"') || v.contains('\n') || v.contains('\r');
    if !needs_quote {
        return v.to_string();
    }
    let escaped = v.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod bulk_tests {
    use super::*;
    use crate::migrate_for_tests;

    fn fixture() -> (Connection, String) {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        migrate_for_tests(&conn).expect("migrate");
        let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
        // Source + two partitions so the SQL join has at least one
        // match; we use the same id everywhere for the joined source.
        conn.execute(
            "INSERT INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
             VALUES('codex', 'test', 'Codex', '/tmp/test', 'ok', ?1, ?1)",
            params![now],
        )
        .unwrap();
        for (i, (start, end)) in [
            ("2026-10-16T10:00:00Z", "2026-10-16T10:30:00Z"),
            ("2026-10-16T11:00:00Z", "2026-10-16T11:45:00Z"),
        ]
        .iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO sessions_summary(
                    id, source_id, started_at, ended_at, event_count,
                    source_event_count, project_key, summary, importance,
                    heuristic_kind, created_at, updated_at
                 ) VALUES(?1, 'codex', ?2, ?3, 4, 4, '/p', '4 raw events (heuristic)', 6,
                         'auto', ?4, ?4)",
                params![
                    format!("part_{i}"),
                    start,
                    end,
                    now
                ],
            )
            .unwrap();
        }
        (conn, now)
    }

    #[test]
    fn bulk_csv_renders_one_row_per_partition() {
        let (conn, _) = fixture();
        let exp = render_sessions_bulk(
            &conn,
            BulkFormat::Csv,
            None,
            None,
            "0.3.4",
        )
        .expect("render");
        let lines: Vec<&str> = exp.content.lines().collect();
        assert_eq!(lines[0], "id,source_id,source_display_name,started_at,ended_at,event_count,project_key,importance,summary");
        assert_eq!(lines.len(), 3, "header + 2 rows");
        assert!(exp.content.contains("part_1"));
        assert!(exp.content.contains("part_0"));
        assert!(exp.content.contains("Codex"));
    }

    #[test]
    fn bulk_json_round_trips_with_known_fields() {
        let (conn, _) = fixture();
        let exp = render_sessions_bulk(
            &conn,
            BulkFormat::Json,
            None,
            None,
            "0.3.4",
        )
        .expect("render");
        let parsed: serde_json::Value =
            serde_json::from_str(&exp.content).expect("parse");
        assert_eq!(parsed["format"], "json");
        assert_eq!(parsed["app"], "Agent Work Ledger");
        assert_eq!(parsed["app_version"], "0.3.4");
        assert_eq!(parsed["total"], 2);
        let sessions = parsed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        let first = &sessions[0];
        assert_eq!(first["source_display_name"], "Codex");
        assert_eq!(first["event_count"], 4);
        assert_eq!(first["importance"], 6);
    }

    #[test]
    fn bulk_csv_field_escapes_commas_quotes_and_newlines() {
        // Multi-line summary triggers RFC 4180 wrapping + quote-escape.
        let nasty = "hello, \"world\"\nline2";
        let escaped = csv_field(nasty);
        assert_eq!(escaped, "\"hello, \"\"world\"\"\nline2\"");
    }

    #[test]
    fn bulk_filter_by_source_id() {
        let (conn, _) = fixture();
        let exp = render_sessions_bulk(
            &conn,
            BulkFormat::Csv,
            Some("codex"),
            None,
            "0.3.4",
        )
        .expect("render");
        // Suggested filename embeds the source id so a multi-source
        // export later stays unambiguous.
        assert!(exp.suggested_filename.contains("codex"));
        assert!(exp.content.contains("part_0"));
        assert!(exp.content.contains("part_1"));
    }

    #[test]
    fn bulk_filter_by_since_ms_excludes_older_partitions() {
        let (conn, _) = fixture();
        // 2026-10-16T10:45:00Z sits between the two partitions
        // (ended 10:30 and 11:45), so only the second row should pass.
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-10-16T10:45:00Z")
            .unwrap()
            .timestamp_millis();
        let exp = render_sessions_bulk(
            &conn,
            BulkFormat::Csv,
            None,
            Some(cutoff),
            "0.3.4",
        )
        .expect("render");
        let lines: Vec<&str> = exp.content.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 row kept");
        assert!(exp.content.contains("part_1"));
        assert!(!exp.content.contains("part_0"));
    }

    // -----------------------------------------------------------------
    // Stage 5.5 / v0.3.5: bulk .md zip
    // -----------------------------------------------------------------

    /// Decode the IPC payload back into raw zip bytes and assert it
    /// round-trips. We intentionally do not assert on per-file
    /// contents here — `render_session_markdown` already has its
    /// own frontend-content tests; this just guards the zip layout.
    #[test]
    fn bulk_zip_produces_parseable_archive_with_index() {
        let (conn, _) = fixture();
        let exp = render_sessions_bulk_zip(&conn, None, None, "0.3.5").expect("zip");
        assert_eq!(exp.count, 2);
        assert!(exp.suggested_filename.starts_with("sessions_all_"));
        assert!(exp.suggested_filename.ends_with(".zip"));

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&exp.content_base64)
            .expect("base64 decode");
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).expect("zip parse");

        // Top-level INDEX.md + one .md per fixture partition.
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "INDEX.md"));
        assert_eq!(
            names.iter().filter(|n| n.starts_with("sessions/")).count(),
            2
        );

        // INDEX.md carries the manifest table.
        let mut index_str = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("INDEX.md").unwrap(),
            &mut index_str,
        )
        .unwrap();
        assert!(index_str.contains("# Sessions bulk export — all"));
        assert!(index_str.contains("source_filter: all"));
        assert!(index_str.contains("session_count: 2"));
        assert!(index_str.contains("| 1 |"));
        assert!(index_str.contains("| 2 |"));
    }

    /// An empty filter (e.g. a future `since_ms` that matches nothing)
    /// should still produce a valid zip — just INDEX.md with the
    /// "no sessions matched" stub.
    #[test]
    fn bulk_zip_handles_empty_filter_gracefully() {
        let (conn, _) = fixture();
        // Cutoff well after every fixture partition ended.
        let cutoff = chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        let exp =
            render_sessions_bulk_zip(&conn, None, Some(cutoff), "0.3.5").expect("zip empty");
        assert_eq!(exp.count, 0);
        // suggested_filename is the export stamp, not the cutoff —
        // it still anchors a `.zip` path the dialog can open with.
        assert!(exp.suggested_filename.starts_with("sessions_all_"));
        assert!(exp.suggested_filename.ends_with(".zip"));

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&exp.content_base64)
            .expect("base64 decode");
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).expect("zip parse");
        assert_eq!(archive.len(), 1, "INDEX.md only when no rows");
        let mut name = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("INDEX.md").unwrap(),
            &mut name,
        )
        .unwrap();
        assert!(name.contains("session_count: 0"));
        assert!(name.contains("no sessions matched"));
    }
}
