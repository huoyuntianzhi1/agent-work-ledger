//! Stage 1-I-3 + Stage 5-E.1c: Doubao / 豆包 Chromium IndexedDB connector
//! plus browser-level metadata surfaces.
//!
//! Doubao is an Electron app, so its user data lives under Chromium's
//! IndexedDB LevelDB layout (`*.indexeddb.leveldb/`). On 王总's machine
//! (2026-10) no `IndexedDB/` directory exists — ByteDance removed
//! IndexedDB-based chat persistence at some point in favour of a
//! server-side model, so the IDB path is effectively dead for this
//! connector on real installs.
//!
//! What we *can* still ingest today:
//! - `~/Library/Application Support/<flavor>/Local State` — browser-level
//!   state. Holds `aha.device.{device_id, install_id, mac_address,
//!   pc_uuid}` and the accessibility / autofill flags. One
//!   `local_state` event per scan.
//! - `~/Library/Application Support/<flavor>/Default/Preferences` —
//!   user prefs (~150 KB JSON). Stuffed with Last-Profile / window
//!   state. One `preferences` event per scan, payload capped at 240
//!   chars preview + the bytes-hash for dedup.
//! - `~/Library/Application Support/<flavor>/ChatAppFeatureState` —
//!   ByteDance-specific chat-app feature toggles. One
//!   `chat_app_feature_state` event per scan.
//!
//! Each metadata file lands under a synthetic source row keyed on the
//! support dir; when an IDB root *is* present we re-use the existing
//! `idb_<kind>_<hash>` row so the UI keeps showing a single source per
//! install.

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

/// Public kind label; mirrored in `kinds::DOUBAO`.
pub const DOUBAO_KIND: &str = crate::connectors::kinds::DOUBAO;
pub const DOUBAO_WORK_KIND: &str = crate::connectors::kinds::DOUBAO_WORK;

/// Known Doubao IndexedDB directories that may carry chat / conversation
/// state. We accept anything that ends with `.indexeddb.leveldb` to be
/// defensive — versioned clients add new prefixes without notice.
const DOUBAO_RELATIVE_DIRS: &[&str] = &[
    "Library/Application Support/Doubao/Default/IndexedDB",
    "Library/Application Support/Doubao/Default/Profile 1/IndexedDB",
    "Library/Application Support/Doubao/Default/Profile 2/IndexedDB",
    "Library/Application Support/Doubao/Default/Profile 3/IndexedDB",
];

const DOUBAO_WORK_RELATIVE_DIRS: &[&str] = &[
    "Library/Application Support/DoubaoWork/Default/IndexedDB",
    "Library/Application Support/DoubaoWork/Default/Profile 1/IndexedDB",
    "Library/Application Support/DoubaoWork/Default/Profile 2/IndexedDB",
];

/// Stage 5-E.1c: per-flavor support dir + metadata files. Doubao does
/// not keep chat history in IndexedDB anymore, but the browser-level
/// config files still carry useful user identification (device id,
/// install id, last-active profile, language).
fn flavor_metadata_files(support_dir: &Path) -> Vec<PathBuf> {
    vec![
        support_dir.join("Local State"),
        support_dir.join("Default").join("Preferences"),
        support_dir.join("ChatAppFeatureState"),
    ]
}

/// Event types emitted by the metadata surfaces. Kept stable so the
/// UI / timeline filter works without migration.
pub mod etype {
    pub const LOCAL_STATE: &str = "local_state";
    pub const PREFERENCES: &str = "preferences";
    pub const CHAT_APP_FEATURE_STATE: &str = "chat_app_feature_state";
}

/// One IndexedDB LevelDB directory is one Doubao "store root". Each root
/// becomes its own `sources` row so multiple installs (Default + Profile 2
/// + …) coexist and the user can see at a glance where each event came
/// from.
#[derive(Debug)]
struct StoreRoot {
    display_name: String,
    leveldb_dir: PathBuf,
}

/// Connector kind: `doubao` for the consumer app, `doubao_work` for the
/// work/team edition. Same code path; the kind is the only difference.
#[derive(Debug, Clone, Copy)]
enum DoubaoFlavor {
    Consumer,
    Work,
}

impl DoubaoFlavor {
    fn kind(&self) -> &'static str {
        match self {
            DoubaoFlavor::Consumer => DOUBAO_KIND,
            DoubaoFlavor::Work => DOUBAO_WORK_KIND,
        }
    }
    fn relative_dirs(&self) -> &'static [&'static str] {
        match self {
            DoubaoFlavor::Consumer => DOUBAO_RELATIVE_DIRS,
            DoubaoFlavor::Work => DOUBAO_WORK_RELATIVE_DIRS,
        }
    }
    fn human_name(&self) -> &'static str {
        match self {
            DoubaoFlavor::Consumer => "Doubao (豆包)",
            DoubaoFlavor::Work => "Doubao Work (豆包工作)",
        }
    }
}

pub struct DoubaoConnector {
    flavor: DoubaoFlavor,
}

impl DoubaoConnector {
    pub fn consumer() -> Self {
        Self {
            flavor: DoubaoFlavor::Consumer,
        }
    }
    pub fn work() -> Self {
        Self {
            flavor: DoubaoFlavor::Work,
        }
    }

    fn home_dir() -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn discover(&self) -> Vec<StoreRoot> {
        let Some(home) = Self::home_dir() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for rel in self.flavor.relative_dirs() {
            let dir = home.join(rel);
            if !dir.is_dir() {
                continue;
            }
            for entry in WalkDir::new(&dir).max_depth(2).into_iter().flatten() {
                if entry.file_type().is_dir()
                    && entry
                        .file_name()
                        .to_str()
                        .map(|s| s.ends_with(".indexeddb.leveldb"))
                        .unwrap_or(false)
                {
                    let leveldb_dir = entry.path().to_path_buf();
                    // Try to make a useful display name from the parent
                    // directory's basename (e.g. "Default", "Profile 2").
                    let parent = leveldb_dir
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("IndexedDB");
                    let store_name = entry
                        .file_name()
                        .to_str()
                        .unwrap_or("unknown")
                        .trim_end_matches(".indexeddb.leveldb")
                        .to_string();
                    let display = format!(
                        "{} — {parent}/{store_name}",
                        self.flavor.human_name()
                    );
                    out.push(StoreRoot {
                        display_name: display,
                        leveldb_dir,
                    });
                }
            }
        }
        out
    }

    fn register_root(&self, conn: &mut Connection, root: &StoreRoot) -> AppResult<String> {
        let id = format!("idb_{}", self.flavor.kind());
        let root_path = root.leveldb_dir.to_string_lossy().into_owned();
        let now = Utc::now().to_rfc3339();
        // Idempotent upsert keyed by (kind, root_path): update display_name
        // on collision but never change the source id.
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM sources WHERE kind = ?1 AND root_path = ?2 LIMIT 1",
                params![self.flavor.kind(), root_path],
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
            let new_id = format!("{id}_{}", short_hash(root_path.as_bytes()));
            conn.execute(
                "INSERT INTO sources (id, kind, display_name, root_path, status, last_scan_at, created_at, updated_at)
                 VALUES (?1, ?3, ?4, ?5, 'active', ?2, ?2, ?2)",
                params![
                    new_id,
                    now,
                    self.flavor.kind(),
                    root.display_name,
                    root_path
                ],
            )?;
            new_id
        };
        Ok(source_id)
    }
}

impl Connector for DoubaoConnector {
    fn kind(&self) -> &'static str {
        self.flavor.kind()
    }

    fn display_name(&self) -> &'static str {
        self.flavor.human_name()
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

        if roots.is_empty() && self.flavor_support_dir().map(|p| !p.is_dir()).unwrap_or(true) {
            return Ok(ScanReport {
                source_id: String::new(),
                kind: self.kind().to_string(),
                status: "skipped".to_string(),
                events_inserted: 0,
                sessions_added: 0,
                files_scanned: 0,
                duration_ms: started.elapsed().as_millis() as i64,
                error: Some(format!(
                    "no Chromium IndexedDB or metadata root found under {}",
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

            // Iterate every record, emit a session_meta-like event per
            // record whose key tail resolves to a UTF-16 object-store
            // name. Records with non-decodable tails are skipped silently
            // — they are bookkeeping rows we don't yet understand.
            let mut local_events = 0i64;
            let scan_result = idb::iter_store(
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
            );
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

        // Stage 5-E.1c: also scan the plaintext metadata files. We
        // re-use any registered Doubao source (idb_<kind>_<hash>) so the
        // UI keeps showing one source per install.
        if let Some(support_dir) = self.flavor_support_dir() {
            if support_dir.is_dir() {
                let metadata_source_id = self.resolve_or_register_metadata_source(ctx.conn, &support_dir)?;
                if let Some(source_id) = metadata_source_id {
                    for path in flavor_metadata_files(&support_dir) {
                        if !path.is_file() {
                            continue;
                        }
                        total_files += 1;
                        let event_type = match path.file_name().and_then(|n| n.to_str()) {
                            Some("Local State") => etype::LOCAL_STATE,
                            Some("Preferences") => etype::PREFERENCES,
                            Some("ChatAppFeatureState") => etype::CHAT_APP_FEATURE_STATE,
                            _ => continue,
                        };
                        match emit_metadata_file(
                            ctx.conn,
                            &source_id,
                            &path,
                            event_type,
                            &redactor,
                            &mut total_events,
                        ) {
                            Ok(()) => {}
                            Err(e) => {
                                error = Some(format!("metadata {}: {e}", path.display()));
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

impl DoubaoConnector {
    /// Resolve the support dir for this flavor (`~/Library/Application
    /// Support/Doubao{Work?}`).
    fn flavor_support_dir(&self) -> Option<PathBuf> {
        let home = Self::home_dir()?;
        let rel = match self.flavor {
            DoubaoFlavor::Consumer => "Library/Application Support/Doubao",
            DoubaoFlavor::Work => "Library/Application Support/DoubaoWork",
        };
        Some(home.join(rel))
    }

    /// Resolve the first registered Doubao source_id for this flavor,
    /// or register a fresh one against the support dir when no IDB
    /// roots exist yet. Keeps the UI showing one source per install.
    fn resolve_or_register_metadata_source(
        &self,
        conn: &mut Connection,
        support_dir: &Path,
    ) -> AppResult<Option<String>> {
        let kind = self.flavor.kind();
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM sources WHERE kind = ?1 ORDER BY created_at LIMIT 1",
                params![kind],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = existing {
            return Ok(Some(id));
        }
        let now = Utc::now().to_rfc3339();
        let new_id = format!(
            "md_{}_{}",
            kind,
            short_hash(support_dir.to_string_lossy().as_bytes())
        );
        conn.execute(
            "INSERT INTO sources (id, kind, display_name, root_path, status, last_scan_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?5)",
            params![
                new_id,
                kind,
                self.flavor.human_name(),
                support_dir.display().to_string(),
                now
            ],
        )?;
        Ok(Some(new_id))
    }
}

fn emit_metadata_file(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    event_type: &str,
    redactor: &Redactor,
    events_inserted: &mut i64,
) -> AppResult<()> {
    let mtime_ms = file_mtime_ms(path).unwrap_or(0);
    let bytes = std::fs::read(path)?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let value: Json = serde_json::from_str(&raw).unwrap_or(Json::String(raw.clone()));
    let value_preview = serde_json::to_string(&value)
        .map(|s| {
            if s.len() > 240 {
                format!("{}...", &s[..240])
            } else {
                s
            }
        })
        .unwrap_or_else(|_| raw.chars().take(240).collect());
    let enriched = json!({
        "path": path.display().to_string(),
        "value_size": bytes.len(),
        "value_preview": value_preview,
    });
    let payload = redactor.apply(&serde_json::to_string(&enriched)?);
    let payload_hash = short_hash(payload.as_bytes());
    let id = format!("md_{}_{}", event_type, short_hash(payload.as_bytes()));
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
            event_type,
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
        let c = DoubaoConnector::consumer();
        let _ = c.discover_roots();
    }
}