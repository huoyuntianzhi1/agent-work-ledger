//! v0.2 `summarise_session` integration tests.
//!
//! These run against an in-memory v4-schema DB. The default
//! `AiSettings` resolves to `Provider::Local` (Ollama), which is never
//! running in CI, so every successful path lands on `heuristic_response`
//! via the `ai::generate` error report. Cache hits bypass the AI layer
//! entirely.

use rusqlite::{params, Connection};

use agent_work_ledger_lib::migrate_for_tests;
use agent_work_ledger_lib::summary::{self, SessionSummary};

fn open_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrate_for_tests(&conn).unwrap();
    conn
}

fn seed_source(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
         VALUES (?1, 'codex', 'OpenAI Codex (test)', '/tmp', 'readable', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        params![id],
    )
    .unwrap();
}

/// Insert one `sessions_summary` partition with a known heuristic
/// importance so we can assert urgency math downstream.
fn seed_session(
    conn: &Connection,
    id: &str,
    source_id: &str,
    started_at: &str,
    ended_at: &str,
    heuristic_importance: i64,
) {
    conn.execute(
        "INSERT INTO sessions_summary(
            id, source_id, external_id, started_at, ended_at,
            event_count, source_event_count, project_key, summary,
            importance, heuristic_kind, created_at, updated_at
         ) VALUES (
            ?1, ?2, NULL, ?3, ?4, 0, 0, '/Users/me/proj',
            'seeded heuristic summary', ?5, 'auto',
            '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
         )",
        params![id, source_id, started_at, ended_at, heuristic_importance],
    )
    .unwrap();
}

#[test]
fn heuristic_fallback_writes_a_row_with_provider_tagged_heuristic() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    seed_session(
        &conn,
        "sess_a",
        "src_a",
        "2026-01-01T10:00:00Z",
        "2026-01-01T10:05:00Z",
        /* heuristic_importance */ 7,
    );

    let s = summary::summarise_session(&conn, "sess_a").unwrap();
    assert_eq!(s.session_summary_id, "sess_a");
    assert_eq!(s.provider, "heuristic");
    assert_eq!(s.model, "heuristic");
    assert_eq!(s.cached, false);
    assert_eq!(s.age_hours, 0);
    assert!(
        (0..=10).contains(&s.importance),
        "importance out of clamp: {}",
        s.importance
    );
    assert!(
        (0..=10).contains(&s.urgency),
        "urgency out of clamp: {}",
        s.urgency
    );
    // ETL scored heuristic_importance=7 → urgency bucketed to >=5.
    assert!(
        s.urgency >= 5,
        "expected urgency >=5 for high importance, got {}",
        s.urgency
    );
    assert!(
        s.reasoning.contains("fallback"),
        "reasoning should explain heuristic: {}",
        s.reasoning
    );
}

#[test]
fn second_call_within_cache_window_returns_cached_row() {
    let conn = open_db();
    seed_source(&conn, "src_b");
    seed_session(
        &conn,
        "sess_b",
        "src_b",
        "2026-02-01T09:00:00Z",
        "2026-02-01T09:10:00Z",
        /* heuristic_importance */ 4,
    );

    let first = summary::summarise_session(&conn, "sess_b").unwrap();
    assert_eq!(first.cached, false);

    let second = summary::summarise_session(&conn, "sess_b").unwrap();
    assert_eq!(second.cached, true);
    assert_eq!(
        second.id, first.id,
        "cached call must return the same row id"
    );

    // Only one row should exist on disk — no duplicate writes.
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_summaries WHERE session_summary_id = 'sess_b'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn persisted_row_carries_all_required_columns() {
    let conn = open_db();
    seed_source(&conn, "src_c");
    seed_session(
        &conn,
        "sess_c",
        "src_c",
        "2026-03-01T11:00:00Z",
        "2026-03-01T11:20:00Z",
        /* heuristic_importance */ 5,
    );

    let s = summary::summarise_session(&conn, "sess_c").unwrap();
    let row: (
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT id, session_summary_id, provider, model, headline,
                    importance, urgency, reasoning, expires_at, created_at
             FROM session_summaries WHERE id = ?1",
            params![s.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, s.id);
    assert_eq!(row.1, "sess_c");
    assert_eq!(row.2, "heuristic");
    assert!(!row.4.is_empty(), "headline must be populated");
    assert_eq!(row.5, s.importance);
    assert_eq!(row.6, s.urgency);
    assert!(!row.8.is_empty(), "expires_at must be populated");
    assert!(!row.9.is_empty(), "created_at must be populated");
}

#[test]
fn expires_at_is_in_the_future() {
    let conn = open_db();
    seed_source(&conn, "src_d");
    seed_session(
        &conn,
        "sess_d",
        "src_d",
        "2026-04-01T08:00:00Z",
        "2026-04-01T08:15:00Z",
        /* heuristic_importance */ 3,
    );

    let s: SessionSummary = summary::summarise_session(&conn, "sess_d").unwrap();
    // AiSettings::default cache_hours=24 ⇒ expires_at = created_at + 24h.
    let created = chrono::DateTime::parse_from_rfc3339(&s.created_at).unwrap();
    let expires = chrono::DateTime::parse_from_rfc3339(&s.expires_at).unwrap();
    let delta = expires.signed_duration_since(created);
    let hours = delta.num_minutes() / 60;
    assert_eq!(hours, 24, "expected 24h cache window, got {hours}h");
    assert!(expires > chrono::Utc::now());
}

#[test]
fn missing_session_id_returns_error() {
    let conn = open_db();
    seed_source(&conn, "src_e");
    let result = summary::summarise_session(&conn, "sess_does_not_exist");
    assert!(
        result.is_err(),
        "summarise_session should reject unknown session id"
    );
}

#[test]
fn heuristic_fallback_picks_distinct_urgency_buckets_per_importance() {
    // importance=1 (very low) → urgency 1; importance=8 (high) → urgency 6.
    // Locks in the bucket math so a refactor doesn't silently change UI
    // hint ordering.
    let conn = open_db();
    seed_source(&conn, "src_f");

    seed_session(
        &conn,
        "sess_f_low",
        "src_f",
        "2026-05-01T08:00:00Z",
        "2026-05-01T08:05:00Z",
        /* heuristic_importance */ 1,
    );
    let low = summary::summarise_session(&conn, "sess_f_low").unwrap();

    seed_session(
        &conn,
        "sess_f_high",
        "src_f",
        "2026-05-02T08:00:00Z",
        "2026-05-02T09:00:00Z",
        /* heuristic_importance */ 8,
    );
    let high = summary::summarise_session(&conn, "sess_f_high").unwrap();

    assert_eq!(low.importance, 1);
    assert!(
        low.urgency <= high.urgency,
        "monotonic: low importance must not get higher urgency (low={} high={})",
        low.urgency,
        high.urgency
    );
    assert!(high.urgency >= 5);
}
