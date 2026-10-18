# Changelog

All notable changes to Agent Work Ledger are documented here.
Versions follow `vX.Y.Z` where `X` is a breaking schema change,
`Y` is a notable feature, and `Z` is a polish / bugfix.

## Unreleased

- (placeholder — see git history for the actual entries)

## v0.5.2 — 2026-10-18

- **Hotfix — keychain service name for `trae_solo`.** v0.5.1
  shipped with `TRAE_SOLO_KEYCHAIN_SERVICE = "TRAE SOLO Safe Storage"`
  but the actual entry Trae's Electron fork (`com.trae.app`,
  v3.5.50) writes is `"Trae Safe Storage"` (account `"Trae Key"`),
  per `security dump-keychain`. With the wrong name `security
  find-generic-password` always exits 44 ("could not be found"),
  the connector lands in `NoKeychainEntry` forever, and the GUI
  prompt never fires. **Fix**: const renamed to
  `TRAE_KEYCHAIN_SERVICE = "Trae Safe Storage"` with a
  `TRAE_SOLO_KEYCHAIN_SERVICE` back-compat alias, `security`
  call switched from `-w` to `-wa` (so the access-ACL check
  forces a one-time GUI prompt even when the entry has been
  previously read headlessly), doc comments updated.
- 2 new tests (`keychain_service_name_is_what_trae_writes`,
  `security_cli_finds_real_entry`) act as regression guards
  against future renames; 143 total pass (was 141).
- Public tracking: v0.5.1 stays the canonical "Stage 6
  SQLCipher unlock" feature release; v0.5.2 is a 1-line
  diagnosis hotfix. Both share the same connector code path.

## v0.5.1 — 2026-10-18

- **Stage 6 / Track B: SQLCipher unlock for TRAE SOLO.** New
  connector `trae_solo` reads the encrypted database at
  `~/Library/Application Support/TRAE SOLO/ModularData/ai-agent/database.db`
  using the key held in the macOS keychain under service
  `TRAE SOLO Safe Storage`. The first scan per session
  triggers a one-time GUI prompt; clicking "Always Allow"
  is enough to make all subsequent scans headless.
- **New IPC commands**: `scan_trae_solo` (mirrors the other
  per-source scan handlers) and `trae_solo_status` (read-only
  state probe used by the Home page card). Both registered
  in `awl_ipc::register`.
- **`auto_rescan_all` now sweeps 11 sources** (was 10):
  `codex`, `claude_code`, `cursor`, `opencode`, `workbuddy`,
  `doubao`, `doubao_work`, `minimax_code`, `trae`,
  `trae_solo`, `markdown_summary`. The new entry is
  gated by the keychain — if the user denies the prompt
  the connector returns a `skipped` scan and the rest of
  the auto-rescan continues unaffected.
- **Home page status card** (v0.5.1, App.tsx): a small
  badge that polls `trae_solo_status` every 5 seconds and
  renders one of five states:
  - **未检测到 keychain 条目** (amber) — TRAE SOLO has not
    been launched yet.
  - **keychain 访问被拒绝** (amber) — the user clicked
    "Deny" on the GUI prompt. The "重新触发" button retries.
  - **已解锁 · N 条对话** (emerald) — ingest succeeded,
    N messages counted in the decrypted DB.
  - **schema 不匹配** (red) — both `peanuts` and
    `saltysalt` PBKDF2 salts failed (TRAE SOLO is on a
    Chromium version we don't recognize).
  - **错误: …** (red) — sqlcipher CLI missing, file
    permissions, etc.
- **Pure-CLI decryption pipeline**: the connector shells
  out to the homebrew `sqlcipher` CLI for the actual
  decryption; the main `awl-db` stays plain `rusqlite`
  (no `bundled-sqlcipher` feature was enabled, so the
  workspace never had to deal with feature unification).
  The plaintext copy lives in `$TMPDIR` and is removed
  after the ingest.
- **Keychain key derivation**: Chrome's Safe Storage
  algorithm, PBKDF2-HMAC-SHA1 with 1 iteration and a
  16-byte output. The connector tries `salt = "peanuts"`
  first (Chrome 80+) and falls back to
  `salt = "saltysalt"` (legacy Chromium v10).
- **`AppError::TraeSolo(String)`** new variant in
  `awl-core/src/error.rs`; `From<TraeSoloError> for
  AppError` lives in `awl-core/src/trae_solo.rs` so the
  connector can `?`-bubble without per-crate wrappers.
- **Tests**: 3 new unit tests in `awl-core` exercise
  the round-trip (sqlcipher encrypt → our decrypt → row
  read) and the no-keychain-entry classification. Total
  workspace test count: **141** (was 138 in v0.5.0).
- No binary behavior change for users who have not
  launched TRAE SOLO — the new connector reports
  `NoKeychainEntry` and stays out of the way. The Home
  page card silently says "未检测到 keychain 条目".

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
