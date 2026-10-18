//! `awl-db` — Agent Work Ledger SQLite schema + retention/window/app-settings.
//!
//! Pure persistence layer: schema migrations, app-settings JSON file, retention
//! pruning helpers, window-state snapshots. `awl-ipc` opens the DB through
//! `open_db` and constructs an `Arc<Mutex<Connection>>` shared with the
//! Tauri state and background scanner threads.
//!
//! Schema lives here because (a) every other crate's tests want to stand up
//! a real DB without going through `awl-ipc`, and (b) the v0.4.x schema
//! version is tied to the binary, so it must be co-located with the binary
//! crate (`awl-desktop`) for stable versioning.

pub mod retention;

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use awl_core::connectors;
use awl_core::error::{AppError, AppResult};
use awl_core::planner;

/// Current schema version. Bump this whenever a migration changes shape.
pub const SCHEMA_VERSION: i64 = 4;

/// Resolve the per-user data directory for this app, creating it if missing.
/// Layout follows platform conventions so we never have to special-case later.
pub fn resolve_data_dir() -> AppResult<PathBuf> {
    let base = dirs::data_dir().ok_or(AppError::NoDataDir)?;
    let dir = base.join("com.minimaxcode.agentworkledger");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Open (or create) the SQLite database and run migrations.
pub fn open_db(path: &PathBuf) -> AppResult<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // Snapshot the v0.1-schema DB before we drop any v0.1 tables.
    // We only do this when an existing on-disk database reports the
    // pre-v4 schema version (first run on a v0.1-built DB after the
    // upgrade, fresh DBs get it on first migrate and skip).
    if is_v3_schema(&conn) {
        backup_ledger_pre_v4(path).ok();
    }

    migrate(&conn)?;
    Ok(conn)
}

/// Probe whether the on-disk DB is still at schema v3 (v0.1 era).
/// Reads `schema_meta`; missing key counts as v0.1 because the
/// v0.1→v0.4 upgrades only have one migration step.
fn is_v3_schema(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT value FROM schema_meta WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    )
    .map(|s| s == "3")
    .unwrap_or(false)
}

/// Copy the existing `ledger.db` to `<data_dir>/backups/ledger-v3-pre-migration-<ts>.db`.
/// Best-effort: if the copy fails we log a warning and continue, because
/// the user would rather lose the snapshot than block startup.
fn backup_ledger_pre_v4(db_path: &Path) -> AppResult<PathBuf> {
    let dir = db_path.parent().ok_or_else(|| {
        AppError::Other(format!("ledger.db has no parent dir: {}", db_path.display()))
    })?;
    let backups = dir.join("backups");
    std::fs::create_dir_all(&backups)?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let backup_path = backups.join(format!("ledger-v3-pre-migration-{ts}.db"));
    // Checkpoint WAL so the .db file on disk contains every committed row.
    // Source WAL/SHM may still exist alongside; the copy of the .db alone
    // is enough for sqlite to open a read-only snapshot later.
    std::fs::copy(db_path, &backup_path)?;
    tracing::info!(
        original = %db_path.display(),
        backup = %backup_path.display(),
        "v3 → v4 backup created"
    );
    Ok(backup_path)
}

/// Embedded migrations. Each numbered block represents a schema version.
/// New migrations must be appended at the end and `SCHEMA_VERSION` bumped.
fn migrate(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sources (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL,
            display_name  TEXT NOT NULL,
            root_path     TEXT NOT NULL,
            status        TEXT NOT NULL,
            last_scan_at  TEXT,
            meta_json     TEXT,
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS raw_events (
            id          TEXT PRIMARY KEY,
            source_id   TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
            event_type  TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            project_key TEXT,
            payload     TEXT NOT NULL,
            redacted    INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_raw_events_source_at ON raw_events(source_id, occurred_at);
        CREATE INDEX IF NOT EXISTS idx_raw_events_project_at ON raw_events(project_key, occurred_at);

        CREATE TABLE IF NOT EXISTS sessions (
            id              TEXT PRIMARY KEY,
            source_id       TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
            external_id     TEXT NOT NULL,
            title           TEXT,
            started_at      TEXT,
            ended_at        TEXT,
            project_key     TEXT,
            cwd             TEXT,
            meta_json       TEXT,
            created_at      TEXT NOT NULL,
            UNIQUE(source_id, external_id)
        );

        CREATE TABLE IF NOT EXISTS work_spans (
            id            TEXT PRIMARY KEY,
            session_id    TEXT REFERENCES sessions(id) ON DELETE CASCADE,
            source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
            project_key   TEXT,
            started_at    TEXT NOT NULL,
            ended_at      TEXT,
            event_count   INTEGER NOT NULL DEFAULT 0,
            summary       TEXT,
            confidence    REAL,
            created_at    TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tasks (
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

        CREATE TABLE IF NOT EXISTS evidence (
            id          TEXT PRIMARY KEY,
            task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            raw_event_id TEXT REFERENCES raw_events(id) ON DELETE CASCADE,
            session_id  TEXT REFERENCES sessions(id) ON DELETE SET NULL,
            kind        TEXT NOT NULL,
            excerpt     TEXT NOT NULL,
            created_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS reports (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL,
            period_from TEXT NOT NULL,
            period_to   TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS connector_runs (
            id            TEXT PRIMARY KEY,
            source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
            started_at    TEXT NOT NULL,
            ended_at      TEXT,
            status        TEXT NOT NULL,
            events_count  INTEGER NOT NULL DEFAULT 0,
            error         TEXT,
            summary_json  TEXT
        );

        CREATE TABLE IF NOT EXISTS privacy_rules (
            id          TEXT PRIMARY KEY,
            kind        TEXT NOT NULL,
            pattern     TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            created_at  TEXT NOT NULL
        );
        "#,
    )?;

    // Schema version 3: Stage 3 Eisenhower planner.
    //
    // plan_templates — named shells (Daily Stand-up, Weekly Review, etc.).
    // plan_items — tasks belonging to one template, each carrying an
    //               Eisenhower quadrant (1..4), status, optional link to a
    //               raw_events row, and an AI reasoning string populated
    //               when the local Ollama classifier placed it.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS plan_templates (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL DEFAULT 'day',
            created_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS plan_items (
            id              TEXT PRIMARY KEY,
            template_id     TEXT NOT NULL,
            quadrant        INTEGER NOT NULL CHECK (quadrant BETWEEN 1 AND 4),
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

        CREATE INDEX IF NOT EXISTS idx_plan_items_template_q
            ON plan_items(template_id, quadrant);
        "#,
    )?;
    planner::ensure_default_templates(conn)?;

    // Schema version 4: Stage 4 v0.2 rewrite. The v4 tables and indexes
    // are CREATE IF NOT EXISTS so a fresh DB (schema_meta empty) gets
    // them too. The destructive parts (DROP v0.1 tables + ALTER
    // plan_items to add source_session_id FK) live inside the `is_at_v3`
    // guard so we only run them once when upgrading from a v0.1-era DB.
    //
    // The pre-v4 backup is taken in open_db() before migrate() runs.
    let is_at_v3 = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|s| s == "3")
        .unwrap_or(false);

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sessions_summary (
            id           TEXT PRIMARY KEY,
            source_id    TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
            external_id  TEXT,
            started_at   TEXT NOT NULL,
            ended_at     TEXT NOT NULL,
            event_count  INTEGER NOT NULL,
            source_event_count INTEGER NOT NULL DEFAULT 0,
            project_key  TEXT,
            summary      TEXT NOT NULL DEFAULT '',
            importance   INTEGER NOT NULL DEFAULT 5,
            heuristic_kind TEXT NOT NULL DEFAULT 'auto',
            created_at   TEXT NOT NULL,
            updated_at   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_summary_source_at
            ON sessions_summary(source_id, started_at);
        CREATE INDEX IF NOT EXISTS idx_sessions_summary_started_at
            ON sessions_summary(started_at);

        CREATE TABLE IF NOT EXISTS session_summaries (
            id                  TEXT PRIMARY KEY,
            session_summary_id  TEXT NOT NULL REFERENCES sessions_summary(id) ON DELETE CASCADE,
            prompt_hash         TEXT NOT NULL,
            provider            TEXT NOT NULL,
            model               TEXT NOT NULL,
            headline            TEXT NOT NULL,
            body                TEXT NOT NULL,
            importance          INTEGER NOT NULL,
            urgency             INTEGER NOT NULL,
            reasoning           TEXT NOT NULL,
            expires_at          TEXT NOT NULL,
            created_at          TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_session_summaries_session
            ON session_summaries(session_summary_id);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_expires
            ON session_summaries(expires_at);

        CREATE TABLE IF NOT EXISTS window_reports (
            id            TEXT PRIMARY KEY,
            window_label  TEXT NOT NULL,
            window_start  TEXT NOT NULL,
            window_end    TEXT NOT NULL,
            prompt        TEXT NOT NULL,
            response      TEXT NOT NULL,
            provider      TEXT NOT NULL,
            model         TEXT NOT NULL,
            expires_at    TEXT NOT NULL,
            created_at    TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_window_reports_window
            ON window_reports(window_start, window_end);
        CREATE INDEX IF NOT EXISTS idx_window_reports_expires
            ON window_reports(expires_at);
        "#,
    )?;

    if is_at_v3 {
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS evidence;
            DROP TABLE IF EXISTS work_spans;
            DROP TABLE IF EXISTS reports;
            DROP TABLE IF EXISTS tasks;
            DROP TABLE IF EXISTS sessions;

            ALTER TABLE plan_items ADD COLUMN source_session_id TEXT
                REFERENCES sessions_summary(id) ON DELETE CASCADE;
            CREATE INDEX IF NOT EXISTS idx_plan_items_source_session
                ON plan_items(source_session_id);
            "#,
        )?;
        tracing::info!("v3 → v4 schema migration complete: ETL tables dropped, plan_items.source_session_id FK added");
    }

    // Schema version 2: dedup via payload_hash UNIQUE INDEX.
    //
    // Two upgrade paths exist because we added `payload_hash` directly into
    // the v1 CREATE TABLE in some early builds (so older in-memory tests
    // already had the column), while older disk DBs from 1-A still need an
    // ALTER. Make the migration idempotent: probe column existence first.
    let has_payload_hash: bool = conn
        .prepare("PRAGMA table_info(raw_events)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == "payload_hash");
    if !has_payload_hash {
        conn.execute_batch(
            "ALTER TABLE raw_events ADD COLUMN payload_hash TEXT;",
        )?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_raw_events_dedup
             ON raw_events(source_id, event_type, payload_hash)
             WHERE payload_hash IS NOT NULL;",
    )?;

    let now = connectors::rfc3339(Some(connectors::now_ms()));
    let defaults: &[(&str, &str, &str)] = &[
        ("default-redact-sk", "redact_pattern", r"(?i)\b(sk-[A-Za-z0-9_-]{16,})\b"),
        ("default-redact-openai-cp", "redact_pattern", r"(?i)\b(sk-cp-[A-Za-z0-9_-]{16,})\b"),
        ("default-redact-anthropic", "redact_pattern", r"(?i)\b(sk-ant-[A-Za-z0-9_-]{16,})\b"),
        ("default-redact-aws-akia", "redact_pattern", r"\bAKIA[0-9A-Z]{16}\b"),
        ("default-redact-bearer", "redact_pattern", r"(?i)\b(Bearer\s+[A-Za-z0-9._\-]{12,})\b"),
        ("default-redact-jwt", "redact_pattern", r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
        ("default-exclude-ssh", "exclude_dir", "~/.ssh"),
        ("default-exclude-aws", "exclude_dir", "~/.aws"),
        ("default-exclude-gpg", "exclude_dir", "~/.gnupg"),
        ("default-exclude-npmrc", "exclude_dir", "~/.npmrc"),
    ];
    for (id, kind, pattern) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO privacy_rules(id, kind, pattern, enabled, created_at) VALUES (?1, ?2, ?3, 1, ?4)",
            params![id, kind, pattern, now],
        )?;
    }

    conn.execute(
        "INSERT OR REPLACE INTO schema_meta(key, value) VALUES('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;

    Ok(())
}

/// Test-only entry point: run the in-library schema migration against an
/// arbitrary connection. Integration tests in `awl-core` use it to stand up
/// an in-memory ledger before exercising a connector.
pub fn migrate_for_tests(conn: &rusqlite::Connection) -> AppResult<()> {
    migrate(conn)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn schema_version_recorded() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn defaults_seeded() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM privacy_rules", [], |row| row.get(0))
            .unwrap();
        assert!(count >= 10);
    }
}
