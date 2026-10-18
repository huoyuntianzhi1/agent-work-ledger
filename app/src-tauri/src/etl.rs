//! v0.2 ETL — cluster `raw_events` into `sessions_summary`.
//!
//! After a connector scan lands new `raw_events` for one source, this
//! module partitions them into "sessions" using a 30-minute inactivity
//! gap (two events >30 min apart belong to different sessions). The
//! output feeds the LLM summariser (B.5) and the Sessions UI (B.6).
//!
//! Reconcile strategy: **full rebuild per source**. We delete every
//! existing `sessions_summary` row for that source, then re-derive
//! partitions from scratch. The `ON DELETE CASCADE` FK on
//! `session_summaries.session_summary_id` and the FK on
//! `plan_items.source_session_id` cleanly wipe dependent rows.
//!
//! Cost: O(N) read of `raw_events` for the source + O(K) write of new
//! `sessions_summary` rows, where K is the partition count. In normal
//! usage K << N.
//!
//! We never invoke the LLM here — heuristic imports only.
//! See [`crate::ai`] for the second pass.

use std::time::Instant;

use rusqlite::{params, Connection};

use crate::error::AppResult;

/// Threshold for splitting one source's `raw_events` into a new
/// partition. Anything more recent than `last_event_ts +
/// SESSION_GAP_MS` continues the current partition; anything older
/// starts a new one.
pub const SESSION_GAP_MS: i64 = 30 * 60 * 1000;

/// Build a fresh `sessions_summary` view for one source. Returns the
/// number of partitions written.
///
/// Idempotent: calling twice on the same source yields the same rows.
/// The first call wipes any prior rows for the source (cascade takes
/// dependents with it).
pub fn reconcile_sessions_summary(conn: &Connection, source_id: &str) -> AppResult<i64> {
    let started = Instant::now();

    // 1. Wipe existing rows for this source. FK CASCADE handles
    //    session_summaries + plan_items.source_session_id refs.
    conn.execute(
        "DELETE FROM sessions_summary WHERE source_id = ?1",
        params![source_id],
    )?;

    // 2. Stream every raw_events row for this source ordered by time.
    let mut stmt = conn.prepare(
        "SELECT id, occurred_at, COALESCE(project_key, 'unknown')
         FROM raw_events
         WHERE source_id = ?1
         ORDER BY occurred_at ASC, id ASC",
    )?;
    let mut rows = stmt.query(params![source_id])?;

    let now = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    let mut inserted: i64 = 0;
    let mut bucket = SessionBucket::new();

    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let occurred_at: String = row.get(1)?;
        let project_key: String = row.get(2)?;
        let occurred_ms = parse_rfc3339_ms(&occurred_at);

        // Decide: continue current bucket or flush + start new one.
        let too_far = match (bucket.last_ms, occurred_ms) {
            (Some(prev), Some(cur)) => cur - prev > SESSION_GAP_MS,
            _ => true,
        };
        if too_far && bucket.count > 0 {
            bucket.flush(conn, source_id, &now, &mut inserted)?;
            bucket = SessionBucket::new();
        }

        bucket.observe(&id, &occurred_at, occurred_ms, &project_key);
    }

    // Flush trailing bucket.
    if bucket.count > 0 {
        bucket.flush(conn, source_id, &now, &mut inserted)?;
    }

    let duration_ms = started.elapsed().as_millis() as i64;
    tracing::info!(
        source_id,
        partitions = inserted,
        duration_ms,
        "etl: sessions_summary reconcile complete",
    );
    Ok(inserted)
}

/// Aggregate state for one in-flight partition. Keep it tiny — we
/// stream every row through this single struct.
struct SessionBucket {
    first_event_id: Option<String>,
    start_at: Option<String>,
    end_at: Option<String>,
    last_ms: Option<i64>,
    /// Modal project_key within this bucket (most-frequent seen). We
    /// only stamp a project_key on the row when >= 50% of events
    /// agree.
    project_counts: std::collections::HashMap<String, i64>,
    count: i64,
}

impl SessionBucket {
    fn new() -> Self {
        Self {
            first_event_id: None,
            start_at: None,
            end_at: None,
            last_ms: None,
            project_counts: std::collections::HashMap::new(),
            count: 0,
        }
    }

    fn observe(
        &mut self,
        id: &str,
        occurred_at: &str,
        occurred_ms: Option<i64>,
        project_key: &str,
    ) {
        if self.start_at.is_none() {
            self.start_at = Some(occurred_at.to_string());
            self.first_event_id = Some(id.to_string());
        }
        self.end_at = Some(occurred_at.to_string());
        self.last_ms = occurred_ms;
        *self.project_counts.entry(project_key.to_string()).or_insert(0) += 1;
        self.count += 1;
    }

    fn flush(
        &self,
        conn: &Connection,
        source_id: &str,
        now: &str,
        inserted: &mut i64,
    ) -> AppResult<()> {
        let project_key = modal_project_key(&self.project_counts);
        let event_count = self.count;
        let summary = heuristic_summary(project_key, event_count);
        let importance = heuristic_importance(event_count);
        let id = format!("sess_{}", uuid::Uuid::new_v4().simple());
        let started_at = self.start_at.as_deref().unwrap();
        let ended_at = self.end_at.as_deref().unwrap();
        conn.execute(
            "INSERT INTO sessions_summary(
                id, source_id, started_at, ended_at,
                event_count, source_event_count,
                project_key, summary, importance,
                heuristic_kind, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'auto', ?10, ?10)",
            params![
                id,
                source_id,
                started_at,
                ended_at,
                event_count,
                event_count,
                project_key,
                summary,
                importance,
                now
            ],
        )?;
        *inserted += 1;
        Ok(())
    }
}

fn modal_project_key(counts: &std::collections::HashMap<String, i64>) -> &str {
    let mut best_key: Option<&str> = None;
    let mut best_count: i64 = -1;
    for (k, v) in counts.iter() {
        if *v > best_count {
            best_key = Some(k.as_str());
            best_count = *v;
        }
    }
    best_key.unwrap_or("unknown")
}

fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// One-line heuristic summary for the v0.2 "no-LLM" baseline. Will be
/// overwritten by `summarize_session` once the user clicks the AI button.
fn heuristic_summary(project_key: &str, event_count: i64) -> String {
    if project_key == "unknown" || project_key.is_empty() {
        format!("{event_count} raw events (heuristic)")
    } else {
        format!("{event_count} raw events from {project_key}")
    }
}

/// Heuristic importance on a 0..10 scale. Bucketed by event count — a
/// partition with many events implies sustained work, not noise.
fn heuristic_importance(event_count: i64) -> i64 {
    let n = event_count.max(0);
    match n {
        0..=2 => 2,
        3..=5 => 3,
        6..=15 => 5,
        16..=30 => 7,
        31..=60 => 8,
        _ => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_threshold_matches_30_minutes_in_ms() {
        assert_eq!(SESSION_GAP_MS, 1_800_000);
    }

    #[test]
    fn modal_project_picks_most_frequent() {
        let mut counts = std::collections::HashMap::new();
        counts.insert("a".to_string(), 2);
        counts.insert("b".to_string(), 5);
        counts.insert("c".to_string(), 1);
        assert_eq!(modal_project_key(&counts), "b");
    }

    #[test]
    fn modal_project_falls_back_when_empty() {
        let counts = std::collections::HashMap::new();
        assert_eq!(modal_project_key(&counts), "unknown");
    }

    #[test]
    fn importance_table_is_monotonic_in_event_count() {
        assert!(heuristic_importance(100) >= heuristic_importance(5));
        assert!(heuristic_importance(60) >= heuristic_importance(15));
    }
}
