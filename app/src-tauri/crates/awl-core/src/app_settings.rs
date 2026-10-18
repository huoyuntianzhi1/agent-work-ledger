//! Stage 5-E.2: app-level (non-AI) settings.
//!
//! Lives in `<data_dir>/app_settings.json`. Currently exposes one knob:
//! the on-disk directory the markdown scanner (Stage 5-E.2b) walks for
//! hand-written agent summaries. The default is
//! `~/Documents/agent_summaries` — chosen because it sits next to the
//! user's other docs and survives reinstalls of the app.
//!
//! Why a separate file from `ai_settings.json`?
//! - AI provider config may legitimately live in `~/Library/Application
//!   Support/<bundle>/`; summary-dir config is a user content choice
//!   that should not be tied to the provider switch.
//! - Future knobs (theme, language, morning-brief hour, …) all want
//!   the same IPC surface and one read at startup.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// In-memory representation of `<data_dir>/app_settings.json`. All
/// fields default-construct to safe values so a missing/corrupt file
/// never blocks the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// Absolute path to the directory the user keeps their
    /// hand-written markdown summaries in. The scanner walks one
    /// level deep looking for `.md` files with optional YAML
    /// frontmatter. We never create files in this dir ourselves;
    /// the user owns the contents.
    pub summary_dir: String,
    /// v0.4.3: how many days of `raw_events` to keep on disk before
    /// the auto-rescan pass deletes them. `0` = 永久保留 (prune
    /// becomes a no-op). Default is 90 — long enough to cover a
    /// project cycle, short enough to keep the DB lean.
    /// `#[serde(default)]` so old `app_settings.json` (pre-v0.4.3)
    /// loads fine and picks up the default.
    #[serde(default = "default_event_retention_days")]
    pub event_retention_days: i64,
}

fn default_event_retention_days() -> i64 {
    // Matches `awl_db::retention::DEFAULT_RETENTION_DAYS`; the value lives
    // here as well so `app_settings::Default` doesn't need to cross the
    // `awl-core → awl-db` dependency edge (which would cycle).
    90
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            summary_dir: default_summary_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "~/Documents/agent_summaries".into()),
            event_retention_days: default_event_retention_days(),
        }
    }
}

/// `~/Documents/agent_summaries` when we can resolve home, otherwise
/// the literal `~/Documents/agent_summaries` placeholder (the user can
/// fix it in the UI).
fn default_summary_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Documents").join("agent_summaries"))
}

fn settings_path() -> Option<PathBuf> {
    // Inlined rather than calling `awl_db::resolve_data_dir` to avoid an
    // `awl-core ↔ awl-db` cycle (awl-db needs awl-core for the planner
    // hook inside migrate). The two implementations stay in lock-step —
    // see awl-db/src/lib.rs `resolve_data_dir`.
    let base = dirs::data_dir()?;
    let dir = base.join("com.minimaxcode.agentworkledger");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("app_settings.json"))
}

pub fn load() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return AppSettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save(s: &AppSettings) -> Result<(), String> {
    let Some(path) = settings_path() else {
        return Err("could not resolve data dir".into());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(s).map_err(|e| format!("encode: {e}"))?;
    std::fs::write(&path, raw).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Expand `~` to `$HOME` so the UI can show a real absolute path in
/// tooltips without us hand-rolling shell expansion. Returns the input
/// untouched if it doesn't start with `~/`.
pub fn expand_tilde(input: &str) -> String {
    if let Some(stripped) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped).to_string_lossy().into_owned();
        }
    }
    if input == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
    }
    input.to_string()
}

/// One built-in prompt template exposed via `app_list_prompts`. The
/// Stage 5-E.3 Eisenhower classifier and morning-brief injector both
/// read from this list; user-defined templates live in the
/// `summary_dir` (see Stage 5-E.2b markdown scanner).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub id: String,
    pub title: String,
    pub description: String,
    pub body: String,
}

/// Built-in templates shipped with the app. Kept inline so the UI
/// can render them without any disk lookup; user templates come from
/// the summary_dir scanner in a later stage.
pub fn builtin_templates() -> Vec<PromptTemplate> {
    vec![
        PromptTemplate {
            id: "eisenhower_classifier".into(),
            title: "Eisenhower 分类器".into(),
            description: "把 raw_events 行按 重要×紧急 分到 4 象限 (Q1/Q2/Q3/Q4)。本地 Ollama / 云端 LLM 都用同一个 prompt 模板。".into(),
            body: EISENHOWER_PROMPT.into(),
        },
        PromptTemplate {
            id: "daily_brief".into(),
            title: "早 8 点日报".into(),
            description: "聚合昨日 raw_events + sessions_summary,生成 3-5 句中文 brief。launchd 早 8 点自动跑,落 raw_events.event_type=daily_brief。".into(),
            body: DAILY_BRIEF_PROMPT.into(),
        },
        PromptTemplate {
            id: "session_summary".into(),
            title: "单 session 摘要".into(),
            description: "对单个 sessions_summary 行做 head + body 摘要,带 expires_at 24h cache,过期前 re-click 不重复调用 LLM。".into(),
            body: SESSION_SUMMARY_PROMPT.into(),
        },
    ]
}

const EISENHOWER_PROMPT: &str = r#"你是一个个人时间管理助手。根据用户提供的 raw_events JSON 数组,把它
们按"重要 × 紧急"两个维度分类,落到 4 个象限:

- Q1 重要 + 紧急:今天必须做(do now)
- Q2 重要 + 不紧急:这周该做(schedule)
- Q3 不重要 + 紧急:能委托就委托(delegate)
- Q4 不重要 + 不紧急:丢进垃圾桶 / 周末再说(delete)

输入 JSON 形如:
```json
{
  "events": [
    {"id": "ev_…", "occurred_at": "2026-04-15T10:00:00Z", "event_type": "task_snapshot",
     "project_key": "/path/to/proj", "payload_preview": "…"}
  ]
}
```

输出严格 JSON:
```json
{
  "quadrants": {
    "Q1": [{"event_id": "ev_…", "reasoning": "…"}],
    "Q2": [...],
    "Q3": [...],
    "Q4": [...]
  }
}
```

每个 event 必须落到一个象限。reasoning 一句话解释为什么。Q1 不要超过 5
条 — 真正常 Q1 没那么多事,超过说明分类标准太松。
"#;

const DAILY_BRIEF_PROMPT: &str = r#"你是一个个人助理。生成昨天的工作 brief,3-5 句中文,务实不啰嗦。

输入包含:
- `events`: 昨天 24h 内的 raw_events(去重后,按项目分组)
- `sessions_summary`: 昨天关闭的 session 列表
- `pending_quadrants`: 当前 Eisenhower 4 象限里还没做的 Q1/Q2

输出 markdown 三段:
1. **昨天做了什么**(3 行以内,只列真完成的,不要说"完成了 X 个任务")
2. **今天该做什么**(从 Q1/Q2 拉,但只列 top 3,带为什么是它)
3. **风险/阻塞**(有就说,没有就这行省略)
"#;

const SESSION_SUMMARY_PROMPT: &str = r#"为一个 agent session 生成 2 段摘要:

1. **headline**(≤ 80 字):一句话说清楚这个 session 在干啥
2. **body**(200-400 字):分 3 小段 — 目标 / 关键动作 / 产出物链接

输入是 sessions_summary 行 + 该 session 的全部 raw_events。

输出 JSON:
```json
{
  "headline": "…",
  "body": "目标:…\n\n关键动作:…\n\n产出物:…"
}
```
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_resolves_home() {
        let home = dirs::home_dir().expect("home");
        let got = expand_tilde("~/Documents/agent_summaries");
        assert!(got.starts_with(&home.to_string_lossy().into_owned()));
    }

    #[test]
    fn expand_tilde_passes_absolute_paths_through() {
        let got = expand_tilde("/tmp/somewhere");
        assert_eq!(got, "/tmp/somewhere");
    }

    #[test]
    fn expand_tilde_passes_non_tilde_through() {
        assert_eq!(expand_tilde("relative/path"), "relative/path");
    }

    #[test]
    fn default_summary_dir_uses_home() {
        let d = AppSettings::default().summary_dir;
        // Default should resolve home on a real machine; we only
        // smoke-test the shape.
        assert!(d.contains("agent_summaries"));
    }

    #[test]
    fn builtin_templates_have_unique_ids_and_nonempty_bodies() {
        let t = builtin_templates();
        assert!(t.len() >= 3);
        let mut ids: Vec<&str> = t.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), t.len(), "template ids must be unique");
        for p in &t {
            assert!(!p.title.is_empty());
            assert!(!p.description.is_empty());
            assert!(!p.body.is_empty());
        }
    }
}
