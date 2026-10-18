# Changelog

All notable changes to Agent Work Ledger are documented here.
Versions follow `vX.Y.Z` where `X` is a breaking schema change,
`Y` is a notable feature, and `Z` is a polish / bugfix.

## Unreleased

- (placeholder — see git history for the actual entries)

## v0.5.0 — 2026-10-18

- **Workspace split (Track A of the v0.5.0 roadmap)**. The
  monolithic `src/lib.rs` (2423 lines, 30+ `#[tauri::command]`
  handlers) is broken into 5 crates:
  - `awl-core` — pure domain logic. 12 modules (ai, app_settings,
    brief, connectors × 10, eisenhower, error, etl, idb, planner,
    redactor, reports, search, summary), `build.rs` for
    linker-level `leveldb` symbol lookup, and no `tauri`
    consumer-facing API. Owns `AppError::Zip` + `AppError::Tauri`
    conversions so downstream crates stay composable.
  - `awl-db` — schema v4 + `migrate` + `backup_ledger_pre_v4` +
    `is_v3_schema` + `migrate_for_tests` + 6 schema tests + the
    `retention` sub-module. Backed by `awl-core` for the
    connector-side `redactor` / `etl` types used during ingest.
  - `awl-export` — bulk + zip export. Owns its own
    `migrate_for_tests` use via a dev-only `awl-db` dep, so tests
    can exercise real DB plumbing without dragging the export
    crate into the workspace runtime graph.
  - `awl-ipc` — every `#[tauri::command]` plus `run_scan_once_mode`
    / `run_brief_once_mode` / `auto_rescan_all` /
    `background_scan_loop` / `reset_events` / launchd
    install + uninstall. Exposes `pub fn register<R>(builder)`
    that wraps `tauri::generate_handler![…]` so the 47 IPC
    functions stay private (the macro would otherwise error on
    duplicate `__cmd__*` symbols across crates).
  - `awl-desktop` — `pub fn run` (Tauri 2 `Builder::default`
    glue + setup + tray icon + window event) + `main.rs` binary
    entry + `window_state.rs`. Owns the only `tauri` runtime
    dependency in the workspace.
- **Cycle-breaking decisions**:
  1. `app_settings` lives in `awl-core`, not `awl-db`. The
     `awl_settings.json` read/write path needed by both the
     connectors (which sit in `awl-core`) and the schema
     migration (which sits in `awl-db`) is the natural source
     of the cycle. Moving `app_settings` up into `awl-core`
     lets `awl-db` depend on `awl-core` without reversing it.
  2. `window_state` lives in `awl-desktop`, not `awl-db`.
     It needs `tauri::WebviewWindow` + `LogicalPosition` and
     `LogicalSize`, which are runtime-only concepts `awl-db`
     cannot reasonably depend on.
  3. Two retention constants are kept in lock-step by
     documentation rather than sharing a type:
     `awl_core::app_settings::DEFAULT_EVENT_RETENTION_DAYS = 90`
     and `awl_db::retention::DEFAULT_RETENTION_DAYS = 90`.
     `ai.rs` inlines a 4-line `resolve_data_dir` that mirrors
     `awl_db::resolve_data_dir` for the same reason.
- **Connectors stay put**. All 10 connector modules
  (claude / codex / cursor / doubao / markdown_summary /
  minimax / opencode / trae / workbuddy) move with `awl-core`,
  and their test mods use `awl_db::migrate_for_tests` via a
  dev-only `awl-db` dep.
- **No binary behavior change for the user**: same `run`,
  same `setup`, same tray, same window state, same IPC
  surface, same `capabilities/default.json` permission set,
  same launchd plist payloads (now bundled as tauri
  resources from `crates/launchd/`). Same DB schema (v4);
  same command count (47). Same hash pipeline. Same
  redactor (3 API-key patterns, all cloud providers forced
  through `Redactor::cloud`).
- **`cargo check --workspace`**: 0 errors.
  Carried warnings: a small set of `unused` / `dead_code` /
  `private_interfaces` / `parens` items (see git diff for
  the exact list). None affect runtime or build output.
- **`cargo test --workspace`**: 138 tests pass
  (awl-core 109 + 1 ignored + 5 doc-tests, awl-db 6,
  awl-desktop 8, awl-export 13, binary stubs 0 each).
- Track B (Stage 6 SQLCipher) for `TRAE SOLO` / `ModularData`
  follows the same work-product tag cadence once the user
  grants one keychain access via
  `security find-generic-password -s 'TRAE SOLO Safe Storage' -wa`.

## v0.4.10 — 2026-10-18

- **OSS launch prep**: CONTRIBUTING.md, CODE_OF_CONDUCT.md
  (Contributor Covenant 2.1), SECURITY.md (private disclosure
  path via GitHub Security Advisories), CHANGELOG.md, two
  issue templates, one PR template, and `docs/OSS-LAUNCH.md`
  runbook. No binary change.

## v0.4.10.1 — 2026-10-18

- **Repo created**: <https://github.com/huoyuntianzhi1/agent-work-ledger>
  4 commits pushed to `master`, tags `v0.4.9` + `v0.4.10` pushed.
  Topics: ai-tools, conversation-history, developer-tools,
  local-first, macos, ollama, rust, search, sqlite, tauri.

## v0.4.9 — 2026-10-18

- **Contacted upstream peer project.** Filed
  [issue #1 on `chrzamz/sessio`](https://github.com/chrzamz/sessio/issues/1)
  proposing collaboration on the connector layer, FTS5 schema
  knowledge-share, and a co-spec for the Phase 4 (project
  aliases / incremental indexing / starring) gap. Tone: technical,
  no-pressure, no-merge-expectation.
- No binary change; this version exists as a work-product tag.

## v0.4.8 — 2026-10-18

- **LICENSE**: dual-licensed under MIT and Apache-2.0 (files in
  repo root). Mirrors `chrzamz/sessio` and `tauri` themselves.
- **README**: rewritten to v0.4.8 — 4 badges (MIT / Apache / macOS
  / Tauri 2), 10+ connector matrix, "What's different from the
  alternatives" section, Stage 6 SQLCipher roadmap.
- **Test fix**: `brief::tests::g()` was hard-coding
  `last_event_at = "2026-10-17T03:00:00+00:00"`, which fell
  outside `eisenhower::generate`'s 24 h lookback the moment
  the test date passed. Now derived from
  `Utc::now() - Duration::hours(2)`. Affects
  `brief_picks_top_3_by_score_desc` and two other tests.

## v0.4.7 — 2026-10-18

- **Bug fix**: every export entry point in the Sessions page was
  failing with a generic "导出失败" toast. Root cause: the
  Tauri 2 capability file was missing `dialog:allow-save`
  (and `dialog:allow-message` for `confirm()` in Planner /
  App). Added both, plus a `tests/capabilities_lint.rs` that
  keeps the capability set in lock-step with the actual
  `@tauri-apps/plugin-dialog` calls in the React code.

## v0.4.6 — 2026-10-18

- **CLI guard**: `--scan-once` and `--brief-once` no longer
  load or save the window position, so the launchd plist can
  re-invoke the binary without the window jumping to the
  cached position from the last GUI session.
- New `window_state::is_cli_mode()` helper + `CLI_FLAGS`
  constant in `app/src-tauri/src/window_state.rs` to keep
  the flag list in one place.

## v0.4.5 — 2026-10-18

- **New connector**: `cursor` — reads the Cursor plaintext
  stash at `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
  (table `cursorDiskKV`, 127 `composerData:<uuid>` rows). This
  is the only place on a real macOS install where Cursor's
  conversation history is recoverable without a keychain
  unlock.

## v0.4.4 — 2026-10-17

- **Event payload search**: the Sessions page now lets the user
  search by payload substring, not just by partition key.
  Result preview shows the matching event's first 160 chars,
  redacted, with the source badge.

## v0.4.3 — 2026-10-17

- **DB retention policy**: 90-day `raw_events` auto-prune on
  each launchd tick. Configurable via `app_settings.json`.
  Default is 90 days; setting it to 0 disables pruning.

## v0.4.2 — 2026-10-17

- **Brief digest增强**: morning brief now reports
  per-source Q1 rollup (e.g. "Q1 dominated by Codex (3) and
  Cursor (2)") and a day-over-day delta ("Q1 +3 / -1 vs
  yesterday"). Same card, two new fields, no extra IPC.

## v0.4.1 and earlier

See `git log` — the canonical history begins at commit
`1d5d48e` ("v0.4.8 baseline (LICENSE + README + brief test
fix) — pre workspace split"), which is the first commit
that has LICENSE + README + a green test suite together.
Prior work lives in the pre-git history; the release notes
from the local-only era are in `docs/RELEASE-NOTES.md`.
