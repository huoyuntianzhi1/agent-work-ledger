//! WorkBuddy connector — reads `~/Library/Application Support/WorkBuddyExtension/`.
//!
//! WorkBuddy (Tencent's IDE agent, formerly CodeBuddy CN) stores conversation
//! snapshots as JSON on disk under `Data/<machine>/VSCode/<machine>/`:
//! - `check-point/<workspace-hash>/<conversation-id>/<version-id>/meta.json`
//!   captures one snapshot per agent run; the file includes the user's
//!   prompt (`input`), workspace path, expert/agent role, and config flags.
//! - `plan-task/<workspace-hash>/<conversation-id>/meta.json` holds the
//!   todo list for a conversation.
//!
//! Only **read** access. Nothing here mutates WorkBuddy files.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{params, Connection};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::connectors::codex::upsert_source;
use crate::connectors::{kinds, should_skip_iso, should_skip_ms, Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::redactor::Redactor;

/// Default WorkBuddy data root on macOS.
fn default_roots() -> Vec<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        vec![home
            .join("Library")
            .join("Application Support")
            .join("WorkBuddyExtension")]
    } else {
        Vec::new()
    }
}

/// Resolve all candidate `<machine>/VSCode/<machine>/check-point` and
/// `<machine>/VSCode/<machine>/plan-task` roots beneath a WorkBuddy
/// `Data/` directory. WorkBuddy may have multiple machine profiles.
fn resolve_data_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let data = root.join("Data");
    if !data.is_dir() {
        return out;
    }
    let entries = match std::fs::read_dir(&data) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // First-level: <machine>/VSCode/<machine>/...
        for sub in std::fs::read_dir(path.join("VSCode")).into_iter().flatten().flatten() {
            if sub.path().is_dir() {
                out.push(sub.path());
            }
        }
    }
    out
}

pub struct WorkBuddyConnector;

impl Connector for WorkBuddyConnector {
    fn kind(&self) -> &'static str {
        kinds::WORKBUDDY
    }
    fn display_name(&self) -> &'static str {
        "WorkBuddy"
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
                    error: Some("~/Library/Application Support/WorkBuddyExtension not present".into()),
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

        // Discover every machine's VSCode data dir.
        let data_dirs = resolve_data_dirs(&root);
        let mut events_inserted: usize = 0;
        let mut files_scanned: i64 = 0;

        for data_dir in &data_dirs {
            // 1. Walk check-point/<workspace>/<conversation>/<version>/meta.json.
            let checkpoint_root = data_dir.join("check-point");
            if checkpoint_root.is_dir() {
                let walker = walkdir::WalkDir::new(&checkpoint_root)
                    .into_iter()
                    .filter_map(|e| e.ok());
                for entry in walker {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let path = entry.path();
                    if path.file_name().and_then(|n| n.to_str()) != Some("meta.json") {
                        continue;
                    }
                    files_scanned += 1;
                    if redactor.is_path_excluded(conn, path)? {
                        continue;
                    }
                    match process_checkpoint(conn, &source_id, path, &redactor, ctx.since_ms) {
                        Ok(ev) => {
                            events_inserted += ev;
                        }
                        Err(e) => {
                            tracing::warn!(file = %path.display(), error = %e, "workbuddy: failed checkpoint meta, skipping");
                        }
                    }
                }
            }

            // 2. Walk plan-task/<workspace>/<conversation>/meta.json (one
            //    per conversation).
            let plan_root = data_dir.join("plan-task");
            if plan_root.is_dir() {
                let walker = walkdir::WalkDir::new(&plan_root)
                    .into_iter()
                    .filter_map(|e| e.ok());
                for entry in walker {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let path = entry.path();
                    if path.file_name().and_then(|n| n.to_str()) != Some("meta.json") {
                        continue;
                    }
                    files_scanned += 1;
                    if redactor.is_path_excluded(conn, path)? {
                        continue;
                    }
                    match process_plan(conn, &source_id, path, &redactor, ctx.since_ms) {
                        Ok(ev) => {
                            events_inserted += ev;
                        }
                        Err(e) => {
                            tracing::warn!(file = %path.display(), error = %e, "workbuddy: failed plan meta, skipping");
                        }
                    }
                }
            }
        }

        // v0.2: ETL (Phase B.4) clusters raw_events into sessions_summary;
        // connector no longer writes v0.1 sessions rows directly.

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
            files_scanned,
            duration_ms: started.elapsed().as_millis() as i64,
            error: None,
        })
    }
}

/// Parse a single `check-point/.../meta.json` and emit one event per
/// snapshot.
///
/// `since_ms` skips check-points older than the caller-supplied cutoff.
/// WorkBuddy stores `createTime` as a millisecond Unix timestamp, so the
/// comparison is direct.
fn process_checkpoint(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    let bytes = std::fs::read(path)?;
    let raw_payload = String::from_utf8_lossy(&bytes).into_owned();
    let value: Json = serde_json::from_str(&raw_payload).unwrap_or(Json::Null);

    // Pull the most useful fields for grouping and timeline display.
    let version_id = value
        .get("versionId")
        .and_then(|v| v.as_str())
        .map(String::from);
    let conversation_id = value
        .get("conversationId")
        .and_then(|v| v.as_str())
        .map(String::from);
    let create_time_ms = value.get("createTime").and_then(|v| v.as_i64()).unwrap_or(0);

    // Workspace + user input live under context[].userContext.value.*.
    let mut workspace = String::from("unknown");
    let mut user_input: Option<String> = None;
    let mut agent_name: Option<String> = None;
    let mut model_name: Option<String> = None;
    if let Some(context) = value.get("context").and_then(|v| v.as_array()) {
        for ctx in context {
            let name = ctx.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name == "agentName" {
                agent_name = ctx.get("value").and_then(|v| v.as_str()).map(String::from);
            } else if name == "modelName" {
                model_name = ctx.get("value").and_then(|v| v.as_str()).map(String::from);
            } else if name == "userContext" {
                if let Some(uc) = ctx.get("value") {
                    workspace = uc
                        .get("workspace")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or(workspace);
                    user_input = uc
                        .get("input")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
        }
    }

    // Stamp an enriched payload so the UI can show a one-liner without
    // parsing the full meta blob.
    let mut summary = serde_json::Map::new();
    summary.insert("workspace".into(), Json::String(workspace.clone()));
    if let Some(uid) = user_input.clone() {
        summary.insert("input".into(), Json::String(uid));
    }
    if let Some(an) = agent_name {
        summary.insert("agent_name".into(), Json::String(an));
    }
    if let Some(mn) = model_name {
        summary.insert("model_name".into(), Json::String(mn));
    }
    if let Some(vid) = version_id.clone() {
        summary.insert("version_id".into(), Json::String(vid));
    }
    summary.insert("raw".into(), value.clone());
    let enriched = Json::Object(summary);
    let redacted_payload = redactor.apply(&serde_json::to_string(&enriched)?);

    let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    let occurred_at = ms_to_rfc3339(create_time_ms);
    let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    let payload_hash = sha256_hex(&redacted_payload);

    if should_skip_ms(since_ms, create_time_ms) {
        return Ok(0);
    }

    conn.execute(
        "INSERT OR IGNORE INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
        params![
            id,
            source_id,
            "task_snapshot",
            occurred_at,
            workspace,
            redacted_payload,
            payload_hash,
            now
        ],
    )?;
    Ok(1)
}

/// Parse a `plan-task/.../meta.json` and emit one event per todo item.
///
/// `since_ms` skips todo rows whose `updatedAt` (RFC-3339 string) is
/// older than the cutoff.
fn process_plan(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    let bytes = std::fs::read(path)?;
    let raw_payload = String::from_utf8_lossy(&bytes).into_owned();
    let value: Json = serde_json::from_str(&raw_payload).unwrap_or(Json::Null);

    let conversation_id = path
        .components()
        .nth(path.components().count().saturating_sub(2))
        .and_then(|c| c.as_os_str().to_str())
        .map(String::from);

    let items = value
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut count = 0usize;
    for item in items {
        let item_id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        let status = item
            .get("status")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "unknown".into());
        let updated_at = item
            .get("updatedAt")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| crate::connectors::rfc3339(Some(crate::connectors::now_ms())));

        if should_skip_iso(since_ms, &updated_at) {
            continue;
        }

        let data = serde_json::json!({
            "id": item_id,
            "title": title,
            "status": status,
            "source": "plan-task",
        });
        let redacted_payload = redactor.apply(&serde_json::to_string(&data)?);

        let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
        let payload_hash = sha256_hex(&redacted_payload);
        let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));

        conn.execute(
            "INSERT OR IGNORE INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
            params![
                id,
                source_id,
                "plan_item",
                updated_at,
                "unknown",
                redacted_payload,
                payload_hash,
                now
            ],
        )?;
        count += 1;
    }
    Ok(count)
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

/// Convert a millisecond Unix timestamp into our RFC 3339 string.
fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
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
}

// =====================================================================
// WorkBuddy Web — Chromium IndexedDB (Stage 4-D.5a)
//
// Tencent ships WorkBuddy as a desktop IDE (the local check-point +
// plan-task tree we already scan above) AND as a web app hosted at
// workbuddy.cn / workbuddy.com. 王总's "昨天我还在用,怎么没数据" was
// because 王总 was actually using the web app, not the desktop
// extension — the desktop extension stopped writing on 2026-05-15 but
// the Chrome IndexedDB at
// `~/Library/Application Support/Google/Chrome/Default/IndexedDB/https_www.workbuddy.cn_0.indexeddb.leveldb/`
// was last touched 2026-10-16 08:54 (the day before the report).
//
// We use the same Chromium IDB walker the Doubao connector uses
// (`crate::idb::iter_store`). Like Doubao / MiniMax, we emit one raw
// event per stored record whose key tail resolves to a UTF-16 object
// store name — V8 IndexedDB value decoding is still a follow-up so the
// downstream AI summariser is what extracts the actual conversation
// transcript. The point of this connector is to make sure the records
// show up in raw_events at all; before this change the only audit trail
// the user had was "WorkBuddyExtension/ 未在写入" with zero signal that
// the Web client was the actual source of truth.
//
// Both connectors share the `kinds::WORKBUDDY` kind so they surface
// as one agent in the UI. We register one source row per discovered
// LevelDB root (keyed on root_path, idempotent) so multiple browsers
// coexist.
// =====================================================================

mod web {
    use std::path::{Path, PathBuf};

    use chrono::Utc;
    use rusqlite::{params, Connection};
    use sha2::{Digest, Sha256};
    use walkdir::WalkDir;

    use crate::connectors::{kinds, Connector, ScanCtx, ScanReport};
    use crate::error::AppResult;
    use crate::idb;

    /// Chromium-style IndexedDB roots we know about. Each tuple is
    /// `(browser, profile, indexeddb_root)` — we walk two levels deep
    /// for any child directory whose name ends in
    /// `.indexeddb.leveldb` and whose name contains `workbuddy`.
    const CHROMIUM_BROWSER_DIRS: &[&str] = &[
        // Default Chrome profile (the one 王总 actually uses).
        "Library/Application Support/Google/Chrome/Default/IndexedDB",
        // Edge / Brave / Arc on macOS, in case the user has them.
        "Library/Application Support/Microsoft Edge/Default/IndexedDB",
        "Library/Application Support/BraveSoftware/Brave-Browser/Default/IndexedDB",
        // Arc is Chromium-based too.
        "Library/Application Support/Arc/User Data/Default/IndexedDB",
    ];

    /// Substring a Chromium IndexedDB directory name must contain for
    /// us to treat it as a WorkBuddy web root. Match both the global
    /// (`workbuddy.com`) and the China (`workbuddy.cn`) hostnames, plus
    /// `tencent_workbuddy` in case the format changes.
    fn looks_like_workbuddy(name: &str) -> bool {
        let l = name.to_ascii_lowercase();
        l.contains("workbuddy") || l.contains("tencent_workbuddy")
    }

    #[derive(Debug, Clone)]
    struct StoreRoot {
        display_name: String,
        leveldb_dir: PathBuf,
    }

    pub struct WorkBuddyWebConnector;

    impl WorkBuddyWebConnector {
        pub fn new() -> Self {
            Self
        }

        fn home_dir() -> Option<PathBuf> {
            dirs::home_dir()
        }

        fn discover(&self) -> Vec<StoreRoot> {
            let Some(home) = Self::home_dir() else {
                return Vec::new();
            };
            let mut out: Vec<StoreRoot> = Vec::new();
            for rel in CHROMIUM_BROWSER_DIRS {
                let dir = home.join(rel);
                if !dir.is_dir() {
                    continue;
                }
                for entry in WalkDir::new(&dir)
                    .max_depth(2)
                    .into_iter()
                    .flatten()
                {
                    if !entry.file_type().is_dir() {
                        continue;
                    }
                    let name = entry
                        .file_name()
                        .to_str()
                        .unwrap_or_default()
                        .to_string();
                    if !name.ends_with(".indexeddb.leveldb") {
                        continue;
                    }
                    if !looks_like_workbuddy(&name) {
                        continue;
                    }
                    // Derive a useful display name from the parent
                    // profile (e.g. "Default", "Profile 2"). The host
                    // is encoded in the directory name itself
                    // (`https_www.workbuddy.cn_0.indexeddb.leveldb`).
                    let profile = entry
                        .path()
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("profile");
                    let host_slug = name
                        .trim_end_matches(".indexeddb.leveldb")
                        .trim_start_matches("https_")
                        .trim_start_matches("http_")
                        .replace('_', ".");
                    let display = format!("WorkBuddy Web ({profile} — {host_slug})");
                    out.push(StoreRoot {
                        display_name: display,
                        leveldb_dir: entry.path().to_path_buf(),
                    });
                }
            }
            out
        }

        fn register_root(
            &self,
            conn: &mut Connection,
            root: &StoreRoot,
        ) -> AppResult<String> {
            let root_path = root.leveldb_dir.to_string_lossy().into_owned();
            let now = Utc::now().to_rfc3339();
            let existing: Option<String> = conn
                .query_row(
                    "SELECT id FROM sources WHERE kind = ?1 AND root_path = ?2 LIMIT 1",
                    params![kinds::WORKBUDDY, root_path],
                    |row| row.get(0),
                )
                .ok();
            let source_id = if let Some(existing_id) = existing {
                conn.execute(
                    "UPDATE sources SET display_name = ?1, last_scan_at = ?2, updated_at = ?2 WHERE id = ?3",
                    params![root.display_name, now, existing_id],
                )?;
                existing_id
            } else {
                let new_id = format!(
                    "src_{}_{}",
                    kinds::WORKBUDDY,
                    short_hash(root_path.as_bytes())
                );
                conn.execute(
                    "INSERT INTO sources (id, kind, display_name, root_path, status, last_scan_at, created_at, updated_at)
                     VALUES (?1, ?3, ?4, ?5, 'active', ?2, ?2, ?2)",
                    params![
                        new_id,
                        now,
                        kinds::WORKBUDDY,
                        root.display_name,
                        root_path,
                    ],
                )?;
                new_id
            };
            Ok(source_id)
        }
    }

    impl Connector for WorkBuddyWebConnector {
        fn kind(&self) -> &'static str {
            kinds::WORKBUDDY
        }
        fn display_name(&self) -> &'static str {
            "WorkBuddy Web"
        }

        fn discover_roots(&self) -> Vec<PathBuf> {
            self.discover().into_iter().map(|r| r.leveldb_dir).collect()
        }

        fn scan(&self, ctx: ScanCtx<'_>) -> AppResult<ScanReport> {
            let started = std::time::Instant::now();
            let roots = self.discover();

            if roots.is_empty() {
                return Ok(ScanReport {
                    source_id: String::new(),
                    kind: self.kind().to_string(),
                    status: "skipped".into(),
                    events_inserted: 0,
                    sessions_added: 0,
                    files_scanned: 0,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error: Some(
                        "no Chromium IndexedDB roots with `workbuddy` in the directory name"
                            .into(),
                    ),
                });
            }

            let mut total_events = 0i64;
            let mut total_files = 0i64;
            let mut status = "ok".to_string();
            let mut last_error: Option<String> = None;

            for root in &roots {
                let source_id = match self.register_root(ctx.conn, root) {
                    Ok(id) => id,
                    Err(e) => {
                        last_error = Some(format!(
                            "register {}: {e}",
                            root.leveldb_dir.display()
                        ));
                        status = "partial".into();
                        continue;
                    }
                };
                total_files += 1;

                let mut local_events = 0i64;
                let scan_result = idb::iter_store(
                    &root.leveldb_dir,
                    &mut |key, value| {
                        // WorkBuddy Web IDs come out of the V8 binary
                        // encoder so `idb::upsert_chromium_idb_record`
                        // is the only piece that can decode the key tail
                        // into an object-store name + record id. The
                        // value stays opaque until we have a V8 decoder
                        // (a follow-up).
                        if let Some(emitted) = idb::upsert_chromium_idb_record(
                            ctx.conn, &source_id, key, value,
                        ) {
                            local_events += emitted;
                        }
                        true
                    },
                    Some(50_000),
                );
                match scan_result {
                    Ok(_) => {
                        total_events += local_events;
                    }
                    Err(e) => {
                        last_error = Some(format!("{}: {e}", root.leveldb_dir.display()));
                        status = "partial".into();
                    }
                }
            }

            Ok(ScanReport {
                source_id: String::new(),
                kind: self.kind().to_string(),
                status,
                events_inserted: total_events,
                sessions_added: 0,
                files_scanned: total_files,
                duration_ms: started.elapsed().as_millis() as i64,
                error: last_error,
            })
        }
    }

    fn short_hash(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut h = Sha256::new();
        h.update(bytes);
        let digest = h.finalize();
        let mut s = String::with_capacity(16);
        for byte in digest.iter().take(8) {
            let _ = write!(s, "{:02x}", byte);
        }
        s
    }
}

pub use web::WorkBuddyWebConnector;