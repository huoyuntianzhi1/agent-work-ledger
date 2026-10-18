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

## Connectors (v0.4.8)

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

## Architecture (one screen, v0.4.8)

- **Tauri 2 shell** — single window, webview, native menu
- **Rust core** in `app/src-tauri/src/`:
  - `lib.rs` — DB schema migration, IPC surface, background loop, CLI
    flags (`--scan-once`, `--brief-once`)
  - `connectors/` — one module per agent
    (`codex.rs`, `claude.rs`, `opencode.rs`, `workbuddy.rs`,
    `doubao.rs`, `minimax.rs`, `trae.rs`, `cursor.rs`,
    `markdown_summary.rs`)
  - `redactor.rs` — privacy pre-filter (OpenAI sk-* / Anthropic /
    Bearer / JWT / 32-char hex) + DB-driven custom rules
  - `idb.rs` — thin FFI over `libleveldb` for Chromium IndexedDB
  - `etl.rs` — raw_events → sessions_summary aggregation
  - `search.rs` — payload-aware FTS-style event search
  - `brief.rs` — top-3 Q1 morning brief + source rollup + day-over-day
    delta (v0.4.2)
  - `eisenhower.rs` — 4-quadrant matrix with heuristic + LLM blend
  - `planner.rs` — daily / weekly plan templates
  - `export.rs` — JSON / CSV / `.zip` (single .md per partition +
    `INDEX.md`) (Stage 5.5)
  - `retention.rs` — 90-day raw_events auto-prune (v0.4.3)
  - `window_state.rs` — cross-launch window position persistence,
    skipped for `--scan-once` / `--brief-once` (v0.4.6)
  - `app_settings.rs` — settings.json (Prompt library + retention +
    redact flags + summary dir)
  - `ai.rs` — local Ollama HTTP/1.1 client (zero new deps)
- **React + TS** UI in `app/src/` + `app/src/pages/` (Home, Timeline,
  Sessions, Planner, Settings)
- **SQLite (rusqlite)** in WAL mode at
  `~/Library/Application Support/com.minimaxcode.agentworkledger/ledger.db`

Schema is versioned; `SCHEMA_VERSION = 7` (v0.4.8).

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
├── app/
│   ├── launchd/                        — launchd plist template
│   ├── src/                            — React + TS UI
│   │   ├── App.tsx                     — home + Settings cards
│   │   ├── pages/                      — Home / Timeline / Sessions / Planner / Settings
│   │   └── components/                 — reusable UI primitives
│   └── src-tauri/                      — Rust core
│       └── src/
│           ├── ai.rs                   — Ollama HTTP client + settings
│           ├── connectors/             — per-agent ingest modules (10+)
│           ├── error.rs / redactor.rs / idb.rs / lib.rs / planner.rs
│           ├── brief.rs / eisenhower.rs / export.rs / search.rs
│           ├── retention.rs / window_state.rs / app_settings.rs
│           └── tests/                  — integration tests
├── connectors/                         — legacy C PoC tools (kept for re-verification)
├── docs/
│   ├── 00-product-plan.md
│   ├── 01-connectors-feasibility.md    (appendix C = Chromium IDB key schema)
│   └── RELEASE-NOTES.md
└── scripts/
```

## Roadmap (next)

- **Stage 6-P1 / SQLCipher unlock** — decrypt TRAE SOLO's
  `ai-agent/database.db` with the user's keychain entry. This is the
  last big gap on the connector side.
- **Cargo workspace split** — `connectors-core` / `db` /
  `ipc-commands` / `export` crates. The single-crate `lib.rs` is now
  >2 500 lines.
- **OSS launch** — first public GitHub release once Stage 6-P1 ships.