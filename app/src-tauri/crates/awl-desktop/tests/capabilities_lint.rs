//! Capability lint — keeps the runtime permission set in lock-step
//! with the actual `@tauri-apps/plugin-dialog` calls in
//! `app/src/**`. If you add a new IPC surface that goes through
//! `plugin-dialog` (or any other permission-gated plugin), bump
//! `REQUIRED_PERMISSIONS` and add the matching capability in
//! `crates/awl-desktop/capabilities/default.json`.
//!
//! Originally `app/src-tauri/tests/capabilities_lint.rs` in the
//! pre-workspace-split era. After Track A that crate went away;
//! the test moved to `crates/awl-desktop/tests/` because it is
//! the only binary crate that owns `tauri.conf.json` and
//! `capabilities/default.json`.

use std::fs;
use std::path::PathBuf;

// Permissions currently granted by `default` capability. The
// order here matches the JSON file so a diff is trivial.
const REQUIRED_PERMISSIONS: &[&str] = &[
    "core:default",
    "dialog:allow-open",
    "dialog:allow-save",
    "dialog:allow-message",
    "fs:allow-app-read",
    "fs:allow-app-write",
    "shell:allow-open",
];

fn capabilities_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .join("default.json")
}

#[test]
fn capability_set_matches_runtime_required_permissions() {
    let raw = fs::read_to_string(capabilities_path())
        .expect("capabilities/default.json must exist; see `crates/awl-desktop/capabilities/`");

    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .expect("capabilities/default.json must be valid JSON");

    let permissions = parsed["permissions"]
        .as_array()
        .expect("`permissions` must be a JSON array")
        .iter()
        .map(|v| v.as_str().expect("each permission must be a string"))
        .collect::<Vec<_>>();

    for required in REQUIRED_PERMISSIONS {
        assert!(
            permissions.contains(required),
            "capabilities/default.json is missing `{required}`. \
             Either add it to the JSON file, or update \
             REQUIRED_PERMISSIONS in \
             crates/awl-desktop/tests/capabilities_lint.rs if the \
             permission is no longer needed.",
        );
    }

    // No accidental typos in the JSON. Every entry must match a
    // well-known tauri-plugin-dialog permission shape or this test
    // would just be a no-op.
    for perm in &permissions {
        assert!(
            REQUIRED_PERMISSIONS.contains(perm),
            "capabilities/default.json grants `{perm}` but \
             REQUIRED_PERMISSIONS does not know about it. \
             Add it to the test list so the lint stays honest.",
        );
    }
}

#[test]
fn default_capability_targets_only_main_window() {
    let raw = fs::read_to_string(capabilities_path())
        .expect("capabilities/default.json must exist");
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let windows = parsed["windows"].as_array().expect("`windows`");
    let labels: Vec<&str> = windows.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(labels, vec!["main"]);
}
