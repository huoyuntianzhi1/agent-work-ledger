//! Stage 3: Eisenhower 4-quadrant planner + day/week templates.
//!
//! Data model:
//! - `plan_templates` is a named plan shell ("Daily Stand-up", "Weekly
//!   Review", …). `kind` is one of `'day'` or `'week'` and is purely
//!   informational — the matrix does not differ by template kind.
//! - `plan_items` are the actual tasks; each carries one of four
//!   quadrants, a status, an optional `source_event_id` link back to
//!   `raw_events` (so a task can be promoted from an observed agent
//!   action), and an `ai_reasoning` field populated by the optional
//!   local Ollama classifier.
//!
//! Quadrant numbering:
//!   1 = Urgent + Important       (Do first)
//!   2 = Not urgent + Important   (Schedule)
//!   3 = Urgent + Not important   (Delegate / quick win)
//!   4 = Not urgent + Not important (Drop / park)

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppResult;

/// Quadrant enum kept as a `u8` so we can store it directly without a
/// mapping table. Matches the Eisenhower matrix naming most productivity
/// literature uses.
pub const Q1_URGENT_IMPORTANT: i64 = 1;
pub const Q2_NOT_URGENT_IMPORTANT: i64 = 2;
pub const Q3_URGENT_NOT_IMPORTANT: i64 = 3;
pub const Q4_NEITHER: i64 = 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlanItemStatus {
    Todo,
    InProgress,
    Done,
}

impl PlanItemStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanItemStatus::Todo => "todo",
            PlanItemStatus::InProgress => "in_progress",
            PlanItemStatus::Done => "done",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "in_progress" => PlanItemStatus::InProgress,
            "done" => PlanItemStatus::Done,
            _ => PlanItemStatus::Todo,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTemplate {
    pub id: String,
    pub name: String,
    pub kind: String, // "day" | "week"
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub template_id: String,
    pub quadrant: i64,
    pub title: String,
    pub description: String,
    pub status: String, // "todo" | "in_progress" | "done"
    pub priority: i64,
    pub source_event_id: Option<String>,
    pub ai_reasoning: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

/// Used by the IPC layer to return a freshly-classified suggestion that
/// the UI can either accept or override. `reasoning` is the raw Ollama
/// explanation so the user can see *why* the local model put the item
/// where it did (helps when the classification is wrong).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuadrantSuggestion {
    pub quadrant: i64,
    pub reasoning: String,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

// ---------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------
pub fn list_templates(conn: &Connection) -> AppResult<Vec<PlanTemplate>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, created_at FROM plan_templates ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PlanTemplate {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn create_template(conn: &Connection, name: &str, kind: &str) -> AppResult<PlanTemplate> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(crate::error::AppError::Other(
            "template name must not be empty".into(),
        ));
    }
    let valid_kind = matches!(kind, "day" | "week");
    let safe_kind = if valid_kind { kind } else { "day" };
    let id = new_id("tmpl");
    let now = now_iso();
    conn.execute(
        "INSERT INTO plan_templates (id, name, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, trimmed, safe_kind, now],
    )?;
    Ok(PlanTemplate {
        id,
        name: trimmed.to_string(),
        kind: safe_kind.to_string(),
        created_at: now,
    })
}

pub fn delete_template(conn: &Connection, id: &str) -> AppResult<i64> {
    // ON DELETE CASCADE on plan_items.template_id handles the children.
    let n = conn.execute("DELETE FROM plan_templates WHERE id = ?1", params![id])?;
    Ok(n as i64)
}

/// Seed the two starter templates if the user has none yet. Idempotent.
pub fn ensure_default_templates(conn: &Connection) -> AppResult<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM plan_templates",
        [],
        |row| row.get(0),
    )?;
    if count == 0 {
        let _ = create_template(conn, "Daily Stand-up", "day")?;
        let _ = create_template(conn, "Weekly Review", "week")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------
pub fn list_items(conn: &Connection, template_id: &str) -> AppResult<Vec<PlanItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, template_id, quadrant, title, description, status, priority, source_event_id,
                ai_reasoning, created_at, updated_at, completed_at
         FROM plan_items
         WHERE template_id = ?1
         ORDER BY quadrant ASC, created_at ASC",
    )?;
    let rows = stmt.query_map(params![template_id], |row| {
        Ok(PlanItem {
            id: row.get(0)?,
            template_id: row.get(1)?,
            quadrant: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            status: row.get(5)?,
            priority: row.get(6)?,
            source_event_id: row.get(7)?,
            ai_reasoning: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
            completed_at: row.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertItemInput {
    pub id: Option<String>,
    pub template_id: String,
    pub quadrant: i64,
    pub title: String,
    pub description: String,
    pub status: Option<String>,
    pub priority: Option<i64>,
    pub source_event_id: Option<String>,
    pub ai_reasoning: Option<String>,
}

pub fn upsert_item(conn: &Connection, input: UpsertItemInput) -> AppResult<PlanItem> {
    if input.title.trim().is_empty() {
        return Err(crate::error::AppError::Other("title must not be empty".into()));
    }
    let q = input.quadrant.clamp(1, 4);
    let status = PlanItemStatus::from_str(input.status.as_deref().unwrap_or("todo"));
    let priority = input.priority.unwrap_or(0).clamp(0, 3);
    let now = now_iso();
    let completed_at = if matches!(status, PlanItemStatus::Done) {
        Some(now.clone())
    } else {
        None
    };
    let id = match input.id.as_deref() {
        Some(existing) => {
            conn.execute(
                "UPDATE plan_items
                 SET title = ?1, description = ?2, quadrant = ?3, status = ?4,
                     priority = ?5, source_event_id = ?6, ai_reasoning = ?7,
                     updated_at = ?8, completed_at = ?9
                 WHERE id = ?10",
                params![
                    input.title.trim(),
                    input.description,
                    q,
                    status.as_str(),
                    priority,
                    input.source_event_id,
                    input.ai_reasoning,
                    now,
                    completed_at,
                    existing
                ],
            )?;
            existing.to_string()
        }
        None => {
            let new_id = new_id("item");
            conn.execute(
                "INSERT INTO plan_items
                  (id, template_id, quadrant, title, description, status, priority,
                   source_event_id, ai_reasoning, created_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
                params![
                    new_id,
                    input.template_id,
                    q,
                    input.title.trim(),
                    input.description,
                    status.as_str(),
                    priority,
                    input.source_event_id,
                    input.ai_reasoning,
                    now,
                    completed_at
                ],
            )?;
            new_id
        }
    };
    read_item(conn, &id)
}

fn read_item(conn: &Connection, id: &str) -> AppResult<PlanItem> {
    let mut stmt = conn.prepare(
        "SELECT id, template_id, quadrant, title, description, status, priority, source_event_id,
                ai_reasoning, created_at, updated_at, completed_at
         FROM plan_items WHERE id = ?1",
    )?;
    let item = stmt.query_row(params![id], |row| {
        Ok(PlanItem {
            id: row.get(0)?,
            template_id: row.get(1)?,
            quadrant: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            status: row.get(5)?,
            priority: row.get(6)?,
            source_event_id: row.get(7)?,
            ai_reasoning: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
            completed_at: row.get(11)?,
        })
    })?;
    Ok(item)
}

pub fn delete_item(conn: &Connection, id: &str) -> AppResult<i64> {
    let n = conn.execute("DELETE FROM plan_items WHERE id = ?1", params![id])?;
    Ok(n as i64)
}

// ---------------------------------------------------------------------
// Local Ollama classifier
// ---------------------------------------------------------------------
pub fn suggest_quadrant(conn: &Connection, text: &str) -> AppResult<QuadrantSuggestion> {
    let settings = crate::ai::load_settings();
    // No local daemon AND no cloud key = nothing to call; heuristic only.
    if settings.provider == crate::ai::Provider::Local
        && !settings.local_endpoint.starts_with("http://")
    {
        return Ok(default_quadrant(text));
    }
    if settings.provider != crate::ai::Provider::Local && settings.api_key.is_empty() {
        return Ok(default_quadrant(text));
    }
    let prompt = format!(
        "You classify a single work item into Eisenhower quadrants.\n\
         Quadrant 1 = urgent AND important (Do first, today, blocking others)\n\
         Quadrant 2 = important but NOT urgent (Schedule, deep work, long-term)\n\
         Quadrant 3 = urgent but NOT important (Delegate, quick win, noise)\n\
         Quadrant 4 = neither (Drop, hobby, low value)\n\n\
         Reply in this exact format with no extra prose:\n\
         QUADRANT: <1|2|3|4>\nREASONING: <one short sentence>\n\n\
         Item: {text}"
    );
    let report = crate::ai::generate(&settings, &prompt, false);
    if report.ok {
        if let Some(parsed) = parse_quadrant_response(&report.response) {
            return Ok(parsed);
        }
    }
    if let Some(err) = report.error.as_deref() {
        tracing::warn!(error = %err, "ai quadrant suggestion failed, falling back to heuristic");
    }
    Ok(default_quadrant(text))
}

fn parse_quadrant_response(raw: &str) -> Option<QuadrantSuggestion> {
    let mut quadrant = None;
    let mut reasoning = String::new();
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("QUADRANT:") {
            quadrant = rest.trim().chars().next().and_then(|c| c.to_digit(10)).map(|q| q as i64);
        } else if let Some(rest) = t.strip_prefix("REASONING:") {
            reasoning = rest.trim().to_string();
        }
    }
    quadrant.map(|q| QuadrantSuggestion {
        quadrant: q.clamp(1, 4),
        reasoning,
    })
}

/// Fallback heuristic: looks for time-urgency keywords. Conservative —
/// unknown items go to Q4 (neither), forcing the user to consciously
/// promote them rather than accidentally scheduling noise.
fn default_quadrant(text: &str) -> QuadrantSuggestion {
    let lower = text.to_lowercase();
    let urgent = ["asap", "today", "now", "deadline tomorrow", "blocker", "fix", "outage", "fire", "broken", "critical", "p0", "p1", "紧急", "今天", "马上", "阻塞", "故障", "宕机"];
    let important = ["plan", "design", "research", "review", "学习", "规划", "调研", "复盘", "战略", "长期"];
    let quick = ["reply", "rsvp", "schedule meeting", "回邮", "约会议"];
    if urgent.iter().any(|k| lower.contains(k)) {
        QuadrantSuggestion {
            quadrant: 1,
            reasoning: "matched an urgency keyword".into(),
        }
    } else if important.iter().any(|k| lower.contains(k)) {
        QuadrantSuggestion {
            quadrant: 2,
            reasoning: "matched an importance keyword".into(),
        }
    } else if quick.iter().any(|k| lower.contains(k)) {
        QuadrantSuggestion {
            quadrant: 3,
            reasoning: "matched a quick-win keyword".into(),
        }
    } else {
        QuadrantSuggestion {
            quadrant: 4,
            reasoning: "no urgency/importance cue detected — defaulted to drop-or-park".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        awl_db::migrate_for_tests(&conn).unwrap();
        conn
    }

    #[test]
    fn creates_two_default_templates_on_empty_db() {
        let conn = open_test_db();
        ensure_default_templates(&conn).unwrap();
        let tmpls = list_templates(&conn).unwrap();
        assert_eq!(tmpls.len(), 2);
        assert!(tmpls.iter().any(|t| t.name == "Daily Stand-up"));
        assert!(tmpls.iter().any(|t| t.name == "Weekly Review"));
    }

    #[test]
    fn ensure_default_templates_is_idempotent() {
        let conn = open_test_db();
        ensure_default_templates(&conn).unwrap();
        ensure_default_templates(&conn).unwrap();
        let tmpls = list_templates(&conn).unwrap();
        assert_eq!(tmpls.len(), 2);
    }

    #[test]
    fn upsert_item_inserts_then_updates() {
        let conn = open_test_db();
        ensure_default_templates(&conn).unwrap();
        let tmpl = list_templates(&conn).unwrap().remove(0);
        let created = upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: 1,
                title: "Fix prod outage".into(),
                description: "p0 incident".into(),
                status: Some("todo".into()),
                priority: Some(2),
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
        assert_eq!(created.title, "Fix prod outage");
        assert_eq!(created.quadrant, 1);

        let updated = upsert_item(
            &conn,
            UpsertItemInput {
                id: Some(created.id.clone()),
                template_id: tmpl.id.clone(),
                quadrant: 2,
                title: "Fix prod outage".into(),
                description: "p0 incident — escalate".into(),
                status: Some("in_progress".into()),
                priority: Some(3),
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
        assert_eq!(updated.quadrant, 2);
        assert_eq!(updated.status, "in_progress");
        assert_eq!(updated.priority, 3);
    }

    #[test]
    fn upsert_rejects_empty_title() {
        let conn = open_test_db();
        ensure_default_templates(&conn).unwrap();
        let tmpl = list_templates(&conn).unwrap().remove(0);
        let err = upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: 1,
                title: "   ".into(),
                description: "".into(),
                status: None,
                priority: None,
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap_err();
        assert!(format!("{err}").contains("title"));
    }

    #[test]
    fn delete_item_removes_only_target() {
        let conn = open_test_db();
        ensure_default_templates(&conn).unwrap();
        let tmpl = list_templates(&conn).unwrap().remove(0);
        let a = upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: 1,
                title: "a".into(),
                description: "".into(),
                status: None,
                priority: None,
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
        let b = upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: 2,
                title: "b".into(),
                description: "".into(),
                status: None,
                priority: None,
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
        assert_eq!(delete_item(&conn, &a.id).unwrap(), 1);
        assert_eq!(delete_item(&conn, &a.id).unwrap(), 0);
        assert_eq!(list_items(&conn, &tmpl.id).unwrap().len(), 1);
        let _ = b;
    }

    #[test]
    fn default_quadrant_classifies_obvious_urgency() {
        let s = default_quadrant("fix the prod outage ASAP");
        assert_eq!(s.quadrant, 1);
        let s = default_quadrant("design the next-quarter architecture review");
        assert_eq!(s.quadrant, 2);
        let s = default_quadrant("reply to vendor RSVP");
        assert_eq!(s.quadrant, 3);
        let s = default_quadrant("tidy my desk drawer");
        assert_eq!(s.quadrant, 4);
    }

    #[test]
    fn parse_quadrant_response_extracts_clean() {
        let raw = "QUADRANT: 2\nREASONING: long-term planning, low urgency";
        let s = parse_quadrant_response(raw).unwrap();
        assert_eq!(s.quadrant, 2);
        assert!(s.reasoning.contains("planning"));
    }

    #[test]
    fn quadrant_clamped_to_1_through_4() {
        let conn = open_test_db();
        ensure_default_templates(&conn).unwrap();
        let tmpl = list_templates(&conn).unwrap().remove(0);
        let a = upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: 99,
                title: "wildly off-range".into(),
                description: "".into(),
                status: None,
                priority: None,
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
        assert_eq!(a.quadrant, 4);
        let b = upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: -7,
                title: "negative".into(),
                description: "".into(),
                status: None,
                priority: None,
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
        assert_eq!(b.quadrant, 1);
    }
}