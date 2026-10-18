//! Stage 3 integration: prove the planner module creates tables and
//! default templates via the standard in-memory migrate path.

use rusqlite::Connection;

use agent_work_ledger_lib::migrate_for_tests;
use agent_work_ledger_lib::planner::{
    self, PlanItemStatus, UpsertItemInput,
};

fn open_mem_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrate_for_tests(&conn).unwrap();
    conn
}

#[test]
fn migrate_creates_planner_tables_and_default_templates() {
    let conn = open_mem_db();
    let tmpls = planner::list_templates(&conn).unwrap();
    assert!(!tmpls.is_empty(), "default templates should be seeded");
    let names: Vec<&str> = tmpls.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"Daily Stand-up"));
    assert!(names.contains(&"Weekly Review"));

    let n_templates: i64 = conn
        .query_row("SELECT COUNT(*) FROM plan_templates", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n_templates, 2);
}

#[test]
fn cascade_delete_removes_items_with_template() {
    let conn = open_mem_db();
    let tmpl = planner::list_templates(&conn).unwrap().remove(0);
    for title in ["fix prod", "review design", "tidy desk"] {
        planner::upsert_item(
            &conn,
            UpsertItemInput {
                id: None,
                template_id: tmpl.id.clone(),
                quadrant: 1,
                title: title.into(),
                description: "".into(),
                status: None,
                priority: None,
                source_event_id: None,
                ai_reasoning: None,
            },
        )
        .unwrap();
    }
    let items = planner::list_items(&conn, &tmpl.id).unwrap();
    assert_eq!(items.len(), 3);
    planner::delete_template(&conn, &tmpl.id).unwrap();
    let items_after = planner::list_items(&conn, &tmpl.id).unwrap();
    assert!(items_after.is_empty());
}

#[test]
fn completed_status_records_completed_at() {
    let conn = open_mem_db();
    let tmpl = planner::list_templates(&conn).unwrap().remove(0);
    let created = planner::upsert_item(
        &conn,
        UpsertItemInput {
            id: None,
            template_id: tmpl.id.clone(),
            quadrant: 2,
            title: "review the architecture plan".into(),
            description: "low-urgency deep work".into(),
            status: Some("todo".into()),
            priority: None,
            source_event_id: None,
            ai_reasoning: None,
        },
    )
    .unwrap();
    assert!(created.completed_at.is_none());
    let done = planner::upsert_item(
        &conn,
        UpsertItemInput {
            id: Some(created.id.clone()),
            template_id: tmpl.id.clone(),
            quadrant: 2,
            title: created.title.clone(),
            description: created.description.clone(),
            status: Some(PlanItemStatus::Done.as_str().into()),
            priority: None,
            source_event_id: None,
            ai_reasoning: None,
        },
    )
    .unwrap();
    assert!(done.completed_at.is_some());
}