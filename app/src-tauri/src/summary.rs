//! v0.2: per-`sessions_summary` AI summarisation.
//!
//! The user explicitly clicks "Summarise" on a session card to enrich it.
//! We fetch the raw events that fall inside that partition's
//! `[started_at, ended_at]` window, build a compact JSON-shaped prompt,
//! call `ai::generate` (which already redacts cloud-bound payloads and
//! applies the active provider), and persist the result on
//! `session_summaries`.
//!
//! Cache window is per-user via `AiSettings.cache_hours` (default 24h).
//! Re-clicking before expiry returns the cached row without a roundtrip.
//!
//! Heuristic fallback: when the provider is unconfigured or the LLM
//! returns non-JSON / an error, we assemble a deterministic local
//! summary so the UI never shows an empty card.

use std::time::Instant;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ai::{self, AiSettings, GenerateReport};
use crate::error::AppResult;

/// One enrichment written to `session_summaries`. Returned to the UI.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub session_summary_id: String,
    pub headline: String,
    pub body: String,
    pub importance: i64,
    pub urgency: i64,
    pub reasoning: String,
    /// `"heuristic"` if the LLM wasn't used (no provider / parse error).
    pub provider: String,
    pub model: String,
    pub expires_at: String,
    pub created_at: String,
    /// True if served from the `session_summaries` cache; false when the
    /// row was just freshly written.
    pub cached: bool,
    pub age_hours: i64,
}

/// Top-level entry. Returns the enrichment for one session, freshly
/// written or served from cache.
pub fn summarise_session(conn: &Connection, session_id: &str) -> AppResult<SessionSummary> {
    let started = Instant::now();
    let settings = ai::load_settings();

    let sess = load_session_row(conn, session_id)?;

    // 1. Cache check.
    if let Some(cached) = read_cache(conn, session_id, &settings)? {
        tracing::info!(
            session_id,
            age_hours = cached.age_hours,
            "summary cache hit"
        );
        return Ok(cached);
    }

    // 2. Build the prompt from raw_events in the time window.
    let events = fetch_session_events(conn, session_id)?;

    let prompt = build_prompt(&sess, &events);

    // 3. Call the active provider (cloud force-redacts at the ai layer).
    let report = ai::generate(&settings, &prompt, /* json_mode */ true);

    // 4. Either parse the LLM JSON or assemble heuristic fallback.
    let (headline, body, importance, urgency, reasoning) =
        parse_json_response(&report.response).unwrap_or_else(|| {
            heuristic_response(&sess, &events, &report)
        });

    // 5. Persist. Cache lifetime is `AiSettings.cache_hours` (default 24).
    let id = format!("sum_{}", uuid::Uuid::new_v4().simple());
    let now = now_rfc3339();
    let expires = plus_hours(cache_hours(&settings));
    let prompt_hash = sha256_hex(&prompt);

    let used_llm = report.ok && parse_json_response(&report.response).is_some();
    let (provider, model) = if used_llm {
        (report.provider.clone(), report.model.clone())
    } else {
        ("heuristic".to_string(), "heuristic".to_string())
    };

    conn.execute(
        "INSERT INTO session_summaries(
            id, session_summary_id, prompt_hash, provider, model,
            headline, body, importance, urgency, reasoning,
            expires_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            session_id,
            prompt_hash,
            provider,
            model,
            headline,
            body,
            importance,
            urgency,
            reasoning,
            expires,
            now
        ],
    )?;

    tracing::info!(
        session_id,
        provider = %provider,
        importance,
        urgency,
        cached = false,
        duration_ms = started.elapsed().as_millis() as i64,
        "summary written"
    );

    Ok(SessionSummary {
        id,
        session_summary_id: session_id.to_string(),
        headline,
        body,
        importance,
        urgency,
        reasoning,
        provider,
        model,
        expires_at: expires,
        created_at: now,
        cached: false,
        age_hours: 0,
    })
}

/// Look up `sessions_summary` row by id; fail fast if missing.
pub fn load_session_row(conn: &Connection, session_id: &str) -> AppResult<SessionRow> {
    conn.query_row(
        "SELECT id, source_id, started_at, ended_at, event_count, project_key, summary, importance
         FROM sessions_summary WHERE id = ?1",
        params![session_id],
        |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                source_id: row.get(1)?,
                started_at: row.get(2)?,
                ended_at: row.get(3)?,
                event_count: row.get(4)?,
                project_key: row.get(5)?,
                heuristic_summary: row.get(6)?,
                heuristic_importance: row.get(7)?,
            })
        },
    )
    .map_err(Into::into)
}

fn read_cache(
    conn: &Connection,
    session_id: &str,
    settings: &AiSettings,
) -> AppResult<Option<SessionSummary>> {
    let now = now_rfc3339();
    let row = conn.query_row(
        "SELECT id, headline, body, importance, urgency, reasoning,
                provider, model, expires_at, created_at
         FROM session_summaries
         WHERE session_summary_id = ?1 AND expires_at > ?2
         ORDER BY created_at DESC LIMIT 1",
        params![session_id, now],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        },
    );
    let r = match row {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let created_ms = parse_ms(&r.9);
    let age_ms = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(created_ms);
    let _ = settings; // keep param for future per-user cache tweaks
    Ok(Some(SessionSummary {
        id: r.0,
        session_summary_id: session_id.to_string(),
        headline: r.1,
        body: r.2,
        importance: r.3,
        urgency: r.4,
        reasoning: r.5,
        provider: r.6,
        model: r.7,
        expires_at: r.8,
        created_at: r.9,
        cached: true,
        age_hours: age_ms / 3_600_000,
    }))
}

/// Pull the source's raw events whose timestamp falls inside
/// `[started_at, ended_at]`. We don't (yet) have a FK from raw_events
/// to sessions_summary, so we correlate by time window — accurate as
/// long as the source's events stay within the window. Capped at 200
/// events to keep the prompt bounded.
/// One raw event inside a session's `[started_at, ended_at]` window.
/// Returned by `list_session_events` for the Sessions page master-detail
/// UI — keeps the *full* payload so the right pane can show context
/// (heuristic summary call above still uses `RawEventRow` with a 200-char
/// preview to bound its prompt).
#[derive(Debug, Clone, Serialize)]
pub struct SessionEventRow {
    pub id: String,
    pub event_type: String,
    pub occurred_at: String,
    pub project_key: Option<String>,
    pub payload: String,
}

/// Shared underlying query: every raw_event row inside this session
/// window, ordered by time. The summarise path caps at 200 events and
/// truncates the payload for prompt size, while the UI path asks for
/// the full payload and no limit.
fn query_session_events_window(
    conn: &Connection,
    source_id: &str,
    started_at: &str,
    ended_at: &str,
    limit: Option<usize>,
    truncate_payload: Option<usize>,
) -> AppResult<Vec<(String, String, String, String, String)>> {
    let limit_clause = match limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    let sql = format!(
        "SELECT id, event_type, occurred_at, COALESCE(project_key, ''), payload
         FROM raw_events
         WHERE source_id = ?1 AND occurred_at >= ?2 AND occurred_at <= ?3
         ORDER BY occurred_at ASC{limit_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![source_id, started_at, ended_at], |row| {
            let payload_full: String = row.get(4)?;
            let payload = match truncate_payload {
                Some(n) => payload_full.chars().take(n).collect(),
                None => payload_full,
            };
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                payload,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn fetch_session_events(
    conn: &Connection,
    session_id: &str,
) -> AppResult<Vec<RawEventRow>> {
    let (source_id, started_at, ended_at): (String, String, String) = conn.query_row(
        "SELECT source_id, started_at, ended_at FROM sessions_summary WHERE id = ?1",
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let tuples = query_session_events_window(
        conn,
        &source_id,
        &started_at,
        &ended_at,
        Some(200),
        Some(200),
    )?;
    Ok(tuples
        .into_iter()
        .map(|(id, event_type, occurred_at, project_key, payload_preview)| RawEventRow {
            id,
            event_type,
            occurred_at,
            project_key,
            payload_preview,
        })
        .collect())
}

/// Public UI loader: returns every raw event inside the partition's
/// time window with the full payload so the master-detail view can show
/// all the activity without artificial limits.
pub fn fetch_session_events_for_ui(
    conn: &Connection,
    session_id: &str,
) -> AppResult<Vec<SessionEventRow>> {
    let (source_id, started_at, ended_at): (String, String, String) = conn.query_row(
        "SELECT source_id, started_at, ended_at FROM sessions_summary WHERE id = ?1",
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let tuples = query_session_events_window(
        conn,
        &source_id,
        &started_at,
        &ended_at,
        None,
        None,
    )?;
    Ok(tuples
        .into_iter()
        .map(
            |(id, event_type, occurred_at, project_key, payload)| SessionEventRow {
                id,
                event_type,
                occurred_at,
                project_key: Some(project_key).filter(|s| !s.is_empty()),
                payload,
            },
        )
        .collect())
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub source_id: String,
    pub started_at: String,
    pub ended_at: String,
    pub event_count: i64,
    pub project_key: Option<String>,
    pub heuristic_summary: String,
    pub heuristic_importance: i64,
}

#[derive(Debug, Clone)]
struct RawEventRow {
    #[allow(dead_code)]
    id: String,
    event_type: String,
    occurred_at: String,
    #[allow(dead_code)]
    project_key: String,
    payload_preview: String,
}

fn build_prompt(sess: &SessionRow, events: &[RawEventRow]) -> String {
    let project = sess.project_key.as_deref().unwrap_or("unknown");
    let mut ev_lines = String::new();
    for ev in events.iter().take(40) {
        let preview: String = ev.payload_preview.chars().take(120).collect();
        ev_lines.push_str(&format!(
            "  - [{}] {} ({})\n",
            ev.occurred_at, ev.event_type, preview
        ));
    }
    let events_block = if ev_lines.is_empty() {
        "  (no events captured)".to_string()
    } else {
        ev_lines
    };
    format!(
        "You summarise one agent work session for a personal productivity dashboard.\n\
         Session window: {start} → {end}\n\
         Event count: {n}\n\
         Project: {project}\n\
         Heuristic preview: {hint}\n\n\
         Events (chronological, capped at 40):\n{events}\n\n\
         Respond ONLY with a JSON object in this exact shape, no prose or fences:\n\
         {{\"headline\": \"<one short sentence>\",\
         \"body\": \"<2-3 short sentences describing what was done>\",\
         \"importance\": <0-10 integer>,\
         \"urgency\": <0-10 integer>,\
         \"reasoning\": \"<one sentence explaining the importance+urgency scores>\"}}",
        start = sess.started_at,
        end = sess.ended_at,
        n = sess.event_count,
        project = project,
        hint = sess.heuristic_summary,
        events = events_block,
    )
}

fn parse_json_response(raw: &str) -> Option<(String, String, i64, i64, String)> {
    let s = raw.trim();
    // Tolerate ```json ... ``` fences.
    let json_str = if s.starts_with("```") {
        let inner = s
            .trim_start_matches("```")
            .trim_start_matches("json")
            .trim();
        inner.trim_end_matches("```").trim()
    } else {
        s
    };
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = v.as_object()?;
    let headline = obj.get("headline")?.as_str()?.to_string();
    let body = obj.get("body")?.as_str()?.to_string();
    let importance = obj.get("importance")?.as_i64()?;
    let urgency = obj.get("urgency")?.as_i64()?;
    let reasoning = obj.get("reasoning")?.as_str()?.to_string();
    Some((
        headline,
        body,
        importance.clamp(0, 10),
        urgency.clamp(0, 10),
        reasoning,
    ))
}

fn heuristic_response(
    sess: &SessionRow,
    events: &[RawEventRow],
    report: &GenerateReport,
) -> (String, String, i64, i64, String) {
    let n = events.len();
    let last = events
        .last()
        .map(|e| e.event_type.as_str())
        .unwrap_or("unknown");
    let headline = sess.heuristic_summary.clone();
    let body = format!(
        "{} raw events captured between {} and {}. Last event type: {}.",
        sess.event_count, sess.started_at, sess.ended_at, last
    );
    let importance = sess.heuristic_importance.clamp(0, 10);
    let urgency = match importance {
        i if i >= 7 => 6,
        i if i >= 5 => 4,
        i if i >= 3 => 2,
        _ => 1,
    };
    let reasoning = match report.error.as_deref() {
        Some(e) => format!(
            "Heuristic fallback (LLM error: {e}). Used ETL event count + heuristic importance."
        ),
        None => "Heuristic fallback (no provider configured). Used ETL event count + heuristic importance.".into(),
    };
    (headline, body, importance, urgency, reasoning)
}

fn cache_hours(settings: &AiSettings) -> i64 {
    settings.cache_hours.max(1)
}

fn now_rfc3339() -> String {
    crate::connectors::rfc3339(Some(crate::connectors::now_ms()))
}

fn plus_hours(hours: i64) -> String {
    chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(hours))
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(now_rfc3339)
}

fn parse_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let out = h.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_response_accepts_bare_object() {
        let raw = r#"{"headline":"hi","body":"worked","importance":5,"urgency":3,"reasoning":"ok"}"#;
        let parsed = parse_json_response(raw).unwrap();
        assert_eq!(parsed.0, "hi");
        assert_eq!(parsed.2, 5);
        assert_eq!(parsed.3, 3);
    }

    #[test]
    fn parse_json_response_strips_code_fence() {
        let raw = "```json\n{\"headline\":\"x\",\"body\":\"y\",\"importance\":1,\"urgency\":2,\"reasoning\":\"z\"}\n```";
        let parsed = parse_json_response(raw).unwrap();
        assert_eq!(parsed.0, "x");
    }

    #[test]
    fn parse_json_response_rejects_non_object() {
        assert!(parse_json_response("\"plain string\"").is_none());
        assert!(parse_json_response("[1,2,3]").is_none());
    }

    #[test]
    fn parse_json_response_clamps_importance_to_0_10() {
        let raw = r#"{"headline":"","body":"","importance":42,"urgency":-3,"reasoning":""}"#;
        let parsed = parse_json_response(raw).unwrap();
        assert_eq!(parsed.2, 10);
        assert_eq!(parsed.3, 0);
    }

    #[test]
    fn cache_hours_uses_setting_floor_at_one() {
        let mut s = AiSettings::default();
        s.cache_hours = 0;
        assert_eq!(cache_hours(&s), 1);
        s.cache_hours = 24;
        assert_eq!(cache_hours(&s), 24);
    }

    #[test]
    fn prompt_mentions_window_and_events() {
        let s = SessionRow {
            id: "s1".into(),
            source_id: "src1".into(),
            started_at: "2026-10-16T10:00:00Z".into(),
            ended_at: "2026-10-16T10:30:00Z".into(),
            event_count: 2,
            project_key: Some("/Users/me/proj".into()),
            heuristic_summary: "2 raw events from /Users/me/proj".into(),
            heuristic_importance: 5,
        };
        let evs = vec![
            RawEventRow {
                id: "e1".into(),
                event_type: "user_input".into(),
                occurred_at: "2026-10-16T10:00:00Z".into(),
                project_key: "/Users/me/proj".into(),
                payload_preview: "hello".into(),
            },
            RawEventRow {
                id: "e2".into(),
                event_type: "assistant_message".into(),
                occurred_at: "2026-10-16T10:05:00Z".into(),
                project_key: "/Users/me/proj".into(),
                payload_preview: "world".into(),
            },
        ];
        let prompt = build_prompt(&s, &evs);
        assert!(prompt.contains("2026-10-16T10:00:00Z"));
        assert!(prompt.contains("user_input"));
        assert!(prompt.contains("assistant_message"));
        assert!(prompt.contains("/Users/me/proj"));
    }

    #[test]
    fn heuristic_response_uses_etl_importance_to_set_urgency() {
        let s = SessionRow {
            id: "s".into(),
            source_id: "x".into(),
            started_at: "2026-01-01T10:00:00Z".into(),
            ended_at: "2026-01-01T10:10:00Z".into(),
            event_count: 5,
            project_key: None,
            heuristic_summary: "5 raw events (heuristic)".into(),
            heuristic_importance: 8,
        };
        let rep = GenerateReport {
            ok: false,
            response: String::new(),
            provider: "local".into(),
            model: "gemma4:e4b".into(),
            duration_ms: 0,
            redacted_chars: 0,
            error: Some("no key configured".into()),
        };
        let (_, _, imp, urg, reason) =
            heuristic_response(&s, &[], &rep);
        assert_eq!(imp, 8);
        assert!(urg >= 5, "high ETL importance → high urgency; got {urg}");
        assert!(reason.contains("no key"));
    }

    /// Regression test for the master-detail UI loader: prove it
    /// returns every raw_event inside the partition window with the
    /// full payload (no 200-char preview truncation the summarise path
    /// applies) and respects ascending time order.
    #[test]
    fn fetch_session_events_for_ui_returns_full_payload_no_limit() {
        use crate::connectors::now_ms;
        use crate::connectors::rfc3339;

        // Build an isolated in-memory DB so the test doesn't fight
        // with parallel test threads sharing the live DB mutex.
        let conn = Connection::open_in_memory().expect("open in-memory db");
        crate::migrate_for_tests(&conn).expect("schema migrate");

        let now_iso = rfc3339(Some(now_ms()));

        // Minimal source + partition. The fetch path joins by source
        // id + window, so we replicate the same shape.
        conn.execute(
            "INSERT INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
             VALUES('src', 'test', 'src', '/tmp/test', 'ok', ?1, ?1)",
            params![now_iso],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions_summary(
                id, source_id, started_at, ended_at, event_count,
                source_event_count, project_key, summary, importance,
                heuristic_kind, created_at, updated_at
             ) VALUES('part', 'src', '2026-10-16T10:00:00Z', '2026-10-16T10:30:00Z',
                     3, 3, '/Users/me/p', '3 raw events (heuristic)', 5,
                     'auto', ?1, ?1)",
            params![now_iso],
        )
        .unwrap();

        // 3 events with payloads that vary in length so any premature
        // truncation would stand out.
        let long_payload: String = "x".repeat(2_000);
        for (i, ts) in [
            "2026-10-16T10:01:00Z",
            "2026-10-16T10:10:00Z",
            "2026-10-16T10:25:00Z",
        ]
        .iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO raw_events(
                    id, source_id, event_type, occurred_at, project_key,
                    payload, redacted, created_at
                 ) VALUES(?1, 'src', 'user_input', ?2, '/Users/me/p',
                          ?3, 0, ?4)",
                params![
                    format!("e{i}"),
                    ts,
                    long_payload.clone(),
                    now_iso
                ],
            )
            .unwrap();
        }

        let rows = fetch_session_events_for_ui(&conn, "part").expect("fetch");
        assert_eq!(rows.len(), 3, "should include every event in window");

        // Order must be ascending by occurred_at.
        assert_eq!(rows[0].id, "e0");
        assert_eq!(rows[1].id, "e1");
        assert_eq!(rows[2].id, "e2");

        // Payload must round-trip intact — UI needs full content.
        for r in &rows {
            assert_eq!(
                r.payload.len(),
                2_000,
                "payload must not be truncated by UI loader"
            );
        }
        // project_key is exposed as Option<String> with empty dropped.
        assert_eq!(rows[0].project_key.as_deref(), Some("/Users/me/p"));
    }
}
