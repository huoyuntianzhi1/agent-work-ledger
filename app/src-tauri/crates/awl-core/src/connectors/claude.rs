//! Claude Code connector — reads `~/.claude/projects/**/*.jsonl`.
//!
//! Each line in the file is one event in the session transcript. We treat the
//! whole file as a single Claude Code session because that matches Claude
//! Code's own `sessionId` column on every line. We only **read** — nothing
//! here mutates Claude Code files.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{params, Connection};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::connectors::codex::upsert_source;
use crate::connectors::{kinds, should_skip_iso, Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::redactor::Redactor;

/// Default Claude Code root on macOS. Other platforms: `%USERPROFILE%\.claude`.
fn default_roots() -> Vec<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        vec![home.join(".claude")]
    } else {
        Vec::new()
    }
}

pub struct ClaudeConnector;

impl Connector for ClaudeConnector {
    fn kind(&self) -> &'static str {
        kinds::CLAUDE_CODE
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
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

        // 1. Locate the root path. If absent, report a clean "no root"
        //    without failing the whole scan.
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
                    error: Some("~/.claude not present on this host".into()),
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

        // 3. Walk projects/**/*.jsonl. Skip the sibling `skills/`, `agents/`,
        //    `plugins/` etc. — only transcript jsonl files belong here.
        let projects_dir = root.join("projects");
        let mut events_inserted: i64 = 0;
        let mut files_scanned: i64 = 0;

        if projects_dir.is_dir() {
            let walker = walkdir::WalkDir::new(&projects_dir)
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
                // Only transcripts are JSONL; sessions-index.json is JSON.
                if !name.ends_with(".jsonl") {
                    continue;
                }
                files_scanned += 1;

                if redactor.is_path_excluded(conn, path)? {
                    continue;
                }

                match process_jsonl_file(conn, &source_id, path, &redactor, ctx.since_ms) {
                    Ok(ev) => {
                        events_inserted += ev as i64;
                    }
                    Err(e) => {
                        tracing::warn!(file = %path.display(), error = %e, "claude: failed to process transcript, skipping file");
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
            sessions_added: 0,
            files_scanned,
            duration_ms: started.elapsed().as_millis() as i64,
            error: None,
        })
    }
}

/// Read + parse + insert all events from one transcript JSONL file. Each
/// file is one Claude Code session; the `sessionId` is repeated on every
/// line so we only need to capture it once.
///
/// `since_ms` filters out lines whose `timestamp` is older than the
/// caller-supplied cutoff. We do not filter whole files at the path
/// level — Claude's transcripts live under flat `<project-hash>/<uuid>.jsonl`
/// without a YYYY/MM/DD hierarchy, so per-row is the only sensible layer.
fn process_jsonl_file(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    redactor: &Redactor,
    since_ms: Option<i64>,
) -> AppResult<usize> {
    // 1. Read whole file (transcripts are small plaintext JSONL — no zstd).
    let bytes = {
        let mut f = File::open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        buf
    };

    let reader = BufReader::new(&bytes[..]);
    let mut session_id: Option<String> = None;
    let mut project_key: Option<String> = None;
    let mut count = 0usize;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: Json = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines; do not abort the file
        };

        // Pull fields used for session/project identity. We tolerate missing
        // values so the connector never crashes on a partial transcript.
        let parsed_session_id = parsed
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from);
        let parsed_cwd = parsed.get("cwd").and_then(|v| v.as_str()).map(String::from);
        let parsed_kind = parsed
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let parsed_timestamp = parsed
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(String::from);

        if session_id.is_none() {
            session_id = parsed_session_id.clone();
        }
        if project_key.is_none() {
            project_key = parsed_cwd.clone();
        }

        let event_kind = map_event_kind(&parsed_kind);

        // Redact the full payload so secrets in tool args, assistant
        // thinking, etc. never reach SQLite.
        let raw_payload = serde_json::to_string(&parsed)?;
        let redacted_payload = redactor.apply(&raw_payload);

        let occurred_at = parsed_timestamp
            .clone()
            .unwrap_or_else(|| crate::connectors::rfc3339(Some(crate::connectors::now_ms())));

        // Honour the user-supplied scan window. Unparseable timestamps
        // fall through to INSERT — keep rather than silently drop.
        if should_skip_iso(since_ms, &occurred_at) {
            continue;
        }

        let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
        let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
        let payload_hash = sha256_hex(&redacted_payload);

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

/// Map a Claude Code transcript `type` onto our internal `event_type`. We
/// accept all real Claude Code shapes seen on disk; everything else goes
/// into `other:<name>` so we never drop data.
fn map_event_kind(t: &str) -> String {
    match t {
        "user" => "message".into(),
        "assistant" => "message".into(),
        // tool_use / tool_result typically live inside `message.content`
        // rather than as top-level types, but Claude Code also emits them
        // directly in some flows — accept both.
        "tool_use" => "tool_call".into(),
        "tool_result" => "tool_call".into(),
        "progress" => "progress".into(),
        "system" => "system".into(),
        "file-history-snapshot" => "snapshot".into(),
        "rate_limit_reached_error" => "error".into(),
        // session metadata variants — kept as `session_state` for parity
        // with the Codex connector.
        "session_meta" | "session_state" => "session_state".into(),
        // Defensive legacy / hypothetical shapes — never drop data.
        "message" | "user_message" | "assistant_message" => "message".into(),
        "file_change" | "file_edit" => "file_change".into(),
        "shell_command" | "command" => "shell_command".into(),
        "git_commit" => "git_commit".into(),
        "summary" => "message".into(),
        other => format!("other:{other}"),
    }
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
    fn map_event_kind_known_shapes() {
        assert_eq!(map_event_kind("user"), "message");
        assert_eq!(map_event_kind("assistant"), "message");
        assert_eq!(map_event_kind("tool_use"), "tool_call");
        assert_eq!(map_event_kind("tool_result"), "tool_call");
        assert_eq!(map_event_kind("file-history-snapshot"), "snapshot");
        assert_eq!(map_event_kind("rate_limit_reached_error"), "error");
        assert_eq!(map_event_kind("progress"), "progress");
    }

    #[test]
    fn map_event_kind_unknown_keeps_data() {
        assert_eq!(map_event_kind("totally_new_thing"), "other:totally_new_thing");
    }
}