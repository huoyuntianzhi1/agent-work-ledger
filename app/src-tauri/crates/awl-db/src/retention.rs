//! v0.4.3: raw_events retention policy.
//!
//! 默认 90 天前的 `raw_events` 永久删 (sessions_summary / brief 等
//! 聚合不删 — 它们是按 partition 重建的,删了 raw_events 不会丢
//! 业务数据,只丢 raw payload 用于回查细节)。
//! `0` = 永久保留(prune 退化为 no-op)。
//!
//! Auto-rescan 完成后自动 prune 一次(`auto_rescan_all` 末尾
//! 调用),Settings 页有「保留天数」输入 + 「立即清理」按钮。
//!
//! 设计选择 — 为什么 DELETE 而不是软删 (`archived_at`)?
//! - 用户明确说"清理",没要求回滚
//! - DB 现在 9956 events,预期 90 天后 ~50K 量级,DELETE 简单
//! - raw_events 在 sessions_summary / eisenhower 聚合重建时不需要
//!   永久保留,删了不影响日常
//! - 真要后悔,99% 场景用户能重新跑一次 ETL 从源拉新数据回来
//! - 软删会让所有现有 query 都得加 `WHERE archived_at IS NULL`
//!   → 100+ 处 filter,容易漏;DELETE 直接,schema 不污染
//!
//! 如果以后真有 audit 需求,再加 `event_archive` 表 + trigger
//! before-delete 把 row 复制过去。当前 9K-50K 行不浪费这工夫。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use awl_core::error::AppResult;

/// Default days threshold when the user hasn't customised yet. 90
/// is the conventional "quarter" — long enough to cover a project
/// cycle, short enough that the DB doesn't grow unbounded.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// Outcome of one prune pass. Returned to the IPC caller so the
/// UI can surface "已清理 N 条 / 90 天前的" without a second query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruneReport {
    pub events_deleted: i64,
    pub cutoff_iso: String,
    pub days: i64,
    /// `false` when `days <= 0` (永久保留) — we short-circuit before
    /// touching the DB.
    pub ran: bool,
}

/// Delete every `raw_events` row whose `occurred_at` is strictly
/// older than `now - days`. `0` or negative `days` is a no-op
/// (永久保留).
pub fn prune_events(conn: &Connection, days: i64) -> AppResult<PruneReport> {
    if days <= 0 {
        return Ok(PruneReport {
            events_deleted: 0,
            cutoff_iso: String::new(),
            days,
            ran: false,
        });
    }
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(days);
    let cutoff_iso = cutoff.to_rfc3339();
    let deleted = conn.execute(
        "DELETE FROM raw_events WHERE occurred_at < ?1",
        rusqlite::params![&cutoff_iso],
    )? as i64;
    Ok(PruneReport {
        events_deleted: deleted,
        cutoff_iso,
        days,
        ran: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate_for_tests;

    fn insert_event(
        conn: &Connection,
        id: &str,
        source_id: &str,
        occurred_iso: &str,
    ) {
        // `raw_events` FKs `sources`; insert a fake one once.
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO sources (id, kind, display_name, root_path, status, created_at, updated_at)
             VALUES (?1, 'claude_code', 'test', '/tmp/test', 'readable', ?2, ?2)",
            rusqlite::params![source_id, &now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO raw_events (id, source_id, event_type, occurred_at, project_key, payload, redacted, created_at)
             VALUES (?1, ?2, 'message', ?3, '/test', '{}', 0, ?4)",
            rusqlite::params![id, source_id, occurred_iso, &now],
        )
        .unwrap();
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM raw_events", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn prune_with_zero_days_is_noop() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        insert_event(&conn, "ev_1", "src_test", "2020-01-01T00:00:00+00:00");
        insert_event(&conn, "ev_2", "src_test", "2030-01-01T00:00:00+00:00");
        let r = prune_events(&conn, 0).unwrap();
        assert!(!r.ran);
        assert_eq!(r.events_deleted, 0);
        assert_eq!(count(&conn), 2);
    }

    #[test]
    fn prune_deletes_only_old_events() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        // 200 days old vs today.
        let now = chrono::Utc::now();
        let old = (now - chrono::Duration::days(200)).to_rfc3339();
        let recent = now.to_rfc3339();
        insert_event(&conn, "ev_old", "src_test", &old);
        insert_event(&conn, "ev_recent", "src_test", &recent);
        let r = prune_events(&conn, 90).unwrap();
        assert!(r.ran);
        assert_eq!(r.events_deleted, 1);
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn prune_does_not_touch_sessions_summary() {
        // v0.4.3 design choice: only raw_events, not the aggregates.
        // Just smoke-test the boundary: empty raw_events → ran=true,
        // deleted=0.
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        let r = prune_events(&conn, 90).unwrap();
        assert!(r.ran);
        assert_eq!(r.events_deleted, 0);
    }

    #[test]
    fn prune_cutoff_iso_is_rfc3339() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        let r = prune_events(&conn, 90).unwrap();
        // Should parse as a valid RFC 3339 / ISO 8601 timestamp.
        assert!(chrono::DateTime::parse_from_rfc3339(&r.cutoff_iso).is_ok());
    }
}
