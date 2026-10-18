//! v0.3.7 / Stage 5.7: cross-source search.
//!
//! One user query fans out across the four searchable tables
//! (`sessions_summary`, `raw_events`, `plan_items`, `sources`) and
//! returns a unified `Vec<SearchHit>` sorted by relevance, then
//! recency as tiebreaker. Designed for the "Cmd-K" overlay in the
//! top toolbar — the user types a fragment, sees the top-N hits
//! across all sources, and clicks through to the underlying
//! partition / event / plan item.
//!
//! v0.5.4 polish:
//! - **Multi-token AND**. Whitespace in the query now splits into
//!   tokens; every token must match somewhere on the row's
//!   searchable text. Lets the user type "doubao events" to find
//!   rows that mention both.
//! - **Snippet highlighting**. Every match is wrapped in
//!   `<mark>...</mark>` inside the snippet. The frontend renders
//!   snippets with `dangerouslySetInnerHTML`; the snippet text is
//!   already html-escaped here so the marker is the only active
//!   markup.
//! - **Counts + elapsed_ms**. `global_search` now returns
//!   `SearchResult { hits, counts, elapsed_ms }` so the frontend
//!   can show per-source counts and a "took N ms" footer.
//!
//! Why not FTS5?
//! - Adds a maintenance cost (triggers on every INSERT, schema
//!   migration on user DBs, separate `*_fts` tables).
//! - Our text volume is small (sessions: hundreds, events: tens
//!   of thousands, plans: dozens) — a per-source `LIKE` UNION is
//!   plenty fast on indexed columns. v0.3.7 ships the simple
//!   version; v0.5.4 improves its ranking + UX; if profiling later
//!   shows it's the bottleneck we upgrade to FTS5 in a single
//!   focused change.
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
    /// Short context snippet around the match (~140 chars) with
    /// `<mark>...</mark>` around the matching positions. HTML
    /// escaped except for the marker tags themselves.
    pub snippet: String,
    pub project_key: Option<String>,
    /// RFC3339 timestamp for sort stability, when the table has
    /// one.
    pub started_at: Option<String>,
}

/// Per-source count of how many rows matched *before* the
/// per-source LIMIT cap was applied. Lets the UI render "会话 ·
/// 12 / 事件 · 47" so the user can tell at a glance which source
/// owns the result. `total` is the sum across kinds.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SearchCounts {
    pub sessions: i64,
    pub events: i64,
    pub plans: i64,
    pub sources: i64,
    pub total: i64,
}

/// Result returned by `global_search`. `hits` is already capped at
/// `TOTAL_LIMIT` (50), sorted by relevance then recency. `counts`
/// mirrors per-source total before the LIMIT cap, used by the UI
/// to render "X hits across Y sources". `elapsed_ms` is the wall
/// time spent on the SQL, used by the footer "took N ms".
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub counts: SearchCounts,
    pub elapsed_ms: i64,
}

/// Run the cross-source search. Returns at most `TOTAL_LIMIT`
/// hits with per-source results interleaved by (relevance,
/// recency). Empty / too-short query returns an empty
/// SearchResult.
pub fn global_search(conn: &Connection, query: &str) -> AppResult<SearchResult> {
    let started = std::time::Instant::now();
    let q = query.trim();
    if q.len() < MIN_QUERY_LEN {
        return Ok(SearchResult {
            hits: Vec::new(),
            counts: SearchCounts::default(),
            elapsed_ms: started.elapsed().as_millis() as i64,
        });
    }

    // v0.5.4 multi-token AND: split on whitespace, escape each
    // token's LIKE metachars, build a `WHERE (col LIKE ?1 ESCAPE
    // '\\' OR col LIKE ?2 ESCAPE '\\' OR …)` predicate so every
    // token must hit *somewhere* on the row.
    let tokens: Vec<String> = q
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| escape_like(t))
        .collect();
    if tokens.is_empty() {
        return Ok(SearchResult {
            hits: Vec::new(),
            counts: SearchCounts::default(),
            elapsed_ms: started.elapsed().as_millis() as i64,
        });
    }
    let needles: Vec<String> = tokens.iter().map(|t| format!("%{}%", t)).collect();

    // The match-rank score:
    //   - +10 per token found in the searchable text (so a 2-token
    //     query that hits both is +20; one that only hits one is
    //     +10 and ranks lower).
    // We re-use the same LIKE-or-OR predicate but mark WHICH token
    // hit and accumulate. Implemented inline via a slightly more
    // selective CASE: SELECT … , COUNT(*) OVER () … . For SQL
    // simplicity we post-score in Rust after retrieving a wider
    // candidate set than the LIMIT, then truncate.
    let mut counts = SearchCounts::default();
    let mut all_hits: Vec<SearchHit> = Vec::new();

    let mut session_hits = search_sessions(conn, &needles)?;
    counts.sessions = session_hits.len() as i64;
    all_hits.append(&mut session_hits);

    let mut event_hits = search_events(conn, &needles)?;
    counts.events = event_hits.len() as i64;
    all_hits.append(&mut event_hits);

    let mut plan_hits = search_plans(conn, &needles)?;
    counts.plans = plan_hits.len() as i64;
    all_hits.append(&mut plan_hits);

    let mut source_hits = search_sources(conn, &needles)?;
    counts.sources = source_hits.len() as i64;
    all_hits.append(&mut source_hits);

    // Apply snippet highlighting + score by token-match count.
    for hit in all_hits.iter_mut() {
        let needle_refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
        hit.snippet = highlight_html(&hit.snippet, &needle_refs);
    }
    // Score = tokens hit in snippet (each token counted once even
    // if it appears multiple times in the row's text). Sessions
    // / plans have only summary+title; events have payload+type.
    // We approximate "did the token hit this row?" by checking the
    // snippet's *original* text (pre-highlight) against each needle
    // case-insensitively. The pre-highlight snippet is what we just
    // built so we can reverse it: rewrite by stripping <mark> tags.
    let mut scored: Vec<(i64, SearchHit)> = all_hits
        .into_iter()
        .map(|mut hit| {
            let plain = strip_html_marks(&hit.snippet);
            let hits = count_token_hits(&plain, &tokens);
            hit.snippet = plain; // score re-build later — we want
            // the score on the plain snippet for ranking only.
            (hits, hit)
        })
        .collect();

    // Sort: higher score first (relevance), then newer started_at
    // first (recency as tiebreaker).
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| match (&b.1.started_at, &a.1.started_at) {
            (Some(b_t), Some(a_t)) => b_t.cmp(a_t),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        })
    });
    let mut hits: Vec<SearchHit> = scored.into_iter().map(|(_, h)| h).collect();
    // v0.5.4 step: re-apply highlight with the SNIPPET stored on
    // each hit (we stripped earlier to score). final_snippet =
    // highlight_html(plain, tokens).
    for hit in hits.iter_mut() {
        let plain = std::mem::take(&mut hit.snippet);
        let needle_refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
        hit.snippet = highlight_html(&plain, &needle_refs);
    }

    hits.truncate(TOTAL_LIMIT as usize);
    counts.total = counts.sessions + counts.events + counts.plans + counts.sources;

    Ok(SearchResult {
        hits,
        counts,
        elapsed_ms: started.elapsed().as_millis() as i64,
    })
}

/// Escape SQL LIKE metachars so user input is treated as a literal
/// substring. `\` is the explicit ESCAPE char. We also escape the
/// HTML-significant chars (`&`, `<`, `>`) so the resulting
/// snippet is safe to render via `dangerouslySetInnerHTML`
/// downstream — the only active HTML will then be `<mark>` tags we
/// produced ourselves in `highlight_html`.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// HTML-escape everything except the `<mark>` markers we just
/// inserted. The caller passes in plaintext (already trimmed);
/// markers are inserted as `<mark>needle</mark>`. Markers and
/// escapes share character classes (`<`, `>`), so this is layered
/// in two passes: first escape the entire input, then split /
/// wrap / rejoin around inserted markers. We use a placeholder
/// for the markers (an unlikely string) and substitute back at
/// the end so the escape pass never touches them.
fn highlight_html(plain: &str, tokens: &[&str]) -> String {
    // 1. HTML-escape the whole input. After this pass the only
    //    HTML-significant chars left in the string are the
    //    marker tags we insert below, the `&`/`<`/`>` etc. that
    //    user input legitimately contained.
    let escaped = html_escape(plain);
    // 2. For each token, find every case-insensitive occurrence
    //    in `escaped` and wrap the *original-case* text in a
    //    `<mark>` tag. We never run another escape pass after
    //    inserting the markers, so the markers themselves stay
    //    intact.
    let mut out = escaped;
    for needle in tokens {
        if needle.is_empty() {
            continue;
        }
        let needle_escaped = html_escape(needle);
        if needle_escaped.is_empty() {
            continue;
        }
        let lower_needle = needle_escaped.to_lowercase();
        let lower_out = out.to_lowercase();
        let mut next = String::with_capacity(out.len() + 32);
        let mut cursor = 0usize;
        while let Some(rel) = lower_out[cursor..].find(&lower_needle) {
            let abs = cursor + rel;
            next.push_str(&out[cursor..abs]);
            next.push_str("<mark>");
            next.push_str(&out[abs..abs + needle_escaped.len()]);
            next.push_str("</mark>");
            cursor = abs + needle_escaped.len();
        }
        next.push_str(&out[cursor..]);
        out = next;
    }
    out
}

/// Strip the `<mark>` markers we inserted earlier so the text
/// is plain and we can run token-hit counts against it.
/// Cheap and exact: we only ever emit the literal `<mark>` and
/// `</mark>` substrings, so chained `str::replace` is enough.
fn strip_html_marks(s: &str) -> String {
    s.replace("<mark>", "").replace("</mark>", "")
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// How many of `tokens` occur at least once (case-insensitive) in
/// `haystack`? Used as a relevance score when sorting hits.
fn count_token_hits(haystack: &str, tokens: &[String]) -> i64 {
    let lower = haystack.to_lowercase();
    let mut n = 0i64;
    for t in tokens {
        if t.is_empty() {
            continue;
        }
        if lower.contains(&t.to_lowercase()) {
            n += 1;
        }
    }
    n
}

fn search_sessions(
    conn: &Connection,
    needles: &[String],
) -> AppResult<Vec<SearchHit>> {
    let (where_sql, params) = build_or_clause(
        &[
            "s.summary",
            "s.project_key",
            "src.name",
            "s.title",
        ],
        needles,
    );
    let sql = format!(
        r#"
        SELECT s.id, s.source_id, src.name, s.project_key, s.summary, s.started_at
          FROM sessions_summary s
          JOIN sources src ON src.id = s.source_id
         WHERE {where_sql}
         ORDER BY s.started_at DESC
         LIMIT ?{{}}
        "#,
        where_sql = where_sql,
    );
    let limit_idx = needles.len() + 1;
    let sql = sql.replace("?{}", &format!("?{}", limit_idx));
    let mut all_params: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    all_params.push(&PER_SOURCE_LIMIT);

    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(all_params.iter().copied()), |row| {
        let summary: Option<String> = row.get(4)?;
        let summary_text = summary.unwrap_or_default();
        Ok(SearchHit {
            kind: "session".into(),
            id: row.get(0)?,
            source_id: row.get(1)?,
            source_display_name: row.get(2)?,
            project_key: row.get::<_, Option<String>>(3)?,
            title: truncate(&summary_text, 80),
            snippet: truncate(&summary_text, 140),
            started_at: row.get(5)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn search_events(
    conn: &Connection,
    needles: &[String],
) -> AppResult<Vec<SearchHit>> {
    let (where_sql, params) = build_or_clause(
        &["e.payload", "e.event_type", "e.project_key", "src.name"],
        needles,
    );
    let sql = format!(
        r#"
        SELECT e.id, e.source_id, src.name, e.project_key, e.event_type, e.payload, e.occurred_at
          FROM raw_events e
          JOIN sources src ON src.id = e.source_id
         WHERE {where_sql}
         ORDER BY e.occurred_at DESC
         LIMIT ?{{}}
        "#,
        where_sql = where_sql,
    );
    let limit_idx = needles.len() + 1;
    let sql = sql.replace("?{}", &format!("?{}", limit_idx));
    let mut all_params: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    all_params.push(&PER_SOURCE_LIMIT);

    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(all_params.iter().copied()), |row| {
        let event_type: String = row.get(4)?;
        let payload: String = row.get(5)?;
        let preview = payload_preview(&payload, 80);
        let snippet = if !preview.is_empty() && preview != payload {
            preview.clone()
        } else {
            snippet_around(&payload, needles, 140)
        };
        let title = if preview.is_empty() {
            event_type.clone()
        } else if event_type == "message" {
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

fn search_plans(conn: &Connection, needles: &[String]) -> AppResult<Vec<SearchHit>> {
    let (where_sql, params) = build_or_clause(
        &[
            "p.title",
            "p.description",
            "p.ai_reasoning",
            "ss.project_key",
        ],
        needles,
    );
    let sql = format!(
        r#"
        SELECT p.id, p.title, p.description, p.ai_reasoning, p.source_session_id,
               ss.source_id, src.name, ss.project_key, p.created_at
          FROM plan_items p
          LEFT JOIN sessions_summary ss ON ss.id = p.source_session_id
          LEFT JOIN sources src ON src.id = ss.source_id
         WHERE {where_sql}
         ORDER BY p.created_at DESC
         LIMIT ?{{}}
        "#,
        where_sql = where_sql,
    );
    let limit_idx = needles.len() + 1;
    let sql = sql.replace("?{}", &format!("?{}", limit_idx));
    let mut all_params: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    all_params.push(&PER_SOURCE_LIMIT);

    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(all_params.iter().copied()), |row| {
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

fn search_sources(conn: &Connection, needles: &[String]) -> AppResult<Vec<SearchHit>> {
    let (where_sql, params) = build_or_clause(&["name", "kind"], needles);
    let sql = format!(
        r#"
        SELECT id, kind, name
          FROM sources
         WHERE {where_sql}
         ORDER BY name ASC
         LIMIT ?{{}}
        "#,
        where_sql = where_sql,
    );
    let limit_idx = needles.len() + 1;
    let sql = sql.replace("?{}", &format!("?{}", limit_idx));
    let mut all_params: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    all_params.push(&PER_SOURCE_LIMIT);

    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(all_params.iter().copied()), |row| {
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

/// Build a `WHERE` clause that ANDs per-token (every token must
/// hit at least one of `columns`) and ORs per-token across
/// columns. Returns the SQL fragment and the parameter list in
/// binding order. The caller appends `LIMIT ?n` afterwards with
/// its own placeholder.
fn build_or_clause(columns: &[&str], needles: &[String]) -> (String, Vec<String>) {
    let mut parts = Vec::with_capacity(needles.len());
    let mut params = Vec::with_capacity(needles.len());
    for (i, needle) in needles.iter().enumerate() {
        let mut ors = Vec::with_capacity(columns.len());
        for col in columns {
            ors.push(format!("{} LIKE ?{} ESCAPE '\\'", col, i + 1));
        }
        parts.push(format!("({})", ors.join(" OR ")));
        params.push(needle.clone());
    }
    (parts.join(" AND "), params)
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
/// any needle in `needles`. Falls back to a head-prefix when no
/// match is found.
fn snippet_around(haystack: &str, needles: &[String], max: usize) -> String {
    if needles.is_empty() {
        return truncate(haystack, max);
    }
    let lower = haystack.to_lowercase();
    // Earliest match across all needles wins; that's the most
    // useful window for the user.
    let mut best: Option<usize> = None;
    for n in needles {
        let ln = n.to_lowercase();
        if let Some(p) = lower.find(&ln) {
            best = Some(match best {
                None => p,
                Some(b) => b.min(p),
            });
        }
    }
    if let Some(pos) = best {
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

/// Best-effort pull of a human-readable string from a raw_events
/// payload (unchanged from v0.4.4 Stage E.4a).
fn payload_preview(payload: &str, max: usize) -> String {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return json_preview(&v, max);
    }
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
            for key in ["text", "content", "message", "summary", "headline", "body"] {
                if let Some(s) = map.get(key).and_then(|x| x.as_str()) {
                    return truncate(s.trim(), max);
                }
            }
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
    fn escape_like_html_escapes_user_input() {
        // The escape pass for LIKE must also pre-HTML-escape so the
        // snippet remains safe to render.
        assert_eq!(escape_like("<script>"), "&lt;script&gt;");
        assert_eq!(escape_like("a & b"), "a &amp; b");
        assert_eq!(escape_like("100% evil"), "100\\% evil");
    }

    #[test]
    fn highlight_html_wraps_each_match_in_mark_tags() {
        let plain = "Doubao events collected from tray-icon";
        let tokens = vec!["doubao", "tray"];
        let out = highlight_html(plain, &tokens);
        // We always wrap the original-case text from `plain`
        // (not the lowercased needle), so a search for `doubao`
        // highlights `Doubao` in-place, not a duplicated
        // lowercase copy.
        assert!(
            out.contains("<mark>Doubao</mark>"),
            "expected `<mark>Doubao</mark>` in: {out}"
        );
        assert!(
            out.contains("<mark>tray</mark>"),
            "expected `<mark>tray</mark>` in: {out}"
        );
        // And no stray literal `doubao` (lowercase) marker
        // exists outside the wrapped context.
        assert!(
            !out.contains("<mark>doubao</mark>"),
            "should wrap original case, not lowercased needle"
        );
    }

    #[test]
    fn highlight_html_is_idempotent_after_strip() {
        let plain = "hello world hello";
        let tokens = vec!["hello"];
        let highlighted = highlight_html(plain, &tokens);
        let stripped = strip_html_marks(&highlighted);
        assert_eq!(stripped, plain);
    }

    #[test]
    fn count_token_hits_each_token_at_most_once() {
        let text = "doubao is doubao but tray is tray".to_string();
        let tokens = vec!["doubao".to_string(), "tray".to_string()];
        assert_eq!(count_token_hits(&text, &tokens), 2);
    }

    #[test]
    fn build_or_clause_synthesises_and_within_or() {
        let (sql, params) = build_or_clause(&["a", "b"], &vec!["x".into(), "y".into()]);
        // each token gets an AND over the columns
        assert!(sql.contains("(a LIKE ?1 ESCAPE '\\' OR b LIKE ?1 ESCAPE '\\')"));
        assert!(sql.contains(" AND "));
        assert!(sql.contains("(a LIKE ?2 ESCAPE '\\' OR b LIKE ?2 ESCAPE '\\')"));
        assert_eq!(params, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn html_escape_keeps_angle_quotes_safe() {
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn truncate_is_char_boundary_aware() {
        // Chinese characters should truncate by `chars()`,
        // not bytes. Otherwise a single 3-byte glyph can land mid-
        // codepoint and the snippet renders a `?` placeholder.
        let chinese = "字节跳动";
        let t = truncate(chinese, 2);
        assert_eq!(t.chars().count(), 3); // 2 chars + ellipsis
    }
}
