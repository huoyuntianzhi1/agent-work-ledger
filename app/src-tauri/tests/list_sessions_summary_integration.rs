//! v0.2 `list_sessions_summary` integration tests.
//!
//! The Sessions page calls `list_sessions_summary` to render its card
//! grid. We test the inner helper directly against an in-memory v4
//! schema to keep the IPC lock boilerplate out of the assertions.

use rusqlite::{params, Connection};

use agent_work_ledger_lib::etl;
use agent_work_ledger_lib::list_sessions_summary_inner;
use agent_work_ledger_lib::migrate_for_tests;

fn open_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrate_for_tests(&conn).unwrap();
    conn
}

fn seed_source(conn: &Connection, id: &str, display_name: &str) {
    conn.execute(
        "INSERT INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
         VALUES (?1, 'codex', ?2, '/tmp', 'readable', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        params![id, display_name],
    )
    .unwrap();
}

fn seed_raw_event(
    conn: &Connection,
    source_id: &str,
    occurred_at: &str,
    project_key: &str,
) {
    let id = format!("ev_{}", uuid::Uuid::new_v4().simple());
    conn.execute(
        "INSERT INTO raw_events(id, source_id, event_type, occurred_at, project_key, payload, redacted, payload_hash, created_at)
         VALUES (?1, ?2, 'user_input', ?3, ?4, '{}', 0, NULL, '2026-01-01T00:00:00Z')",
        params![id, source_id, occurred_at, project_key],
    )
    .unwrap();
}

#[test]
fn empty_db_returns_empty_vec() {
    let conn = open_db();
    let rows = list_sessions_summary_inner(&conn);
    assert!(rows.is_empty());
}

#[test]
fn partitions_returned_in_started_at_desc_order() {
    let conn = open_db();
    seed_source(&conn, "src_a", "OpenAI Codex");
    // Three buckets 30+min apart so each ends up as its own partition.
    for at in &[
        "2026-01-01T08:00:00Z",
        "2026-01-01T10:00:00Z",
        "2026-01-01T12:00:00Z",
    ] {
        seed_raw_event(&conn, "src_a", at, "/Users/me/proj");
    }
    let n = etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(n, 3);

    let rows = list_sessions_summary_inner(&conn);
    assert_eq!(rows.len(), 3);
    // Newest-first ordering so the UI's grid mirrors the timeline's
    // "most recent" policy.
    assert!(
        rows[0].started_at > rows[1].started_at,
        "expected newest first: {} > {}",
        rows[0].started_at,
        rows[1].started_at
    );
    assert!(rows[1].started_at > rows[2].started_at);
}

#[test]
fn joins_source_display_name_when_present() {
    let conn = open_db();
    seed_source(&conn, "src_a", "OpenAI Codex");
    seed_raw_event(&conn, "src_a", "2026-02-01T10:00:00Z", "/Users/me/proj");
    etl::reconcile_sessions_summary(&conn, "src_a").unwrap();

    let rows = list_sessions_summary_inner(&conn);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source_display_name, "OpenAI Codex");
    assert_eq!(rows[0].source_id, "src_a");
}

#[test]
fn falls_back_to_source_id_when_source_row_missing() {
    // FK on sessions_summary.source_id → sources.id prevents INSERT-ing
    // a partition without a source row, so we cannot synthesise a true
    // orphan. Instead we exercise the CASCADE path: deleting a source
    // removes its `sessions_summary` rows (and via the v4 schema also
    // any `session_summaries` AI rows + `plan_items.source_session_id`
    // FKs). The list helper must then return the empty set for the
    // dropped partitions even though the underlying `sources` table
    // still references the id historically.
    let conn = open_db();
    seed_source(&conn, "src_a", "Codex");
    seed_raw_event(&conn, "src_a", "2026-03-01T10:00:00Z", "/Users/me/proj");
    etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    assert_eq!(list_sessions_summary_inner(&conn).len(), 1);

    conn.execute("DELETE FROM sources WHERE id = ?1", params!["src_a"])
        .unwrap();
    let rows = list_sessions_summary_inner(&conn);
    assert!(
        rows.is_empty(),
        "sessions_summary should cascade-delete with its source"
    );
}

#[test]
fn partitions_from_two_sources_are_mixed_in_started_at_order() {
    let conn = open_db();
    seed_source(&conn, "src_a", "Codex");
    seed_source(&conn, "src_b", "Claude");
    seed_raw_event(&conn, "src_a", "2026-04-01T09:00:00Z", "/p1");
    seed_raw_event(&conn, "src_b", "2026-04-01T10:00:00Z", "/p2");
    etl::reconcile_sessions_summary(&conn, "src_a").unwrap();
    etl::reconcile_sessions_summary(&conn, "src_b").unwrap();

    let rows = list_sessions_summary_inner(&conn);
    assert_eq!(rows.len(), 2);
    assert!(
        rows[0].source_display_name == "Claude"
            && rows[1].source_display_name == "Codex",
        "newest bucket should be Claude at 10:00, oldest Codex at 09:00"
    );
}
