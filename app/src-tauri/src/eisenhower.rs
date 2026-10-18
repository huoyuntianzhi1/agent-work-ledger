//! Stage 5-E.3a: Eisenhower 4-quadrant batch digest.
//!
//! Reads `raw_events` grouped by `project_key` over a configurable
//! look-back window (default 24 h), asks a local LLM (or cloud) to
//! score each project's `importance` and `urgency` 0-10, and returns
//! four ranked buckets matching the Eisenhower matrix:
//!
//! - **Q1** Do first: urgent AND important (`importance >= 7` and
//!   `urgency >= 7`)
//! - **Q2** Schedule: important, NOT urgent (`importance >= 7` and
//!   `urgency < 7`)
//! - **Q3** Delegate: urgent, NOT important (`importance < 7` and
//!   `urgency >= 7`)
//! - **Q4** Drop: neither (everything else, scores < 7/7)
//!
//! The LLM is optional; a deterministic heuristic (`heuristic_*`) is
//! always produced so the UI can render even with no AI configured.
//! Every `EisenhowerItem` records whether it came from the LLM or the
//! fallback so the UI can show a small "AI" badge.
//!
//! Prompt shape is intentionally strict-JSON so downstream can
//! `serde_json` decode without regex massaging. A lenient line-based
//! parser (`parse_response_loose`) handles model output that adds a
//! single prefatory sentence.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::ai::{self, AiSettings};
use crate::error::{AppError, AppResult};
use crate::planner;

/// Default look-back window when caller does not pass a value.
pub const DEFAULT_LOOKBACK_HOURS: i64 = 24;
/// Max number of project groups we'll feed the LLM in a single prompt.
/// Beyond this we collapse to the top-N by event count and drop the
/// rest, to keep the prompt under ~6 K tokens.
const MAX_GROUPS_PER_PROMPT: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectGroup {
    pub project_key: String,
    pub source_id: String,
    pub source_display_name: String,
    pub event_count: i64,
    pub first_event_at: String,
    pub last_event_at: String,
    pub top_event_types: Vec<String>,
    pub heuristic_importance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EisenhowerItem {
    pub project_key: String,
    /// v0.4.2: added so the daily brief can roll Q1 up by source
    /// (`brief::aggregate_by_source`). Empty string for items the
    /// LLM produced before we cross-walk to `ProjectGroup.source_id`
    /// (which `merge_with_heuristic` fills in).
    pub source_id: String,
    pub source_display_name: String,
    pub quadrant: i64,
    pub importance: i64,
    pub urgency: i64,
    pub label: String,
    pub reasoning: String,
    pub event_count: i64,
    pub ai_used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EisenhowerDigest {
    pub lookback_hours: i64,
    pub generated_at: String,
    pub items: Vec<EisenhowerItem>,
    pub ai_used: bool,
    pub note: Option<String>,
}

/// Query `raw_events` over the look-back window, GROUP BY (project_key,
/// source_id), and return one row per group with quick stats.
///
/// A synthetic `(unknown)` project_key is folded into one bucket so
/// the UI doesn't drown in noise from sessions without a project_key
/// (which is common for coding tools that don't tag sessions).
pub fn fetch_project_groups(
    conn: &Connection,
    lookback_hours: i64,
) -> AppResult<Vec<ProjectGroup>> {
    if lookback_hours <= 0 {
        return Err(AppError::Other("lookback_hours must be > 0".into()));
    }
    let start_iso: DateTime<Utc> = Utc::now() - chrono::Duration::hours(lookback_hours);
    let mut stmt = conn.prepare(
        "SELECT project_key, source_id, COUNT(*),
                MIN(occurred_at), MAX(occurred_at), GROUP_CONCAT(DISTINCT event_type)
         FROM raw_events
         WHERE occurred_at >= ?1
         GROUP BY project_key, source_id
         ORDER BY COUNT(*) DESC
         LIMIT 200",
    )?;
    let rows = stmt
        .query_map(params![start_iso.to_rfc3339()], |row| {
            let pk: Option<String> = row.get(0)?;
            let source_id: String = row.get(1)?;
            let event_count: i64 = row.get(2)?;
            let first: String = row.get(3)?;
            let last: String = row.get(4)?;
            let event_types: Option<String> = row.get(5)?;
            Ok((
                pk,
                source_id,
                event_count,
                first,
                last,
                event_types.unwrap_or_default(),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Resolve source display names in a single second query to keep the
    // GROUP BY scan cheap. Trade one extra round-trip for cleaner SQL.
    let mut name_stmt = conn.prepare("SELECT id, display_name FROM sources")?;
    let names: std::collections::HashMap<String, String> = name_stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut out = Vec::with_capacity(rows.len());
    for (pk, source_id, event_count, first, last, event_types) in rows {
        let project_key = match pk {
            Some(ref p) if !p.is_empty() => p.clone(),
            _ => "(unknown)".to_string(),
        };
        let top_event_types: Vec<String> = event_types
            .split(',')
            .filter(|s| !s.is_empty())
            .take(5)
            .map(|s| s.to_string())
            .collect();
        // Heuristic importance is computed below the same way reports.rs
        // does — capped event-count tier with `markdown_summary` getting
        // a constant 9 because those are the user's hand-written
        // summaries.
        let heuristic_importance =
            importance_from_event_count(event_count, top_event_types.first().map(|s| s.as_str()));
        out.push(ProjectGroup {
            project_key,
            source_id: source_id.clone(),
            source_display_name: names
                .get(&source_id)
                .cloned()
                .unwrap_or(source_id),
            event_count,
            first_event_at: first,
            last_event_at: last,
            top_event_types,
            heuristic_importance,
        });
    }
    Ok(out)
}

fn importance_from_event_count(n: i64, top_etype: Option<&str>) -> i64 {
    if top_etype == Some("markdown_summary") {
        return 9;
    }
    match n {
        0..=2 => 2,
        3..=10 => 5,
        11..=50 => 7,
        _ => 8,
    }
}

/// Deterministic fallback that buckets each group using
/// `heuristic_importance + recency`. Recent (last_event_at within the
/// last 3 hours) bumps urgency; everything else lands in Q2/Q4.
///
/// Always returns — this is the cold-start path when no LLM is
/// configured.
pub fn heuristic_eisenhower(groups: &[ProjectGroup]) -> Vec<EisenhowerItem> {
    let now: DateTime<Utc> = Utc::now();
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let last_dt = DateTime::parse_from_rfc3339(&g.last_event_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(now);
        let age_hours = (now - last_dt).num_hours().max(0);
        // Urgency rises the closer `last_event_at` is to "now".
        let urgency = if age_hours <= 3 {
            9
        } else if age_hours <= 12 {
            7
        } else if age_hours <= 48 {
            5
        } else {
            3
        };
        let importance = g.heuristic_importance;
        let quadrant = quadrant_for(importance, urgency);
        // Heuristic label is best-effort: prefer the project_key
        // when it's not the synthetic `(unknown)` bucket, otherwise
        // fall back to the source display name so the brief isn't
        // a wall of "跟进 (unknown) 的 2052 events" cards.
        let label = if g.project_key == "(unknown)" {
            format!("跟进 {} 的 {} events", g.source_display_name, g.event_count)
        } else {
            // Trim long path tails so the brief stays scannable.
            let short = g
                .project_key
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or(&g.project_key);
            format!("跟进 {} ({} events)", short, g.event_count)
        };
        let reasoning = format!(
            "heuristic: importance={} (events={}, tier={}); urgency={} (last event {} h ago); Q{}",
            importance, g.event_count, g.heuristic_importance, urgency, age_hours, quadrant
        );
        out.push(EisenhowerItem {
            project_key: g.project_key.clone(),
            source_id: g.source_id.clone(),
            source_display_name: g.source_display_name.clone(),
            quadrant,
            importance,
            urgency,
            label,
            reasoning,
            event_count: g.event_count,
            ai_used: false,
        });
    }
    // Sort: Q1 first, then Q2, Q3, Q4 — within each by score desc.
    out.sort_by(|a, b| {
        a.quadrant
            .cmp(&b.quadrant)
            .then_with(|| (b.importance + b.urgency).cmp(&(a.importance + a.urgency)))
    });
    out
}

/// Threshold mapping shared with the LLM prompt so the parser's
/// understanding of "Q1" matches the prompt's claim.
pub fn quadrant_for(importance: i64, urgency: i64) -> i64 {
    if importance >= 7 && urgency >= 7 {
        1
    } else if importance >= 7 {
        2
    } else if urgency >= 7 {
        3
    } else {
        4
    }
}

/// Build the Eisenhower prompt for `MAX_GROUPS_PER_PROMPT` groups,
/// truncating the tail. The strict-JSON shape keeps the parser simple.
pub fn build_prompt(groups: &[ProjectGroup], lookback_hours: i64) -> String {
    let rendered = groups
        .iter()
        .take(MAX_GROUPS_PER_PROMPT)
        .enumerate()
        .map(|(i, g)| {
            format!(
                "  {n}. project_key=\"{pk}\" source=\"{src}\" events={ec} imp_hint={ih}/10 first={first} last={last} types=[{types}]",
                n = i + 1,
                pk = g.project_key,
                src = g.source_display_name,
                ec = g.event_count,
                ih = g.heuristic_importance,
                first = g.first_event_at,
                last = g.last_event_at,
                types = g.top_event_types.join(","),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let dropped = groups.len().saturating_sub(MAX_GROUPS_PER_PROMPT);
    let dropped_note = if dropped > 0 {
        format!(
            "\n(plus {dropped} additional project groups omitted — keep responses tight.)"
        )
    } else {
        String::new()
    };
    format!(
        "You score a list of cross-agent projects observed in the last {hours} hours \
         and bucket them into Eisenhower quadrants.\n\n\
         Thresholds:\n\
         - Q1 (Do first):   importance>=7 AND urgency>=7\n\
         - Q2 (Schedule):   importance>=7 AND urgency<7\n\
         - Q3 (Delegate):   importance<7  AND urgency>=7\n\
         - Q4 (Drop):       both < 7\n\n\
         importance = long-term / strategic / blocking value (0-10).\n\
         urgency    = has a deadline today, blocks other work, or had activity in the last 3 h (0-10).\n\n\
         Project groups (newest activity first):\n{rows}\n{dropped}\n\n\
         Respond ONLY with a JSON array. One object per project_key (cap at the top 20). \
         Keep `label` under 8 Chinese characters / 4 English words; keep `reasoning` under 60 chars. \
         Strict shape, no prose or code fences outside the JSON:\n\
         [\n  \
         {{\"project_key\": \"<path or (unknown)>\", \"source\": \"<display name>\", \"importance\": <0-10>, \"urgency\": <0-10>, \"quadrant\": <1|2|3|4>, \"label\": \"<short action>\", \"reasoning\": \"<one line why>\"}},\n  ...\n]",
        hours = lookback_hours,
        rows = rendered,
        dropped = dropped_note,
    )
}

/// Lenient LLM response parser — first tries strict JSON, then a
/// line-oriented scan for "project_key=...; QUADRANT N; importance=N;
/// urgency=N; label=...; reasoning=..." patterns. Returns a Vec even
/// on partial failures so the caller can show `ai_used: false` plus
/// whatever the heuristic produced.
pub fn parse_response(raw: &str) -> Vec<EisenhowerItem> {
    match parse_strict_json(raw) {
        Some(v) if !v.is_empty() => v,
        _ => parse_loose_lines(raw),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LlmEisenhowerItem {
    project_key: String,
    source: Option<String>,
    importance: i64,
    urgency: i64,
    quadrant: i64,
    label: String,
    reasoning: Option<String>,
}

fn parse_strict_json(raw: &str) -> Option<Vec<EisenhowerItem>> {
    // Strip code fences if the LLM added them despite the "no fences"
    // instruction — the fuzzy trim lets the parser succeed on either
    // `[{...}]` or `[ ...json... ]`.
    let trimmed = raw.trim();
    let body = if trimmed.starts_with("```") {
        let stripped = trimmed
            .trim_start_matches("```")
            .trim_start_matches("json")
            .trim_start_matches("JSON");
        let end = stripped.rfind("```").unwrap_or(stripped.len());
        stripped[..end].trim()
    } else {
        trimmed
    };
    let parsed: Vec<LlmEisenhowerItem> = serde_json::from_str(body).ok()?;
    let out: Vec<EisenhowerItem> = parsed
        .into_iter()
        .map(|it| EisenhowerItem {
            project_key: it.project_key,
            // LLM never returns source_id — `merge_with_heuristic`
            // backfills this from the heuristic pass (which carries
            // it from `fetch_project_groups`). Empty string here is
            // intentional and only visible during the merge window.
            source_id: String::new(),
            source_display_name: it.source.unwrap_or_default(),
            quadrant: it.quadrant.clamp(1, 4),
            importance: it.importance.clamp(0, 10),
            urgency: it.urgency.clamp(0, 10),
            label: it.label,
            reasoning: it.reasoning.unwrap_or_default(),
            event_count: 0,
            ai_used: true,
        })
        .collect();
    Some(out)
}

fn parse_loose_lines(raw: &str) -> Vec<EisenhowerItem> {
    let mut out = Vec::new();
    let mut current: Option<EisenhowerItem> = None;
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("project_key=") {
            // Flush previous.
            if let Some(it) = current.take() {
                out.push(it);
            }
            let pk = rest.trim().trim_matches('"').trim_end_matches(';').to_string();
            current = Some(EisenhowerItem {
                project_key: pk,
                source_id: String::new(),
                source_display_name: String::new(),
                quadrant: 4,
                importance: 0,
                urgency: 0,
                label: String::new(),
                reasoning: String::new(),
                event_count: 0,
                ai_used: true,
            });
        } else if let Some(rest) = t.strip_prefix("QUADRANT") {
            let q: i64 = rest
                .trim()
                .trim_start_matches(':')
                .trim()
                .chars()
                .next()
                .and_then(|c| c.to_digit(10))
                .map(|n| n as i64)
                .unwrap_or(4);
            if let Some(it) = current.as_mut() {
                it.quadrant = q.clamp(1, 4);
            }
        } else if let Some(rest) = t.strip_prefix("importance=") {
            let v: i64 = rest
                .trim()
                .trim_end_matches(';')
                .parse()
                .unwrap_or(0)
                .clamp(0, 10);
            if let Some(it) = current.as_mut() {
                it.importance = v;
            }
        } else if let Some(rest) = t.strip_prefix("urgency=") {
            let v: i64 = rest
                .trim()
                .trim_end_matches(';')
                .parse()
                .unwrap_or(0)
                .clamp(0, 10);
            if let Some(it) = current.as_mut() {
                it.urgency = v;
            }
        } else if let Some(rest) = t.strip_prefix("label=") {
            let v = rest.trim().trim_matches('"').trim_end_matches(';').to_string();
            if let Some(it) = current.as_mut() {
                it.label = v;
            }
        } else if let Some(rest) = t.strip_prefix("reasoning=") {
            let v = rest.trim().trim_matches('"').trim_end_matches(';').to_string();
            if let Some(it) = current.as_mut() {
                it.reasoning = v;
            }
        }
    }
    if let Some(it) = current.take() {
        out.push(it);
    }
    out
}

/// Top-level entry. If local Ollama or cloud is configured AND
/// responds, prefer the LLM's output; merge in `heuristic` scores for
/// any project_key the LLM omitted (so the matrix is never empty). If
/// the LLM fails / isn't configured, return only the heuristic.
pub fn generate(
    conn: &Connection,
    lookback_hours: i64,
) -> AppResult<EisenhowerDigest> {
    let groups = fetch_project_groups(conn, lookback_hours)?;
    let heuristic = heuristic_eisenhower(&groups);
    let settings = ai::load_settings();
    let (items, ai_used, note) = run_llm(&settings, &groups, lookback_hours, &heuristic);

    let merged = merge_with_heuristic(items, &heuristic, &groups);
    Ok(EisenhowerDigest {
        lookback_hours,
        generated_at: Utc::now().to_rfc3339(),
        items: merged,
        ai_used,
        note,
    })
}

fn run_llm(
    settings: &AiSettings,
    groups: &[ProjectGroup],
    lookback_hours: i64,
    heuristic_fallback: &[EisenhowerItem],
) -> (Vec<EisenhowerItem>, bool, Option<String>) {
    // Cold-start: nothing to classify.
    if groups.is_empty() {
        return (
            heuristic_fallback.to_vec(),
            false,
            Some("no project groups in window — only heuristic".into()),
        );
    }
    // Decide if calling LLM is even possible.
    let can_call = match settings.provider {
        ai::Provider::Local => settings.local_endpoint.starts_with("http://"),
        _ => !settings.api_key.is_empty(),
    };
    if !can_call {
        return (
            heuristic_fallback.to_vec(),
            false,
            Some("LLM not configured — heuristic only".into()),
        );
    }
    let prompt = build_prompt(groups, lookback_hours);
    let report = ai::generate(settings, &prompt, false);
    if !report.ok {
        return (
            heuristic_fallback.to_vec(),
            false,
            report.error,
        );
    }
    let parsed = parse_response(&report.response);
    if parsed.is_empty() {
        return (
            heuristic_fallback.to_vec(),
            false,
            Some("LLM response did not parse — heuristic only".into()),
        );
    }
    (parsed, true, None)
}

fn merge_with_heuristic(
    primary: Vec<EisenhowerItem>,
    heuristic: &[EisenhowerItem],
    groups: &[ProjectGroup],
) -> Vec<EisenhowerItem> {
    if primary.is_empty() {
        return heuristic.to_vec();
    }
    // Build a lookup so unknown project_keys from the LLM still get
    // event_count + ai_reasoning filled in from the heuristic pass.
    // Collect owned project_key strings up front so the HashMap can
    // outlive any later borrowing of `out`.
    let h_pairs: Vec<(String, EisenhowerItem)> = heuristic
        .iter()
        .map(|h| (h.project_key.clone(), h.clone()))
        .collect();
    let h_index: std::collections::HashMap<&str, EisenhowerItem> = h_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();

    let mut out = primary;
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    for it in out.iter_mut() {
        if let Some(h) = h_index.get(it.project_key.as_str()) {
            it.event_count = h.event_count;
            if it.reasoning.is_empty() {
                it.reasoning = h.reasoning.clone();
            }
            if it.source_display_name.is_empty() {
                it.source_display_name = h.source_display_name.clone();
            }
            // v0.4.2: heuristic carries `source_id` from
            // `fetch_project_groups`; the LLM doesn't know it.
            // Backfill whenever the primary side didn't bring one.
            if it.source_id.is_empty() {
                it.source_id = h.source_id.clone();
            }
        }
        present.insert(it.project_key.clone());
    }
    // Drop the borrow on `out` (and on the heuristic clones) by
    // moving `present` to owned before the backfill push.
    let name_for_pk: std::collections::HashMap<String, String> = groups
        .iter()
        .map(|g| (g.project_key.clone(), g.source_display_name.clone()))
        .collect();
    for h in heuristic {
        if !present.contains(&h.project_key) {
            let mut cloned = h.clone();
            cloned.ai_used = false;
            if let Some(name) = name_for_pk.get(&h.project_key) {
                cloned.source_display_name = name.clone();
            }
            out.push(cloned);
        }
    }
    // Final sort: Q1 first, then by (importance + urgency) desc.
    out.sort_by(|a, b| {
        a.quadrant
            .cmp(&b.quadrant)
            .then_with(|| (b.importance + b.urgency).cmp(&(a.importance + a.urgency)))
    });
    out
}

/// Promote every Q1 item into the default "Daily" plan as a `todo`
/// row. Returns the number of items actually inserted (de-duped
/// against existing rows with the same title on the same template).
pub fn promote_q1_to_plan(conn: &Connection, digest: &EisenhowerDigest) -> AppResult<i64> {
    planner::ensure_default_templates(conn)?;
    let templates = planner::list_templates(conn)?;
    let day_id = match templates.iter().find(|t| t.kind == "day") {
        Some(t) => t.id.clone(),
        None => planner::create_template(conn, "Daily Stand-up", "day")?.id,
    };

    // Existing titles on this template so we don't double-import Q1
    // items the user already accepted.
    let existing: std::collections::HashSet<String> = {
        let items = planner::list_items(conn, &day_id)?;
        items.into_iter().map(|it| it.title).collect()
    };

    let mut inserted = 0;
    for it in digest.items.iter().filter(|i| i.quadrant == 1) {
        let title = if it.label.is_empty() {
            format!("Q1: {}", it.project_key)
        } else {
            format!("Q1: {}", it.label)
        };
        if existing.contains(&title) {
            continue;
        }
        let description = format!(
            "{}\n\npriority: importance={} urgency={} event_count={}{}",
            it.project_key,
            it.importance,
            it.urgency,
            it.event_count,
            if it.ai_used { "\n\n(generated by AI)" } else { "" }
        );
        let input = planner::UpsertItemInput {
            id: None,
            template_id: day_id.clone(),
            quadrant: 1,
            title,
            description,
            status: Some("todo".into()),
            priority: Some(0),
            source_event_id: None,
            ai_reasoning: Some(it.reasoning.clone()),
        };
        planner::upsert_item(conn, input)?;
        inserted += 1;
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(pk: &str, ec: i64, last_iso: &str, types: &[&str]) -> ProjectGroup {
        ProjectGroup {
            project_key: pk.into(),
            source_id: "src_test".into(),
            source_display_name: "test".into(),
            event_count: ec,
            first_event_at: last_iso.into(),
            last_event_at: last_iso.into(),
            top_event_types: types.iter().map(|s| s.to_string()).collect(),
            heuristic_importance: importance_from_event_count(ec, types.first().copied()),
        }
    }

    #[test]
    fn quadrant_for_obvious_cases() {
        assert_eq!(quadrant_for(9, 9), 1);
        assert_eq!(quadrant_for(9, 3), 2);
        assert_eq!(quadrant_for(3, 9), 3);
        assert_eq!(quadrant_for(2, 2), 4);
    }

    #[test]
    fn heuristic_puts_recent_into_q1_q3_and_stale_into_q2_q4() {
        let now = Utc::now();
        let three_h_ago = (now - chrono::Duration::hours(3)).to_rfc3339();
        let one_h_ago = (now - chrono::Duration::hours(1)).to_rfc3339();
        let two_days_ago = (now - chrono::Duration::hours(50)).to_rfc3339();
        let groups = vec![
            g("/proj-A", 60, &one_h_ago, &["message"]),
            g("/proj-B", 25, &three_h_ago, &["message"]),
            g("/proj-C", 5, &two_days_ago, &["message"]),
            g("/proj-D", 60, &two_days_ago, &["message"]),
        ];
        let out = heuristic_eisenhower(&groups);
        // proj-A: importance=8, urgency=9 → Q1
        assert_eq!(out[0].quadrant, 1, "got {:?}", out[0]);
        assert_eq!(out[0].project_key, "/proj-A");
        // proj-B: importance=7, urgency=7 → Q1 (last 3 h)
        assert_eq!(out[1].quadrant, 1);
        // proj-D stale heavy → Q2 (imp high, urgency low)
        assert_eq!(out[2].project_key, "/proj-D");
        assert_eq!(out[2].quadrant, 2, "got {:?}", out[2]);
        // proj-C tiny stale → Q4 (both low).
        assert_eq!(out[3].project_key, "/proj-C");
        assert_eq!(out[3].quadrant, 4);
    }

    #[test]
    fn importance_boosts_markdown_summary() {
        let n = importance_from_event_count(2, Some("markdown_summary"));
        assert!(n >= 8, "markdown_summary must outrank normal small buckets");
        let normal = importance_from_event_count(2, Some("message"));
        assert!(n > normal);
    }

    #[test]
    fn parse_response_strict_json_succeeds_on_canonical_shape() {
        let raw = r#"[{"project_key":"/x","source":"test","importance":8,"urgency":9,"quadrant":1,"label":"ship eisenhower","reasoning":"core deliverable"}]"#;
        let v = parse_response(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].project_key, "/x");
        assert_eq!(v[0].quadrant, 1);
        assert!(v[0].ai_used);
    }

    #[test]
    fn parse_response_strips_code_fence_and_round_trips() {
        let raw = "```json\n[{\"project_key\":\"/y\",\"importance\":7,\"urgency\":3,\"quadrant\":2,\"label\":\"review\"}]\n```";
        let v = parse_response(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].project_key, "/y");
        assert_eq!(v[0].quadrant, 2);
    }

    #[test]
    fn parse_response_loose_lines_falls_back() {
        let raw = "\
project_key=/z;
QUADRANT 3
importance=4
urgency=8
label=delegate
reasoning=quick win
";
        let v = parse_response(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].project_key, "/z");
        assert_eq!(v[0].quadrant, 3);
        assert_eq!(v[0].importance, 4);
        assert_eq!(v[0].urgency, 8);
    }

    #[test]
    fn build_prompt_includes_all_groups_and_window() {
        let groups = vec![
            g("/p1", 12, "2026-10-17T00:00:00+00:00", &["message"]),
            g("/p2", 30, "2026-10-17T01:00:00+00:00", &["session_message"]),
        ];
        let p = build_prompt(&groups, 24);
        assert!(p.contains("/p1"));
        assert!(p.contains("/p2"));
        assert!(p.contains("last 24 hours"));
        assert!(p.contains("event_count=12") || p.contains("events=12"));
    }
}
