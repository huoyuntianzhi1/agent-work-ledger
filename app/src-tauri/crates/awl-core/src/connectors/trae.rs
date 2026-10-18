//! Stage 5-E.1a: Trae (ByteDance) IDE metadata connector.
//!
//! Trae is a VSCode-fork AI IDE. Its main conversation DB
//! (`~/Library/Application Support/Trae/ModularData/ai-agent/database.db`)
//! is SQLCipher-encrypted and unreadable without the platform key —
//! a follow-up stage will surface a Stage 6 plan for key extraction.
//!
//! This connector covers every **plaintext** surface Trae writes today:
//!
//! 1. `ModularData/ai-agent/sandbox/<UUID>.json` — sandbox metadata,
//!    names each project the user opened and lists the per-path
//!    permissions the AI agent was granted (read-only / inherit /
//!    fully-trust). Each file becomes one `ai_agent_sandbox` event.
//! 2. `User/mcp.json` — Model-Context-Protocol server list (4 entries
//!    observed: sequential-thinking / Puppeteer / thinking-models /
//!    webresearch). One `ai_agent_mcp` event per scan; payload is the
//!    redacted server map.
//! 3. `User/settings.json` — AI toolcall confirm modes + workspace
//!    trust. One `ai_agent_settings` event per scan.
//! 4. `User/globalStorage/storage.json` — telemetry ids +
//!    `iCubeAuthInfo://icube.cloudide` JWT. We redact the JWT through
//!    the standard `Redactor` before insert. One `ai_agent_auth_info`
//!    event per scan.
//! 5. `ModularData/ttnet/server.json` — Tencent ttnet dispatch config.
//!    One `ai_agent_dispatch` event per scan.
//! 6. `ModularData/ckg_server/local_env.json` — device_id + per-user
//!    host map. One `ai_agent_env` event per scan.
//! 7. `User/workspaceStorage/<UUID>/workspace.json` — one
//!    `workspace_state` event per workspace; the `folder` URL is
//!    decoded into the `project_key` column.
//! 8. `User/workspaceStorage/<UUID>/state.vscdb` ItemTable (plaintext
//!    SQLite). Three keys are real conversation state:
//!    - `ChatStore` — UI turn expansion state
//!    - `icube-ai-agent-storage-input-history` — user input text history
//!    - `memento/icube-ai-agent-storage` — session list + message blobs
//!      (we never decrypt `encrypted_model_params`; we surface the
//!      session id, role, turn index, and the literal content fields
//!      that are already plaintext).
//!
//! All events land under **one** `sources` row keyed by the Trae
//! `User/` root path, so the UI surfaces Trae as one agent while the
//! ETL can still partition events per-workspace through `project_key`.
//!
//! Only **read** access. Nothing here mutates Trae files. Trae is
//! typically running while we scan, so each `state.vscdb` open is
//! read-only and we skip-and-warn any workspace whose DB is locked.

use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::connectors::codex::upsert_source;
use crate::connectors::{should_skip_ms, Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::redactor::Redactor;

/// Public kind label; mirrored in `kinds::TRAE`.
pub const TRAE_KIND: &str = crate::connectors::kinds::TRAE;

/// Per-event-type strings emitted into `raw_events.event_type`. Kept
/// stable so the UI can render icons / colours by event_type.
pub mod etype {
    pub const SANDBOX: &str = "ai_agent_sandbox";
    pub const MCP: &str = "ai_agent_mcp";
    pub const SETTINGS: &str = "ai_agent_settings";
    pub const AUTH_INFO: &str = "ai_agent_auth_info";
    pub const DISPATCH: &str = "ai_agent_dispatch";
    pub const ENV: &str = "ai_agent_env";
    pub const WORKSPACE_STATE: &str = "workspace_state";
    pub const CHAT_STORE: &str = "chat_session";
    pub const INPUT_HISTORY: &str = "input_history";
    pub const SESSION_MESSAGE: &str = "session_message";
}

/// Subset of `state.vscdb` `ItemTable` keys we surface. Anything else
/// is part of VSCode-internal bookkeeping (UI state, telemetry, search
/// index) that we deliberately ignore.
const TARGET_IDB_KEYS: &[&str] = &[
    "ChatStore",
    "icube-ai-agent-storage-input-history",
    "memento/icube-ai-agent-storage",
];

/// Resolve the Trae `User/` root on macOS. All metadata files we care
/// about live under or alongside this directory.
fn trae_user_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join("Library")
            .join("Application Support")
            .join("Trae")
            .join("User")
    })
}

/// Resolve the sibling `ModularData` root. Trae's per-workspace sandbox
/// metadata and ttnet / ckg_server config live here.
fn trae_modular_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join("Library")
            .join("Application Support")
            .join("Trae")
            .join("ModularData")
    })
}

pub struct TraeMetadataConnector;

impl TraeMetadataConnector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TraeMetadataConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Connector for TraeMetadataConnector {
    fn kind(&self) -> &'static str {
        TRAE_KIND
    }

    fn display_name(&self) -> &'static str {
        "Trae"
    }

    fn discover_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(p) = trae_user_root() {
            if p.is_dir() {
                roots.push(p);
            }
        }
        if let Some(p) = trae_modular_root() {
            if p.is_dir() {
                roots.push(p);
            }
        }
        roots
    }

    fn scan(&self, ctx: ScanCtx<'_>) -> AppResult<ScanReport> {
        let started = Instant::now();

        // We register against the `User/` root if it exists, otherwise
        // fall back to the `ModularData/` root, otherwise the parent
        // `Trae/` dir — `upsert_source` only stores the first match so
        // the row stays stable across partial installs.
        let root = self
            .discover_roots()
            .into_iter()
            .min_by_key(|p| p.components().count())
            .or_else(|| {
                dirs::home_dir().map(|h| {
                    h.join("Library")
                        .join("Application Support")
                        .join("Trae")
                })
            });

        let source_id = match &root {
            Some(p) => upsert_source(
                ctx.conn,
                self.kind(),
                self.display_name(),
                Some(p),
                "readable",
            )?,
            None => upsert_source(ctx.conn, self.kind(), self.display_name(), None, "discovered")?,
        };

        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let now = crate::connectors::now_ms();
        ctx.conn.execute(
            "INSERT INTO connector_runs(id, source_id, started_at, status, events_count) VALUES (?1, ?2, ?3, 'running', 0)",
            params![run_id, source_id, crate::connectors::rfc3339(Some(now))],
        )?;

        let redactor = Redactor::from_db(ctx.conn)?;

        let mut events_inserted: i64 = 0;
        let mut files_scanned: i64 = 0;
        let mut status = "ok".to_string();
        let mut last_error: Option<String> = None;

        // ---- ModularData siblings (sandbox + ttnet + ckg_server) ----
        if let Some(modular) = trae_modular_root() {
            if let Err(e) = scan_sandbox_dir(ctx.conn, &source_id, &modular, &redactor, ctx.since_ms, &mut events_inserted, &mut files_scanned) {
                last_error = Some(format!("sandbox: {e}"));
                status = "partial".into();
            }
            if let Err(e) = scan_ttnet_server(ctx.conn, &source_id, &modular, &redactor, ctx.since_ms, &mut events_inserted, &mut files_scanned) {
                last_error = Some(format!("ttnet: {e}"));
                status = "partial".into();
            }
            if let Err(e) = scan_ckg_local_env(ctx.conn, &source_id, &modular, &redactor, ctx.since_ms, &mut events_inserted, &mut files_scanned) {
                last_error = Some(format!("ckg_server: {e}"));
                status = "partial".into();
            }
        }

        // ---- User-level files (mcp / settings / globalStorage) ----
        if let Some(user_root) = trae_user_root() {
            for (path, etype, project_key) in [
                (
                    user_root.join("mcp.json"),
                    etype::MCP.to_string(),
                    "trae_user".to_string(),
                ),
                (
                    user_root.join("settings.json"),
                    etype::SETTINGS.to_string(),
                    "trae_user".to_string(),
                ),
                (
                    user_root.join("globalStorage").join("storage.json"),
                    etype::AUTH_INFO.to_string(),
                    "trae_user".to_string(),
                ),
            ] {
                if !path.is_file() {
                    continue;
                }
                files_scanned += 1;
                if let Err(e) = emit_json_file(
                    ctx.conn,
                    &source_id,
                    &path,
                    &etype,
                    &project_key,
                    &redactor,
                    ctx.since_ms,
                    &mut events_inserted,
                ) {
                    last_error = Some(format!("{}: {e}", path.display()));
                    status = "partial".into();
                }
            }

            // ---- workspaceStorage/*/{workspace.json, state.vscdb} ----
            let workspace_root = user_root.join("workspaceStorage");
            if workspace_root.is_dir() {
                for ws_entry in WalkDir::new(&workspace_root)
                    .max_depth(2)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if !ws_entry.file_type().is_dir() {
                        continue;
                    }
                    let ws_dir = ws_entry.path().to_path_buf();
                    // Skip the root `workspaceStorage/` itself.
                    if ws_dir == workspace_root {
                        continue;
                    }

                    let project_key = match read_workspace_folder(&ws_dir) {
                        Ok(Some(folder)) => folder,
                        _ => format!("trae_ws:{}", short_id(&ws_dir)),
                    };

                    let workspace_json = ws_dir.join("workspace.json");
                    if workspace_json.is_file() {
                        files_scanned += 1;
                        if let Err(e) = emit_json_file(
                            ctx.conn,
                            &source_id,
                            &workspace_json,
                            etype::WORKSPACE_STATE,
                            &project_key,
                            &redactor,
                            ctx.since_ms,
                            &mut events_inserted,
                        ) {
                            last_error = Some(format!("workspace.json: {e}"));
                            status = "partial".into();
                        }
                    }

                    let state_db = ws_dir.join("state.vscdb");
                    if state_db.is_file() {
                        files_scanned += 1;
                        match scan_state_vscdb(
                            ctx.conn,
                            &source_id,
                            &state_db,
                            &project_key,
                            &redactor,
                            ctx.since_ms,
                            &mut events_inserted,
                        ) {
                            Ok(()) => {}
                            Err(e) => {
                                // Locked DBs are expected while Trae is
                                // running — log + skip, don't fail the
                                // whole scan.
                                tracing::warn!(
                                    workspace = %ws_dir.display(),
                                    error = %e,
                                    "trae: state.vscdb scan failed (likely locked); skipping workspace"
                                );
                                last_error = Some(format!("state.vscdb: {e}"));
                                status = "partial".into();
                            }
                        }
                    }
                }
            }
        }

        // ---- finish connector_runs + bump source timestamp ----
        let ended_at = crate::connectors::now_ms();
        ctx.conn.execute(
            "UPDATE connector_runs SET ended_at = ?1, status = ?2, events_count = ?3 WHERE id = ?4",
            params![
                crate::connectors::rfc3339(Some(ended_at)),
                status,
                events_inserted,
                run_id
            ],
        )?;
        ctx.conn.execute(
            "UPDATE sources SET last_scan_at = ?1, status = ?2, updated_at = ?1 WHERE id = ?3",
            params![crate::connectors::rfc3339(Some(ended_at)), status, source_id],
        )?;

        Ok(ScanReport {
            source_id,
            kind: self.kind().into(),
            status,
            events_inserted,
            sessions_added: 0,
            files_scanned,
            duration_ms: started.elapsed().as_millis() as i64,
            error: last_error,
        })
    }
}

// ---------------------------------------------------------------------
// ModularData helpers
// ---------------------------------------------------------------------

/// Walk every `ModularData/ai-agent/sandbox/<UUID>.json` and emit one
/// `ai_agent_sandbox` event per file. The payload is the raw JSON
/// wrapped with a `sandbox_id` so the timeline can show the project
/// name + permissions summary without a follow-up parse.
fn scan_sandbox_dir(
    conn: &Connection,
    source_id: &str,
    modular_root: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
    events_inserted: &mut i64,
    files_scanned: &mut i64,
) -> AppResult<()> {
    let sandbox_dir = modular_root.join("ai-agent").join("sandbox");
    if !sandbox_dir.is_dir() {
        return Ok(());
    }
    for entry in WalkDir::new(&sandbox_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        *files_scanned += 1;
        if let Err(e) = emit_json_file(
            conn,
            source_id,
            entry.path(),
            etype::SANDBOX,
            "trae_ai_agent",
            redactor,
            since_ms,
            events_inserted,
        ) {
            tracing::warn!(
                file = %entry.path().display(),
                error = %e,
                "trae: sandbox file failed; skipping"
            );
        }
    }
    Ok(())
}

/// Read `ModularData/ttnet/server.json` (dispatch config). Single file,
/// single event. Useful as evidence Trae talks to which API host.
fn scan_ttnet_server(
    conn: &Connection,
    source_id: &str,
    modular_root: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
    events_inserted: &mut i64,
    files_scanned: &mut i64,
) -> AppResult<()> {
    let path = modular_root.join("ttnet").join("server.json");
    if !path.is_file() {
        return Ok(());
    }
    *files_scanned += 1;
    emit_json_file(
        conn,
        source_id,
        &path,
        etype::DISPATCH,
        "trae_ttnet",
        redactor,
        since_ms,
        events_inserted,
    )
}

/// Read `ModularData/ckg_server/local_env.json` (device_id + host map).
fn scan_ckg_local_env(
    conn: &Connection,
    source_id: &str,
    modular_root: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
    events_inserted: &mut i64,
    files_scanned: &mut i64,
) -> AppResult<()> {
    let path = modular_root.join("ckg_server").join("local_env.json");
    if !path.is_file() {
        return Ok(());
    }
    *files_scanned += 1;
    emit_json_file(
        conn,
        source_id,
        &path,
        etype::ENV,
        "trae_ckg",
        redactor,
        since_ms,
        events_inserted,
    )
}

// ---------------------------------------------------------------------
// workspaceStorage helpers
// ---------------------------------------------------------------------

/// Decode `<UUID>/workspace.json` and return the `folder` URL stripped
/// of its `file://` prefix + URL-encoding. `Ok(None)` when the file is
/// absent (5 of 26 workspaces on 王总's machine) or malformed.
fn read_workspace_folder(ws_dir: &Path) -> AppResult<Option<String>> {
    let path = ws_dir.join("workspace.json");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let value: Json = serde_json::from_str(&raw).unwrap_or(Json::Null);
    let folder = value
        .get("folder")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(folder.map(strip_file_url))
}

/// Convert `file:///Users/apple/Desktop/foo%20bar` into
/// `/Users/apple/Desktop/foo bar`. Falls back to the raw string on
/// percent-decode failure so we never lose a project key.
fn strip_file_url(s: String) -> String {
    let stripped = s
        .strip_prefix("file://")
        .map(|s| s.to_string())
        .unwrap_or(s);
    match percent_decode(&stripped) {
        Some(decoded) => decoded,
        None => stripped,
    }
}

/// Tiny percent-decoder that handles `%XX` escapes and leaves invalid
/// sequences intact. We do not pull in the `url` crate just for this.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_digit(bytes[i + 1])?;
            let lo = hex_digit(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Short stable id from a workspaceStorage directory name, used as
/// fallback `project_key` when the workspace has no `workspace.json`.
fn short_id(ws_dir: &Path) -> String {
    let name = ws_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(8);
    for b in digest.iter().take(4) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ---------------------------------------------------------------------
// state.vscdb reader
// ---------------------------------------------------------------------

/// Open one `state.vscdb` read-only and stream every target
/// `ItemTable` row into raw_events. We use a fresh per-workspace
/// `Connection` so a lock on one workspace doesn't block the others.
fn scan_state_vscdb(
    conn: &Connection,
    source_id: &str,
    db_path: &Path,
    project_key: &str,
    redactor: &Redactor,
    since_ms: Option<i64>,
    events_inserted: &mut i64,
) -> AppResult<()> {
    // File mtime is the best timestamp we have for vscdb rows.
    let file_mtime_ms = file_mtime_ms(db_path).unwrap_or(0);
    if should_skip_ms(since_ms, file_mtime_ms) {
        return Ok(());
    }
    let occurred_at = ms_to_rfc3339(file_mtime_ms);

    let db_conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = db_conn.prepare("SELECT key, value FROM ItemTable")?;
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        Ok((key, value))
    })?;
    for row in rows {
        let (key, value) = row?;
        if !TARGET_IDB_KEYS.iter().any(|t| *t == key) {
            continue;
        }
        let event_type = match key.as_str() {
            "ChatStore" => etype::CHAT_STORE,
            "icube-ai-agent-storage-input-history" => etype::INPUT_HISTORY,
            "memento/icube-ai-agent-storage" => etype::SESSION_MESSAGE,
            _ => continue,
        };
        let data = json!({
            "key": key,
            "value": serde_json::from_str::<Json>(&value).unwrap_or(Json::String(value)),
            "workspace": project_key,
        });
        let payload = redactor.apply(&serde_json::to_string(&data)?);
        insert_raw(conn, source_id, event_type, &occurred_at, project_key, &payload, events_inserted)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Generic JSON-file emitter
// ---------------------------------------------------------------------

/// Read a single JSON file and emit one raw_event for it. Honours
/// `since_ms` via the file's mtime. Any redactor match (e.g. JWT in
/// `iCubeAuthInfo`) is applied to the whole payload string before
/// insert.
fn emit_json_file(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    event_type: &str,
    project_key: &str,
    redactor: &Redactor,
    since_ms: Option<i64>,
    events_inserted: &mut i64,
) -> AppResult<()> {
    let mtime_ms = file_mtime_ms(path).unwrap_or(0);
    if should_skip_ms(since_ms, mtime_ms) {
        return Ok(());
    }
    let bytes = std::fs::read(path)?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let value: Json = serde_json::from_str(&raw).unwrap_or(Json::String(raw.clone()));
    let enriched = json!({
        "path": path.display().to_string(),
        "value": value,
    });
    let payload = redactor.apply(&serde_json::to_string(&enriched)?);
    insert_raw(
        conn,
        source_id,
        event_type,
        &ms_to_rfc3339(mtime_ms),
        project_key,
        &payload,
        events_inserted,
    )?;
    Ok(())
}

fn insert_raw(
    conn: &Connection,
    source_id: &str,
    event_type: &str,
    occurred_at: &str,
    project_key: &str,
    payload: &str,
    events_inserted: &mut i64,
) -> AppResult<()> {
    let payload_hash = sha256_hex(payload);
    let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
        params![
            id,
            source_id,
            event_type,
            occurred_at,
            project_key,
            payload,
            payload_hash,
            now
        ],
    )?;
    // `INSERT OR IGNORE` returns 0 rows when the dedup UNIQUE INDEX
    // rejected the row. We still count the attempt so the UI surfaces
    // the file as scanned even when nothing new was emitted.
    *events_inserted += 1;
    Ok(())
}

// ---------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------

fn file_mtime_ms(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dt = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dt.as_millis() as i64)
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::test_locks::HOME_LOCK;

    #[test]
    fn strip_file_url_decodes_percent_escapes() {
        let got = strip_file_url(
            "file:///Users/apple/Desktop/%E5%85%AC%E4%BC%97%E5%8F%B7%E6%8E%92%E7%89%88%E6%8F%92%E4%BB%B6".into(),
        );
        assert_eq!(
            got,
            "/Users/apple/Desktop/公众号排版插件"
        );
    }

    #[test]
    fn strip_file_url_handles_no_prefix() {
        assert_eq!(
            strip_file_url("/tmp/foo bar".into()),
            "/tmp/foo bar"
        );
    }

    #[test]
    fn percent_decode_passes_invalid_sequences_through() {
        // Trailing `%2` (missing second hex digit) is left alone.
        let got = percent_decode("/tmp/foo%2").expect("decodes");
        assert_eq!(got, "/tmp/foo%2");
    }

    #[test]
    fn sha256_matches_known_digest() {
        let got = sha256_hex("hello");
        assert_eq!(
            got,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn short_id_is_stable_and_short() {
        let a = short_id(Path::new("/x/abc123"));
        let b = short_id(Path::new("/x/abc123"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
    }

    /// Round-trip a Trae sandbox JSON file end-to-end through the
    /// scan pipeline against an in-memory ledger. Verifies that the
    /// `INSERT OR IGNORE` dedup path actually emits rows.
    #[test]
    fn scan_emits_sandbox_event_with_redaction() {
        use crate::connectors::kinds;
        // Acquire the process-wide HOME lock so the markdown_summary
        // test (which also mutates HOME) can't race us.
        let _guard = HOME_LOCK.lock().unwrap();
        let mut conn = open_in_memory_ledger();
        awl_db::migrate_for_tests(&conn).expect("migrate");

        // Build a fake Trae install under a unique temp directory and
        // point `dirs::home_dir()` at it via the `HOME` env var.
        let scratch = std::env::temp_dir().join(format!(
            "agent-work-ledger-trae-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let sandbox_dir = scratch
            .join("Library")
            .join("Application Support")
            .join("Trae")
            .join("ModularData")
            .join("ai-agent")
            .join("sandbox");
        std::fs::create_dir_all(&sandbox_dir).expect("mkdir sandbox");
        std::fs::write(
            sandbox_dir.join("abc123.json"),
            r#"{"name":"abc123","permission":[{"file_read_only":"/"}]}"#,
        )
        .expect("write sandbox");

        // mcp.json with a JWT that should be redacted.
        let user_dir = scratch
            .join("Library")
            .join("Application Support")
            .join("Trae")
            .join("User");
        std::fs::create_dir_all(&user_dir).expect("mkdir user");
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.signature_signature_signature";
        std::fs::write(
            user_dir.join("mcp.json"),
            format!(r#"{{"iCubeAuthInfo://icube.cloudide": "{jwt}"}}"#),
        )
        .expect("write mcp");

        // `dirs::home_dir()` reads `HOME` on Linux / macOS. Snapshot the
        // prior value so we can restore it after the test even on panic.
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &scratch);

        let result = (|| -> AppResult<()> {
            let connector = TraeMetadataConnector::new();
            let report = connector.scan(crate::connectors::ScanCtx {
                conn: &mut conn,
                source_id: "",
                since_ms: None,
            })?;

            assert_eq!(report.kind, kinds::TRAE);
            assert_eq!(report.events_inserted, 2);
            assert!(
                report.error.is_none(),
                "scan should be ok, got {:?}",
                report.error
            );

            let mut stmt = conn
                .prepare("SELECT event_type, payload FROM raw_events ORDER BY event_type")
                .expect("prepare");
            let rows: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query")
                .filter_map(|r| r.ok())
                .collect();
            assert_eq!(rows.len(), 2);
            let sandbox_payload = rows
                .iter()
                .find(|(e, _)| e == etype::SANDBOX)
                .map(|(_, p)| p.clone())
                .expect("sandbox payload");
            assert!(sandbox_payload.contains("file_read_only"));
            let mcp_payload = rows
                .iter()
                .find(|(e, _)| e == etype::MCP)
                .map(|(_, p)| p.clone())
                .expect("mcp payload");
            // JWT must be redacted by the safety-net redactor.
            assert!(!mcp_payload.contains(jwt), "JWT leaked through redactor");
            assert!(mcp_payload.contains("[REDACTED:jwt]"));
            Ok(())
        })();

        // Always restore HOME and clean up the scratch dir, even on
        // assertion failure.
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&scratch);
        result.expect("scan assertions");
    }
}

/// Test-only helper: open an in-memory ledger with the v0.2 schema
/// already migrated. Used by the connector unit tests so they don't
/// have to know about the migration surface area.
#[cfg(test)]
fn open_in_memory_ledger() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory");
    awl_db::migrate_for_tests(&conn).expect("migrate");
    conn
}
