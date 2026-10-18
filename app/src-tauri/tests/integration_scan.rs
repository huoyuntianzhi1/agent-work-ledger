//! End-to-end smoke tests that exercise each connector against the live
//! filesystem. These run on demand via `cargo test --test
//! integration_scan -- --ignored` so they don't run by default in CI —
//! the live data is real and may move over time.

use agent_work_ledger_lib::scan_for_tests;

/// Open a fresh in-memory ledger database with the schema migrated.
fn fresh_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-mem");
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    // Run the in-library migration so connector scans can upsert sources
    // and insert into raw_events without panicking.
    agent_work_ledger_lib::migrate_for_tests(&conn).expect("migrate");
    conn
}

#[test]
#[ignore]
fn scans_codex_against_real_home() {
    let mut conn = fresh_db();
    let r = scan_for_tests(&mut conn, "codex", None).expect("scan");
    println!("codex: {:?}", r);
    assert!(r.events_inserted > 0, "codex should produce events");
}

#[test]
#[ignore]
fn scans_claude_against_real_home() {
    let mut conn = fresh_db();
    let r = scan_for_tests(&mut conn, "claude_code", None).expect("scan");
    println!("claude: {:?}", r);
}

#[test]
#[ignore]
fn scans_opencode_against_real_home() {
    let mut conn = fresh_db();
    let r = scan_for_tests(&mut conn, "opencode", None).expect("scan");
    println!("opencode: {:?}", r);
    assert!(r.events_inserted > 0, "opencode should produce events");
}

#[test]
#[ignore]
fn scans_workbuddy_against_real_home() {
    let mut conn = fresh_db();
    let r = scan_for_tests(&mut conn, "workbuddy", None).expect("scan");
    println!("workbuddy: {:?}", r);
    assert!(r.events_inserted > 0, "workbuddy should produce events");
}