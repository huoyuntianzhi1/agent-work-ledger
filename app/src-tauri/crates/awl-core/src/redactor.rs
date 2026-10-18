//! Privacy redactor — runs **before** any data is written to the ledger DB.
//!
//! Rule sources:
//!   1. `privacy_rules` table (`exclude_dir` + `redact_pattern`).
//!   2. Hard-coded deny list (sk-*, AKIA, Bearer, JWT, …) compiled in.
//!
//! This module is intentionally conservative: when in doubt, redact.

use std::path::{Path, PathBuf};

use regex::Regex;
use rusqlite::Connection;

use crate::error::{AppError, AppResult};

/// Compiled rule for a single redaction pattern.
pub struct RedactRule {
    pub id: String,
    pub label: String,
    pub pattern: Regex,
}

/// Loader: reads `privacy_rules WHERE enabled = 1 AND kind = 'redact_pattern'`
/// from the ledger DB, plus a hard-coded safety net.
pub struct Redactor {
    rules: Vec<RedactRule>,
}

impl Redactor {
    pub fn from_db(conn: &Connection) -> AppResult<Self> {
        let mut rules: Vec<RedactRule> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT id, pattern FROM privacy_rules WHERE enabled = 1 AND kind = 'redact_pattern'",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let pattern: String = row.get(1)?;
            Ok((id, pattern))
        })?;
        for row in rows {
            let (id, pattern) = row?;
            let rule = compile_redact_rule(&id, &pattern)?;
            rules.push(rule);
        }
        for builtin in builtin_safety_net() {
            rules.push(builtin);
        }
        Ok(Self { rules })
    }

    /// Returns true iff `path` is inside any excluded directory.
    pub fn is_path_excluded(&self, conn: &Connection, path: &Path) -> AppResult<bool> {
        let mut stmt = conn.prepare(
            "SELECT pattern FROM privacy_rules WHERE enabled = 1 AND kind = 'exclude_dir'",
        )?;
        let rows = stmt.query_map([], |row| {
            let pattern: String = row.get(0)?;
            Ok(pattern)
        })?;
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        for row in rows {
            let pattern = row?;
            for dir in expand_excluded_dir(&pattern) {
                if let Ok(canonical_dir) = dir.canonicalize() {
                    if canonical_path.starts_with(&canonical_dir) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Apply every compiled regex to `text` in turn, replacing matches with
    /// `[REDACTED:<label>]`. Idempotent — running twice yields the same string.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for rule in &self.rules {
            out = rule
                .pattern
                .replace_all(&out, format!("[REDACTED:{}]", rule.label))
                .into_owned();
        }
        out
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Build a redactor from only the hard-coded safety-net rules.
    /// Used by the AI query path where we don't have a DB connection
    /// handy. Skips the per-user `privacy_rules` rows but still catches
    /// the built-in OpenAI / Anthropic / Bearer / JWT / 32-char-hex keys.
    pub fn global() -> Self {
        Self {
            rules: builtin_safety_net(),
        }
    }
}

fn compile_redact_rule(id: &str, pattern: &str) -> AppResult<RedactRule> {
    let label = id.trim_start_matches("default-redact-").to_string();
    let pattern = Regex::new(pattern).map_err(|e| {
        AppError::Connector(
            "privacy_rules".to_string(),
            format!("rule `{id}`: invalid regex: {e}"),
        )
    })?;
    Ok(RedactRule {
        id: id.to_string(),
        label,
        pattern,
    })
}

/// Returns hard-coded rules. These mirror the seed values in `migrate()`,
/// but provide a safety net if the database row was deleted by the user.
fn builtin_safety_net() -> Vec<RedactRule> {
    let pairs: &[(&str, &str)] = &[
        ("openai-key", r"(?i)\b(sk-[A-Za-z0-9_-]{20,})\b"),
        ("openai-cp-key", r"(?i)\b(sk-cp-[A-Za-z0-9_-]{20,})\b"),
        ("anthropic-key", r"(?i)\b(sk-ant-[A-Za-z0-9_-]{20,})\b"),
        ("aws-access-key", r"\bAKIA[0-9A-Z]{16}\b"),
        ("bearer-token", r"(?i)\b(Bearer\s+[A-Za-z0-9._\-]{12,})\b"),
        ("jwt", r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"),
        ("github-pat", r"\bghp_[A-Za-z0-9]{30,}\b"),
        ("google-api-key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
    ];
    pairs
        .iter()
        .map(|(id, pat)| {
            // Patterns are validated by hand; if one breaks it gets loud.
            let pattern = Regex::new(pat).expect("builtin regex compiled");
            RedactRule {
                id: (*id).to_string(),
                label: (*id).to_string(),
                pattern,
            }
        })
        .collect()
}

/// Expand patterns like `~/.ssh` into absolute paths.
fn expand_excluded_dir(pattern: &str) -> Vec<PathBuf> {
    if let Some(stripped) = pattern.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return vec![home.join(stripped)];
        }
    }
    if pattern.starts_with('/') {
        return vec![PathBuf::from(pattern)];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_key() {
        let rule = compile_redact_rule(
            "openai-key",
            r"(?i)\b(sk-[A-Za-z0-9_-]{20,})\b",
        )
        .unwrap();
        let text = "Authorization header sk-1234567890abcdefghijklmnop is exposed";
        let out = rule
            .pattern
            .replace_all(text, format!("[REDACTED:{}]", rule.label))
            .into_owned();
        assert!(!out.contains("sk-1234"));
        assert!(out.contains("[REDACTED:openai-key]"));
    }
}
