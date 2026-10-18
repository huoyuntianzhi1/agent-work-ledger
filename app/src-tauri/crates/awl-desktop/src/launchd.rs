//! macOS LaunchAgent install / uninstall helpers.
//!
//! v0.5.3: a rebuild of the install path after the v0.5.0 workspace
//! split silently broke the existing LaunchAgent. The plist that was
//! loaded into `~/Library/LaunchAgents/` still pointed at the old
//! binary name (`agent-work-ledger`), which no longer exists after
//! the rename to `awl-desktop`. Every 5 minutes launchd would respawn
//! the agent; every respawn failed with "no such file"; the periodic
//! scanner silently did nothing.
//!
//! This module fixes the install path so users can run
//! `awl-desktop --install-launchd` once after an upgrade (or after
//! first install) and have a working scanner + brief agent.
//! Re-running `--install-launchd` is idempotent: it boots out any
//! existing agent with our label (success or failure), deletes the
//! existing plist files, writes a fresh plist pair with the correct
//! `__BINARY_PATH__` placeholder substituted to argv[0]'s absolute
//! path, then bootstraps both agents under the user's GUI launchd
//! domain. `--uninstall-launchd` reverses the install.
//!
//! Plist templates are embedded at compile time so we don't ship any
//! loose `.plist` files in the .app bundle. This matches the
//! comment in each plist header: the path is patched by the install
//! action, never by an external installer step.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCANNER_LABEL: &str = "com.minimaxcode.agentworkledger.scanner";
const BRIEF_LABEL: &str = "com.minimaxcode.agentworkledger.brief";
const SCANNER_PLIST_FILE: &str = "com.minimaxcode.agentworkledger.scanner.plist";
const BRIEF_PLIST_FILE: &str = "com.minimaxcode.agentworkledger.brief.plist";

// Embed the templates at compile time. Paths are relative to
// `crates/awl-desktop/src/launchd.rs`, so `../../launchd/...` reaches
// `crates/launchd/...`. Verified by the workspace split in v0.5.0.
const SCANNER_PLIST_TEMPLATE: &str =
    include_str!("../../launchd/com.minimaxcode.agentworkledger.scanner.plist");
const BRIEF_PLIST_TEMPLATE: &str =
    include_str!("../../launchd/com.minimaxcode.agentworkledger.brief.plist");

/// Resolve `~/Library/LaunchAgents/`, creating the directory if
/// missing. Per-user LaunchAgents live here on macOS; only the
/// current user (UID via `id -u`) can bootstrap them.
fn user_launch_agents_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME not set".to_string())?;
    let dir = PathBuf::from(home).join("Library").join("LaunchAgents");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("create {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// Best-effort discovery of the user's GUI launchd domain
/// (`gui/<UID>`). Used as the `target` for `launchctl bootstrap` /
/// `launchctl bootout`. Falls back to `gui/501` (the historical
/// default for the first created account) — bootstrap with a
/// wrong UID will be rejected by launchd but install will then
/// print a clear error.
fn gui_domain() -> String {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string());
    format!("gui/{}", uid)
}

/// Canonical token: a `<string>` element whose *entire* text body
/// is the placeholder. We anchor on the wrapper so that even if the
/// header comment also contains the bare string `__BINARY_PATH__`
/// (and both our templates do, by design), we only rewrite the
/// operational substitution site and leave the comment untouched.
const PLACEHOLDER_TOKEN: &str = "<string>__BINARY_PATH__</string>";

fn write_plist(dst: &Path, template: &str, binary_path: &str) -> Result<(), String> {
    match template.find(PLACEHOLDER_TOKEN) {
        None => {
            return Err(format!(
                "plist template (target {}) does not contain the `{PLACEHOLDER_TOKEN}` substitution token",
                dst.display()
            ));
        }
        Some(idx) => {
            let mut body = String::with_capacity(template.len() + binary_path.len());
            body.push_str(&template[..idx]);
            body.push_str("<string>");
            body.push_str(binary_path);
            body.push_str("</string>");
            body.push_str(&template[idx + PLACEHOLDER_TOKEN.len()..]);
            fs::write(dst, body).map_err(|e| format!("write {}: {}", dst.display(), e))?;
        }
    }
    Ok(())
}

/// `launchctl bootout` is best-effort and idempotent. Exit codes
/// other than 0 from a bootout are normal (e.g., "not loaded") and
/// do not constitute an install failure.
fn bootout_best_effort(domain: &str, label: &str) {
    let target = format!("{}/{}", domain, label);
    let _ = Command::new("launchctl")
        .args(["bootout", &target])
        .output();
}

/// `launchctl bootstrap <domain> <plist-path>` registers and
/// starts the agent. Non-zero exit fails the install.
fn bootstrap_strict(domain: &str, plist: &Path, label: &str) -> Result<(), String> {
    let plist_str = plist
        .to_str()
        .ok_or_else(|| format!("non-UTF8 plist path {}", plist.display()))?;
    let out = Command::new("launchctl")
        .args(["bootstrap", domain, plist_str])
        .output()
        .map_err(|e| format!("launchctl bootstrap {}: {}", label, e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "launchctl bootstrap {} exited {:?}: {}",
            label,
            out.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Idempotent install. Returns `Ok(report)` with a short human
/// description of what changed so callers can print it from
/// `awl-desktop --install-launchd`. Safe to invoke at any time;
/// re-running just rewrites the plists and re-bootstraps.
pub fn install(binary_path: &str) -> Result<InstallReport, String> {
    if binary_path.is_empty() {
        return Err("empty binary path".into());
    }
    let domain = gui_domain();
    let dir = user_launch_agents_dir()?;

    // Tear down any existing (possibly broken) agents. Failure is
    // expected if the agents are not loaded or not registered for
    // this domain — we ignore it and overwrite the plists anyway.
    bootout_best_effort(&domain, SCANNER_LABEL);
    bootout_best_effort(&domain, BRIEF_LABEL);

    // Always remove stale plists so re-install is a clean state.
    let scanner_dst = dir.join(SCANNER_PLIST_FILE);
    let brief_dst = dir.join(BRIEF_PLIST_FILE);
    let scanner_existed = fs::remove_file(&scanner_dst).is_ok();
    let brief_existed = fs::remove_file(&brief_dst).is_ok();

    // Write fresh plists with the placeholder substituted.
    write_plist(&scanner_dst, SCANNER_PLIST_TEMPLATE, binary_path)?;
    write_plist(&brief_dst, BRIEF_PLIST_TEMPLATE, binary_path)?;

    // Register under the user's GUI launchd domain so the agent
    // runs while they are logged in. launchd will pick up the new
    // plist immediately and `RunAtLoad` fires the first invocation.
    bootstrap_strict(&domain, &scanner_dst, SCANNER_LABEL)?;
    bootstrap_strict(&domain, &brief_dst, BRIEF_LABEL)?;

    Ok(InstallReport {
        domain,
        scanner_plist: scanner_dst,
        brief_plist: brief_dst,
        scanner_replaced: scanner_existed,
        brief_replaced: brief_existed,
    })
}

#[derive(Debug)]
pub struct InstallReport {
    pub domain: String,
    pub scanner_plist: PathBuf,
    pub brief_plist: PathBuf,
    pub scanner_replaced: bool,
    pub brief_replaced: bool,
}

impl std::fmt::Display for InstallReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "installed launchd agents under domain `{}`", self.domain)?;
        writeln!(
            f,
            "  scanner plist: {} ({}replaced)",
            self.scanner_plist.display(),
            if self.scanner_replaced { "" } else { "not " }
        )?;
        writeln!(
            f,
            "  brief   plist: {} ({}replaced)",
            self.brief_plist.display(),
            if self.brief_replaced { "" } else { "not " }
        )?;
        writeln!(
            f,
            "next: launchd will RunAtLoad both agents immediately; `tail -F /tmp/agent-work-ledger.scanner.out.log` to watch scanner ticks"
        )?;
        Ok(())
    }
}

/// Symmetric to `install`. Tears down agents and removes the
/// plists so the user can fully uninstall launchd integration.
/// Idempotent: a missing plist or unloaded agent is not an error.
pub fn uninstall() -> Result<UninstallReport, String> {
    let domain = gui_domain();
    let dir = user_launch_agents_dir()?;

    let scanner_bootout = Command::new("launchctl")
        .args(["bootout", &format!("{}/{}", domain, SCANNER_LABEL)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let brief_bootout = Command::new("launchctl")
        .args(["bootout", &format!("{}/{}", domain, BRIEF_LABEL)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let scanner_removed = fs::remove_file(dir.join(SCANNER_PLIST_FILE)).is_ok();
    let brief_removed = fs::remove_file(dir.join(BRIEF_PLIST_FILE)).is_ok();

    Ok(UninstallReport {
        domain,
        scanner_was_loaded: scanner_bootout,
        brief_was_loaded: brief_bootout,
        scanner_plist_removed: scanner_removed,
        brief_plist_removed: brief_removed,
    })
}

#[derive(Debug)]
pub struct UninstallReport {
    pub domain: String,
    pub scanner_was_loaded: bool,
    pub brief_was_loaded: bool,
    pub scanner_plist_removed: bool,
    pub brief_plist_removed: bool,
}

impl std::fmt::Display for UninstallReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "uninstalled launchd agents under domain `{}`", self.domain)?;
        writeln!(
            f,
            "  scanner: agent {}loaded, plist {}removed",
            if self.scanner_was_loaded { "" } else { "not " },
            if self.scanner_plist_removed { "" } else { "not " },
        )?;
        writeln!(
            f,
            "  brief  : agent {}loaded, plist {}removed",
            if self.brief_was_loaded { "" } else { "not " },
            if self.brief_plist_removed { "" } else { "not " },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both plist templates must reach the loader with the
    /// `__BINARY_PATH__` placeholder preserved verbatim. If a
    /// future contributor edits the templates to spell the
    /// placeholder differently (e.g., `<%BINARY%>`), this test
    /// catches the regression before install writes broken plists.
    #[test]
    fn embedded_templates_still_have_placeholder() {
        assert!(
            SCANNER_PLIST_TEMPLATE.contains("__BINARY_PATH__"),
            "scanner plist template missing __BINARY_PATH__ placeholder"
        );
        assert!(
            BRIEF_PLIST_TEMPLATE.contains("__BINARY_PATH__"),
            "brief plist template missing __BINARY_PATH__ placeholder"
        );
    }

    /// After substitution the operational `<string>__BINARY_PATH__</string>`
    /// token (the one inside `ProgramArguments`) must become the resolved
    /// binary path. The header comment also contains the literal
    /// `__BINARY_PATH__` string and is intentionally left untouched so
    /// the comment remains useful when someone inspects the loaded
    /// plist. We use the scanner template as the representative; both
    /// go through `write_plist` so coverage of one is coverage of both.
    #[test]
    fn placeholder_substitution_replaces_only_the_program_arguments_string() {
        // Mirror `write_plist`'s anchoring strategy: find the
        // `<string>__BINARY_PATH__</string>` token (canonical,
        // unambiguous) and replace only that one occurrence.
        let idx = SCANNER_PLIST_TEMPLATE
            .find(PLACEHOLDER_TOKEN)
            .expect("scanner template should contain the canonical token");
        let resolved = "/Applications/Agent Work Ledger.app/Contents/MacOS/awl-desktop";
        let mut body = String::with_capacity(SCANNER_PLIST_TEMPLATE.len());
        body.push_str(&SCANNER_PLIST_TEMPLATE[..idx]);
        body.push_str("<string>");
        body.push_str(resolved);
        body.push_str("</string>");
        body.push_str(&SCANNER_PLIST_TEMPLATE[idx + PLACEHOLDER_TOKEN.len()..]);

        // (1) The substituted path sits inside a `<string>…</string>`
        //     element closing immediately after the path.
        let path_idx = body
            .find(resolved)
            .expect("substituted path should be present");
        assert!(
            body[path_idx..].starts_with(&format!("{}</string>", resolved)),
            "path must close its <string> tag immediately, got: {}",
            &body[path_idx..path_idx + resolved.len() + 20]
        );

        // (2) The bare `__BINARY_PATH__` placeholder (without the
        //     `<string>` wrapper) still appears in the comment —
        //     proving the substitution only touched the wrapped
        //     token and left the prose untouched.
        assert!(
            body.contains("__BINARY_PATH__"),
            "comment placeholder should remain after substitution"
        );

        // (3) The exact canonical token `<string>__BINARY_PATH__</string>`
        //     is gone (we replaced it).
        assert!(
            !body.contains(PLACEHOLDER_TOKEN),
            "canonical token must not survive substitution"
        );
    }

    /// Whichever template is used at install time, the canonical
    /// token must exist or `write_plist` would fail closed. This
    /// catches a future contributor who forgets to put the
    /// placeholder into a new template variant.
    #[test]
    fn both_templates_carry_the_canonical_token() {
        assert!(SCANNER_PLIST_TEMPLATE.contains(PLACEHOLDER_TOKEN));
        assert!(BRIEF_PLIST_TEMPLATE.contains(PLACEHOLDER_TOKEN));
    }

    /// `gui_domain()` must always produce a target that starts with
    /// `gui/` and has a numeric UID. This guards against malformed
    /// `launchctl bootstrap` invocations that would silently fail.
    #[test]
    fn gui_domain_shape() {
        let d = gui_domain();
        assert!(d.starts_with("gui/"), "got {}", d);
        let uid = &d["gui/".len()..];
        assert!(!uid.is_empty(), "missing UID");
        assert!(
            uid.chars().all(|c| c.is_ascii_digit()),
            "UID must be digits, got {}",
            uid
        );
    }

    /// `install` with an empty path is rejected at the boundary so
    /// the malformed plist never reaches launchd. (Eager validation
    /// is cheaper than an `Invalid property list` error after the
    /// fact.)
    #[test]
    fn install_rejects_empty_binary_path() {
        let res = install("");
        assert!(res.is_err(), "expected Err for empty path");
    }
}
