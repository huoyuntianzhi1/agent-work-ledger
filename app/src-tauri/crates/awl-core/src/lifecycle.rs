//! Persistent lifecycle log for the GUI app.
//!
//! v0.5.6: every meaningful app event (`startup`, `window-hidden`,
//! `exit-requested`, `exit-confirmed`, `exit-prevented`) gets a
//! timestamped line appended to `lifecycle.log` in the per-user
//! data dir. macOS Console unified log and `~/Library/Logs/...`
//! are not reliable for our case (the .app bundle is re-signed
//! after every ship, Console retention is configurable, and a
//! stuck `awl-desktop` simply never logs anything in the moments
//! the user notices a "退出"). The on-disk log is the only place
//! a user can grep for "what actually happened right before the
//! window disappeared" without us having to attach a debugger.
//!
//! Writes are best-effort: a disk full or a read-only volume must
//! not block startup. Each line is a single `write_all` + a
//! `flush()` so a power-cut at the worst possible moment loses
//! at most one event, not the whole buffered history.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

// v0.5.6: written into the per-user data dir as
// `lifecycle.log`. We compute the path via `dirs::data_dir`
// directly instead of depending on `awl_db::resolve_data_dir`;
// the latter is a dev-only dep of `awl-core` (it's a runtime
// dep of `awl-ipc`), and we want this log helper callable from
// any crate without forcing a runtime pull on the SQLite
// migration path. The two paths line up because
// `awl_db::resolve_data_dir` is itself `dirs::data_dir() +
// "com.minimaxcode.agentworkledger"` — see awl-db/src/lib.rs.
const DATA_DIR_BASENAME: &str = "com.minimaxcode.agentworkledger";
const LOG_FILE: &str = "lifecycle.log";
const MAX_LOG_BYTES: u64 = 1_048_576; // 1 MiB rolling cap.

fn log_path() -> Option<PathBuf> {
    let base = dirs::data_dir()?;
    Some(base.join(DATA_DIR_BASENAME).join(LOG_FILE))
}

/// v0.5.6: single source of truth for "the user has already
/// confirmed Quit in the in-app dialog". The `RunEvent::ExitRequested`
/// arm reads this; if true, it allows the exit, if false it
/// `prevent_exit()`s and surfaces the confirm dialog via the
/// frontend event bus. The `confirm_quit` IPC (in `awl-ipc`) is
/// the only writer, and it sets this before calling `app.exit(0)`.
/// Using a process-wide `AtomicBool` keeps the flag out of the
/// public `AppState` struct (it's a UI flow, not a domain
/// concern) and avoids any Tauri state-manager plumbing.
static QUIT_CONFIRMED: AtomicBool = AtomicBool::new(false);

/// Test-only reset hook so unit tests can clear the flag between
/// runs. Not exposed in any public API.
#[cfg(test)]
pub fn reset_quit_confirmed_for_tests() {
    QUIT_CONFIRMED.store(false, Ordering::SeqCst);
}

/// Mark the user as having confirmed the in-app quit dialog, so
/// the next `RunEvent::ExitRequested` is allowed to proceed
/// without re-prompting. Idempotent: repeated calls are a no-op
/// once the flag is set.
pub fn mark_quit_confirmed() {
    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
}

/// Returns true iff the in-app quit dialog has already been
/// confirmed. `RunEvent::ExitRequested` consults this to decide
/// between "allow exit" and "prevent + show dialog".
pub fn quit_confirmed() -> bool {
    QUIT_CONFIRMED.load(Ordering::SeqCst)
}

/// Append a single timestamped event to `lifecycle.log`. Rotates
/// the file when it exceeds `MAX_LOG_BYTES` so a long-running
/// install never grows it past ~1 MiB.
pub fn log_event(event: &str) {
    let Some(path) = log_path() else {
        return;
    };
    log_event_to(&path, event);
}

/// Test-friendly core: append `event` to `path`, rotating first
/// if the file is already over the cap. Public so unit tests can
/// run against a tempdir without touching the user's real
/// `~/Library/Application Support/...` tree.
pub fn log_event_to(path: &Path, event: &str) {
    rotate_if_needed(path);
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let ts = crate::connectors::rfc3339(Some(crate::connectors::now_ms()));
    let line = format!("{ts}\t{event}\n");
    let _ = f.write_all(line.as_bytes());
    let _ = f.flush();
}

fn rotate_if_needed(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    // Rename `lifecycle.log` → `lifecycle.log.1` (single-generation
    // rotation; the previous `.1` is overwritten). The user is
    // grepping for "the most recent session", not an audit trail.
    let rotated = path.with_extension("log.1");
    let _ = std::fs::remove_file(&rotated);
    let _ = std::fs::rename(path, rotated);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(suffix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "awl-lifecycle-{}-{}",
            suffix,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(LOG_FILE)
    }

    #[test]
    fn log_event_writes_line_with_timestamp() {
        let path = tmp_path("write");
        log_event_to(&path, "test-event");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("test-event"),
            "log must contain the event name, got: {body}"
        );
        assert!(
            body.contains('T'),
            "log line must include an ISO-style timestamp, got: {body}"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn rotate_renames_log_when_over_cap() {
        let path = tmp_path("rotate");
        // Seed a > 1 MiB file so the next call rotates it.
        let big = vec![b'x'; (MAX_LOG_BYTES + 1024) as usize];
        std::fs::write(&path, &big).unwrap();

        log_event_to(&path, "after-rotate");

        let rotated = path.with_extension("log.1");
        assert!(rotated.exists(), "rotated file must exist");
        // Active log must contain the new event but be small again
        // (the big seed was rotated away).
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("after-rotate"));
        assert!(
            body.len() < 1024,
            "active log should be tiny post-rotate, got {} bytes",
            body.len()
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn quit_confirmed_flag_round_trip() {
        reset_quit_confirmed_for_tests();
        assert!(!quit_confirmed());
        mark_quit_confirmed();
        assert!(quit_confirmed());
        reset_quit_confirmed_for_tests();
        assert!(!quit_confirmed());
    }
}
