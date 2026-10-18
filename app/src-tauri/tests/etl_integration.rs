//! v0.2 ETL integration tests — clustering `raw_events` into
//! `sessions_summary` partitions using a 30-minute inactivity gap.

use rusqlite::{params, Connection};

use agent_work_ledger_lib::etl;
use agent_work_ledger_lib::migrate_for_tests;

fn seed_source(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
         VALUES (?1, 'codex', 'OpenAI Codex (test)', '/tmp', 'readable', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        params![id],
    )
    .unwrap();
}

fn seed_raw_event(
    conn: &Connection,
    source_id: &str,
    occurred_at: &str,
    project_key: &str,
    event_type: &str,
) -> String {
    let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    conn.execute(
        "INSERT INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, '{}', 0, NULL, '2026-01-01T00:00:00Z')",
        params![id, source_id, event_type, occurred_at, project_key],
    )
    .unwrap();
    id
}

fn open_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrate_for_tests(&conn).unwrap();
    conn
}

#[test]
fn empty_source_writes_zero_partitions() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    let n = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(n, 0);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions_summary WHERE source_id = 'src_a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn dense_events_within_30min_make_one_partition() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    // 5 events 1 minute apart all in /Users/me/proj.
    for off in 0..5 {
        let at = format!("2026-01-01T10:{:02}:00Z", off);
        seed_raw_event(&conn, "src_a", &at, "/Users/me/proj", "user_input");
    }

    let n = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(n, 1);

    let row: (String, String, i64) = conn
        .query_row(
            "SELECT started_at, project_key, event_count FROM sessions_summary WHERE source_id='src_a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.2, 5);
    assert_eq!(row.1, "/Users/me/proj");
}

#[test]
fn gap_over_30min_splits_partition() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    // First cluster: 3 events at 10:00, 10:05, 10:10
    for off in &[0u32, 5, 10] {
        let at = format!("2026-01-01T10:{:02}:00Z", off);
        seed_raw_event(&conn, "src_a", &at, "/A", "user_input");
    }
    // Gap then second cluster: 12:00, 12:10, 12:20, 12:30
    for off in &[0u32, 10, 20, 30] {
        let at = format!("2026-01-01T12:{:02}:00Z", off);
        seed_raw_event(&conn, "src_a", &at, "/A", "user_input");
    }

    let n = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(n, 2);

    let mut counts: Vec<i64> = vec![];
    let mut stmt = conn
        .prepare("SELECT event_count FROM sessions_summary WHERE source_id='src_a' ORDER BY started_at")
        .unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap();
    for r in rows {
        counts.push(r.unwrap());
    }
    assert_eq!(counts, vec![3, 4]);
}

#[test]
fn reconciler_is_idempotent() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    for off in &[0u32, 5, 10] {
        let at = format!("2026-01-01T10:{:02}:00Z", off);
        seed_raw_event(&conn, "src_a", &at, "/A", "user_input");
    }
    let first = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    let second = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(first, second);
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions_summary WHERE source_id='src_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn reconciler_only_owns_one_source_at_a_time() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    seed_source(&conn, "src_b");
    seed_raw_event(&conn, "src_a", "2026-01-01T10:00:00Z", "/A", "user_input");
    seed_raw_event(&conn, "src_b", "2026-01-01T10:00:00Z", "/B", "user_input");

    let n_a = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    let n_b_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions_summary WHERE source_id='src_b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_a, 1);
    assert_eq!(n_b_before, 0, "src_a's reconcile must not touch src_b");

    let n_b = etl::reconcile_sessions_summary(&conn, "src_b").unwrap();
    assert_eq!(n_b, 1);
}

#[test]
fn cascade_fk_drops_existing_partitions() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    seed_raw_event(&conn, "src_a", "2026-01-01T10:00:00Z", "/A", "user_input");
    seed_raw_event(&conn, "src_a", "2026-01-01T10:05:00Z", "/A", "user_input");

    let first = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(first, 1);
    // Drop the underlying event, reconcile again — the partition must vanish.
    conn.execute("DELETE FROM raw_events WHERE source_id='src_a'", [])
        .unwrap();
    let second = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(second, 0);
}

#[test]
fn heuristic_importance_grows_with_event_count() {
    let conn = open_db();
    seed_source(&conn, "src_a");
    seed_source(&conn, "src_b");
    // 50 events in one cluster → high importance
    for off in 0..50 {
        let at = format!("2026-01-01T10:{:02}:00Z", off % 60);
        seed_raw_event(&conn, "src_a", &at, "/A", "user_input");
    }
    // 1 lonely event → low importance
    seed_raw_event(&conn, "src_b", "2026-01-02T10:00:00Z", "/A", "user_input");

    etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    etl::reconcile_sessions_summary(&conn, "src_b").unwrap();

    let imp_a: i64 = conn
        .query_row(
            "SELECT importance FROM sessions_summary WHERE source_id='src_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let imp_b: i64 = conn
        .query_row(
            "SELECT importance FROM sessions_summary WHERE source_id='src_b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(imp_a >= 8, "big cluster must score high; got {imp_a}");
    assert!(imp_b <= 3, "tiny cluster must score low; got {imp_b}");
    assert!(imp_a > imp_b);
}
