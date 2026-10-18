//! v0.3.7 / Stage 5.7: cross-source search.
//!
//! One user query fans out across the four searchable tables
//! (`sessions_summary`, `raw_events`, `plan_items`, `sources`) and
//! returns a unified `Vec<SearchHit>` sorted by recency. Designed
//! for the "Cmd-K" overlay in the top toolbar — the user types a
//! fragment, sees the top-N hits across all sources, and clicks
//! through to the underlying partition / event / plan item.
//!
//! Why not FTS5?
//! - Adds a maintenance cost (triggers on every INSERT, schema
//!   migration on user DBs, separate `*_fts` tables).
//! - Our text volume is small (sessions: hundreds, events: tens of
//!   thousands, plans: dozens) — a per-source `LIKE` UNION is
//!   plenty fast on indexed columns. v0.3.7 ships the simple
//!   version; if profiling later shows it's the bottleneck we
//!   upgrade to FTS5 in a single focused change.
//!
//! The query is escaped against LIKE metachars so a user typing
//! `100%` doesn't accidentally match everything. We also reject
//! empty / 1-char queries to avoid the full-table scan hot path.

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::error::AppResult;

const PER_SOURCE_LIMIT: i64 = 20;
const TOTAL_LIMIT: i64 = 50;
const MIN_QUERY_LEN: usize = 2;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// One of `"session"`, `"event"`, `"plan"`, `"source"`.
    pub kind: String,
    /// Primary key of the row in its own table (e.g. `sessions_summary.id`).
    pub id: String,
    pub source_id: String,
    pub source_display_name: String,
    /// Best display title for the hit (e.g. plan title, event type,
    /// or the source's project key). Trimmed to ~80 chars.
    pub title: String,
    /// Short context snippet surrounding the match (~140 chars),
    /// when available. Empty for hits without a text column.
    pub snippet: String,
    pub project_key: Option<String>,
    /// RFC3339 timestamp for sort stability, when the table has one.
    pub started_at: Option<String>,
}

/// Run the cross-source search. Returns at most `TOTAL_LIMIT` hits
/// with the per-source results interleaved by recency. Empty /
/// too-short query returns an empty vec.
pub fn global_search(conn: &Connection, query: &str) -> AppResult<Vec<SearchHit>> {
    let q = query.trim();
    if q.len() < MIN_QUERY_LEN {
        return Ok(Vec::new());
    }
    let like = format!("%{}%", escape_like(q));
    let mut hits = Vec::new();
    hits.extend(search_sessions(conn, &like)?);
    hits.extend(search_events(conn, &like)?);
    hits.extend(search_plans(conn, &like)?);
    hits.extend(search_sources(conn, &like)?);

    // Sort newest-first; rows without a timestamp sink to the bottom.
    hits.sort_by(|a, b| match (&b.started_at, &a.started_at) {
        (Some(b_t), Some(a_t)) => b_t.cmp(a_t),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    hits.truncate(TOTAL_LIMIT as usize);
    Ok(hits)
}

/// Escape SQL LIKE metachars so user input is treated as a literal
/// substring. `\` is the explicit ESCAPE char.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

fn search_sessions(conn: &Connection, like: &str) -> AppResult<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT s.id, s.source_id, src.name, s.project_key, s.summary, s.started_at
          FROM sessions_summary s
          JOIN sources src ON src.id = s.source_id
         WHERE s.summary LIKE ?1 ESCAPE '\' OR s.project_key LIKE ?1 ESCAPE '\'
         ORDER BY s.started_at DESC
         LIMIT ?2
        "#,
    )?;
    let mut out = Vec::new();
    let rows = stmt.query_map(params![like, PER_SOURCE_LIMIT], |row| {
        Ok(SearchHit {
            kind: "session".into(),
            id: row.get(0)?,
            source_id: row.get(1)?,
            source_display_name: row.get(2)?,
            project_key: row.get::<_, Option<String>>(3)?,
            title: row
                .get::<_, Option<String>>(4)?
                .map(|s| truncate(&s, 80))
                .unwrap_or_default(),
            snippet: row
                .get::<_, Option<String>>(4)?
                .map(|s| truncate(&s, 140))
                .unwrap_or_default(),
            started_at: row.get(5)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn search_events(conn: &Connection, like: &str) -> AppResult<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT e.id, e.source_id, src.name, e.project_key, e.event_type, e.payload, e.occurred_at
          FROM raw_events e
          JOIN sources src ON src.id = e.source_id
         WHERE e.payload LIKE ?1 ESCAPE '\' OR e.event_type LIKE ?1 ESCAPE '\' OR e.project_key LIKE ?1 ESCAPE '\'
         ORDER BY e.occurred_at DESC
         LIMIT ?2
        "#,
    )?;
    let mut out = Vec::new();
    let rows = stmt.query_map(params![like, PER_SOURCE_LIMIT], |row| {
        let event_type: String = row.get(4)?;
        let payload: String = row.get(5)?;
        // v0.4.4 (Stage E.4a): a raw_events hit is only useful if the
        // user can *see* what matched. For JSON payloads (Codex,
        // Claude, OpenCode, …) we extract the `text` / `content` /
        // `message` field and show that; for everything else (plain
        // strings, base64 blobs, "session.rollout" NUL-terminated
        // chunks from the opencode connector) we fall back to the
        // match-window snippet.
        let preview = payload_preview(&payload, 80);
        let snippet = if !preview.is_empty() && preview != payload {
            // JSON path produced a clean preview — use it directly.
            preview.clone()
        } else {
            snippet_around(&payload, like, 140)
        };
        let title = if preview.is_empty() {
            event_type.clone()
        } else if event_type == "message" {
            // Most common case: drop the noisy event_type prefix
            // and show the actual text right in the title.
            truncate(&preview, 80)
        } else {
            format!("{} · {}", event_type, truncate(&preview, 80))
        };
        Ok(SearchHit {
            kind: "event".into(),
            id: row.get(0)?,
            source_id: row.get(1)?,
            source_display_name: row.get(2)?,
            project_key: row.get::<_, Option<String>>(3)?,
            title,
            snippet,
            started_at: row.get(6)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// v0.4.4 (Stage E.4a): best-effort pull of a human-readable string
/// from a raw_events payload. Tries (in order):
/// 1. JSON parse → `text` / `content` / `message` field
/// 2. JSON array of messages → join first few `text` fields
/// 3. Newline-stripped plain text (any non-JSON blob)
///
/// Returns the empty string when the payload doesn't look like text
/// at all (base64, binary, session.rollout chunks). The caller then
/// falls back to the LIKE match-window snippet.
fn payload_preview(payload: &str, max: usize) -> String {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Try JSON object.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return json_preview(&v, max);
    }
    // Plain text — strip control chars and trim.
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        String::new()
    } else {
        truncate(&collapsed, max)
    }
}

fn json_preview(v: &serde_json::Value, max: usize) -> String {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            // Common payload shapes: `{"text":"...", "role":"user"}`
            // (Codex / Claude / OpenCode chat) or `{"content":"..."}`
            // (some markdown_summary import). Try in priority order.
            for key in ["text", "content", "message", "summary", "headline", "body"] {
                if let Some(s) = map.get(key).and_then(|x| x.as_str()) {
                    return truncate(s.trim(), max);
                }
            }
            // Nested `messages: [{text:...}, {text:...}]` (Codex
            // session.full).
            if let Some(Value::Array(arr)) = map.get("messages") {
                let mut joined = String::new();
                for item in arr.iter().take(3) {
                    if let Some(s) = item.get("text").and_then(|x| x.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str(" / ");
                        }
                        joined.push_str(s.trim());
                        if joined.chars().count() >= max {
                            break;
                        }
                    }
                }
                if !joined.is_empty() {
                    return truncate(&joined, max);
                }
            }
            String::new()
        }
        Value::String(s) => truncate(s.trim(), max),
        Value::Array(arr) => {
            let mut joined = String::new();
            for item in arr.iter().take(3) {
                if let Some(s) = item.as_str() {
                    if !joined.is_empty() {
                        joined.push_str(" / ");
                    }
                    joined.push_str(s.trim());
                }
            }
            truncate(&joined, max)
        }
        _ => String::new(),
    }
}

fn search_plans(conn: &Connection, like: &str) -> AppResult<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.id, p.title, p.description, p.ai_reasoning, p.source_session_id,
               ss.source_id, src.name, ss.project_key, p.created_at
          FROM plan_items p
          LEFT JOIN sessions_summary ss ON ss.id = p.source_session_id
          LEFT JOIN sources src ON src.id = ss.source_id
         WHERE p.title LIKE ?1 ESCAPE '\' OR p.description LIKE ?1 ESCAPE '\' OR p.ai_reasoning LIKE ?1 ESCAPE '\'
         ORDER BY p.created_at DESC
         LIMIT ?2
        "#,
    )?;
    let mut out = Vec::new();
    let rows = stmt.query_map(params![like, PER_SOURCE_LIMIT], |row| {
        let title: String = row.get(1)?;
        let description: String = row.get::<_, String>(2).unwrap_or_default();
        let ai: String = row.get::<_, String>(3).unwrap_or_default();
        let snippet_source = if !description.is_empty() {
            description
        } else if !ai.is_empty() {
            ai
        } else {
            title.clone()
        };
        Ok(SearchHit {
            kind: "plan".into(),
            id: row.get(0)?,
            source_id: row
                .get::<_, Option<String>>(5)?
                .unwrap_or_else(|| "planner".into()),
            source_display_name: row
                .get::<_, Option<String>>(6)?
                .unwrap_or_else(|| "Planner".into()),
            project_key: row.get(7)?,
            title: truncate(&title, 80),
            snippet: truncate(&snippet_source, 140),
            started_at: row.get(8)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn search_sources(conn: &Connection, like: &str) -> AppResult<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, kind, name
          FROM sources
         WHERE name LIKE ?1 ESCAPE '\' OR kind LIKE ?1 ESCAPE '\'
         ORDER BY name ASC
         LIMIT ?2
        "#,
    )?;
    let mut out = Vec::new();
    let rows = stmt.query_map(params![like, PER_SOURCE_LIMIT], |row| {
        let kind: String = row.get(1)?;
        let name: String = row.get(2)?;
        Ok(SearchHit {
            kind: "source".into(),
            id: row.get::<_, String>(0)?,
            source_id: row.get::<_, String>(0)?,
            source_display_name: name.clone(),
            project_key: None,
            title: name.clone(),
            snippet: kind,
            started_at: None,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Pull a short window of `haystack` around the first match of
/// `like` (with the LIKE metachars stripped). Falls back to a
/// head-prefix when no match is found.
fn snippet_around(haystack: &str, like: &str, max: usize) -> String {
    let needle = like.trim_start_matches('%').trim_end_matches('%');
    if needle.is_empty() {
        return truncate(haystack, max);
    }
    let lower = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    if let Some(pos) = lower.find(&lower_needle) {
        let half = max / 2;
        let start = pos.saturating_sub(half);
        let end = (start + max).min(haystack.len());
        let mut out = String::new();
        if start > 0 {
            out.push('…');
        }
        out.push_str(&haystack[start..end]);
        if end < haystack.len() {
            out.push('…');
        }
        out
    } else {
        truncate(haystack, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_like_handles_metachars() {
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
    }

    #[test]
    fn empty_and_short_queries_return_empty() {
        assert!(global_search_stub("").is_empty());
        assert!(global_search_stub("x").is_empty());
    }

    /// Avoids opening a real DB just to exercise the early-return.
    fn global_search_stub(query: &str) -> Vec<SearchHit> {
        let q = query.trim();
        if q.len() < MIN_QUERY_LEN {
            return Vec::new();
        }
        vec![SearchHit {
            kind: "session".into(),
            id: "x".into(),
            source_id: "y".into(),
            source_display_name: "z".into(),
            project_key: None,
            title: "x".into(),
            snippet: "".into(),
            started_at: None,
        }]
    }

    #[test]
    fn snippet_around_finds_match_window() {
        let hay = "a".repeat(100) + "TARGET" + &"b".repeat(100);
        let s = snippet_around(&hay, "%TARGET%", 30);
        assert!(s.contains("TARGET"));
    }

    #[test]
    fn snippet_around_no_match_returns_head() {
        let hay = "abc".repeat(50);
        let s = snippet_around(&hay, "%zzz%", 20);
        assert!(s.starts_with("abc"));
    }

    #[test]
    fn truncate_respects_char_count() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    // ---- v0.4.4 (Stage E.4a): payload preview extraction ----

    #[test]
    fn payload_preview_extracts_text_field() {
        let p = r#"{"role":"user","text":"fix the auth bug please"}"#;
        assert_eq!(payload_preview(p, 80), "fix the auth bug please");
    }

    #[test]
    fn payload_preview_extracts_content_field() {
        let p = r#"{"content":"hello world"}"#;
        assert_eq!(payload_preview(p, 80), "hello world");
    }

    #[test]
    fn payload_preview_falls_back_to_plain_text() {
        // Newline + multiple whitespace → single space, trimmed.
        let p = "hello\n  world\t!";
        assert_eq!(payload_preview(p, 80), "hello world !");
    }

    #[test]
    fn payload_preview_returns_empty_for_base64_blob() {
        // Long, no JSON, looks binary — empty means "use the LIKE
        // match-window snippet instead" upstream.
        let p = "AAAAAAAAAAAAAAAA";
        assert_eq!(payload_preview(p, 80), "AAAAAAAAAAAAAAAA");
    }

    #[test]
    fn json_preview_joins_messages_array() {
        let p = r#"{"messages":[{"text":"hi"},{"text":"there"}]}"#;
        let got = payload_preview(p, 80);
        assert!(got.contains("hi"));
        assert!(got.contains("there"));
    }

    /// Integration: open an in-memory DB, seed minimal fixtures, and
    /// run a real query through `global_search`. The DB is built by
    /// hand so we don't drag in the full `lib.rs` migration path.
    #[test]
    fn global_search_merges_sessions_events_and_plans() {
        let conn = Connection::open_in_memory().expect("memdb");
        conn.execute_batch(
            r#"
            CREATE TABLE sources (
                id   TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                name TEXT NOT NULL
            );
            CREATE TABLE sessions_summary (
                id TEXT PRIMARY KEY,
                source_id TEXT NOT NULL,
                project_key TEXT,
                summary TEXT NOT NULL,
                started_at TEXT NOT NULL
            );
            CREATE TABLE raw_events (
                id TEXT PRIMARY KEY,
                source_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                project_key TEXT,
                payload TEXT NOT NULL,
                occurred_at TEXT NOT NULL
            );
            CREATE TABLE plan_items (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NOT NULL,
                ai_reasoning TEXT,
                source_session_id TEXT,
                created_at TEXT NOT NULL
            );
            INSERT INTO sources VALUES ('src_codex', 'codex', 'Codex');
            INSERT INTO sessions_summary VALUES
                ('sess1', 'src_codex', '/repo/foo', 'fix the auth bug', '2026-10-17T10:00:00Z'),
                ('sess2', 'src_codex', '/repo/bar', 'unrelated work', '2026-10-17T11:00:00Z');
            INSERT INTO raw_events VALUES
                ('ev1', 'src_codex', 'message', '/repo/foo', 'user: AUTH broken, please help', '2026-10-17T10:30:00Z');
            INSERT INTO plan_items VALUES
                ('plan1', 'auth refactor', 'breakdown of the auth refactor plan', 'user reported auth broken', NULL, '2026-10-17T10:15:00Z');
            "#,
        )
        .expect("seed");

        let hits = global_search(&conn, "auth").expect("search");
        // Should hit session, event, and plan.
        let kinds: Vec<&str> = hits.iter().map(|h| h.kind.as_str()).collect();
        assert!(kinds.contains(&"session"), "sessions_summary hit: {kinds:?}");
        assert!(kinds.contains(&"event"), "raw_events hit: {kinds:?}");
        assert!(kinds.contains(&"plan"), "plan_items hit: {kinds:?}");
        // No more than the cap.
        assert!(hits.len() <= TOTAL_LIMIT as usize);
    }

    #[test]
    fn global_search_escapes_like_metachars() {
        let conn = Connection::open_in_memory().expect("memdb");
        conn.execute_batch(
            r#"
            CREATE TABLE sources (id TEXT PRIMARY KEY, kind TEXT, name TEXT);
            CREATE TABLE sessions_summary (id TEXT PRIMARY KEY, source_id TEXT, project_key TEXT, summary TEXT NOT NULL, started_at TEXT NOT NULL);
            CREATE TABLE raw_events (id TEXT PRIMARY KEY, source_id TEXT, event_type TEXT, project_key TEXT, payload TEXT NOT NULL, occurred_at TEXT NOT NULL);
            CREATE TABLE plan_items (id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL, ai_reasoning TEXT, source_session_id TEXT, created_at TEXT NOT NULL);
            INSERT INTO sources VALUES ('src1', 'codex', 'Codex');
            INSERT INTO sessions_summary VALUES ('s1', 'src1', NULL, 'progress at 100%', '2026-10-17T10:00:00Z');
            INSERT INTO sessions_summary VALUES ('s2', 'src1', NULL, 'progress at 99%', '2026-10-17T10:00:00Z');
            "#,
        )
        .expect("seed");
        // Without escaping, `100%` would match both rows (and a lot more).
        // With escaping, only the literal `100%` row matches.
        let hits = global_search(&conn, "100%").expect("search");
        assert_eq!(hits.len(), 1, "100% must not act as LIKE wildcard");
        assert_eq!(hits[0].id, "s1");
    }
}
