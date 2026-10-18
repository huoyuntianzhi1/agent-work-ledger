//! Stage 5-E.2b integration: prove the markdown summary scanner can run
//! end-to-end. The connector is a no-op when `summary_dir` is not
//! configured (the typical fresh-install state) and starts emitting
//! real events once the user fills in the path via Settings → 本地配置.

use std::sync::Mutex;

use rusqlite::Connection;

use agent_work_ledger_lib::connectors::markdown_summary::MarkdownSummaryConnector;
use agent_work_ledger_lib::connectors::{Connector, ScanCtx};

#[test]
fn end_to_end_markdown_summary_scan() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    agent_work_ledger_lib::migrate_for_tests(&conn).expect("migrate");

    let mtx = Mutex::new(conn);
    let connector = MarkdownSummaryConnector::new();
    let mut guard = mtx.lock().unwrap();
    let ctx = ScanCtx {
        conn: &mut guard,
        source_id: "",
        since_ms: None,
    };
    let report = connector.scan(ctx).expect("markdown scan");

    eprintln!(
        "markdown scan: status={} events_inserted={} files_scanned={} error={:?}",
        report.status, report.events_inserted, report.files_scanned, report.error
    );

    // The connector must always succeed — when summary_dir is not
    // configured it reports a friendly hint via `error` instead of
    // failing. Once the user fills in Settings, events_inserted > 0.
    assert!(
        ["ok", "partial"].contains(&report.status.as_str()),
        "unexpected status: {}",
        report.status
    );
}
