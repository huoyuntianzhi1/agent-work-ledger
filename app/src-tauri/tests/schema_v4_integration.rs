//! Stage 4 (v0.2) integration: prove the v3 → v4 migration actually
//! drops legacy tables, seeds the new summaries tables, attaches the FK
//! on plan_items.source_session_id, and bumps schema_version.

use rusqlite::Connection;

use agent_work_ledger_lib::migrate_for_tests;

/// Stand up an in-memory DB shaped like the v0.1 / v0.3 era schema and
/// pin schema_version = 3. We deliberately construct it by hand instead
/// of running migrate_for_tests (which would otherwise short-circuit to
/// v4 on a fresh DB) so the migration's v3-detection branch actually
/// fires.
fn open_v3_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE sources (
            id           TEXT PRIMARY KEY,
            kind         TEXT NOT NULL,
            display_name TEXT NOT NULL,
            root_path    TEXT NOT NULL,
            status       TEXT NOT NULL,
            last_scan_at TEXT,
            meta_json    TEXT,
            created_at   TEXT NOT NULL,
            updated_at   TEXT NOT NULL
        );
        CREATE TABLE raw_events (
            id           TEXT PRIMARY KEY,
            source_id    TEXT NOT NULL,
            event_type   TEXT NOT NULL,
            occurred_at  TEXT NOT NULL,
            project_key  TEXT,
            payload      TEXT NOT NULL,
            redacted     INTEGER NOT NULL DEFAULT 0,
            payload_hash TEXT,
            created_at   TEXT NOT NULL
        );
        CREATE TABLE connector_runs (
            id           TEXT PRIMARY KEY,
            source_id    TEXT NOT NULL,
            started_at   TEXT NOT NULL,
            ended_at     TEXT,
            status       TEXT NOT NULL,
            events_count INTEGER NOT NULL DEFAULT 0,
            error        TEXT,
            summary_json TEXT
        );
        CREATE TABLE privacy_rules (
            id         TEXT PRIMARY KEY,
            kind       TEXT NOT NULL,
            pattern    TEXT NOT NULL,
            enabled    INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL
        );
        CREATE TABLE plan_templates (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            kind       TEXT NOT NULL DEFAULT 'day',
            created_at TEXT NOT NULL
        );
        CREATE TABLE plan_items (
            id              TEXT PRIMARY KEY,
            template_id     TEXT NOT NULL,
            quadrant        INTEGER NOT NULL,
            title           TEXT NOT NULL,
            description     TEXT NOT NULL DEFAULT '',
            status          TEXT NOT NULL DEFAULT 'todo',
            priority        INTEGER NOT NULL DEFAULT 0,
            source_event_id TEXT,
            ai_reasoning    TEXT,
            created_at      TEXT NOT NULL,
            updated_at      TEXT NOT NULL,
            completed_at    TEXT,
            FOREIGN KEY (template_id) REFERENCES plan_templates(id) ON DELETE CASCADE
        );
        -- v0.1 ETL tables that v3→v4 must DROP.
        CREATE TABLE sessions (
            id          TEXT PRIMARY KEY,
            source_id   TEXT NOT NULL,
            external_id TEXT NOT NULL,
            title       TEXT,
            started_at  TEXT,
            ended_at    TEXT,
            project_key TEXT,
            cwd         TEXT,
            meta_json   TEXT,
            created_at  TEXT NOT NULL
        );
        CREATE TABLE work_spans (
            id          TEXT PRIMARY KEY,
            session_id  TEXT,
            source_id   TEXT NOT NULL,
            project_key TEXT,
            started_at  TEXT NOT NULL,
            ended_at    TEXT,
            event_count INTEGER NOT NULL DEFAULT 0,
            summary     TEXT,
            confidence  REAL,
            created_at  TEXT NOT NULL
        );
        CREATE TABLE tasks (
            id                  TEXT PRIMARY KEY,
            project_key         TEXT,
            title               TEXT NOT NULL,
            summary             TEXT,
            status              TEXT NOT NULL DEFAULT 'open',
            artifact_level      TEXT NOT NULL DEFAULT 'draft',
            eisenhower_quadrant TEXT NOT NULL DEFAULT 'unsure',
            importance_score    INTEGER,
            urgency_score       INTEGER,
            started_at          TEXT,
            ended_at            TEXT,
            duration_minutes    INTEGER,
            source_ids          TEXT NOT NULL DEFAULT '[]',
            confidence          REAL,
            evidence_ids        TEXT NOT NULL DEFAULT '[]',
            created_by          TEXT NOT NULL DEFAULT 'auto',
            user_notes          TEXT,
            created_at          TEXT NOT NULL,
            updated_at          TEXT NOT NULL
        );
        CREATE TABLE evidence (
            id          TEXT PRIMARY KEY,
            task_id     TEXT NOT NULL,
            raw_event_id TEXT,
            session_id  TEXT,
            kind        TEXT NOT NULL,
            excerpt     TEXT NOT NULL,
            created_at  TEXT NOT NULL
        );
        CREATE TABLE reports (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL,
            period_from TEXT NOT NULL,
            period_to   TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  TEXT NOT NULL
        );

        INSERT INTO sources(id, kind, display_name, root_path, status, created_at, updated_at)
            VALUES ('src_test', 'codex', 'OpenAI Codex (test)', '/tmp', 'readable',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
        INSERT INTO sessions(id, source_id, external_id, created_at)
            VALUES ('sess_legacy_1', 'src_test', 'ext-1', '2026-01-01T00:00:00Z');
        INSERT INTO tasks(id, title, created_at, updated_at)
            VALUES ('task_legacy_1', 'legacy task', '2026-01-01T00:00:00Z',
                    '2026-01-01T00:00:00Z');
        INSERT INTO plan_templates(id, name, created_at)
            VALUES ('tmpl_legacy', 'Legacy', '2026-01-01T00:00:00Z');
        INSERT INTO plan_items(id, template_id, quadrant, title, created_at, updated_at)
            VALUES ('item_legacy', 'tmpl_legacy', 2, 'legacy item',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
        INSERT INTO schema_meta(key, value) VALUES('schema_version', '3');
        "#,
    )
    .unwrap();
    conn
}

#[test]
fn v3_to_v4_drops_legacy_etl_tables() {
    let conn = open_v3_db();
    migrate_for_tests(&conn).unwrap();

    // The five legacy tables must be gone (query must fail).
    for gone in ["sessions", "work_spans", "tasks", "evidence", "reports"] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                [gone],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0, "legacy table '{gone}' should be dropped");
    }
}

#[test]
fn v3_to_v4_creates_new_summary_tables() {
    let conn = open_v3_db();
    migrate_for_tests(&conn).unwrap();

    for present in ["sessions_summary", "session_summaries", "window_reports"] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                [present],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "table '{present}' must be created");
    }
}

#[test]
fn v3_to_v4_adds_plan_items_source_session_id_column() {
    let conn = open_v3_db();
    migrate_for_tests(&conn).unwrap();

    let col_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('plan_items')
             WHERE name = 'source_session_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(col_count, 1, "plan_items.source_session_id must exist");
}

#[test]
fn v3_to_v4_bumps_schema_version() {
    let conn = open_v3_db();
    migrate_for_tests(&conn).unwrap();

    let v: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(v, "4");
}

#[test]
fn v4_is_idempotent() {
    // Running migrate twice on an already-v4 DB must not corrupt anything
    // and must not re-run the v3 → v4 block.
    let conn = open_v3_db();
    migrate_for_tests(&conn).unwrap();
    let rows_after_first: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions_summary",
            [],
            |row| row.get(0),
        )
        .unwrap();
    migrate_for_tests(&conn).unwrap();
    let rows_after_second: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions_summary",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows_after_first, rows_after_second);
}

#[test]
fn cascade_delete_plan_item_when_source_session_removed() {
    // After v3 → v4, plan_items.source_session_id → sessions_summary(id)
    // with ON DELETE CASCADE. Insert a fake session, link a plan item to
    // it, drop the session, expect the plan item to also drop.
    let conn = open_v3_db();
    migrate_for_tests(&conn).unwrap();

    conn.execute(
        "INSERT INTO sessions_summary(id, source_id, started_at, ended_at, event_count, created_at, updated_at)
         VALUES ('sess1', 'src_test', '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z', 5,
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    // First insert a plan_template so the plan_items FK is satisfied.
    conn.execute(
        "INSERT INTO plan_templates(id, name, created_at) VALUES ('tmpl1', 'Today', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO plan_items(id, template_id, quadrant, title, created_at, updated_at, source_session_id)
         VALUES ('item1', 'tmpl1', 1, 'fix bug', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'sess1')",
        [],
    )
    .unwrap();

    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM plan_items WHERE id='item1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 1);

    conn.execute("DELETE FROM sessions_summary WHERE id='sess1'", [])
        .unwrap();

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM plan_items WHERE id='item1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 0, "FK ON DELETE CASCADE should wipe the plan_item");
}
