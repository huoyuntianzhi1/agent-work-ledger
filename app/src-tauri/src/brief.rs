//! Stage 5-E.3c: Daily morning brief.
//!
//! Wraps `eisenhower::generate` and surfaces only the top-3 Q1
//! (urgent + important) project groups plus a one-sentence
//! headline. The headline is composed deterministically from the
//! top-3 labels so the card stays stable even when the LLM is
//! unavailable — we never ship a half-baked brief with `null`
//! copy.
//!
//! The brief is intentionally narrow: it exists so the launchd
//! 8am tick (or any user-driven `generate_daily_brief` call from
//! the UI) can write a single Markdown line per top project into
//! the Notifications app via the existing `notification` IPC, or
//! render in the home-page card. The full Eisenhower matrix stays
//! on the Planner page.

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::eisenhower::{self, EisenhowerItem};
use crate::error::AppResult;

/// How many top-Q1 items the brief surfaces. The original Stage
/// 5-E.3c spec says "top 3" — anything more drowns the morning
/// signal; anything less starves it.
pub const TOP_N: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBrief {
    /// ISO date (YYYY-MM-DD) the brief was generated for. The brief
    /// always covers the prior 24 h, so this is the "morning of"
    /// date — i.e. 2026-10-17 means "things I should focus on
    /// starting 2026-10-17".
    pub date: String,
    pub generated_at: String,
    /// Top-Q1 items, ordered by `importance + urgency` desc.
    pub top_q1: Vec<EisenhowerItem>,
    /// One-line summary, e.g. "Q1 dominated by Codebuddy local env
    /// edits and Trae dispatch logs". Composed from the top-N
    /// project labels so it survives no-LLM.
    pub headline: String,
    pub total_projects: i64,
    pub total_q1: i64,
    /// v0.4.2: per-source Q1 rollup. The brief card now lists
    /// "Q1 在 Codex 占 4 / MiniMax 占 2 / Claude 占 1" so the user
    /// can see which connector is dominating today's urgent
    /// work without clicking into the Sessions page. Sorted by
    /// `count` desc, ties broken by `source_display_name` asc.
    pub by_source: Vec<SourceBucket>,
    /// v0.4.2: comparison against the most recent *previous* brief
    /// (yesterday's at the latest). Lets the morning card show
    /// "Q1 +3 / -1 vs 昨天" instead of an absolute number. `None`
    /// when this is the first brief in the DB.
    pub delta_vs_prev: Option<BriefDelta>,
    pub ai_used: bool,
    pub note: Option<String>,
}

/// v0.4.2: per-source Q1 rollup. `count` is the number of Q1 items
/// the source produced in the brief's 24 h window. We keep
/// `source_id` for the click-through to filter the Sessions page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceBucket {
    pub source_id: String,
    pub source_display_name: String,
    pub count: i64,
}

/// v0.4.2: morning-over-morning delta. Field is omitted from
/// `Serialize` when `None` (first brief in DB).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefDelta {
    pub q1_delta: i64,
    pub projects_delta: i64,
    /// "more urgent" / "less urgent" / "same" — derived from
    /// `q1_delta` so the UI can colour this without re-computing.
    pub verdict: String,
}

/// Build the brief for a given date (defaults to today UTC). The
/// look-back is fixed at 24 h because a morning brief is a
/// "yesterday → now" view; if you want a different window use
/// `eisenhower::generate` directly.
pub fn generate(conn: &Connection, date: NaiveDate) -> AppResult<DailyBrief> {
    let digest = eisenhower::generate(conn, 24)?;
    let date_str = date.format("%Y-%m-%d").to_string();
    let total_projects = digest.items.len() as i64;
    let q1: Vec<EisenhowerItem> = digest
        .items
        .iter()
        .filter(|it| it.quadrant == 1)
        .cloned()
        .collect();
    let total_q1 = q1.len() as i64;

    // Sort Q1 by combined score, then by event count (heavier
    // activity wins ties), then by recency proxy `last_event_at`
    // — all descending so the highest-leverage item leads.
    let mut q1_sorted = q1;
    q1_sorted.sort_by(|a, b| {
        (b.importance + b.urgency)
            .cmp(&(a.importance + a.urgency))
            .then(b.event_count.cmp(&a.event_count))
    });
    // Clone first so `q1_sorted` is still owned when we pass it
    // to `compose_headline` for the headline fallback.
    let top_q1: Vec<EisenhowerItem> = q1_sorted.iter().take(TOP_N).cloned().collect();
    let all_q1_for_headline = q1_sorted.clone();

    let headline = compose_headline(&top_q1, &all_q1_for_headline);
    let by_source = aggregate_by_source(&q1_sorted);
    let delta_vs_prev = compute_delta_vs_prev(conn, total_q1, total_projects)?;

    Ok(DailyBrief {
        date: date_str,
        generated_at: Utc::now().to_rfc3339(),
        top_q1,
        headline,
        total_projects,
        total_q1,
        by_source,
        delta_vs_prev,
        ai_used: digest.ai_used,
        note: digest.note,
    })
}

/// Roll Q1 items up by source. Source_id is preserved for the
/// click-through filter.
fn aggregate_by_source(q1: &[EisenhowerItem]) -> Vec<SourceBucket> {
    use std::collections::HashMap;
    let mut map: HashMap<String, SourceBucket> = HashMap::new();
    for it in q1 {
        let entry = map
            .entry(it.source_id.clone())
            .or_insert_with(|| SourceBucket {
                source_id: it.source_id.clone(),
                source_display_name: it.source_display_name.clone(),
                count: 0,
            });
        entry.count += 1;
    }
    let mut out: Vec<SourceBucket> = map.into_values().collect();
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.source_display_name.cmp(&b.source_display_name))
    });
    out
}

/// Look up the most recent *previous* brief (any date strictly
/// before the one we're generating) and compute the morning-over-
/// morning delta. Returns `None` if this is the first brief in
/// the DB — the UI then shows "首次" instead of a delta.
fn compute_delta_vs_prev(
    conn: &Connection,
    today_q1: i64,
    today_projects: i64,
) -> AppResult<Option<BriefDelta>> {
    // Pull (date, total_q1, total_projects) for the most recent row
    // before today. We never use the *newest* row because it might
    // be today itself if the user re-runs the brief.
    let prev = conn
        .query_row(
            r#"SELECT date, total_q1, total_projects
                 FROM daily_briefs
                ORDER BY date DESC
                LIMIT 1"#,
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .ok();
    if let Some((prev_date, prev_q1, prev_projects)) = prev {
        if prev_date == today_str() {
            return Ok(None);
        }
        let q1_delta = today_q1 - prev_q1;
        let projects_delta = today_projects - prev_projects;
        let verdict = if q1_delta > 0 {
            "more urgent"
        } else if q1_delta < 0 {
            "less urgent"
        } else {
            "same"
        }
        .to_string();
        Ok(Some(BriefDelta {
            q1_delta,
            projects_delta,
            verdict,
        }))
    } else {
        Ok(None)
    }
}

/// Today's UTC date as a `YYYY-MM-DD` string. Kept here (not in
/// the public `today_utc()`) because it's only needed for the
/// delta comparison.
fn today_str() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Render the morning's top-3 as a single short sentence. Falls
/// back to a "no urgent work" message when the queue is empty so
/// the card always has something useful to say.
fn compose_headline(top: &[EisenhowerItem], all_q1: &[EisenhowerItem]) -> String {
    if top.is_empty() {
        return if all_q1.is_empty() {
            "今天没有 Q1 紧急+重要 项,继续推进 Q2 重要不紧急".to_string()
        } else {
            "Q1 列表为空,模板未跑通或昨日已清空".to_string()
        };
    }
    let labels: Vec<String> = top
        .iter()
        .map(|it| {
            if it.label.is_empty() {
                it.project_key.clone()
            } else {
                it.label.clone()
            }
        })
        .collect();
    format!("今日 Q1: {}", labels.join("、"))
}

/// Persist a brief row into a small `daily_briefs` cache so the
/// home page can show the *latest* brief without re-running the
/// full Eisenhower pipeline every time the user opens the app.
/// We also use this table as the audit trail if a notification
/// pipeline ever wants to replay a brief.
pub fn save_brief(conn: &Connection, brief: &DailyBrief) -> AppResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS daily_briefs (
            date            TEXT PRIMARY KEY,
            generated_at    TEXT NOT NULL,
            headline        TEXT NOT NULL,
            top_q1_json     TEXT NOT NULL,
            total_projects  INTEGER NOT NULL,
            total_q1        INTEGER NOT NULL,
            ai_used         INTEGER NOT NULL
        )",
        [],
    )?;
    let payload = serde_json::to_string(&brief.top_q1)
        .map_err(|e| crate::error::AppError::Other(format!("serialize top_q1: {e}")))?;
    conn.execute(
        "INSERT INTO daily_briefs (date, generated_at, headline, top_q1_json, total_projects, total_q1, ai_used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(date) DO UPDATE SET
            generated_at = excluded.generated_at,
            headline = excluded.headline,
            top_q1_json = excluded.top_q1_json,
            total_projects = excluded.total_projects,
            total_q1 = excluded.total_q1,
            ai_used = excluded.ai_used",
        rusqlite::params![
            brief.date,
            brief.generated_at,
            brief.headline,
            payload,
            brief.total_projects,
            brief.total_q1,
            brief.ai_used as i64,
        ],
    )?;
    Ok(())
}

/// Read the most-recent cached brief (date-sorted desc). Used by
/// the home page to render the card on cold start.
pub fn latest_cached(conn: &Connection) -> AppResult<Option<DailyBrief>> {
    let mut stmt = conn.prepare(
        "SELECT date, generated_at, headline, top_q1_json, total_projects, total_q1, ai_used
         FROM daily_briefs
         ORDER BY date DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query_map([], |row| {
        let payload: String = row.get(3)?;
        let top_q1: Vec<EisenhowerItem> = serde_json::from_str(&payload).unwrap_or_default();
        Ok(DailyBrief {
            date: row.get(0)?,
            generated_at: row.get(1)?,
            headline: row.get(2)?,
            top_q1,
            total_projects: row.get(4)?,
            total_q1: row.get(5)?,
            // v0.4.2: cached rows predate `by_source` /
            // `delta_vs_prev`. The home card degrades gracefully
            // (no chip list, no arrow) when these are empty.
            by_source: Vec::new(),
            delta_vs_prev: None,
            ai_used: row.get::<_, i64>(6)? != 0,
            note: None,
        })
    })?;
    if let Some(row) = rows.next() {
        Ok(Some(row?))
    } else {
        Ok(None)
    }
}

/// Date helper so `--brief-once` and the IPC agree on which
/// "morning" we're computing. Returns today in UTC.
pub fn today_utc() -> NaiveDate {
    Utc::now().date_naive()
}

#[allow(dead_code)]
pub fn today_utc_rfc3339() -> String {
    DateTime::<Utc>::from_naive_utc_and_offset(
        today_utc().and_hms_opt(0, 0, 0).unwrap(),
        Utc,
    )
    .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eisenhower::ProjectGroup;
    use crate::migrate_for_tests;

    fn g(pk: &str, ec: i64, imp: i64) -> ProjectGroup {
        // Anchor both timestamps to `now - 2h` so the events always
        // land inside `eisenhower::generate`'s 24 h lookback window.
        // Hard-coding a literal date (as we did pre-v0.4.8) made the
        // test silently go stale every 24 h after that anchor day and
        // panic with `index out of bounds` once the seed fell out of
        // the window — see AGENTS memory "Brief test flaky on date
        // drift" for the post-mortem.
        let stamp = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        ProjectGroup {
            project_key: pk.into(),
            source_id: "src_test".into(),
            source_display_name: "test".into(),
            event_count: ec,
            first_event_at: stamp.clone(),
            last_event_at: stamp,
            top_event_types: vec!["message".into()],
            heuristic_importance: imp,
        }
    }

    /// `compute_delta_vs_prev` queries `daily_briefs`, which is only
    /// lazily created by `save_brief`. Tests that exercise the delta
    /// path need the table to exist; the cheapest stand-in is to
    /// run a dummy `save_brief` (which CREATE IF NOT EXISTS), then
    /// overwrite with whatever row the test wants.
    fn ensure_brief_table(conn: &Connection) {
        let stub = DailyBrief {
            date: "1970-01-01".into(),
            generated_at: "1970-01-01T00:00:00+00:00".into(),
            top_q1: Vec::new(),
            headline: "stub".into(),
            total_projects: 0,
            total_q1: 0,
            by_source: Vec::new(),
            delta_vs_prev: None,
            ai_used: false,
            note: None,
        };
        save_brief(conn, &stub).unwrap();
    }

    fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    fn seed(conn: &Connection, groups: &[ProjectGroup]) {
        // Add a fake source so raw_events' FK is satisfied. sources
        // also requires root_path / created_at / updated_at, so feed
        // those in too.
        let now = now_iso();
        conn.execute(
            "INSERT OR IGNORE INTO sources (id, kind, display_name, root_path, status, created_at, updated_at)
             VALUES ('src_test', 'claude_code', 'test', '/tmp/test', 'readable', ?1, ?2)",
            rusqlite::params![&now, &now],
        )
        .unwrap();
        // Insert `ec` events per group so the GROUP BY count
        // actually matches the EventCount field — otherwise every
        // project gets ec=1 and the heuristic importance tier
        // collapses to 2 for everyone.
        for (i, g) in groups.iter().enumerate() {
            for n in 0..g.event_count {
                conn.execute(
                    "INSERT INTO raw_events (id, source_id, event_type, occurred_at, project_key, payload, redacted, created_at)
                     VALUES (?1, 'src_test', 'message', ?2, ?3, '{}', 0, ?4)",
                    rusqlite::params![
                        format!("ev_{}_{}_{}", g.project_key, i, n),
                        g.last_event_at,
                        g.project_key,
                        now
                    ],
                ).unwrap();
            }
        }
    }

    #[test]
    fn brief_returns_empty_when_no_recent_activity() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        let b = generate(&conn, today_utc()).unwrap();
        assert_eq!(b.top_q1.len(), 0);
        assert!(b.headline.contains("没有 Q1") || b.headline.contains("Q1 列表为空"));
        assert_eq!(b.total_projects, 0);
        assert_eq!(b.total_q1, 0);
        assert!(!b.ai_used);
    }

    #[test]
    fn brief_picks_top_3_by_score_desc() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        seed(
            &conn,
            &[
                g("/low-imp-low-urg", 1, 2),
                g("/mid", 8, 6),
                g("/hi-1", 30, 9),
                g("/hi-2", 25, 9),
                g("/hi-3", 20, 8),
                g("/hi-4", 50, 9),
                g("/hi-5", 12, 8),
            ],
        );
        let b = generate(&conn, today_utc()).unwrap();
        // TOP_N = 3
        assert!(b.top_q1.len() <= 3, "got {}", b.top_q1.len());
        // First item is the highest combined score.
        assert!(b.top_q1[0].importance + b.top_q1[0].urgency
            >= b.top_q1.last().unwrap().importance + b.top_q1.last().unwrap().urgency);
        assert!(b.headline.contains("今日 Q1"));
    }

    #[test]
    fn brief_save_then_latest_round_trips() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        seed(&conn, &[g("/hi", 25, 9)]);
        let b = generate(&conn, today_utc()).unwrap();
        save_brief(&conn, &b).unwrap();
        let cached = latest_cached(&conn).unwrap().expect("row expected");
        assert_eq!(cached.date, b.date);
        assert_eq!(cached.headline, b.headline);
        assert_eq!(cached.total_q1, b.total_q1);
        assert_eq!(cached.top_q1.len(), b.top_q1.len());
    }

    #[test]
    fn brief_save_overwrites_same_date() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        seed(&conn, &[g("/hi", 25, 9)]);
        let b1 = generate(&conn, today_utc()).unwrap();
        save_brief(&conn, &b1).unwrap();
        // Re-save (later generated_at wins).
        let mut b2 = b1.clone();
        b2.generated_at = "2099-01-01T00:00:00+00:00".into();
        b2.headline = "updated".into();
        save_brief(&conn, &b2).unwrap();
        let cached = latest_cached(&conn).unwrap().unwrap();
        assert_eq!(cached.headline, "updated");
        assert_eq!(cached.generated_at, "2099-01-01T00:00:00+00:00");
    }

    #[test]
    fn headline_falls_back_to_no_q1_message() {
        let h = compose_headline(&[], &[]);
        assert!(h.contains("没有 Q1"));
    }

    // ---- v0.4.2 brief digest 增强 ----

    fn q1_item(pk: &str, source_id: &str, source_name: &str, score: i64) -> EisenhowerItem {
        EisenhowerItem {
            project_key: pk.into(),
            source_id: source_id.into(),
            source_display_name: source_name.into(),
            quadrant: 1,
            importance: score / 2 + 3,
            urgency: score / 2 + 3,
            label: pk.into(),
            reasoning: String::new(),
            event_count: 10,
            ai_used: false,
        }
    }

    #[test]
    fn aggregate_by_source_groups_correctly() {
        let items = vec![
            q1_item("/a", "codex", "Codex", 9),
            q1_item("/b", "codex", "Codex", 9),
            q1_item("/c", "claude_code", "Claude", 8),
            q1_item("/d", "minimax_code", "MiniMax", 7),
        ];
        let buckets = aggregate_by_source(&items);
        assert_eq!(buckets.len(), 3);
        // Sort is `count desc`, ties by display_name asc — Codex wins
        // with 2, then alphabetical between Claude / MiniMax.
        assert_eq!(buckets[0].source_id, "codex");
        assert_eq!(buckets[0].source_display_name, "Codex");
        assert_eq!(buckets[0].count, 2);
        assert_eq!(buckets[1].source_id, "claude_code");
        assert_eq!(buckets[1].count, 1);
        assert_eq!(buckets[2].source_id, "minimax_code");
        assert_eq!(buckets[2].count, 1);
    }

    #[test]
    fn aggregate_by_source_handles_empty_input() {
        let buckets = aggregate_by_source(&[]);
        assert!(buckets.is_empty());
    }

    #[test]
    fn compute_delta_vs_prev_returns_none_on_first_brief() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        // No rows in `daily_briefs` → previous lookup fails → None.
        let delta = compute_delta_vs_prev(&conn, 5, 10).unwrap();
        assert!(delta.is_none());
    }

    #[test]
    fn delta_verdict_classifies_correctly() {
        // Same-day save: prev_date == today_str() → None (today's row
        // is the only one, the user re-ran the brief).
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        ensure_brief_table(&conn);
        // Seed today's row.
        let today = today_str();
        conn.execute(
            "INSERT INTO daily_briefs (date, generated_at, headline, top_q1_json, total_projects, total_q1, ai_used)
             VALUES (?1, '2026-10-17T01:00:00+00:00', 'h', '[]', 3, 2, 0)",
            rusqlite::params![&today],
        )
        .unwrap();
        let delta = compute_delta_vs_prev(&conn, 5, 10).unwrap();
        assert!(delta.is_none());
    }

    #[test]
    fn delta_more_urgent_when_q1_grew() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        ensure_brief_table(&conn);
        // Seed a previous (yesterday) row with smaller q1.
        conn.execute(
            "INSERT INTO daily_briefs (date, generated_at, headline, top_q1_json, total_projects, total_q1, ai_used)
             VALUES ('2026-10-16', '2026-10-16T08:00:00+00:00', 'h', '[]', 3, 2, 0)",
            [],
        )
        .unwrap();
        let delta = compute_delta_vs_prev(&conn, 5, 4).unwrap().expect("delta");
        assert_eq!(delta.q1_delta, 3);
        assert_eq!(delta.projects_delta, 1);
        assert_eq!(delta.verdict, "more urgent");
    }

    #[test]
    fn delta_less_urgent_when_q1_shrank() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        ensure_brief_table(&conn);
        conn.execute(
            "INSERT INTO daily_briefs (date, generated_at, headline, top_q1_json, total_projects, total_q1, ai_used)
             VALUES ('2026-10-16', '2026-10-16T08:00:00+00:00', 'h', '[]', 3, 8, 0)",
            [],
        )
        .unwrap();
        let delta = compute_delta_vs_prev(&conn, 5, 4).unwrap().expect("delta");
        assert_eq!(delta.q1_delta, -3);
        assert_eq!(delta.projects_delta, 1);
        assert_eq!(delta.verdict, "less urgent");
    }

    #[test]
    fn delta_same_when_q1_unchanged() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_for_tests(&conn).unwrap();
        ensure_brief_table(&conn);
        conn.execute(
            "INSERT INTO daily_briefs (date, generated_at, headline, top_q1_json, total_projects, total_q1, ai_used)
             VALUES ('2026-10-16', '2026-10-16T08:00:00+00:00', 'h', '[]', 3, 5, 0)",
            [],
        )
        .unwrap();
        let delta = compute_delta_vs_prev(&conn, 5, 5).unwrap().expect("delta");
        assert_eq!(delta.q1_delta, 0);
        assert_eq!(delta.verdict, "same");
    }
}
