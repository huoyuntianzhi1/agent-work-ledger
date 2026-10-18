# Agent Work Ledger

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE-MIT)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE-APACHE)
[![Platform: macOS](https://img.shields.io/badge/platform-macOS%20(Apple%20Silicon)-lightgrey)](#)
[![Tauri](https://img.shields.io/badge/Tauri-2-blueviolet)](https://tauri.app)

Local-first macOS desktop app that aggregates every artifact the
agentic CLIs and IDEs on this machine produce into a single searchable
timeline, then turns it into a daily / weekly Eisenhower plan with
local-Ollama-assisted classification.

> No cloud roundtrip. No screen capture. All ingest is read-only against
> the agent's own data directory; API keys are redacted before they hit
> the database.

## Why this exists (v0.4.8)

If you work with Codex, Claude Code, OpenCode, Cursor, WorkBuddy, or any
of the half-dozen VSCode-fork AI IDEs now shipping, every one of those
apps keeps its own private JSONL / SQLite / IndexedDB stash of your
work. The transcripts that prove what you shipped yesterday are split
across 6-10 separate opaque folders, and there is no way to ask "what
did I work on last Tuesday?" across all of them at once.

Agent Work Ledger is the cross-tool timeline. One SQLite file, one
search bar, one daily brief — for **every** agent on your machine, not
just the one you happen to be running right now.

**What you can do today on this machine** (production, 9 968 events /
70 sessions captured):

- Search every conversation across every connector (Codex, Claude
  Code, OpenCode, Cursor, WorkBuddy, Doubao, MiniMax Agent, …) with
  one query.
- Open the morning card and see "Q1 dominated by `cursor` (3) and
  `codex` (2) this morning, +2 vs yesterday" before you open Slack.
- Export any session as Markdown, or every session as a single
  bundled `.zip` with a top-level `INDEX.md`.
- Run the scanner as a `launchd` LaunchAgent that re-runs every 5
  minutes even when the app window is closed.

## What's different from the alternatives

I wrote Agent Work Ledger because nothing in the same space does what I
actually want on a single machine. Concretely:

- **10+ connectors, not 1-2.** Codex, Claude Code, OpenCode, Cursor
  (plaintext stash), WorkBuddy, Doubao, Doubao Work, MiniMax Agent,
  markdown-summary, and Trae/CodeBuddy CN metadata. Every connector
  is one Rust module in `app/src-tauri/src/connectors/`. A new IDE
  ships in roughly 200 lines when its data shape is readable.
- **A real production database, not a demo.** 9 968 events / 70
  sessions / 0 JWT leaks on this machine, scanned nightly by a
  `launchd` plist. The read paths are exercised against the real
  data, not synthetic fixtures.
- **Local Ollama, no cloud.** Eisenhower classification runs against
  `http://localhost:11434`; if Ollama is offline the heuristic
  fallback keeps the brief useful, and prompts are redacted through
  the same regex pre-filter as the imported payloads.
- **Privacy-first ingest.** The connectors are `read-only` against the
  agent's own data dir. API keys (`sk-…`, `AKIA…`, `Bearer …`, JWT,
  32-char hex) are replaced with `[REDACTED:<label>]` **before** any
  payload hits SQLite. Cloud providers get the redactor bolted on at
  the IPC layer; no LLM provider call ever sees a raw secret.

## Connectors (v0.5.1 — 12 sources)

> Twelve sources as of v0.5.1. The latest addition is
> `trae_solo`, which decrypts a SQLCipher-4 database
> (`~/Library/Application Support/TRAE SOLO/ModularData/ai-agent/database.db`)
> using the key held in the macOS keychain under service
> `TRAE SOLO Safe Storage`. The first ingest per session
> triggers a one-time GUI prompt; clicking "Always Allow"
> is enough to make ingest headless from then on.

| Connector | Source path | Status | Last seen |
|---|---|---|---|
| `codex` | `~/.codex/` (zstd + SQLite) | ✅ events | v0.1.0 |
| `claude_code` | `~/.claude/projects/` | ✅ events | v0.1.0 |
| `opencode` | `~/.local/share/opencode/opencode.db` | ✅ events | v0.1.0 |
| `doubao` | `~/Library/Application Support/Doubao/Default/IndexedDB/` | ✅ events | v0.1.0 |
| `doubao_work` | `~/Library/Application Support/DoubaoWork/Default/IndexedDB/` | ✅ events | v0.1.0 |
| `minimax_code` | `~/Library/Application Support/MiniMax Agent/IndexedDB/` | ✅ events | v0.1.0 |
| `workbuddy` | `~/Library/Application Support/WorkBuddyExtension/` | ✅ events | v0.1.0 |
| `markdown_summary` | `~/Library/Application Support/…/summary-*.md` (YAML frontmatter) | ✅ events | v0.3.0 |
| `cursor` | `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` | ✅ events (plaintext stash) | v0.4.5 |
| `trae` | `~/Library/Application Support/Trae/IndexedDB/` | ⚠ metadata only | v0.1.0 |
| `codebuddy_cn` | `~/Library/Application Support/CodeBuddy CN/codebuddy-sessions.vscdb` | ⚠ metadata only | v0.1.0 |
| `trae_solo` | `~/Library/Application Support/TRAE SOLO/ModularData/ai-agent/database.db` (SQLCipher-4) | ✅ events (keychain-gated) | v0.5.1 |

**Cursor plaintext stash (v0.4.5)** is the standout capability: Cursor
stores its chat history unencrypted in a SQLite file at
`~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
(127 `composerData:<uuid>` rows). Every bubble (user prompt and
assistant reply) is one `raw_event` row in the ledger, with the
project path pulled from `relevantFiles[0]`.

## Run it (dev)

```sh
cd app
pnpm install
pnpm tauri dev
```

The dev launch auto-rescans every supported connector on startup and
re-runs every 5 minutes while the app is open. From the Home page you
can install a `launchd` LaunchAgent that re-runs the same scan even
when the window is closed.

## Build the release `.app`

```sh
cd app
pnpm tauri build
```

Artifacts:

```
app/src-tauri/target/release/bundle/macos/Agent Work Ledger.app
app/src-tauri/target/release/bundle/dmg/rw.<rev>.Agent Work Ledger_0.4.8_aarch64.dmg
```

## Architecture (one screen, v0.5.0)

- **Tauri 2 shell** — single window, webview, native menu
- **Rust core** split into 5 crates (Track A of v0.5.0, see
  `docs/V0.5.0-CHANGELOG.md`) under `app/src-tauri/crates/`:
  - `awl-core/` — pure domain logic.
    - 12 modules: `ai.rs` (Ollama client), `app_settings.rs`
      (settings I/O), `brief.rs` (morning top-3), `eisenhower.rs`
      (4-quadrant matrix), `error.rs` (one `AppError` shared
      across crates), `etl.rs` (ingest pipeline), `idb.rs`
      (LevelDB FFI), `planner.rs` (promote + templates),
      `redactor.rs` (3 API-key patterns + cloud gate),
      `reports.rs` (HTML templates), `search.rs` (payload FTS),
      `summary.rs` (heuristic fallback)
    - 10 connectors: `claude`, `codex`, `cursor`, `doubao`,
      `markdown_summary`, `minimax`, `opencode`, `trae`,
      `workbuddy`, plus `mod.rs` (12th)
    - `build.rs` runs linker-level search for the LevelDB
      symbols that Cursor's `state.vscdb` plaintext stash
      depends on
  - `awl-db/` — schema v4 + `migrate` + `backup_ledger_pre_v4` +
    `is_v3_schema` + `migrate_for_tests` + 6 schema tests +
    `retention.rs` (`DEFAULT_RETENTION_DAYS = 90`).
    **Dependencies**: `awl-core` (for `redactor` +
    `etl` during ingest).
  - `awl-export/` — bulk + zip export. Same shape as v0.4.8's
    `export.rs`; tests pull `awl-db` in via a dev-only dep.
  - `awl-ipc/` — every `#[tauri::command]` (47 total) plus
    `run_scan_once_mode` / `run_brief_once_mode` /
    `auto_rescan_all` / `background_scan_loop` /
    `reset_events` / launchd install + uninstall. Exposes a
    `pub fn register<R>(builder)` wrapper around
    `tauri::generate_handler![...]` so the 47 commands can
    stay private to the crate without colliding on the
    macro-generated `__cmd__*` symbols. **Dependencies**:
    `awl-core` + `awl-db` + `awl-export`.
  - `awl-desktop/` — `pub fn run` (Tauri 2 `Builder::default`
    glue + `setup` + system tray + window event handlers),
    `main.rs` (binary entry, parses `--scan-once` /
    `--brief-once` before `run`), and `window_state.rs` (lifted
    from the old `awl-db` because it needs `tauri::WebviewWindow`).
    **Dependencies**: `awl-core` + `awl-db` + `awl-ipc` +
    `awl-export`.
- **Cycle-breaking decisions** (full discussion in
  `docs/V0.5.0-CHANGELOG.md`):
  - `app_settings.rs` lives in `awl-core`, not `awl-db`
    — both connectors and the migration need it, so putting
    it in `awl-core` keeps the dep graph a DAG.
  - `window_state.rs` lives in `awl-desktop`, not `awl-db`
    — it needs `tauri::WebviewWindow`.
  - `awl_core::app_settings::DEFAULT_EVENT_RETENTION_DAYS = 90`
    and `awl_db::retention::DEFAULT_RETENTION_DAYS = 90` are
    duplicated by intent and held in lock-step by
    cross-pointing code comments.
- **React + TS** UI in `app/src/` + `app/src/pages/` (Home, Timeline,
  Sessions, Planner, Settings)
- **SQLite (rusqlite)** in WAL mode at
  `~/Library/Application Support/com.minimaxcode.agentworkledger/ledger.db`

Schema is versioned; `SCHEMA_VERSION = 4`. The 5-crate split
shipped in v0.5.0 is a pure refactor — same schema version,
same IPC surface, same permissions.

## Privacy

- All ingest is **read-only** — the connectors never write back to
  the source dir.
- API keys (`sk-…`, `AKIA…`, `Bearer …`, JWT, 32-char hex) are
  detected by regex and replaced with `[REDACTED:<label>]` **before**
  any payload hits the database.
- The Ollama integration defaults to `http://localhost:11434`. Prompts
  are passed through the same redactor; toggling `redact_prompts = false`
  is opt-in only.
- No cloud provider is wired up. When one is added, every prompt will
  run through `Redactor::global()` first.

## Tests

```sh
cd app/src-tauri
cargo test --lib                          # 136 lib tests
cargo test --test capabilities_lint       # 2 capability lint tests
cargo test --test ai_integration          # graceful Ollama-offline handling
cargo test --test planner_integration     # migrate + cascade + status
cargo test --test search_integration      # payload preview extraction
```

138 active tests pass on every release commit. Integration tests for
Chromium IndexedDB are opt-in via a real IDB on the host and ignored
in CI.

## License

Dual-licensed under either of:

- [MIT](./LICENSE-MIT) — permissive, shortest, most-used
- [Apache-2.0](./LICENSE-APACHE) — permissive + explicit patent grant

Pick whichever you prefer; this is the same dual-licensing shape that
Tauri itself uses. New contributions are accepted under both.

## Repository layout

```
Agent Work Ledger/
├── LICENSE-MIT                         — MIT text
├── LICENSE-APACHE                      — Apache 2.0 text
├── README.md                           — this file
├── CHANGELOG.md                        — canonical release log
├── CONTRIBUTING.md / CODE_OF_CONDUCT.md / SECURITY.md
├── app/
│   ├── src/                            — React + TS UI
│   │   ├── App.tsx                     — home + Settings cards
│   │   ├── pages/                      — Home / Timeline / Sessions / Planner / Settings
│   │   └── components/                 — reusable UI primitives
│   └── src-tauri/                      — Rust core (Cargo workspace, v0.5.0+)
│       ├── Cargo.toml                  — workspace root, members = 5 crates
│       └── crates/
│           ├── awl-core/               — pure domain (12 modules + 10 connectors)
│           ├── awl-db/                 — schema v4 + migrate + retention
│           ├── awl-export/             — bulk + zip export
│           ├── awl-ipc/                — 47 #[tauri::command] + launchd glue
│           ├── awl-desktop/            — binary entry + Builder + tray + window state
│           │   ├── tauri.conf.json      — bundle / icons / launchd resources
│           │   ├── capabilities/       — plugin permission set
│           │   └── icons/               — full macOS icon set
│           └── launchd/                — LaunchAgent plist templates (bundled as tauri resources)
├── connectors/                         — legacy C PoC tools (kept for re-verification)
├── docs/
│   ├── 00-product-plan.md
│   ├── 01-connectors-feasibility.md    (appendix C = Chromium IDB key schema)
│   ├── RELEASE-NOTES.md                — local-era release notes (pre-git)
│   ├── ISSUES_DRAFTS.md                — pre-written GitHub issue drafts
│   ├── OSS-LAUNCH.md                   — public-launch runbook
│   └── V0.5.0-CHANGELOG.md / V0.5.0-ROADMAP.md — Track A + Track B design
├── .github/
│   ├── ISSUE_TEMPLATE/                 — bug_report.md + feature_request.md
│   └── PULL_REQUEST_TEMPLATE.md
└── scripts/
```

## Roadmap (next)

- **OSS visibility** (shipped v0.4.10.1) — live at
  <https://github.com/huoyuntianzhi1/agent-work-ledger>. Stars,
  watchers, and one external PR will gate the next milestone.
- **Upstream collaboration** (in flight) — issue
  [#1 on `chrzamz/sessio`](https://github.com/chrzamz/sessio/issues/1)
  (the parallel Rust agent-ledger project) has been opened
  proposing connector-layer interop, FTS5 schema knowledge-share,
  and a co-spec for Phase 4 (project aliases / incremental
  indexing / starring). No merge expectation.
- **Track B (shipped v0.5.1)** — TRAE SOLO SQLCipher unlock
  via the `TRAE SOLO Safe Storage` keychain entry, with a
  one-time GUI prompt on first ingest. Connector landed
  (see `awl-core/src/connectors/trae_solo.rs`); schema
  discovery auto-tries both Chrome salt variants
  (`"peanuts"` for Chrome 80+ and `"saltysalt"` for the
  legacy Chromium v10).