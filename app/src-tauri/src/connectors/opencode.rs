//! OpenCode connector — reads `~/.local/share/opencode/opencode.db`.
//!
//! OpenCode uses a Drizzle-managed SQLite file as its primary store. We open
//! it **read-only** so concurrent runs of the live OpenCode process never
//! conflict with us. Scans cover `session` / `message` / `part` / `todo`
//! and skip empty tables (e.g. `event`, `session_message`, `session_input`,
//! `account`, `credential`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::connectors::codex::upsert_source;
use crate::connectors::{kinds, should_skip_ms, Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::redactor::Redactor;

/// Default OpenCode data root on macOS. Other platforms: %LOCALAPPDATA%.
fn default_roots() -> Vec<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        vec![home.join(".local").join("share").join("opencode")]
    } else {
        Vec::new()
    }
}

/// Resolve the on-disk SQLite file (1) for the OpenCode data root.
fn resolve_db(root: &Path) -> Option<PathBuf> {
    let p = root.join("opencode.db");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// One row from the live OpenCode SQLite. Holds the raw `data` JSON plus a
/// foreign-key reference back to the owning session so we can resolve the
/// project directory.
#[derive(Debug)]
struct OpenCodeRow {
    id: String,
    session_id: String,
    time_created: i64,
    data: Json,
}

pub struct OpenCodeConnector;

impl Connector for OpenCodeConnector {
    fn kind(&self) -> &'static str {
        kinds::OPENCODE
    }
    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn discover_roots(&self) -> Vec<PathBuf> {
        default_roots()
            .into_iter()
            .filter(|p| p.exists())
            .collect()
    }

    fn scan(&self, ctx: ScanCtx<'_>) -> AppResult<ScanReport> {
        let started = Instant::now();
        let conn = ctx.conn;

        let roots = self.discover_roots();
        let root = match roots.first() {
            Some(r) => r.clone(),
            None => {
                upsert_source(conn, self.kind(), self.display_name(), None, "discovered")?;
                return Ok(ScanReport {
                    source_id: String::new(),
                    kind: self.kind().into(),
                    status: "ok".into(),
                    events_inserted: 0,
                    sessions_added: 0,
                    files_scanned: 0,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error: Some("~/.local/share/opencode not present on this host".into()),
                });
            }
        };
        let db_path = match resolve_db(&root) {
            Some(p) => p,
            None => {
                upsert_source(conn, self.kind(), self.display_name(), Some(&root), "readable")?;
                return Ok(ScanReport {
                    source_id: String::new(),
                    kind: self.kind().into(),
                    status: "ok".into(),
                    events_inserted: 0,
                    sessions_added: 0,
                    files_scanned: 0,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error: Some("opencode.db not present".into()),
                });
            }
        };

        let source_id = upsert_source(
            conn,
            self.kind(),
            self.display_name(),
            Some(&root),
            "readable",
        )?;

        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let now = crate::connectors::now_ms();
        conn.execute(
            "INSERT INTO connector_runs(id, source_id, started_at, status, events_count) VALUES (?1, ?2, ?3, 'running', 0)",
            params![run_id, source_id, crate::connectors::rfc3339(Some(now))],
        )?;

        let redactor = Redactor::from_db(conn)?;

        // 1. Open OpenCode's DB read-only. A live OpenCode process may be
        //    holding a write tx; we never want to fight it.
        let opencode_conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        // 2. Read every session → build a session_id → (directory, project)
        //    map we can use to fill `project_key` on events.
        let session_lookup = read_session_directory_map(&opencode_conn)?;

        // 3. Run the table scans. ETL (Phase B.4) handles sessions_summary;
        //    we only insert raw_events here.
        let mut events_inserted: usize = 0;

        events_inserted += scan_messages(
            conn,
            &source_id,
            &opencode_conn,
            &session_lookup,
            &redactor,
            ctx.since_ms,
        )?;
        events_inserted += scan_parts(
            conn,
            &source_id,
            &opencode_conn,
            &session_lookup,
            &redactor,
            ctx.since_ms,
        )?;
        events_inserted += scan_todos(
            conn,
            &source_id,
            &opencode_conn,
            &session_lookup,
            &redactor,
            ctx.since_ms,
        )?;

        let events_count = events_inserted as i64;
        let ended_at = crate::connectors::now_ms();
        conn.execute(
            "UPDATE connector_runs SET ended_at = ?1, status = 'ok', events_count = ?2 WHERE id = ?3",
            params![
                crate::connectors::rfc3339(Some(ended_at)),
                events_count,
                run_id
            ],
        )?;
        conn.execute(
            "UPDATE sources SET last_scan_at = ?1, status = 'readable', updated_at = ?1 WHERE id = ?2",
            params![crate::connectors::rfc3339(Some(ended_at)), source_id],
        )?;

        Ok(ScanReport {
            source_id,
            kind: self.kind().into(),
            status: "ok".into(),
            events_inserted: events_count,
            sessions_added: 0,
            files_scanned: 1,
            duration_ms: started.elapsed().as_millis() as i64,
            error: None,
        })
    }
}

/// Build `session_id → directory` so we can stamp `project_key` on every
/// event without re-querying per row.
fn read_session_directory_map(conn: &Connection) -> AppResult<std::collections::HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT id, directory FROM session")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

fn scan_messages(
    ledger: &Connection,
    source_id: &str,
    opencode: &Connection,
    session_lookup: &std::collections::HashMap<String, String>,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    let mut stmt = opencode.prepare(
        "SELECT id, session_id, time_created, data FROM message ORDER BY time_created ASC",
    )?;
    let rows: Vec<OpenCodeRow> = stmt
        .query_map([], |row| {
            let data_str: String = row.get(3)?;
            let data: Json = serde_json::from_str(&data_str).unwrap_or(Json::Null);
            Ok(OpenCodeRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                time_created: row.get(2)?,
                data,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0usize;
    for row in rows {
        let project_key = session_lookup
            .get(&row.session_id)
            .cloned()
            .unwrap_or_else(|| "unknown".into());
        let role = row
            .data
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let event_type = map_message_role(role);
        if should_skip_ms(since_ms, row.time_created) {
            continue;
        }
        insert_event(
            ledger,
            source_id,
            &row.id,
            &row.session_id,
            &event_type,
            row.time_created,
            &project_key,
            &row.data,
            redactor,
        )?;
        count += 1;
    }
    Ok(count)
}

fn scan_parts(
    ledger: &Connection,
    source_id: &str,
    opencode: &Connection,
    session_lookup: &std::collections::HashMap<String, String>,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    let mut stmt = opencode.prepare(
        "SELECT id, session_id, time_created, data FROM part ORDER BY time_created ASC",
    )?;
    let rows: Vec<OpenCodeRow> = stmt
        .query_map([], |row| {
            let data_str: String = row.get(3)?;
            let data: Json = serde_json::from_str(&data_str).unwrap_or(Json::Null);
            Ok(OpenCodeRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                time_created: row.get(2)?,
                data,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0usize;
    for row in rows {
        let project_key = session_lookup
            .get(&row.session_id)
            .cloned()
            .unwrap_or_else(|| "unknown".into());
        let ptype = row
            .data
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let event_type = map_part_type(ptype);
        if should_skip_ms(since_ms, row.time_created) {
            continue;
        }
        insert_event(
            ledger,
            source_id,
            &row.id,
            &row.session_id,
            &event_type,
            row.time_created,
            &project_key,
            &row.data,
            redactor,
        )?;
        count += 1;
    }
    Ok(count)
}

fn scan_todos(
    ledger: &Connection,
    source_id: &str,
    opencode: &Connection,
    session_lookup: &std::collections::HashMap<String, String>,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    let mut stmt = opencode.prepare(
        "SELECT session_id, content, status, priority, position, time_created FROM todo ORDER BY time_created ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    let mut count = 0usize;
    for (session_id, content, status, priority, position, time_created) in rows {
        let project_key = session_lookup
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| "unknown".into());
        let id = format!("todo_{}_{}", session_id, position);
        let data = serde_json::json!({
            "content": content,
            "status": status,
            "priority": priority,
            "position": position,
        });
        if should_skip_ms(since_ms, time_created) {
            continue;
        }
        insert_event(
            ledger,
            source_id,
            &id,
            &session_id,
            "task",
            time_created,
            &project_key,
            &data,
            redactor,
        )?;
        count += 1;
    }
    Ok(count)
}

fn insert_event(
    ledger: &Connection,
    source_id: &str,
    row_id: &str,
    session_id: &str,
    event_type: &str,
    time_created_ms: i64,
    project_key: &str,
    data: &Json,
    redactor: &Redactor,
) -> AppResult<()> {
    let raw_payload = serde_json::to_string(data)?;
    let redacted_payload = redactor.apply(&raw_payload);
    let occurred_at = ms_to_rfc3339(time_created_ms);
    let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    let payload_hash = sha256_hex(&redacted_payload);

    ledger.execute(
        "INSERT OR IGNORE INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
        params![
            id,
            source_id,
            event_type,
            occurred_at,
            project_key,
            redacted_payload,
            payload_hash,
            now
        ],
    )?;
    // Hint: row_id / session_id not stored separately yet; reserved for
    // 1-G (Timeline cross-reference).
    let _ = (row_id, session_id);
    Ok(())
}

/// Compute a stable, lowercase hex SHA-256 digest of an arbitrary string.
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Convert an OpenCode millisecond Unix timestamp into our RFC 3339 string.
fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

fn map_message_role(role: &str) -> String {
    match role {
        "user" | "human" => "message".into(),
        "assistant" | "ai" => "message".into(),
        "system" => "system".into(),
        other => format!("other:{other}"),
    }
}

fn map_part_type(ptype: &str) -> String {
    match ptype {
        "text" => "message".into(),
        "reasoning" => "message".into(),
        "tool" | "tool_use" | "tool_result" | "tool-call" | "tool-result" => "tool_call".into(),
        "step-start" | "step-finish" | "step_start" | "step_finish" => "progress".into(),
        "patch" | "snapshot" | "file" => "file_change".into(),
        "agent" | "subtask" => "system".into(),
        other => format!("other:{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_digest() {
        let got = sha256_hex("hello");
        assert_eq!(
            got,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn map_message_role_known() {
        assert_eq!(map_message_role("user"), "message");
        assert_eq!(map_message_role("assistant"), "message");
        assert_eq!(map_message_role("system"), "system");
    }

    #[test]
    fn map_part_type_known() {
        assert_eq!(map_part_type("text"), "message");
        assert_eq!(map_part_type("tool"), "tool_call");
        assert_eq!(map_part_type("reasoning"), "message");
        assert_eq!(map_part_type("step-start"), "progress");
        assert_eq!(map_part_type("file"), "file_change");
        assert_eq!(map_part_type("patch"), "file_change");
    }

    #[test]
    fn map_unknowns_keep_data() {
        assert_eq!(map_message_role("weirdo"), "other:weirdo");
        assert_eq!(map_part_type("future-thing"), "other:future-thing");
    }
}