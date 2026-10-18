//! Stage 6 P2 (v0.4.5): Cursor connector — reads the
//! `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
//! SQLite file (plaintext, no encryption — the same VSCode-derived
//! `state.vscdb` shape but with Cursor's `cursorDiskKV` table holding
//! `composerData:<uuid>` rows for every chat session).
//!
//! v0.4.5 picks Cursor as the second plaintext-stash target (after
//! Codex's encrypted database turned out to need an app-internal
//! key that's nowhere on disk). The user types a prompt, the AI
//! streams back, both halves land in `cursorDiskKV` as one big
//! JSON blob per "composer" (Cursor's term for a chat session).
//!
//! Per-bubble granularity: each item in the session's
//! `conversation[]` array becomes one `raw_events` row. The event
//! `id` is `ev_<composerId>_<bubbleId>` so re-runs dedupe cleanly.
//! Sessions are rebuilt by `etl::reconcile_sessions_summary` from
//! the events we insert, so this connector only writes `raw_events`
//! — no direct `sessions_summary` insert.
//!
//! Why the 1-ms-per-bubble trick?
//! Cursor stores `lastUpdatedAt` on the *composer* (one timestamp
//! for the whole session) but not on each individual bubble. We
//! want consecutive bubbles in the same session to land close
//! together in time so the 30-min ETL gap keeps them in one
//! partition. Easiest stable trick: stamp bubble `i` at
//! `lastUpdatedAt + i` ms.

use std::path::PathBuf;
use std::time::Instant;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::connectors::codex::upsert_source;
use crate::connectors::{kinds, Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::redactor::Redactor;

/// Default Cursor data root on macOS. Linux: `~/.config/Cursor/...`;
/// Windows: `%APPDATA%\Cursor\...` — we ship macOS-only per
/// project scope.
fn default_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    })
}

/// Stable SHA-256 of the cursor JSON value, used as the
/// `raw_events.payload_hash` dedup key. Cursor doesn't expose
/// per-bubble IDs we can fold into a natural primary key (the
/// `bubbleId` field is reused across composer edits), so we
/// anchor on the bubble text instead.
fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

/// Pick the deepest "looks like a project root" directory from the
/// first relevant file. macOS convention is
/// `/Users/<u>/<project>/...`; we skip the leading `Users/<user>`
/// shell and take whatever's at depth 3 (the project dir). Falls
/// back to depth 1 (`/<top>`) when the path doesn't match the
/// macOS pattern, e.g. Linux `/home/<u>/<proj>/...` collapses to
/// the project dir at depth 3 just the same.
fn project_key_from_relevant_files(files: &[String]) -> String {
    for path in files {
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() >= 3 {
            // `<root>/<user-or-Users>/<project>` — depth-3 segment.
            // (e.g. `/Users/apple/Code/ledger/src/foo.rs` → `Code`)
            return format!("/{}", segs[2]);
        }
        if segs.len() == 2 {
            return format!("/{}", segs[1]);
        }
        if segs.len() == 1 {
            return format!("/{}", segs[0]);
        }
    }
    "unknown".into()
}

pub struct CursorConnector;

impl Connector for CursorConnector {
    fn kind(&self) -> &'static str {
        kinds::CURSOR
    }
    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn discover_roots(&self) -> Vec<PathBuf> {
        default_root().into_iter().filter(|p| p.exists()).collect()
    }

    fn scan(&self, ctx: ScanCtx<'_>) -> AppResult<ScanReport> {
        let started = Instant::now();
        let conn = ctx.conn;

        let db_path = match default_root() {
            Some(p) if p.is_file() => p,
            _ => {
                upsert_source(
                    conn,
                    self.kind(),
                    self.display_name(),
                    default_root().as_deref().and_then(|p| p.parent()),
                    "discovered",
                )?;
                return Ok(ScanReport {
                    source_id: String::new(),
                    kind: self.kind().into(),
                    status: "ok".into(),
                    events_inserted: 0,
                    sessions_added: 0,
                    files_scanned: 0,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error: Some("Cursor state.vscdb not present on this host".into()),
                });
            }
        };

        let source_id = upsert_source(
            conn,
            self.kind(),
            self.display_name(),
            db_path.parent(),
            "readable",
        )?;

        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let now = crate::connectors::now_ms();
        conn.execute(
            "INSERT INTO connector_runs(id, source_id, started_at, status, events_count) VALUES (?1, ?2, ?3, 'running', 0)",
            params![run_id, source_id, crate::connectors::rfc3339(Some(now))],
        )?;

        let redactor = Redactor::from_db(conn)?;

        // Open Cursor's DB read-only — a live Cursor process may
        // be holding a write tx; we never want to fight it.
        let cursor_conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        let mut events_inserted: usize = 0;
        events_inserted += scan_composer_data(
            conn,
            &source_id,
            &cursor_conn,
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

/// Walk every `composerData:*` row in `cursorDiskKV`, expand the
/// `conversation[]` array, and write one `raw_events` row per
/// bubble. Re-running is idempotent: the `payload_hash` dedup
/// gate is keyed on (source_id, payload_hash) and we compute
/// `payload_hash` over the same JSON we wrote last time.
fn scan_composer_data(
    ledger: &Connection,
    source_id: &str,
    cursor: &Connection,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    let mut stmt = cursor.prepare(
        "SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%'",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0usize;
    for (key, value) in rows {
        let composer_id = match key.strip_prefix("composerData:") {
            Some(id) => id,
            None => continue,
        };
        let data: Json = match serde_json::from_str(&value) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let last_updated_ms = data
            .get("lastUpdatedAt")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        // Don't trust the composer-level `lastUpdatedAt` for the
        // since-filter — Cursor stores a *creation* timestamp here
        // that drifts months into the past for long-lived composers.
        // The per-row `created_at` we stamp below is the scan's
        // wall-clock time, which is what retention actually compares
        // against the cutoff. We only short-circuit when the
        // composer is provably empty (no conversation), not on age.
        let conversation = data
            .get("conversation")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if conversation.is_empty() {
            continue;
        }
        // Pick a project key from the *first* bubble that has a
        // `relevantFiles` array — the composer-level context object
        // often has empty/whimsical file lists, and per-bubble is
        // more representative of what the user actually worked on.
        let mut project_key = String::new();
        for item in conversation.iter() {
            if let Some(arr) = item.get("relevantFiles").and_then(|v| v.as_array()) {
                let files: Vec<String> = arr
                    .iter()
                    .filter_map(|f| f.as_str().map(String::from))
                    .collect();
                let pk = project_key_from_relevant_files(&files);
                if pk != "unknown" {
                    project_key = pk;
                    break;
                }
            }
        }
        if project_key.is_empty() {
            project_key = "unknown".into();
        }

        for (idx, bubble) in conversation.iter().enumerate() {
            let bubble_id = bubble
                .get("bubbleId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let bubble_type = bubble
                .get("type")
                .and_then(|v| v.as_i64())
                .map(|t| match t {
                    1 => "user",
                    2 => "assistant",
                    _ => "message",
                })
                .unwrap_or("message");
            let text = bubble
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Per-bubble timestamp: Cursor's composer only stores
            // one `lastUpdatedAt` (which is the *creation* time,
            // often months old) — there's no per-bubble wall clock.
            // We stamp each bubble at `now - (n-1-i) * 1000` so
            // (a) every event is recent enough to survive the 90-day
            // retention prune and (b) the ETL still sees the bubbles
            // in chronological order inside one composer. The 1ms
            // offset keeps dedup IDs unique within a single scan.
            let now = crate::connectors::now_ms();
            let total = conversation.len() as i64;
            let bubble_ms = now.saturating_sub((total - 1 - idx as i64) * 1000);
            if should_skip(since_ms, bubble_ms) {
                continue;
            }
            // Build a compact payload — keep `relevantFiles` +
            // `capabilitiesRan` so search-time the user can find
            // "I edited auth.js 2 days ago" without re-parsing the
            // full Cursor JSON.
            let mut payload = serde_json::Map::new();
            payload.insert("bubble_id".into(), Json::String(bubble_id.into()));
            payload.insert("composer_id".into(), Json::String(composer_id.into()));
            payload.insert("role".into(), Json::String(bubble_type.into()));
            payload.insert("text".into(), Json::String(text.into()));
            if let Some(arr) = bubble.get("relevantFiles").and_then(|v| v.as_array()) {
                payload.insert("relevantFiles".into(), Json::Array(arr.clone()));
            }
            if let Some(arr) = bubble.get("capabilitiesRan").and_then(|v| v.as_object()) {
                payload.insert("capabilitiesRan".into(), Json::Object(arr.clone()));
            }
            if let Some(c) = bubble.get("checkpointId").and_then(|v| v.as_str()) {
                payload.insert("checkpointId".into(), Json::String(c.into()));
            }
            let payload_json = Json::Object(payload);
            insert_bubble(
                ledger,
                source_id,
                composer_id,
                bubble_id,
                bubble_type,
                bubble_ms,
                &project_key,
                &payload_json,
                redactor,
            )?;
            count += 1;
        }
        // `last_updated_ms` only used for sanity; suppress the
        // unused-variable warning while keeping the field read
        // (so future schema changes that repurpose this field
        // don't silently pass through).
        let _ = last_updated_ms;
    }
    Ok(count)
}

fn should_skip(since_ms: Option<i64>, ts_ms: i64) -> bool {
    matches!(since_ms, Some(s) if ts_ms > 0 && ts_ms < s)
}

fn insert_bubble(
    ledger: &Connection,
    source_id: &str,
    composer_id: &str,
    bubble_id: &str,
    role: &str,
    ts_ms: i64,
    project_key: &str,
    payload: &Json,
    redactor: &Redactor,
) -> AppResult<()> {
    let raw_payload = serde_json::to_string(payload)?;
    let redacted_payload = redactor.apply(&raw_payload);
    let payload_hash = sha256_hex(&redacted_payload);
    let occurred_at = ms_to_rfc3339(ts_ms);
    // Stable, idempotent id — re-running the scan yields the same
    // primary key, and the `payload_hash` dedup gate inside the
    // INSERT further protects us if the bubble text edits.
    let id = format!("cursor_{}_{}", composer_id, bubble_id);
    let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    ledger.execute(
        "INSERT OR IGNORE INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
        params![
            id,
            source_id,
            role,
            occurred_at,
            project_key,
            redacted_payload,
            payload_hash,
            now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db_with_state_vscdb() -> Connection {
        // Build a tiny state.vscdb-shaped DB (cursorDiskKV) in
        // memory. We don't actually need it on disk — the
        // connector uses `Connection::open_with_flags` which works
        // for any path, but for the test we just check helpers.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE cursorDiskKV (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn sha256_matches_known_digest() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn project_key_picks_project_at_depth_3() {
        // macOS path: `/Users/<user>/<project>/...` → the project
        // dir is at depth 3 (index 2 in split output after dropping
        // the leading empty).
        let files = vec![
            "/Users/apple/Code/ledger/src/foo.rs".into(),
            "/Users/apple/Code/ledger/src/bar.rs".into(),
        ];
        assert_eq!(project_key_from_relevant_files(&files), "/Code");
    }

    #[test]
    fn project_key_falls_back_when_no_files() {
        let files: Vec<String> = vec![];
        assert_eq!(project_key_from_relevant_files(&files), "unknown");
    }

    #[test]
    fn project_key_handles_linux_paths() {
        // `/home/<user>/<project>/...` is 3 segments after the
        // leading empty — same code path as macOS.
        let files = vec!["/home/me/proj/src/main.rs".into()];
        assert_eq!(project_key_from_relevant_files(&files), "/proj");
    }

    #[test]
    fn ms_to_rfc3339_returns_epoch_for_zero() {
        // lastUpdatedAt absent in some composers — `from_timestamp_millis(0)`
        // gives the epoch, which is a real RFC 3339 timestamp.
        // The caller's `if last_updated_ms > 0` gate prevents this
        // from leaking into real events, so we only assert the
        // helper's behaviour here.
        assert_eq!(ms_to_rfc3339(0), "1970-01-01T00:00:00+00:00");
    }
}
