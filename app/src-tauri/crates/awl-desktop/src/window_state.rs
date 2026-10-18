//! v0.3.6 / Stage 5.6: window geometry persistence.
//!
//! Persists `{x, y, width, height, maximized}` for the single main
//! window to `<data_dir>/window-state.json` so the next launch opens
//! at the same size/position the user left it at. We do not use
//! `tauri-plugin-window-state` — the data shape is tiny (5 fields)
//! and avoiding a plugin keeps the dep list lean. The disk format
//! is version-tagged in case we want to add fields later without
//! breaking existing files.
//!
//! Save is best-effort and never returns errors to the caller:
//! a stale or corrupt file is a soft failure, the user just gets
//! the default window size/position once.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::{LogicalPosition, LogicalSize, WebviewWindow};

use awl_core::error::AppResult;

const FILE_NAME: &str = "window-state.json";
const FORMAT_VERSION: u32 = 1;

/// Every CLI mode flag that bypasses the GUI. Used by `is_cli_mode`
/// to short-circuit window-state persistence — a `--scan-once` or
/// `--brief-once` invocation has no window, so loading/saving its
/// (non-existent) geometry would either no-op or write garbage that
/// overwrites the real GUI state.
///
/// Keep in sync with the explicit early-return checks at the top of
/// `lib.rs::run()`. New CLI flags belong here too so the guard in
/// `setup` can recognise them defensively.
pub const CLI_FLAGS: &[&str] = &["--scan-once", "--brief-once"];

/// Detect whether `args` (typically `std::env::args().collect()`)
/// contains any known CLI mode flag. The check is by exact string
/// match on each element, so flags like `--scan-once-foo` or
/// `--scan-once=1` are intentionally NOT matched — those would
/// indicate a new flag we haven't taught the helper about, and we
/// prefer a false negative (defensive guard skips) over silently
/// treating them as GUI launches.
pub fn is_cli_mode(args: &[String]) -> bool {
    args.iter().any(|a| CLI_FLAGS.contains(&a.as_str()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        // Matches the tauri.conf.json default so a fresh install
        // does not visibly snap on first launch.
        Self {
            x: 100,
            y: 100,
            width: 1100,
            height: 720,
            maximized: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(flatten)]
    pub state: WindowState,
}

fn default_version() -> u32 {
    FORMAT_VERSION
}

/// Read the persisted window state. A missing or unreadable file
/// returns `None` — caller should fall back to the Tauri default
/// window config from `tauri.conf.json` (which is what the framework
/// already does if we never call `set_size`/`set_position`).
pub fn load(data_dir: &Path) -> Option<WindowState> {
    let path = data_dir.join(FILE_NAME);
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: OnDisk = serde_json::from_str(&raw).ok()?;
    if parsed.version != FORMAT_VERSION {
        tracing::info!(
            on_disk = parsed.version,
            current = FORMAT_VERSION,
            "window-state.json: version mismatch, ignoring"
        );
        return None;
    }
    // Sanity-clamp: a corrupt file that claims e.g. 0×0 must not
    // produce an unusable window. The Tauri default is 1100×720;
    // anything < 320×240 is almost certainly garbage.
    if parsed.state.width < 320 || parsed.state.height < 240 {
        return None;
    }
    Some(parsed.state)
}

/// Persist the window state. Best-effort: any IO or serialisation
/// failure is logged and swallowed. The OS may kill the process
/// mid-write (e.g. force-quit), so we accept partial writes — a
/// half-written file will fail the next `load` and be ignored.
pub fn save(data_dir: &Path, state: &WindowState) {
    let path = data_dir.join(FILE_NAME);
    let on_disk = OnDisk {
        version: FORMAT_VERSION,
        state: state.clone(),
    };
    let json = match serde_json::to_string_pretty(&on_disk) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "window-state: serialise failed");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, json) {
        tracing::warn!(error = %e, path = %path.display(), "window-state: write failed");
    }
}

/// Snapshot the current window geometry into a `WindowState`. Pulled
/// out so unit tests can build a state struct without needing a real
/// `WebviewWindow`.
pub fn snapshot_from_window(window: &WebviewWindow) -> AppResult<WindowState> {
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = window.outer_position()?;
    let size = window.outer_size()?;
    let maximized = window.is_maximized().unwrap_or(false);
    Ok(WindowState {
        x: (pos.x as f64 / scale).round() as i32,
        y: (pos.y as f64 / scale).round() as i32,
        width: (size.width as f64 / scale).round() as u32,
        height: (size.height as f64 / scale).round() as u32,
        maximized,
    })
}

/// Apply a previously-persisted state to a freshly-created window.
/// Silently no-ops on the macOS menu-bar-hide case where the window
/// may be hidden at the moment we run; the next `Moved`/`Resized`
/// event will save the actual current geometry.
pub fn apply_to_window(window: &WebviewWindow, state: &WindowState) {
    if state.maximized {
        let _ = window.maximize();
        return;
    }
    let pos = LogicalPosition::new(state.x as f64, state.y as f64);
    let size = LogicalSize::new(state.width as f64, state.height as f64);
    if let Err(e) = window.set_position(pos) {
        tracing::warn!(error = %e, "window-state: set_position failed");
    }
    if let Err(e) = window.set_size(size) {
        tracing::warn!(error = %e, "window-state: set_size failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "awl-window-state-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = temp_dir();
        let original = WindowState {
            x: 120,
            y: 80,
            width: 1280,
            height: 800,
            maximized: false,
        };
        save(&dir, &original);
        let loaded = load(&dir).expect("load after save");
        assert_eq!(loaded.x, 120);
        assert_eq!(loaded.width, 1280);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = temp_dir();
        assert!(load(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_garbage_size() {
        let dir = temp_dir();
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{"version":1,"x":0,"y":0,"width":0,"height":0}"#,
        )
        .unwrap();
        assert!(load(&dir).is_none(), "0x0 must be rejected as garbage");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_version_mismatch() {
        let dir = temp_dir();
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{"version":999,"x":0,"y":0,"width":1280,"height":800}"#,
        )
        .unwrap();
        assert!(load(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn is_cli_mode_detects_scan_once() {
        // argv[0] is the binary itself; the flag lives further in.
        assert!(is_cli_mode(&args(&[
            "/Applications/Agent Work Ledger.app/Contents/MacOS/agent-work-ledger",
            "--scan-once",
        ])));
    }

    #[test]
    fn is_cli_mode_detects_brief_once() {
        assert!(is_cli_mode(&args(&["binary", "--brief-once"])));
    }

    #[test]
    fn is_cli_mode_returns_false_for_gui_launch() {
        // Plain GUI launch (no flags) must not trip the guard.
        assert!(!is_cli_mode(&args(&[
            "/Applications/Agent Work Ledger.app/Contents/MacOS/agent-work-ledger",
        ])));
        // Unrelated flags should not match either; the helper whitelists
        // only CLI_FLAGS exactly.
        assert!(!is_cli_mode(&args(&["binary", "--verbose", "--debug"])));
    }

    #[test]
    fn is_cli_mode_does_not_match_partial_flag() {
        // `--scan-once-foo` is a deliberate false negative: if anyone
        // adds a flag like that, they'd rather have the guard skip
        // (window state no-op) than silently treat it as GUI.
        assert!(!is_cli_mode(&args(&["binary", "--scan-once-foo"])));
        assert!(!is_cli_mode(&args(&["binary", "--scan-once=1"])));
    }
}
