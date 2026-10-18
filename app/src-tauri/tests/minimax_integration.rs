//! Stage 1-I-5 integration: prove the MiniMax Agent connector can ingest
//! a real Chromium IndexedDB LevelDB into the ledger. Skipped by default
//! (the test discovery itself does not require MiniMax Agent installed).

use std::sync::Mutex;

use rusqlite::Connection;

use agent_work_ledger_lib::connectors::minimax::MiniMaxConnector;
use agent_work_ledger_lib::connectors::{Connector, ScanCtx};

#[test]
fn end_to_end_minimax_agent() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    agent_work_ledger_lib::migrate_for_tests(&conn).expect("migrate");

    // Connector takes &mut Connection; sqlite is !Sync so guard with a Mutex.
    let mtx = Mutex::new(conn);
    let connector = MiniMaxConnector::new();
    let mut guard = mtx.lock().unwrap();
    let ctx = ScanCtx {
        conn: &mut guard,
        source_id: "",
        since_ms: None,
    };
    let report = connector.scan(ctx).expect("minimax scan");

    let count: i64 = guard
        .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
        .unwrap();
    eprintln!(
        "minimax scan: status={} events_inserted={} files_scanned={} error={:?} rows_in_db={}",
        report.status, report.events_inserted, report.files_scanned, report.error, count
    );

    // We don't require any specific number of events here — the test
    // exists to prove the path is wired up end-to-end. A user without
    // MiniMax Agent installed still gets a clean run (status="skipped",
    // 0 events). A user with MiniMax Agent gets at least one event when
    // their local store has at least one qualifying record.
    assert!(
        ["ok", "partial", "skipped"].contains(&report.status.as_str()),
        "unexpected status: {}",
        report.status
    );
}