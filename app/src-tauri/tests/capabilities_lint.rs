//! Regression guard: ensure the `default` capability file lists every
//! dialog command the frontend actually calls.
//!
//! Background: v0.4.6 shipped with the export feature working in Rust
//! (`render_session_markdown` / `export_sessions_bulk` /
//! `render_sessions_bulk_zip` all returned bytes) but every export
//! click in the GUI surfaced "导出失败" because `@tauri-apps/plugin-dialog`'s
//! `save()` is gated on `dialog:allow-save` and that permission was
//! missing from `capabilities/default.json`. The Tauri runtime throws
//! a `dialog:save not allowed` error before the IPC ever reaches Rust.
//!
//! This test fails loudly the next time someone adds a new dialog
//! command in the frontend without updating the capability allow-list.
//! Keep the matrix in sync with every `@tauri-apps/plugin-dialog` call
//! site in `app/src`.

use std::fs;
use std::path::PathBuf;

fn capabilities_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is `app/src-tauri/`
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set (run via cargo test)");
    PathBuf::from(manifest).join("capabilities").join("default.json")
}

fn granted_permissions() -> Vec<String> {
    let raw = fs::read_to_string(capabilities_path()).expect("read default.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("parse default.json");
    v.get("permissions")
        .and_then(|p| p.as_array())
        .expect("permissions[]")
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect()
}

/// Every dialog permission the frontend actually uses. The grep
/// targets in `app/src` are: `save(`, `open(`, `message(`, `confirm(`,
/// `ask(`. Add a new entry here the same commit that introduces a
/// matching `plugin-dialog` call site, and the lint will force the
/// capability file to keep up.
const REQUIRED_DIALOG_PERMS: &[&str] = &[
    // `save()` from @tauri-apps/plugin-dialog in
    // pages/Sessions.tsx (3 export buttons) + pages/Timeline.tsx.
    "dialog:allow-save",
    // `open()` from @tauri-apps/plugin-dialog in
    // pages/Timeline.tsx (attach-import for connectors).
    "dialog:allow-open",
    // `confirm()` and `message()` from @tauri-apps/plugin-dialog in
    // pages/Planner.tsx + App.tsx runReset. Tauri 2 maps confirm
    // and message to allow-message, so a single allow-message entry
    // covers both.
    "dialog:allow-message",
];

#[test]
fn default_capability_grants_every_dialog_command_used_by_frontend() {
    let granted = granted_permissions();
    for required in REQUIRED_DIALOG_PERMS {
        assert!(
            granted.iter().any(|p| p == required),
            "capabilities/default.json is missing `{required}` — the frontend \
             calls a @tauri-apps/plugin-dialog command that the runtime will \
             block. Add it to the `permissions` array and re-run this test."
        );
    }
}

#[test]
fn capability_uses_default_known_identifiers_only() {
    // Catch typos like `dialog:allow-svae` or `dialog:allow-save-extra`
    // that would otherwise pass silently because no command actually
    // needs them and the runtime would not raise.
    let granted = granted_permissions();
    for p in &granted {
        if let Some(rest) = p.strip_prefix("dialog:") {
            assert!(
                rest == "allow-save"
                    || rest == "allow-open"
                    || rest == "allow-message"
                    || rest == "allow-ask"
                    || rest == "allow-confirm"
                    || rest == "default",
                "unknown dialog permission `{p}` in default.json"
            );
        }
    }
}
