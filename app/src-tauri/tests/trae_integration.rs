//! Stage 5-E.1a integration: prove the Trae metadata connector can run
//! end-to-end against the live Trae install on the developer's machine.
//! Like the other local-store connectors, this is a probe — the test
//! always passes as long as the scan path runs and either surfaces real
//! data or cleanly reports "no Trae install".

use std::sync::Mutex;

use rusqlite::Connection;

use agent_work_ledger_lib::connectors::trae::TraeMetadataConnector;
use agent_work_ledger_lib::connectors::{Connector, ScanCtx};

#[test]
fn end_to_end_trae_metadata_scan() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    agent_work_ledger_lib::migrate_for_tests(&conn).expect("migrate");

    let mtx = Mutex::new(conn);
    let connector = TraeMetadataConnector::new();
    let mut guard = mtx.lock().unwrap();
    let ctx = ScanCtx {
        conn: &mut guard,
        source_id: "",
        since_ms: None,
    };
    let report = connector.scan(ctx).expect("trae scan");

    let count: i64 = guard
        .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
        .unwrap();
    eprintln!(
        "trae scan: status={} events_inserted={} files_scanned={} error={:?} rows_in_db={}",
        report.status, report.events_inserted, report.files_scanned, report.error, count
    );

    // The connector must always land in one of the three known
    // outcomes. Without Trae installed we get status="ok" with 0
    // events and an empty error string (root not present). With Trae
    // we expect at least one workspace metadata event.
    assert!(
        ["ok", "partial"].contains(&report.status.as_str()),
        "unexpected status: {}",
        report.status
    );
}
