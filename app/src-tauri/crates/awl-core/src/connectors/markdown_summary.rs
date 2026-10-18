//! Stage 5-E.2b/c: Markdown summary scanner.
//!
//! Walks the user-configured `summary_dir` (Stage 5-E.2a) one level
//! deep for `.md` files, parses the YAML frontmatter block (delimited
//! by `---` lines at the top of the file), and emits one
//! `markdown_summary` raw_event per file with:
//!
//! - `occurred_at`: frontmatter `date:` if present (ISO-8601), else
//!   file mtime.
//! - `project_key`: frontmatter `project:` if present, else
//!   `markdown_summary`.
//! - `payload`: JSON `{ frontmatter, body }` so downstream consumers
//!   can re-render the markdown without re-reading the file.
//!
//! Files are deduplicated via the existing `payload_hash` UNIQUE INDEX
//! on `raw_events`, so re-scans are no-ops unless the user edited the
//! file (or the frontmatter `date:` changed).
//!
//! Frontmatter parsing is intentionally minimal: only flat
//! `key: value` lines are understood. No nested maps, no lists, no
//! quoted strings beyond the simple `"..."` form. Users with richer
//! needs can fall back to writing their own YAML and having a
//! downstream consumer re-parse the `frontmatter` JSON blob.

use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::connectors::codex::upsert_source;
use crate::connectors::{Connector, ScanCtx, ScanReport};
use crate::error::AppResult;
use crate::redactor::Redactor;

/// Public kind label; mirrored in `kinds::MARKDOWN_SUMMARY`.
pub const MARKDOWN_KIND: &str = crate::connectors::kinds::MARKDOWN_SUMMARY;

/// Event types emitted by the markdown scanner. Kept distinct from
/// the legacy `session_meta` / `ai_agent_*` vocabulary so the timeline
/// can filter / colour by hand-written vs auto-captured.
pub mod etype {
    pub const MARKDOWN_SUMMARY: &str = "markdown_summary";
}

/// Result of splitting a `.md` file into its frontmatter (parsed
/// JSON object) and body (raw markdown, no frontmatter markers).
#[derive(Debug)]
struct ParsedMarkdown {
    frontmatter: Json,
    body: String,
}

pub struct MarkdownSummaryConnector;

impl MarkdownSummaryConnector {
    pub fn new() -> Self {
        Self
    }

    /// Read the configured summary_dir from `app_settings.json` and
    /// return it if the directory exists. Returns `None` when the user
    /// hasn't configured anything yet — the connector then becomes a
    /// no-op rather than erroring on every scan.
    fn resolve_summary_dir() -> Option<PathBuf> {
        let s = crate::app_settings::load();
        let expanded = crate::app_settings::expand_tilde(&s.summary_dir);
        let p = PathBuf::from(expanded);
        if p.is_dir() {
            Some(p)
        } else {
            None
        }
    }
}

impl Default for MarkdownSummaryConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Connector for MarkdownSummaryConnector {
    fn kind(&self) -> &'static str {
        MARKDOWN_KIND
    }

    fn display_name(&self) -> &'static str {
        "Markdown Summary"
    }

    fn discover_roots(&self) -> Vec<PathBuf> {
        Self::resolve_summary_dir().into_iter().collect()
    }

    fn scan(&self, ctx: ScanCtx<'_>) -> AppResult<ScanReport> {
        let started = Instant::now();
        let conn = ctx.conn;

        let summary_dir = match Self::resolve_summary_dir() {
            Some(d) => d,
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
                    error: Some(
                        "summary_dir not configured or directory does not exist; \
                         open Settings → 本地配置 → Markdown 摘要目录"
                            .into(),
                    ),
                });
            }
        };

        let source_id = upsert_source(
            conn,
            self.kind(),
            self.display_name(),
            Some(&summary_dir),
            "readable",
        )?;

        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let now = crate::connectors::now_ms();
        conn.execute(
            "INSERT INTO connector_runs(id, source_id, started_at, status, events_count) VALUES (?1, ?2, ?3, 'running', 0)",
            params![run_id, source_id, crate::connectors::rfc3339(Some(now))],
        )?;

        let redactor = Redactor::from_db(conn)?;
        let mut files_scanned: i64 = 0;
        let mut events_inserted: i64 = 0;

        for entry in WalkDir::new(&summary_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            files_scanned += 1;
            if let Err(e) = ingest_one(
                conn,
                &source_id,
                entry.path(),
                &redactor,
                &mut events_inserted,
            ) {
                tracing::warn!(
                    file = %entry.path().display(),
                    error = %e,
                    "markdown_summary: failed to ingest file; skipping"
                );
            }
        }

        let events_count = events_inserted;
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

// ---------------------------------------------------------------------
// Per-file ingest
// ---------------------------------------------------------------------

fn ingest_one(
    conn: &Connection,
    source_id: &str,
    path: &Path,
    redactor: &Redactor,
    events_inserted: &mut i64,
) -> AppResult<()> {
    let bytes = std::fs::read(path)?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = parse_frontmatter(&raw);
    let fm = &parsed.frontmatter;

    let project_key = fm
        .get("project")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "markdown_summary".to_string());

    let occurred_at = fm
        .get("date")
        .and_then(|v| v.as_str())
        .and_then(parse_iso_to_rfc3339)
        .unwrap_or_else(|| {
            file_mtime_rfc3339(path).unwrap_or_else(|| Utc::now().to_rfc3339())
        });

    let payload = json!({
        "path": path.display().to_string(),
        "frontmatter": fm,
        "body": parsed.body,
    });
    let redacted_payload = redactor.apply(&payload.to_string());
    let payload_hash = short_hash(redacted_payload.as_bytes());

    let id = format!("md_{}", short_hash(
        format!("{}:{}", path.display(), payload_hash).as_bytes()
    ));
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO raw_events
            (id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
        params![
            id,
            source_id,
            etype::MARKDOWN_SUMMARY,
            occurred_at,
            project_key,
            redacted_payload,
            payload_hash,
            now
        ],
    )?;
    *events_inserted += 1;
    Ok(())
}

// ---------------------------------------------------------------------
// Frontmatter parser (intentionally minimal)
// ---------------------------------------------------------------------

/// Split a markdown document into frontmatter JSON + body. If the
/// file does not start with a `---` line, the frontmatter is an
/// empty object and the entire file is body.
pub fn parse_frontmatter(raw: &str) -> ParsedMarkdown {
    let trimmed = raw.trim_start_matches('\u{feff}');
    if !trimmed.starts_with("---") {
        return ParsedMarkdown {
            frontmatter: Json::Object(Map::new()),
            body: raw.to_string(),
        };
    }
    // The first line must be exactly "---" (possibly with trailing
    // whitespace). Otherwise we don't trust it as a frontmatter
    // delimiter.
    let first_line = trimmed.lines().next().unwrap_or("");
    if first_line.trim() != "---" {
        return ParsedMarkdown {
            frontmatter: Json::Object(Map::new()),
            body: raw.to_string(),
        };
    }
    let rest = &trimmed[first_line.len() + 1..];
    let closing_idx = rest.find("\n---");
    let (yaml_block, body_start_offset) = match closing_idx {
        Some(i) => (&rest[..i], i + 4), // skip "\n---"
        None => {
            // Unterminated frontmatter — treat whole file as body.
            return ParsedMarkdown {
                frontmatter: Json::Object(Map::new()),
                body: raw.to_string(),
            };
        }
    };
    // Skip the newline after closing `---`.
    let body = rest[body_start_offset..]
        .strip_prefix('\n')
        .unwrap_or(&rest[body_start_offset..])
        .to_string();
    ParsedMarkdown {
        frontmatter: parse_simple_yaml(yaml_block),
        body,
    }
}

/// Parse flat `key: value` lines into a JSON object. Empty lines and
/// `#`-prefixed comments are ignored. Values are stored as strings
/// (with surrounding quotes stripped) or coerced to bool / int /
/// float when the textual form matches. Anything weirder stays a
/// string.
fn parse_simple_yaml(yaml: &str) -> Json {
    let mut map = Map::new();
    for line in yaml.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let raw_value = v.trim();
        map.insert(key, coerce_yaml_scalar(raw_value));
    }
    Json::Object(map)
}

/// Coerce a YAML scalar to bool / int / float / string. We do not try
/// to parse nested structures — anything that looks like JSON object
/// or array stays a string for now.
fn coerce_yaml_scalar(v: &str) -> Json {
    match v {
        "" => Json::Null,
        "true" | "True" | "TRUE" => Json::Bool(true),
        "false" | "False" | "FALSE" => Json::Bool(false),
        "null" | "Null" | "NULL" | "~" => Json::Null,
        _ => {
            if let Ok(i) = v.parse::<i64>() {
                return Json::Number(i.into());
            }
            if let Ok(f) = v.parse::<f64>() {
                if let Some(n) = serde_json::Number::from_f64(f) {
                    return Json::Number(n);
                }
            }
            // Strip matching surrounding quotes if any.
            let stripped = strip_quotes(v);
            Json::String(stripped)
        }
    }
}

fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

fn parse_iso_to_rfc3339(s: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
}

fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dt = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(dt.as_millis() as i64)
        .map(|t| t.to_rfc3339())
}

fn short_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::test_locks::HOME_LOCK;

    #[test]
    fn parse_frontmatter_with_date_and_project() {
        let raw = "---\nproject: /Users/apple/Desktop/foo\ndate: 2026-04-15T10:00:00Z\nimportance: 7\n---\n\n今天做完了 Y。\n\n明天做 Z。\n";
        let p = parse_frontmatter(raw);
        assert_eq!(
            p.frontmatter.get("project").and_then(|v| v.as_str()),
            Some("/Users/apple/Desktop/foo")
        );
        assert_eq!(
            p.frontmatter.get("date").and_then(|v| v.as_str()),
            Some("2026-04-15T10:00:00Z")
        );
        assert_eq!(
            p.frontmatter.get("importance").and_then(|v| v.as_i64()),
            Some(7)
        );
        assert!(p.body.starts_with("\n今天做完了 Y。"));
    }

    #[test]
    fn parse_frontmatter_no_delimiter_returns_empty_fm() {
        let raw = "Hello world.\nNo frontmatter here.\n";
        let p = parse_frontmatter(raw);
        assert!(p.frontmatter.as_object().unwrap().is_empty());
        assert_eq!(p.body, raw);
    }

    #[test]
    fn coerce_yaml_scalar_handles_bool_int_string() {
        assert_eq!(coerce_yaml_scalar("true"), Json::Bool(true));
        assert_eq!(coerce_yaml_scalar("FALSE"), Json::Bool(false));
        assert_eq!(coerce_yaml_scalar("42"), Json::Number(42.into()));
        assert_eq!(coerce_yaml_scalar("-3"), Json::Number((-3).into()));
        assert_eq!(
            coerce_yaml_scalar("\"hello world\""),
            Json::String("hello world".into())
        );
        assert_eq!(
            coerce_yaml_scalar("plain string"),
            Json::String("plain string".into())
        );
    }

    #[test]
    fn short_hash_is_stable_and_short() {
        let a = short_hash(b"hello");
        let b = short_hash(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    /// Round-trip a hand-written summary through the scan pipeline
    /// against an in-memory ledger. Verifies the parser + dedup path
    /// end-to-end.
    #[test]
    fn scan_emits_markdown_summary_with_frontmatter() {
        let _guard = HOME_LOCK.lock().unwrap();

        let mut conn = Connection::open_in_memory().expect("open in-memory");
        awl_db::migrate_for_tests(&conn).expect("migrate");

        let scratch = std::env::temp_dir().join(format!(
            "agent-work-ledger-md-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&scratch).expect("mkdir scratch");
        let md_path = scratch.join("test-summary.md");
        std::fs::write(
            &md_path,
            "---\nproject: /tmp/test\ndate: 2026-04-15T10:00:00Z\nimportance: 8\nurgency: 7\n---\n\n今天做完了 A。\n",
        )
        .expect("write md");

        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &scratch);

        let result = (|| -> AppResult<()> {
            // Override the configured summary_dir via app_settings
            // would normally be persisted; for this unit test we
            // bypass `app_settings::load()` and patch HOME so the
            // connector sees the directory. We simulate by symlinking
            // `~/Documents/agent_summaries -> scratch`.
            let docs = scratch.join("Documents");
            let summaries = docs.join("agent_summaries");
            std::fs::create_dir_all(&summaries).expect("mkdir summaries");
            std::fs::copy(&md_path, summaries.join("test-summary.md"))
                .expect("copy md");

            // Re-set HOME so resolve_summary_dir sees the right path.
            std::env::set_var("HOME", &scratch);
            let connector = MarkdownSummaryConnector::new();
            let report = connector.scan(ScanCtx {
                conn: &mut conn,
                source_id: "",
                since_ms: None,
            })?;
            assert_eq!(report.events_inserted, 1, "expected 1 event, got {:?}", report);
            assert!(report.error.is_none(), "unexpected error: {:?}", report.error);

            let mut stmt = conn
                .prepare("SELECT event_type, project_key, payload FROM raw_events")
                .expect("prepare");
            let rows: Vec<(String, String, String)> = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .expect("query")
                .filter_map(|r| r.ok())
                .collect();
            assert_eq!(rows.len(), 1);
            let (etype, project_key, payload) = (&rows[0].0, &rows[0].1, &rows[0].2);
            assert_eq!(etype, etype::MARKDOWN_SUMMARY);
            assert_eq!(project_key, "/tmp/test");
            assert!(payload.contains("今天做完了 A"));
            assert!(payload.contains("importance"));
            Ok(())
        })();

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&scratch);
        result.expect("scan assertions");
    }
}
