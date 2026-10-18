//! Stage 1-I-5 + Stage 5-E.1b: MiniMax Agent connector.
//!
//! MiniMax Agent ships as an Electron app installed under
//! `~/Library/Application Support/MiniMax Agent/`. Two well-known
//! directories hold real conversation state; one small JSON file holds
//! configuration. The connector surfaces all three under the
//! `minimax_code` kind so the UI shows MiniMax Code as a single agent:
//!
//! 1. `IndexedDB/app_._0.indexeddb.leveldb/` — Chromium IndexedDB. Most
//!    records are V8-encoded conversation blobs we don't yet decode;
//!    we emit one `session_meta` event per record we can read.
//! 2. `Local Storage/leveldb/` — standard bytewise Chrome Local Storage.
//!    Holds plaintext tokens (`_token`, JWTs), session markers
//!    (`agentarchon:unreadTerminalBySession`), and telemetry channels.
//!    Each non-schema row emits a `local_storage_kv` event after the
//!    built-in JWT redactor runs.
//! 3. `minimax-agent-config.json` — the only plaintext config blob;
//!    exposes `workingDirectory`, `language`, MCP permissions, and
//!    `loginItemInitialized` (auto-start flag). One `agent_config`
//!    event per scan, mtime-based.
//!
//! All events land under existing `minimax_code` source rows when
//! present; config + Local Storage reuse the first registered source
//! so we never create a third source row for the same install.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::connectors::{Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::idb;
use crate::redactor::Redactor;

/// Public kind label; mirrored in `kinds::MINIMAX_CODE`.
pub const MINIMAX_KIND: &str = crate::connectors::kinds::MINIMAX_CODE;

/// Single well-known Electron-app support dir.
const MINIMAX_RELATIVE_DIRS: &[&str] = &[
    "Library/Application Support/MiniMax Agent/IndexedDB",
    "Library/Application Support/MiniMax Agent/Local Storage/leveldb",
    "Library/Application Support/MiniMax Agent",
];

/// Stage 5-E.1b: event types emitted by the metadata surfaces. The
/// IndexedDB side keeps the legacy `session_meta` type so existing UI
/// filters keep working.
pub mod etype {
    pub const SESSION_META: &str = "session_meta";
    pub const LOCAL_STORAGE_KV: &str = "local_storage_kv";
    pub const AGENT_CONFIG: &str = "agent_config";
}

#[derive(Debug)]
struct StoreRoot {
    display_name: String,
    leveldb_dir: PathBuf,
    kind: StoreKind,
}

#[derive(Debug, PartialEq, Eq)]
enum StoreKind {
    IndexedDb,
    LocalStorage,
}

pub struct MiniMaxConnector;

impl MiniMaxConnector {
    pub fn new() -> Self {
        Self
    }

    fn home_dir() -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn support_dir() -> Option<PathBuf> {
        Self::home_dir().map(|h| h.join("Library").join("Application Support").join("MiniMax Agent"))
    }

    fn discover(&self) -> Vec<StoreRoot> {
        let Some(home) = Self::home_dir() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for rel in MINIMAX_RELATIVE_DIRS {
            let dir = home.join(rel);
            if !dir.is_dir() {
                continue;
            }
            // Chromium IndexedDB roots: walk one level for `*.indexeddb.leveldb/`.
            for entry in WalkDir::new(&dir).max_depth(2).into_iter().flatten() {
                if entry.file_type().is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".indexeddb.leveldb") {
                        let leveldb_dir = entry.path().to_path_buf();
                        let parent = leveldb_dir
                            .parent()
                            .and_then(|p| p.parent())
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("IndexedDB");
                        let store_name = name.trim_end_matches(".indexeddb.leveldb").to_string();
                        let display = format!("MiniMax Agent — {parent}/{store_name}");
                        out.push(StoreRoot {
                            display_name: display,
                            leveldb_dir,
                            kind: StoreKind::IndexedDb,
                        });
                    }
                }
            }
            // Chrome Local Storage leveldb: directly under the rel dir.
            if dir.ends_with("leveldb") {
                out.push(StoreRoot {
                    display_name: "MiniMax Agent — Local Storage".to_string(),
                    leveldb_dir: dir.clone(),
                    kind: StoreKind::LocalStorage,
                });
            }
        }
        out
    }

    fn register_root(&self, conn: &mut Connection, root: &StoreRoot) -> AppResult<String> {
        let id_prefix = format!("idb_{MINIMAX_KIND}");
        let root_path = root.leveldb_dir.to_string_lossy().into_owned();
        let now = Utc::now().to_rfc3339();
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM sources WHERE kind = ?1 AND root_path = ?2 LIMIT 1",
                params![MINIMAX_KIND, root_path],
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
            let new_id = format!("{id_prefix}_{}", short_hash(root_path.as_bytes()));
            conn.execute(
                "INSERT INTO sources (id, kind, display_name, root_path, status, last_scan_at, created_at, updated_at)
                 VALUES (?1, ?3, ?4, ?5, 'active', ?2, ?2, ?2)",
                params![new_id, now, MINIMAX_KIND, root.display_name, root_path],
            )?;
            new_id
        };
        Ok(source_id)
    }

    /// Resolve the first registered MiniMax Agent source_id so the
    /// metadata surfaces (config + Local Storage) can attach to the
    /// same logical install. Returns `None` when no IDB scan has ever
    /// registered a source for this install — in that case we register
    /// one against the support dir as a fallback.
    fn resolve_or_register_source_id(&self, conn: &mut Connection) -> AppResult<Option<String>> {
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM sources WHERE kind = ?1 ORDER BY created_at LIMIT 1",
                params![MINIMAX_KIND],
                |row| row.get(0),
            )
            .ok();
        if id.is_some() {
            return Ok(id);
        }
        // Fallback: register against the support dir if the install
        // exists but no IDB root has been walked yet (rare — would
        // happen only on a fresh machine).
        let Some(support) = Self::support_dir() else {
            return Ok(None);
        };
        if !support.is_dir() {
            return Ok(None);
        }
        let now = Utc::now().to_rfc3339();
        let new_id = format!(
            "idb_{MINIMAX_KIND}_{}",
            short_hash(support.to_string_lossy().as_bytes())
        );
        conn.execute(
            "INSERT INTO sources (id, kind, display_name, root_path, status, last_scan_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?5)",
            params![
                new_id,
                MINIMAX_KIND,
                "MiniMax Agent",
                support.display().to_string(),
                now
            ],
        )?;
        Ok(Some(new_id))
    }
}

impl Default for MiniMaxConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Connector for MiniMaxConnector {
    fn kind(&self) -> &'static str {
        MINIMAX_KIND
    }

    fn display_name(&self) -> &'static str {
        "MiniMax Agent"
    }

    fn discover_roots(&self) -> Vec<PathBuf> {
        self.discover()
            .into_iter()
            .map(|r| r.leveldb_dir)
            .collect()
    }

    fn scan(&self, ctx: ScanCtx) -> AppResult<ScanReport> {
        let started = std::time::Instant::now();
        let roots = self.discover();
        let mut total_events = 0i64;
        let mut total_files = 0i64;
        let mut status = "ok".to_string();
        let mut error: Option<String> = None;

        if roots.is_empty() && Self::support_dir().map(|p| !p.is_dir()).unwrap_or(true) {
            return Ok(ScanReport {
                source_id: String::new(),
                kind: self.kind().to_string(),
                status: "skipped".to_string(),
                events_inserted: 0,
                sessions_added: 0,
                files_scanned: 0,
                duration_ms: started.elapsed().as_millis() as i64,
                error: Some(format!(
                    "no MiniMax Agent roots found under {}",
                    Self::home_dir()
                        .map(|h| h.display().to_string())
                        .unwrap_or_else(|| "<unknown home>".into())
                )),
            });
        }

        let redactor = Redactor::from_db(ctx.conn)?;

        for root in &roots {
            let source_id = match self.register_root(ctx.conn, root) {
                Ok(id) => id,
                Err(e) => {
                    error = Some(format!("register {}: {e}", root.leveldb_dir.display()));
                    status = "partial".into();
                    continue;
                }
            };
            total_files += 1;

            let mut local_events = 0i64;
            let scan_result = match root.kind {
                StoreKind::IndexedDb => idb::iter_store(
                    &root.leveldb_dir,
                    &mut |key, value| {
                        if let Some(emitted) =
                            idb::upsert_chromium_idb_record(ctx.conn, &source_id, key, value)
                        {
                            local_events += emitted;
                        }
                        true
                    },
                    Some(50_000),
                ),
                StoreKind::LocalStorage => idb::iter_store_bytewise(
                    &root.leveldb_dir,
                    &mut |key, value| {
                        match upsert_local_storage_record(
                            ctx.conn,
                            &source_id,
                            key,
                            value,
                            &redactor,
                        ) {
                            Ok(0) => {}
                            Ok(n) => local_events += n,
                            Err(e) => {
                                tracing::warn!(
                                    key = %short_hash(&key[..key.len().min(32)]),
                                    error = %e,
                                    "minimax: local_storage upsert failed; skipping record"
                                );
                            }
                        }
                        true
                    },
                    Some(50_000),
                ),
            };
            match scan_result {
                Ok(_n) => {
                    total_events += local_events;
                }
                Err(e) => {
                    error = Some(format!("{}: {e}", root.leveldb_dir.display()));
                    status = "partial".into();
                }
            }
        }

        // Stage 5-E.1b: also scan the plaintext config JSON, attached
        // to the first registered MiniMax source_id.
        if let Some(support_dir) = Self::support_dir() {
            if support_dir.is_dir() {
                let config_path = support_dir.join("minimax-agent-config.json");
                if config_path.is_file() {
                    total_files += 1;
                    let source_id = self.resolve_or_register_source_id(ctx.conn)?;
                    if let Some(source_id) = source_id {
                        match emit_config_file(
                            ctx.conn,
                            &source_id,
                            &config_path,
                            &redactor,
                            &mut total_events,
                        ) {
                            Ok(()) => {}
                            Err(e) => {
                                error = Some(format!("config: {e}"));
                                status = "partial".into();
                            }
                        }
                    }
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
            error,
        })
    }
}

// ---------------------------------------------------------------------
// Local Storage helpers
// ---------------------------------------------------------------------

/// Skip these key prefixes / whole-key matches — they're Chromium's
/// own Local Storage bookkeeping and would drown the timeline.
const LOCAL_STORAGE_SKIP_KEY_PREFIXES: &[&str] = &[
    "META:", "METAACCESS:", "VERSION", "__local_store_support__", "__sak", "__storage_test__",
];

/// Skip these whole-key matches — Chromium telemetry channels we don't
/// want as raw_events (they're 4-line records duplicated every refresh).
const LOCAL_STORAGE_SKIP_KEY_EXACT: &[&str] = &["sawebjssdkchannel"];

/// Emit one `local_storage_kv` raw_event for a Chrome Local Storage
/// record. Filters schema + telemetry rows; applies the global JWT
/// redactor so `_token` etc. land clean in the ledger.
fn upsert_local_storage_record(
    conn: &Connection,
    source_id: &str,
    key: &[u8],
    value: &[u8],
    redactor: &Redactor,
) -> AppResult<i64> {
    if key.len() < 4 || value.is_empty() {
        return Ok(0);
    }
    let key_str = String::from_utf8_lossy(key).into_owned();
    if LOCAL_STORAGE_SKIP_KEY_PREFIXES
        .iter()
        .any(|p| key_str.starts_with(p))
    {
        return Ok(0);
    }
    if LOCAL_STORAGE_SKIP_KEY_EXACT.iter().any(|k| key_str == *k) {
        return Ok(0);
    }
    // Chrome Local Storage sometimes stores the same user key under
    // multiple origins (e.g. `_app://.` + `https://...`). Trim the
    // origin prefix off so the displayed key stays readable.
    let cleaned_key = key_str
        .split_once("\x01")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or(key_str);

    let value_preview = String::from_utf8_lossy(&value[..value.len().min(240)]).into_owned();
    let payload_text = json!({
        "key": cleaned_key,
        "value_size": value.len(),
        "value_preview": value_preview,
        "key_hex": short_hash(&key[..key.len().min(64)]),
    })
    .to_string();
    let redacted_payload = redactor.apply(&payload_text);
    let payload_hash = short_hash(redacted_payload.as_bytes());

    let id = format!("ls_{}", short_hash(&[key, value].concat()));
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO raw_events
            (id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, '', ?5, 1, ?6, ?7)",
        params![
            id,
            source_id,
            etype::LOCAL_STORAGE_KV,
            now,
            redacted_payload,
            payload_hash,
            now
        ],
    )?;
    Ok(1)
}

// ---------------------------------------------------------------------
// Config JSON helper
// ---------------------------------------------------------------------

/// Read `minimax-agent-config.json` and emit one `agent_config` event
/// with the full config object as payload. The JWT-safety-net
/// redactor catches any `tokens` field that might leak a Bearer.
fn emit_config_file(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    redactor: &Redactor,
    events_inserted: &mut i64,
) -> AppResult<()> {
    let mtime_ms = file_mtime_ms(path).unwrap_or(0);
    let bytes = std::fs::read(path)?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let value: Json = serde_json::from_str(&raw).unwrap_or(Json::String(raw.clone()));
    let enriched = json!({
        "path": path.display().to_string(),
        "value": value,
    });
    let payload = redactor.apply(&serde_json::to_string(&enriched)?);
    let payload_hash = short_hash(payload.as_bytes());
    let id = format!("mc_{}", short_hash(payload.as_bytes()));
    let now = Utc::now().to_rfc3339();
    let occurred_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(mtime_ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| now.clone());
    conn.execute(
        "INSERT OR IGNORE INTO raw_events
            (id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, '', ?5, 1, ?6, ?7)",
        params![
            id,
            source_id,
            etype::AGENT_CONFIG,
            occurred_at,
            payload,
            payload_hash,
            now
        ],
    )?;
    *events_inserted += 1;
    Ok(())
}

fn file_mtime_ms(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dt = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dt.as_millis() as i64)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_is_deterministic_and_short() {
        let h1 = short_hash(b"hello");
        let h2 = short_hash(b"hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        let h3 = short_hash(b"world");
        assert_ne!(h1, h3);
    }

    #[test]
    fn discovers_no_roots_for_empty_home() {
        let c = MiniMaxConnector::new();
        let _ = c.discover_roots();
    }

    /// Verifies the Local Storage key filter actually drops schema +
    /// telemetry rows so the timeline stays readable.
    #[test]
    fn local_storage_filter_drops_chromium_internals() {
        let r = &Redactor::global();
        for key in [
            "META:app://.",
            "METAACCESS:app://.",
            "VERSION",
            "__local_store_support__",
            "__sak",
            "__storage_test__",
            "sawebjssdkchannel",
        ] {
            assert_eq!(
                upsert_local_storage_record(
                    &Connection::open_in_memory().unwrap(),
                    "src_x",
                    key.as_bytes(),
                    b"some-value",
                    r,
                )
                .unwrap(),
                0,
                "key {key} should be filtered"
            );
        }
    }
}
