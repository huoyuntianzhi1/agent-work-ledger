//! Codex (OpenAI) connector — reads `~/.codex/sessions/**/*.jsonl.zst` and
//! the supporting SQLite buckets under the same root.
//!
//! Only **read** access. Nothing here mutates Codex files.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::connectors::{kinds, path_day_dir_before, should_skip_iso, Connector, ScanCtx, ScanReport};
use crate::error::{AppError, AppResult};
use crate::redactor::Redactor;

/// Default Codex root on macOS. Other platforms: `%USERPROFILE%\.codex` etc.
fn default_roots() -> Vec<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        vec![home.join(".codex")]
    } else {
        Vec::new()
    }
}

/// Normalize a Codex roll-out file path into the project key this event
/// belongs to. Falls back to `unknown` when there's no `cwd` in the meta.
fn derive_project_key(meta: &RolloutMeta) -> String {
    meta.cwd
        .clone()
        .or_else(|| meta.runtime_workspace_roots.as_ref().and_then(|r| r.first().cloned()))
        .or_else(|| meta.environment_cwd.clone())
        .map(|cwd| normalize_project_path(&cwd))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Strip `file://`-prefixes and return a canonical-like path string. We keep
/// this conservative — full canonicalization on every event would be costly.
fn normalize_project_path(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

/// One concrete CodeX rollout event line, decoded from JSON. We keep the
/// original payload as `payload` so the database can store every type without
/// losing fidelity.
#[derive(Debug, Deserialize)]
struct RolloutLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<Json>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct RolloutMeta {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub runtime_workspace_roots: Option<Vec<String>>,
    #[serde(default)]
    pub environment_cwd: Option<String>,
    #[serde(default)]
    pub thread_source: Option<String>,
    #[serde(default)]
    pub originator: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
}

pub struct CodexConnector;

impl Connector for CodexConnector {
    fn kind(&self) -> &'static str {
        kinds::CODEX
    }
    fn display_name(&self) -> &'static str {
        "OpenAI Codex"
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

        // 1. Locate the root path. If absent, this connector just reports a
        //    clean "no root" without failing the whole scan.
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
                    error: Some("~/.codex not present on this host".into()),
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

        // 2. Apply privacy to file-level exclusions before we even open files.
        let redactor = Redactor::from_db(conn)?;

        // 3. Walk sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl.zst.
        let sessions_dir = root.join("sessions");
        let mut events_inserted: i64 = 0;
        let mut files_scanned: i64 = 0;

        if sessions_dir.is_dir() {
            let walker = walkdir::WalkDir::new(&sessions_dir)
                .into_iter()
                .filter_map(|e| e.ok());
            for entry in walker {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                let name = match path.file_name().and_then(|s| s.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if !name.starts_with("rollout-") || !name.ends_with(".jsonl.zst") {
                    continue;
                }
                files_scanned += 1;

                // Honour the user-supplied `since_ms` by skipping whole
                // session-day directories first — avoids paying the
                // zstd + JSONL parse cost for events the UI never asked
                // for. Returns `false` when the path doesn't fit the
                // YYYY/MM/DD shape so legacy layouts fall through to the
                // per-row timestamp check inside `process_rollout_file`.
                if path_day_dir_before(ctx.since_ms, path) {
                    continue;
                }

                // Skip files inside any excluded directory.
                if redactor.is_path_excluded(conn, path)? {
                    continue;
                }

                match process_rollout_file(conn, &source_id, path, &redactor, ctx.since_ms) {
                    Ok(ev) => {
                        events_inserted += ev as i64;
                    }
                    Err(e) => {
                        tracing::warn!(file = %path.display(), error = %e, "codex: failed to process roll-out, skipping file");
                    }
                }
            }
        }

        // 4. Update the connector_runs summary row.
        let ended_at = crate::connectors::now_ms();
        conn.execute(
            "UPDATE connector_runs SET ended_at = ?1, status = 'ok', events_count = ?2 WHERE id = ?3",
            params![
                crate::connectors::rfc3339(Some(ended_at)),
                events_inserted,
                run_id
            ],
        )?;

        // 5. Update sources.last_scan_at + status.
        conn.execute(
            "UPDATE sources SET last_scan_at = ?1, status = 'readable', updated_at = ?1 WHERE id = ?2",
            params![crate::connectors::rfc3339(Some(ended_at)), source_id],
        )?;

        Ok(ScanReport {
            source_id,
            kind: self.kind().into(),
            status: "ok".into(),
            events_inserted,
            // v0.2: ETL (Phase B.4) owns sessions_summary. The connector
            // surface still kicks off ETL after a scan lands; the field
            // is always 0 from the connector's perspective.
            sessions_added: 0,
            files_scanned,
            duration_ms: started.elapsed().as_millis() as i64,
            error: None,
        })
    }
}

/// Idempotent source-row upsert. Returns the (possibly newly generated) id.
pub fn upsert_source(
    conn: &Connection,
    kind: &str,
    display_name: &str,
    root_path: Option<&Path>,
    status: &str,
) -> AppResult<String> {
    if let Some(root) = root_path {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM sources WHERE kind = ?1 AND root_path = ?2",
                params![kind, root.display().to_string()],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = existing {
            return Ok(id);
        }
    }
    let id = format!("src_{}", uuid::Uuid::new_v4().simple());
    let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    conn.execute(
        "INSERT INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            id,
            kind,
            display_name,
            root_path.map(|p| p.display().to_string()).unwrap_or_default(),
            status,
            now
        ],
    )?;
    Ok(id)
}

/// Read + decompress + parse + insert all roll-out events from one file.
/// v0.2 no longer writes v0.1 `sessions` rows; ETL (Phase B.4) handles
/// `sessions_summary` from the raw_events we insert here.
///
/// `since_ms` propagates the user-supplied scan window (see ScanCtx) to
/// every row. Codex roll-outs use an RFC-3339 `timestamp` field per
/// line, so we compare the parsed event time against the cutoff before
/// paying the redact + INSERT cost.
fn process_rollout_file(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    // 1. Read whole file, zstd-decode.
    let bytes = {
        let mut f = File::open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        zstd::decode_all(&buf[..])?
    };

    // 2. Walk JSONL line by line.
    let reader = BufReader::new(&bytes[..]);
    let mut session_meta: Option<RolloutMeta> = None;
    let mut session_id: Option<String> = None;
    let mut project_key: Option<String> = None;
    let mut count = 0usize;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: RolloutLine = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines; do not abort the file
        };

        // session_meta is the only line we DON'T insert as a raw event —
        // its data rides on the session row instead, kept in meta_json.
        if parsed.kind == "session_meta" {
            if let Some(payload) = parsed.payload.as_ref() {
                let meta: RolloutMeta = serde_json::from_value(payload.clone())
                    .unwrap_or_default();
                if session_id.is_none() {
                    session_id = meta.session_id.clone();
                }
                project_key = Some(derive_project_key(&meta));
                session_meta = Some(meta);
            }
            continue;
        }

        // Carry session_id / project_key forward even if meta wasn't captured.
        if session_id.is_none() {
            session_id = session_meta
                .as_ref()
                .and_then(|m| m.session_id.clone())
                .or_else(|| {
                    parsed.payload.as_ref().and_then(|p| {
                        p.get("session_id").and_then(|v| v.as_str().map(String::from))
                    })
                });
        }
        if project_key.is_none() {
            if let Some(m) = session_meta.as_ref() {
                project_key = Some(derive_project_key(m));
            }
        }

        // Map the rollout `type` onto our internal `event_type`. We default
        // to "other:<kind>" for anything not on the recognised list so we
        // never silently drop data the user might want to inspect.
        let event_kind = map_event_kind(&parsed);

        // Redact the payload text. We keep the structure so that downstream
        // timeline / aggregation can still group on key fields without
        // seeing raw secrets.
        let raw_payload = serde_json::to_string(&parsed.payload.unwrap_or(Json::Null))?;
        let redacted_payload = redactor.apply(&raw_payload);

        let occurred_at = parsed
            .timestamp
            .clone()
            .unwrap_or_else(|| crate::connectors::rfc3339(Some(crate::connectors::now_ms())));

        // Honour the user-supplied scan window. Unparseable timestamps
        // (rare but possible in malformed roll-outs) fall through to the
        // INSERT — we'd rather keep them than silently drop data.
        if should_skip_iso(since_ms, &occurred_at) {
            continue;
        }

        let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
        let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
        let payload_hash = sha256_hex(&redacted_payload);

        // INSERT OR IGNORE: re-scanning the same files becomes idempotent.
        // The unique index on (source_id, event_type, payload_hash) sees the
        // duplicate and silently skips it, so re-runs report 0 inserts.
        conn.execute(
            "INSERT OR IGNORE INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
            params![
                id,
                source_id,
                event_kind,
                occurred_at,
                project_key.clone().unwrap_or_else(|| "unknown".into()),
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
/// Used as the dedup key on `raw_events.payload_hash`.
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

/// Map a Codex roll-out event kind onto our internal `event_type`. Anything
/// not on the recognised list goes into `other:<name>` so we never drop
/// data — `Recent events` will still surface it.
fn map_event_kind(parsed: &RolloutLine) -> String {
    let t = parsed.kind.as_str();
    match t {
        // session_meta is handled separately in the caller; we never hit
        // this branch because of the early `continue`.
        "session_meta" => "session_meta".into(),
        // `event_msg` covers lifecycle markers like task_started /
        // task_complete / agent_reasoning. Promote them to "message".
        "event_msg" => "message".into(),
        // `response_item` carries both text and tool calls; disambiguate by
        // looking at payload.type.
        "response_item" => match parsed
            .payload
            .as_ref()
            .and_then(|p| p.get("type"))
            .and_then(|v| v.as_str())
        {
            Some("function_call") | Some("function_call_output") => "tool_call".into(),
            Some("web_search") => "tool_call".into(),
            Some("custom_tool_call") | Some("custom_tool_call_output") => "tool_call".into(),
            Some("reasoning") => "message".into(),
            _ => "message".into(),
        },
        // Model settings changed within the session — keep as a state event.
        "turn_context" => "session_state".into(),
        // Token usage and similar metrics — useful but not chat-shaped.
        "token_count" => "metric".into(),
        // Compaction is OpenAI's mid-context summarization — log it.
        "compaction" => "metric".into(),
        // Defensive legacy names — accept if we ever see them.
        "message" | "user_message" | "assistant_message" => "message".into(),
        "tool_call" | "tool_call_result" | "function_call" | "function_call_output" => {
            "tool_call".into()
        }
        "file_change" | "file_edit" => "file_change".into(),
        "shell_command" | "command" => "shell_command".into(),
        "git_commit" => "git_commit".into(),
        // Anything else: keep the data, label it generically.
        other => format!("other:{other}"),
    }
}

// Suppress unused-import warnings for items wired up only after API
// expansion in Stage 1-C/D/E.
#[allow(dead_code)]
fn _force_used() -> std::result::Result<(), AppError> {
    let _ = AppError::UnknownConnector(String::new());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_digest() {
        // sha256("hello") -> 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let got = sha256_hex("hello");
        assert_eq!(
            got,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha256_is_stable() {
        let a = sha256_hex("the quick brown fox");
        let b = sha256_hex("the quick brown fox");
        assert_eq!(a, b);
    }
}
