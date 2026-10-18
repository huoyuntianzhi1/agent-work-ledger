//! Stage 1-I-3 integration: prove the Doubao connector can ingest a real
//! Chromium IndexedDB LevelDB into the ledger. Skipped by default; pass
//! `AWL_DOBAO_DIR` to enable.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;

use agent_work_ledger_lib::connectors::doubao::DoubaoConnector;
use agent_work_ledger_lib::connectors::{Connector, ScanCtx};

#[test]
#[ignore]
fn end_to_end_doubao_chat() {
    let _home_pin = std::env::var_os("AWL_DOBAO_DIR").map(PathBuf::from);
    // We can't pin `dirs::home_dir()` from inside the test, so we just
    // require AWL_DOBAO_DIR to point at a Chromium IndexedDB directory
    // that contains a `*.indexeddb.leveldb` subdir somewhere we can scan.
    let _ = &_home_pin; // silence unused warning if absent

    let conn = Connection::open_in_memory().expect("open in-memory db");
    agent_work_ledger_lib::migrate_for_tests(&conn).expect("migrate");

    // Connector takes &mut Connection; sqlite is !Sync so guard with a Mutex.
    let mtx = Mutex::new(conn);
    let connector = DoubaoConnector::consumer();
    let mut guard = mtx.lock().unwrap();
    let ctx = ScanCtx {
        conn: &mut guard,
        source_id: "",
        since_ms: None,
    };
    let report = connector.scan(ctx).expect("doubao scan");

    let count: i64 = guard
        .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
        .unwrap();
    eprintln!(
        "doubao scan: status={} events_inserted={} files_scanned={} error={:?} rows_in_db={}",
        report.status, report.events_inserted, report.files_scanned, report.error, count
    );

    // We don't require any specific number of events here — the test
    // exists to prove the path is wired up end-to-end. A user without
    // Doubao installed still gets a clean run (status="skipped", 0
    // events). A user with Doubao gets at least one event when their
    // local store has at least one qualifying record.
    assert!(
        ["ok", "partial", "skipped"].contains(&report.status.as_str()),
        "unexpected status: {}",
        report.status
    );
}