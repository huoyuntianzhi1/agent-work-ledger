# Changelog

All notable changes to Agent Work Ledger are documented here.
Versions follow `vX.Y.Z` where `X` is a breaking schema change,
`Y` is a notable feature, and `Z` is a polish / bugfix.

## Unreleased

- (placeholder — see git history for the actual entries)

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
