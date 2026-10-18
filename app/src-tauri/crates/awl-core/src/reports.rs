//! v0.2 window-level AI reports — daily & weekly aggregations of
//! `sessions_summary` rows that the user explicitly requests.
//!
//! Unlike the per-session enricher (`summary.rs`) which keys by an
//! individual `sessions_summary.id`, these reports key by an arbitrary
//! wall-clock window (today, this week, a custom range). They re-use the
//! same `ai::generate` pipeline and the same redactor — both layers
//! inherit cloud-force-redact from `ai.rs::redact_outbound_pub`. Cache
//! storage is the pre-existing `window_reports` table (schema v4).
//!
//! Heuristic fallback shape: when no provider is configured (or the
//! parse fails) we return a deterministic local report — top-N sessions
//! by heuristic importance, project breakdown, and a 1-sentence
//! headline. The UI never sees an empty card.

use std::time::Instant;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ai::{self, AiSettings, GenerateReport};
use crate::error::AppResult;

/// Surface shape for one window report. Returned to the UI exactly as
/// stored on `window_reports.response` — the JSON payload stays a single
/// blob so the LLM may freely extend the schema (themes, project
/// breakdown, etc.) without a Rust regeneration.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WindowReport {
    pub id: String,
    pub window_label: String,
    pub window_start: String,
    pub window_end: String,
    pub headline: String,
    pub bullets: Vec<String>,
    pub themes: Vec<String>,
    pub project_breakdown: Vec<ProjectBreakdown>,
    pub provider: String,
    pub model: String,
    pub expires_at: String,
    pub created_at: String,
    /// True when served from the cache (`window_reports` row whose
    /// `expires_at` is still in the future).
    pub cached: bool,
    pub age_hours: i64,
}

/// One row of the heuristic (and LLM) project breakdown.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectBreakdown {
    pub project_key: String,
    pub sessions: i64,
    pub events: i64,
}

/// Aggregates every `sessions_summary` row whose `[started_at,
/// ended_at]` window falls inside `[start, end]`. We deliberately pull
/// the raw ETL partition columns directly: the AI / heuristic layers
/// work better with one canonical row per bucket, not raw events.
#[derive(Debug, Clone)]
pub struct PartitionInWindow {
    pub id: String,
    pub source_id: String,
    pub source_display_name: String,
    pub project_key: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub event_count: i64,
    pub heuristic_summary: String,
    pub heuristic_importance: i64,
}

/// Build (or hit cache for) one window report. `label` is `"day" | "week"`
/// — the contract is informational only, we don't key the cache by it
/// (we key by exact start+end so cached rows survive label changes).
///
/// `window_start` / `window_end` are RFC3339 strings; pass UTC midnight
/// for "day" or Monday 00:00 for "week" to get stable cache hits.
pub fn generate_window_report(
    conn: &Connection,
    label: &str,
    window_start: &str,
    window_end: &str,
) -> AppResult<WindowReport> {
    let started = Instant::now();
    let settings = ai::load_settings();

    if let Some(cached) = read_cache(conn, window_start, window_end)? {
        tracing::info!(
            window_label = label,
            age_hours = cached.age_hours,
            "window-report cache hit"
        );
        return Ok(cached);
    }

    let partitions = fetch_partitions_in_window(conn, window_start, window_end)?;
    let prompt = build_prompt(label, window_start, window_end, &partitions);
    let report = ai::generate(&settings, &prompt, /* json_mode */ true);

    let parsed = parse_json_response(&report.response);
    let used_llm_response = parsed.is_some();
    let (headline, bullets, themes, breakdown) = match parsed {
        Some(v) => v,
        None => heuristic_response(&partitions, &report),
    };

    let id = format!("wrp_{}", uuid::Uuid::new_v4().simple());
    let now = now_rfc3339();
    let expires = plus_hours(cache_hours(&settings));
    let prompt_hash = sha256_hex(&prompt);

    let used_llm = report.ok && used_llm_response;
    let (provider, model) = if used_llm {
        (report.provider.clone(), report.model.clone())
    } else {
        ("heuristic".to_string(), "heuristic".to_string())
    };

    // We persist the LLM response verbatim when it parses; otherwise we
    // wrap the heuristic shape into the same blob so the UI parses a
    // single canonical JSON structure.
    let response_blob = build_response_blob(
        &headline,
        &bullets,
        &themes,
        &breakdown,
        report.ok,
        used_llm_response,
    );

    conn.execute(
        "INSERT INTO window_reports(
            id, window_label, window_start, window_end,
            prompt, response, provider, model, expires_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            label,
            window_start,
            window_end,
            prompt_hash,
            response_blob,
            provider,
            model,
            expires,
            now
        ],
    )?;

    tracing::info!(
        window_label = label,
        provider = %provider,
        partitions = partitions.len(),
        duration_ms = started.elapsed().as_millis() as i64,
        "window-report written"
    );

    Ok(WindowReport {
        id,
        window_label: label.to_string(),
        window_start: window_start.to_string(),
        window_end: window_end.to_string(),
        headline,
        bullets,
        themes,
        project_breakdown: breakdown,
        provider,
        model,
        expires_at: expires,
        created_at: now,
        cached: false,
        age_hours: 0,
    })
}

/// `list_window_reports(label, limit)` — newest-first SELECT used by
/// the Reports page list. Returns a Vec (possibly empty).
pub fn list_window_reports(
    conn: &Connection,
    label: Option<&str>,
    limit: i64,
) -> AppResult<Vec<WindowReport>> {
    let limit = limit.clamp(1, 200);
    let (sql, has_label): (&str, bool) = match label {
        Some(_) => (
            "SELECT id, window_label, window_start, window_end, response,
                    provider, model, expires_at, created_at
             FROM window_reports WHERE window_label = ?1
             ORDER BY created_at DESC LIMIT ?2",
            true,
        ),
        None => (
            "SELECT id, window_label, window_start, window_end, response,
                    provider, model, expires_at, created_at
             FROM window_reports
             ORDER BY created_at DESC LIMIT ?1",
            false,
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<WindowReport> {
        let response_blob: String = row.get(4)?;
        let parsed = decode_response_blob(&response_blob);
        Ok(WindowReport {
            id: row.get(0)?,
            window_label: row.get(1)?,
            window_start: row.get(2)?,
            window_end: row.get(3)?,
            headline: parsed.headline,
            bullets: parsed.bullets,
            themes: parsed.themes,
            project_breakdown: parsed.project_breakdown,
            provider: row.get(5)?,
            model: row.get(6)?,
            expires_at: row.get(7)?,
            created_at: row.get(8)?,
            cached: false,
            age_hours: 0,
        })
    };
    let rows = if has_label {
        stmt.query_map(params![label.unwrap(), limit], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map(params![limit], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

fn read_cache(
    conn: &Connection,
    window_start: &str,
    window_end: &str,
) -> AppResult<Option<WindowReport>> {
    let now = now_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT id, window_label, response, provider, model, expires_at, created_at
         FROM window_reports
         WHERE window_start = ?1 AND window_end = ?2 AND expires_at > ?3
         ORDER BY created_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![window_start, window_end, now], |row| {
        let response_blob: String = row.get(2)?;
        let parsed = decode_response_blob(&response_blob);
        let created_at: String = row.get(6)?;
        let age_hours = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|d| {
                (chrono::Utc::now() - d.with_timezone(&chrono::Utc))
                    .num_minutes()
                    / 60
            })
            .unwrap_or(0);
        Ok(WindowReport {
            id: row.get(0)?,
            window_label: row.get(1)?,
            window_start: window_start.to_string(),
            window_end: window_end.to_string(),
            headline: parsed.headline,
            bullets: parsed.bullets,
            themes: parsed.themes,
            project_breakdown: parsed.project_breakdown,
            provider: row.get(3)?,
            model: row.get(4)?,
            expires_at: row.get(5)?,
            created_at: created_at,
            cached: true,
            age_hours,
        })
    })?;
    if let Some(r) = rows.next() {
        Ok(Some(r?))
    } else {
        Ok(None)
    }
}

fn fetch_partitions_in_window(
    conn: &Connection,
    start: &str,
    end: &str,
) -> AppResult<Vec<PartitionInWindow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.source_id, COALESCE(src.display_name, s.source_id),
                s.project_key, s.started_at, s.ended_at,
                s.event_count, s.summary, s.importance
         FROM sessions_summary s
         LEFT JOIN sources src ON src.id = s.source_id
         WHERE s.started_at >= ?1 AND s.started_at < ?2
         ORDER BY s.started_at DESC
         LIMIT 200",
    )?;
    let rows = stmt
        .query_map(params![start, end], |row| {
            Ok(PartitionInWindow {
                id: row.get(0)?,
                source_id: row.get(1)?,
                source_display_name: row.get(2)?,
                project_key: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                event_count: row.get(6)?,
                heuristic_summary: row.get(7)?,
                heuristic_importance: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseBlob {
    headline: String,
    bullets: Vec<String>,
    themes: Vec<String>,
    project_breakdown: Vec<ProjectBreakdown>,
    #[serde(default)]
    llm_ok: bool,
    #[serde(default)]
    parsed_from_llm: bool,
}

fn build_response_blob(
    headline: &str,
    bullets: &[String],
    themes: &[String],
    breakdown: &[ProjectBreakdown],
    llm_ok: bool,
    parsed: bool,
) -> String {
    let blob = ResponseBlob {
        headline: headline.to_string(),
        bullets: bullets.to_vec(),
        themes: themes.to_vec(),
        project_breakdown: breakdown.to_vec(),
        llm_ok,
        parsed_from_llm: parsed,
    };
    serde_json::to_string(&blob).unwrap_or_else(|_| "{}".into())
}

fn decode_response_blob(blob: &str) -> ResponseBlob {
    serde_json::from_str(blob).unwrap_or(ResponseBlob {
        headline: String::new(),
        bullets: Vec::new(),
        themes: Vec::new(),
        project_breakdown: Vec::new(),
        llm_ok: false,
        parsed_from_llm: false,
    })
}

fn parse_json_response(raw: &str) -> Option<(String, Vec<String>, Vec<String>, Vec<ProjectBreakdown>)> {
    let s = raw.trim();
    let json_str = if s.starts_with("```") {
        let inner = s
            .trim_start_matches("```")
            .trim_start_matches("json")
            .trim();
        inner.trim_end_matches("```").trim()
    } else {
        s
    };
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = v.as_object()?;
    let headline = obj
        .get("headline")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let bullets = obj
        .get("bullets")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let themes = obj
        .get("themes")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let breakdown = obj
        .get("project_breakdown")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let m = item.as_object()?;
                    Some(ProjectBreakdown {
                        project_key: m
                            .get("project_key")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        sessions: m.get("sessions").and_then(|x| x.as_i64()).unwrap_or(0),
                        events: m.get("events").and_then(|x| x.as_i64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some((headline, bullets, themes, breakdown))
}

fn heuristic_response(
    partitions: &[PartitionInWindow],
    report: &GenerateReport,
) -> (String, Vec<String>, Vec<String>, Vec<ProjectBreakdown>) {
    if partitions.is_empty() {
        return (
            "No captured sessions in this window.".into(),
            vec!["Run a connector scan on the home page to populate the ledger.".into()],
            Vec::new(),
            Vec::new(),
        );
    }

    let total_events: i64 = partitions.iter().map(|p| p.event_count).sum();
    let total_sessions = partitions.len();

    let mut by_project: std::collections::BTreeMap<String, (i64, i64)> =
        std::collections::BTreeMap::new();
    for p in partitions {
        let key = p
            .project_key
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string());
        let entry = by_project.entry(key).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += p.event_count;
    }
    let mut breakdown: Vec<ProjectBreakdown> = by_project
        .into_iter()
        .map(|(k, (sessions, events))| ProjectBreakdown {
            project_key: k,
            sessions,
            events,
        })
        .collect();
    breakdown.sort_by(|a, b| b.events.cmp(&a.events));

    let mut top_importance: Vec<&PartitionInWindow> = partitions.iter().collect();
    top_importance.sort_by(|a, b| b.heuristic_importance.cmp(&a.heuristic_importance));

    let mut bullets: Vec<String> = vec![format!(
        "{} sessions captured across {} event(s) in this window.",
        total_sessions, total_events
    )];
    if let Some(top) = top_importance.first() {
        bullets.push(format!(
            "Heaviest session: {} ({}, importance {}/10)",
            top.heuristic_summary, top.source_display_name, top.heuristic_importance
        ));
    }
    if breakdown.len() > 1 {
        bullets.push(format!(
            "Most active project: {} ({} sessions, {} events)",
            breakdown[0].project_key, breakdown[0].sessions, breakdown[0].events
        ));
    }

    let themes: Vec<String> = Vec::new(); // LLM-only field

    let headline = match report.error.as_deref() {
        Some(_) => format!(
            "{} sessions in window (heuristic fallback)",
            total_sessions
        ),
        None => format!(
            "{} sessions in window",
            total_sessions
        ),
    };
    (headline, bullets, themes, breakdown)
}

fn build_prompt(label: &str, start: &str, end: &str, partitions: &[PartitionInWindow]) -> String {
    let mut rows = String::new();
    for p in partitions.iter().take(60) {
        let pk = p.project_key.as_deref().unwrap_or("(unknown)");
        rows.push_str(&format!(
            "  - [{start} → {end}] {name} | project={pk} | events={n} | imp={imp}/10 | {hint}\n",
            start = p.started_at,
            end = p.ended_at,
            name = p.source_display_name,
            n = p.event_count,
            imp = p.heuristic_importance,
            hint = p.heuristic_summary
        ));
    }
    if rows.is_empty() {
        rows = "  (no sessions captured in this window)\n".to_string();
    }
    format!(
        "You produce a one-screen daily/weekly cross-agent work digest for a personal productivity dashboard.\n\
         Window label: {label}\n\
         Window range: {start} → {end}\n\
         Session partition count: {n}\n\n\
         Partitions (newest first, capped at 60):\n{rows}\n\n\
         Respond ONLY with a JSON object in this exact shape, no prose or fences:\n\
         {{\"headline\": \"<one short sentence>\",\n  \
         \"bullets\": [\"<bullet 1>\", \"<bullet 2>\", \"...\"],\n  \
         \"themes\": [\"<theme 1>\", \"<theme 2>\"],\n  \
         \"project_breakdown\": [\n    \
         {{\"project_key\": \"<path>\", \"sessions\": <int>, \"events\": <int>}}, ...\n  \
         ]\n}}",
        label = label,
        start = start,
        end = end,
        n = partitions.len(),
        rows = rows
    )
}

fn cache_hours(settings: &AiSettings) -> i64 {
    settings.cache_hours.max(1)
}

fn now_rfc3339() -> String {
    crate::connectors::rfc3339(Some(crate::connectors::now_ms()))
}

fn plus_hours(hours: i64) -> String {
    chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(hours))
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(now_rfc3339)
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let out = h.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_response_empty_window_yields_no_data_message() {
        let rep = GenerateReport {
            ok: false,
            response: String::new(),
            provider: "local".into(),
            model: "gemma4:e4b".into(),
            duration_ms: 0,
            redacted_chars: 0,
            error: Some("no key".into()),
        };
        let (headline, bullets, _themes, _breakdown) = heuristic_response(&[], &rep);
        assert!(headline.contains("No captured"));
        assert!(bullets[0].contains("Run a connector scan"));
    }

    #[test]
    fn parse_json_response_strips_code_fence() {
        let raw = "```json\n{\"headline\":\"x\",\"bullets\":[\"a\"],\"themes\":[],\"project_breakdown\":[]}\n```";
        let parsed = parse_json_response(raw).unwrap();
        assert_eq!(parsed.0, "x");
        assert_eq!(parsed.1, vec!["a".to_string()]);
    }

    #[test]
    fn parse_json_response_handles_minimal_object() {
        let raw = r#"{"headline":"h","bullets":[],"themes":[],"project_breakdown":[]}"#;
        let parsed = parse_json_response(raw).unwrap();
        assert_eq!(parsed.0, "h");
        assert!(parsed.1.is_empty());
    }

    #[test]
    fn cache_hours_uses_setting_floor_at_one() {
        let mut s = AiSettings::default();
        s.cache_hours = 0;
        assert_eq!(cache_hours(&s), 1);
        s.cache_hours = 48;
        assert_eq!(cache_hours(&s), 48);
    }

    #[test]
    fn prompt_includes_label_and_window() {
        let s = "daily";
        let prompt = build_prompt(
            s,
            "2026-10-16T00:00:00Z",
            "2026-10-17T00:00:00Z",
            &[],
        );
        assert!(prompt.contains("daily"));
        assert!(prompt.contains("2026-10-16T00:00:00Z"));
        assert!(prompt.contains("2026-10-17T00:00:00Z"));
        assert!(prompt.contains("no sessions captured"));
    }

    #[test]
    fn heuristic_response_groups_by_project_key() {
        let partitions = vec![
            PartitionInWindow {
                id: "a".into(),
                source_id: "src1".into(),
                source_display_name: "Codex".into(),
                project_key: Some("/A".into()),
                started_at: "2026-10-16T09:00:00Z".into(),
                ended_at: "2026-10-16T09:30:00Z".into(),
                event_count: 5,
                heuristic_summary: "5 events from /A".into(),
                heuristic_importance: 4,
            },
            PartitionInWindow {
                id: "b".into(),
                source_id: "src1".into(),
                source_display_name: "Codex".into(),
                project_key: Some("/A".into()),
                started_at: "2026-10-16T10:00:00Z".into(),
                ended_at: "2026-10-16T10:30:00Z".into(),
                event_count: 8,
                heuristic_summary: "8 events from /A".into(),
                heuristic_importance: 7,
            },
            PartitionInWindow {
                id: "c".into(),
                source_id: "src2".into(),
                source_display_name: "Claude".into(),
                project_key: Some("/B".into()),
                started_at: "2026-10-16T11:00:00Z".into(),
                ended_at: "2026-10-16T11:15:00Z".into(),
                event_count: 3,
                heuristic_summary: "3 events from /B".into(),
                heuristic_importance: 6,
            },
        ];
        let rep = GenerateReport {
            ok: false,
            response: String::new(),
            provider: "local".into(),
            model: "gemma4:e4b".into(),
            duration_ms: 0,
            redacted_chars: 0,
            error: None,
        };
        let (headline, bullets, _themes, breakdown) = heuristic_response(&partitions, &rep);
        assert_eq!(headline, "3 sessions in window");
        assert!(bullets.iter().any(|b| b.contains("3 sessions captured across 16 event(s)")));
        assert!(bullets.iter().any(|b| b.contains("Heaviest session")));
        assert_eq!(breakdown.len(), 2);
        assert_eq!(breakdown[0].project_key, "/A");
        assert_eq!(breakdown[0].sessions, 2);
        assert_eq!(breakdown[0].events, 13);
        assert_eq!(breakdown[1].project_key, "/B");
    }

    #[test]
    fn response_blob_round_trip_preserves_all_fields() {
        let blob = build_response_blob(
            "h",
            &vec!["a".to_string(), "b".to_string()],
            &vec!["t".to_string()],
            &vec![ProjectBreakdown {
                project_key: "/p".into(),
                sessions: 2,
                events: 5,
            }],
            true,
            true,
        );
        let parsed = decode_response_blob(&blob);
        assert_eq!(parsed.headline, "h");
        assert_eq!(parsed.bullets, vec!["a", "b"]);
        assert_eq!(parsed.themes, vec!["t"]);
        assert_eq!(parsed.project_breakdown.len(), 1);
        assert_eq!(parsed.project_breakdown[0].project_key, "/p");
        assert!(parsed.llm_ok);
    }
}
