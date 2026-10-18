//! v0.2 `window_reports` integration tests — daily/weekly
//! aggregations persisted on top of the `sessions_summary` ETL
//! partitions. Default `AiSettings` (local Ollama, no key) means
//! every successful path lands on `heuristic_response` via the
//! `ai::generate` error report, same as `summary_integration.rs`.

use rusqlite::{params, Connection};

use agent_work_ledger_lib::migrate_for_tests;
use agent_work_ledger_lib::reports;

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

fn seed_partition(
    conn: &Connection,
    id: &str,
    source_id: &str,
    started_at: &str,
    ended_at: &str,
    project_key: Option<&str>,
    heuristic_summary: &str,
    heuristic_importance: i64,
    event_count: i64,
) {
    conn.execute(
        "INSERT INTO sessions_summary(
            id, source_id, external_id, started_at, ended_at,
            event_count, source_event_count, project_key, summary,
            importance, heuristic_kind, created_at, updated_at
         ) VALUES (
            ?1, ?2, NULL, ?3, ?4, ?8, 0, ?5, ?6, ?7, 'auto',
            '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
         )",
        params![
            id,
            source_id,
            started_at,
            ended_at,
            project_key,
            heuristic_summary,
            heuristic_importance,
            event_count
        ],
    )
    .unwrap();
}

fn today_window_utc() -> (String, String) {
    (
        "2026-10-16T00:00:00Z".to_string(),
        "2026-10-17T00:00:00Z".to_string(),
    )
}

#[test]
fn heuristic_window_report_writes_provider_tagged_heuristic() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    seed_partition(
        &conn,
        "sess_x",
        "src_a",
        "2026-10-16T09:00:00Z",
        "2026-10-16T09:30:00Z",
        Some("/Users/me/proj"),
        "5 events from /Users/me/proj",
        /* heuristic_importance */ 7,
        /* event_count */ 5,
    );
    let (start, end) = today_window_utc();

    let r = reports::generate_window_report(&conn, "day", &start, &end).unwrap();
    assert_eq!(r.window_label, "day");
    assert_eq!(r.provider, "heuristic");
    assert_eq!(r.model, "heuristic");
    assert_eq!(r.cached, false);
    assert_eq!(r.age_hours, 0);
    assert!(r.headline.contains("1 sessions"));
    assert!(!r.bullets.is_empty(), "heuristic must include bullets");
    assert_eq!(r.project_breakdown.len(), 1);
    assert_eq!(r.project_breakdown[0].project_key, "/Users/me/proj");
    assert_eq!(r.project_breakdown[0].sessions, 1);
    assert_eq!(r.project_breakdown[0].events, 5);
}

#[test]
fn second_call_within_cache_window_returns_cached_row() {
    let conn = open_db();
    seed_source(&conn, "src_b");
    let (start, end) = today_window_utc();

    let first = reports::generate_window_report(&conn, "week", &start, &end).unwrap();
    assert_eq!(first.cached, false);

    let second = reports::generate_window_report(&conn, "week", &start, &end).unwrap();
    assert_eq!(second.cached, true);
    assert_eq!(
        second.id, first.id,
        "cached hit must return the same row id"
    );

    // Only one row on disk — no duplicate writes.
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM window_reports WHERE window_start = ?1 AND window_end = ?2",
            params![start, end],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn list_window_reports_returns_newest_first_with_label_filter() {
    let conn = open_db();
    seed_source(&conn, "src_c");

    // Force two distinct windows so the cache doesn't collapse them.
    reports::generate_window_report(
        &conn,
        "day",
        "2026-10-15T00:00:00Z",
        "2026-10-16T00:00:00Z",
    )
    .unwrap();
    reports::generate_window_report(
        &conn,
        "week",
        "2026-10-09T00:00:00Z",
        "2026-10-16T00:00:00Z",
    )
    .unwrap();
    reports::generate_window_report(
        &conn,
        "day",
        "2026-10-16T00:00:00Z",
        "2026-10-17T00:00:00Z",
    )
    .unwrap();

    let all = reports::list_window_reports(&conn, None, 10).unwrap();
    assert_eq!(all.len(), 3);

    let days = reports::list_window_reports(&conn, Some("day"), 10).unwrap();
    assert_eq!(days.len(), 2);
    for r in &days {
        assert_eq!(r.window_label, "day");
    }
}

#[test]
fn empty_window_returns_non_empty_heuristic_message() {
    let conn = open_db();
    seed_source(&conn, "src_d");
    let (start, end) = today_window_utc();
    let r = reports::generate_window_report(&conn, "day", &start, &end).unwrap();
    assert!(r.headline.contains("No captured"));
    assert_eq!(r.project_breakdown.len(), 0);
    assert!(!r.bullets.is_empty());
}

#[test]
fn partitions_from_two_sources_are_aggregated_by_project() {
    let conn = open_db();
    seed_source(&conn, "src_e1");
    seed_source(&conn, "src_e2");
    seed_partition(
        &conn,
        "s1",
        "src_e1",
        "2026-10-16T09:00:00Z",
        "2026-10-16T09:30:00Z",
        Some("/A"),
        "5 events from /A",
        4,
        /* event_count */ 5,
    );
    seed_partition(
        &conn,
        "s2",
        "src_e2",
        "2026-10-16T10:00:00Z",
        "2026-10-16T10:30:00Z",
        Some("/B"),
        "8 events from /B",
        7,
        /* event_count */ 12,
    );
    let (start, end) = today_window_utc();
    let r = reports::generate_window_report(&conn, "day", &start, &end).unwrap();
    assert_eq!(r.project_breakdown.len(), 2);
    // /B has more events than /A, so it should be listed first by events
    // (the heuristic sorts descending).
    assert_eq!(r.project_breakdown[0].project_key, "/B");
    assert_eq!(r.project_breakdown[0].events, 12);
    assert_eq!(r.project_breakdown[1].project_key, "/A");
    assert_eq!(r.project_breakdown[1].events, 5);
}
