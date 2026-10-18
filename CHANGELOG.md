# Changelog

All notable changes to Agent Work Ledger are documented here.
Versions follow `vX.Y.Z` where `X` is a breaking schema change,
`Y` is a notable feature, and `Z` is a polish / bugfix.

## Unreleased

- (placeholder — see git history for the actual entries)

## v0.5.6 — 2026-10-18

- **Quit-confirm dialog + lifecycle log.** A real user
  complaint ("应用老是意外退出") drove this one. The fix
  has two parts: a confirmation dialog before the app
  actually quits, and a persistent on-disk lifecycle log
  so the next time someone reports "退出" we have ground
  truth instead of guessing from macOS Console.
- **`RunEvent::ExitRequested` arm** (in
  `awl-desktop::run`) intercepts Cmd+Q / Dock Quit /
  `app.exit(0)` and gates it on
  `awl_core::lifecycle::quit_confirmed()`. First quit
  attempt: log `exit-requested`, `api.prevent_exit()`,
  log `exit-prevented`, emit `show-quit-confirm` to
  the frontend. The frontend surfaces a Radix dialog
  ("退出后,每 5 分钟一次的 launchd 自动扫描会停止").
  On "是的,退出" the `confirm_quit` IPC sets the flag +
  writes `exit-confirmed`, and the frontend calls
  `await exit(0)` from `@tauri-apps/api/app`. The next
  `RunEvent::ExitRequested` finds the flag set, writes
  `exit-allowed`, and the runtime actually exits.
- **`lifecycle.log`** at
  `~/Library/Application Support/com.minimaxcode.agentworkledger/lifecycle.log`
  (1 MiB rolling rotation, single generation). Lines
  look like `2026-10-18T10:42:13Z\texit-requested`. The
  module lives in `awl-core` (not `awl-desktop`) so
  `awl-ipc` and `awl-desktop` can share the flag and
  the log helper without a workspace dependency
  cycle. Writes are best-effort; a disk full or
  read-only volume must never block startup.
- **`window-hidden` event + amber banner.** The
  `WindowEvent::CloseRequested` arm was already
  correctly gating the close-button path (window
  hides, app keeps running, tray icon stays), but
  users saw the dock icon disappear and assumed a
  crash. v0.5.6 emits `window-hidden` and the
  frontend renders an amber banner: "窗口已隐藏到
  菜单栏 — 点击右上角菜单栏图标重新打开。扫描仍在
  后台运行。" Auto-dismisses after 6 s.
- **`confirm_quit` IPC** in `awl-ipc` is a tiny
  command: `mark_quit_confirmed()` + log
  `exit-confirmed`. No `AppHandle` parameter (which
  would force a Tauri runtime feature onto `awl-ipc`
  just for one call) — the actual `app.exit(0)` is
  dispatched by the frontend via
  `import { exit } from '@tauri-apps/api/app'`
  immediately after the IPC returns. Capability
  `core:app:allow-exit` added to
  `crates/awl-desktop/capabilities/default.json`.
- 3 new tests in `awl-core::lifecycle::tests`:
  `log_event_writes_line_with_timestamp`,
  `rotate_renames_log_when_over_cap`, and
  `quit_confirmed_flag_round_trip`. 149 total pass
  (was 146).
- Frontend changes touch `App.tsx` (the lifecycle
  event listeners via `listen()` from
  `@tauri-apps/api/event`, the confirm dialog using
  the existing Radix `Dialog` component, the
  hidden-hint banner). No DB schema change.

## v0.5.5 — 2026-10-18

- **Per-source cleanup (`delete_source` IPC + Settings card).**
  The audit of the `sources` table (run while preparing v0.5.4's
  release notes) found 3 stale `markdown_summary` rows from
  dev/test sessions — no `root_path`, no `last_scan_at`, 0
  events. Without an IPC the only way to clean them was a direct
  `sqlite3 ledger.db "DELETE FROM sources WHERE id = ..."`,
  fine for me, hostile for the user. The new affordance:
- **`awl_ipc::delete_source(state, id: String) -> AppResult<usize>`**
  is the entire handler — `DELETE FROM sources WHERE id = ?1`.
  Returns `usize` so the UI can distinguish "已删除" from
  "找不到该数据源" without a second SELECT. The schema's FK
  CASCADE does the rest (`raw_events` / `sessions_summary` /
  `connector_runs` cascade directly; `plan_items.source_session_id`
  cascades through the deleted `sessions_summary`).
- **New "数据源管理" card on `pages/Settings.tsx`**, between
  本地配置 and 数据保留. Shows every registered source with
  `kind` + `display_name` + a `stale` badge when both
  `root_path` and `last_scan_at` are empty. Per-row 删除 button
  opens a Radix confirm dialog (mirrors the existing
  "重置全部事件" pattern). The list is rendered in full rather
  than filtered out — hide = invisibly broken, the user has
  to *see* the stale row to delete it.
- **We deliberately never auto-prune.** The Connectors scanner
  creates a `sources` row on first discovery; deleting one is a
  manual user action. A future scan that re-discovers the same
  `kind` + `root_path` fingerprint recreates a fresh row, but
  this is a "delete this row, it was junk" affordance, not a
  regular workflow.
- 2 new tests in `awl-db::tests` cover the FK CASCADE contract:
  `delete_source_cascades_to_children` proves deleting one
  source removes its `raw_events` / `sessions_summary` /
  `connector_runs` while a sibling source's rows survive;
  `delete_source_unknown_id_is_noop` proves an unknown id
  returns 0 rows affected so the IPC can map cleanly to the
  UI. 146 total pass (was 144).
- Frontend changes touch `lib/types.ts` (new `DeleteSourceReport`)
  and `pages/Settings.tsx` (新的 card + dialog + 横幅 + 把
  `list_sources` 加到 `refreshAll`). No DB schema change; the
  FK CASCADE that makes this work was already in place from
  v0.5.0's migration.

## v0.5.4 — 2026-10-18

- **Cross-source search polish.** The Cmd-K search overlay
  (`awl-core::search::global_search`, IPC `global_search`,
  frontend `pages/Search.tsx`) gets three meaningful upgrades:
- **Multi-token AND.** A query is now split on whitespace and
  every token must hit at least one searchable column on the
  row. `doubao events` finds rows mentioning both, not rows
  mentioning either alone. UTF-8 tokenization is per-char so
  Chinese queries work token-by-token without an extra
  CJK-segmentation dependency.
- **`<mark>` snippet highlighting.** Each match is wrapped in
  `<mark>...</mark>` inside the snippet. The snippet itself is
  HTML-escaped by the backend so `&`/`<`/`>`/quotes from user
  payloads are safe to render; only the marker tags are
  active markup. The frontend renders snippets via
  `dangerouslySetInnerHTML` and styles `<mark>` with Tailwind
  arbitrary descendants (`[&_mark]:bg-amber-100 …`).
- **`SearchResult` envelope.** `global_search` now returns
  `{ hits, counts, elapsed_ms }`. `counts` carries the per-
  source total BEFORE the per-source LIMIT cap so the UI can
  render "会话 12 · 事件 47 · 规划 3 · 数据源 0"; `elapsed_ms`
  is the wall time spent on the SQL for the footer "用了 47
  ms".
- 8 unit tests in `search::tests` cover LIKE escaping (incl.
  HTML meta-chars), build_or_clause AND-within-OR shape,
  html_escape, the highlight round-trip (markers wrap
  original-case text, idempotent after `strip_html_marks`),
  char-boundary aware Chinese truncation, and the per-token
  hit counter.
- Frontend changes touch `types.ts` (new `SearchCounts` +
  `SearchResult` types) and `pages/Search.tsx` (response
  shape, header counts line, highlight render). No DB schema
  change, no IPC contract break for callers other than the
  search page itself.
- 156 total pass (was 148).

## v0.5.3 — 2026-10-18

- **Hotfix — macOS LaunchAgent plist after workspace split.**
  v0.5.0 split `awl-desktop` out as a separate crate and renamed
  the binary from `agent-work-ledger` to `awl-desktop`. The
  plist that was loaded into `~/Library/LaunchAgents/` for the
  per-5-minute auto-rescan was never rewritten, so launchd tried
  to spawn a binary that no longer existed. Every 5 minutes the
  rescan failed silently — no events were ingested by the
  background scanner, only by the manually-triggered `--scan-once`
  or by an active GUI session. Fixed by adding two CLI flags:
- **`--install-launchd`** — `awl-desktop --install-launchd` boots
  out any existing agent with our label (success or failure),
  deletes the existing plist files, writes a fresh scanner +
  brief pair to `~/Library/LaunchAgents/`, and bootstraps both
  under the user's GUI launchd domain. Re-running is safe and
  idempotent. Plist templates are embedded via `include_str!`
  with `__BINARY_PATH__` substituted to `argv[0]`'s canonical
  path (symlink-resolved absolute form).
- **`--uninstall-launchd`** — `awl-desktop --uninstall-launchd`
  reverses the install. Idempotent.
- 5 new tests in `awl-desktop::launchd::tests`: the canonical
  placeholder survives in both templates, substitution replaces
  only the `<string>` token (header comment is left intact),
  canonical token is present in both templates, `gui_domain`
  always returns `gui/<UID>` shape, and `install("")` is
  rejected. 148 total pass (was 143).
- **One-line user action after upgrading**: open a Terminal
  and run `/Applications/Agent\ Work\ Ledger.app/Contents/MacOS/awl-desktop
  --install-launchd`. After that, `tail -F
  /tmp/agent-work-ledger.scanner.out.log` should show a tick
  every 5 minutes with `scan-once: complete, success`.

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
