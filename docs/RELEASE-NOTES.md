# Agent Work Ledger v0.1.0 — release notes

Local-first macOS menu-bar app that aggregates work artifacts from every
agentic CLI / IDE running on this machine into a single timeline, then
turns them into a daily / weekly Eisenhower plan with local-Ollama-
assisted classification.

Tested on Apple Silicon (aarch64), macOS 15+.

## Headline

- **Seven connectors reading real data on day one** (Codex, Claude
  Code, OpenCode, WorkBuddy, Doubao, Doubao Work, MiniMax Agent)
  — typical ~10k raw events.
- **launchd LaunchAgent** — re-ingests every 5 minutes in the
  background even when the menu-bar app window is closed.
- **Eisenhower 4-quadrant planner** with day / week templates and
  local-Ollama-assisted quadrant suggestion.
- **Local-first AI** — defaults to `http://localhost:11434`, prompts
  are redacted, cloud providers intentionally not implemented.

## Privacy stance

- **Read-only ingest.** Connectors never write back to the source
  directory.
- **API keys are redacted** (OpenAI `sk-*`, Anthropic, Bearer, JWT,
  32-char hex) before any payload is written to the database.
- **No cloud roundtrip.** The Ollama client is wired to
  `localhost:11434`; only an opt-in toggle can disable prompt
  redaction, and only trusted local models should ever see it.

## Connectors

| kind | source | notes |
|---|---|---|
| `codex` | `~/.codex/` | zstd-compressed JSONL + 6 SQLite buckets |
| `claude_code` | `~/.claude/projects/` | plaintext JSONL + stats cache |
| `opencode` | `~/.local/share/opencode/opencode.db` | read-only SQLite, every table |
| `workbuddy` | `~/Library/Application Support/WorkBuddyExtension/` | check-point + plan-task meta.json |
| `doubao` | `~/Library/Application Support/Doubao/Default/IndexedDB/` | Chromium IDB across 11 roots |
| `doubao_work` | `~/Library/Application Support/DoubaoWork/Default/IndexedDB/` | same Chromium IDB layout |
| `minimax_code` | `~/Library/Application Support/MiniMax Agent/IndexedDB/` | Chromium IDB telemetry |
| `trae` | `~/Library/Application Support/Trae/IndexedDB/` | **planned** — only metadata rows |
| `codebuddy_cn` | `~/Library/Application Support/CodeBuddy CN/codebuddy-sessions.vscdb` | **planned** — empty table |

## Architecture notes

- `app/src-tauri/src/idb.rs` — thin FFI over system `libleveldb`
  (`brew install leveldb`). No `leveldb` crate — that one pulls in
  cmake + snappy and fails on Apple Silicon. Comparators must use
  bytewise `memcmp` (`idb_cmp1`); `strcmp` truncates at NUL inside
  UTF-16BE store-name bytes. Chromium IndexedDB LevelDB keys:
  `byte 0 = 0x00`, `byte 1 = database_id` (single byte, observed
  0x00..0x03, NOT a fixed `0x01` magic), then schema/object-store
  id, then `byte 6+` is the user-supplied primary key in UTF-16BE.
  See `docs/01-connectors-feasibility.md` appendix C.
- `app/src-tauri/src/ai.rs` — local Ollama HTTP client built on
  `std::net::TcpStream` + `serde_json`. Zero new dependencies.
- `app/src-tauri/src/planner.rs` — Eisenhower tables
  (`plan_templates`, `plan_items` with `quadrant` `CHECK BETWEEN 1
  AND 4`, cascade delete from template).
- `app/launchd/com.minimaxcode.agentworkledger.scanner.plist` —
  `StartInterval=300`, `LimitLoadToSessionType=Aqua`, `RunAtLoad=true`,
  `StandardOutPath/StandardErrorPath=/tmp/agent-work-ledger.scanner.*.log`.

## Schema

`SCHEMA_VERSION = 3`. The migrate() function is idempotent:

- v1 — sources / raw_events / sessions / work_spans / tasks /
  evidence / reports / connector_runs / privacy_rules
- v2 — `payload_hash UNIQUE INDEX` for dedup
- v3 — `plan_templates` + `plan_items` (+ index + cascade FK)

## Build artifacts

```
app/src-tauri/target/release/bundle/macos/Agent Work Ledger.app
app/src-tauri/target/release/bundle/dmg/rw.39817.Agent Work Ledger_0.1.0_aarch64.dmg
```

The .app is not codesigned in this build (the host has a
`wechat-decrypt` identity but no Developer ID for this bundle). For
personal / non-distribution use the unsigned binary works as-is; for
Gatekeeper-strict environments, sign with a Developer ID first.

## Tests

- `cargo test --lib` — 33 tests (idb helpers, redactor, ai module,
  planner module, schema / defaults)
- `cargo test --test ai_integration` — 3 tests (graceful Ollama
  offline, settings round-trip, defaults)
- `cargo test --test minimax_integration` — 1 ignored (real IDB; run
  without `--ignored` to skip)
- `cargo test --test doubao_integration` — 1 ignored (real IDB; same)
- `cargo test --test planner_integration` — 3 tests (default
  templates, cascade delete, completed_at timestamp)

End-to-end (manual):

- `Agent Work Ledger.app --scan-once` → 7/7 connectors ok, exit 0
- launchctl bootstrap + RunAtLoad → first run within 5 seconds,
  StartInterval tick every 300 seconds, both write to
  `/tmp/agent-work-ledger.scanner.out.log`

## Known limitations

- The Chromium IDB connector emits one `session_meta` event per
  stored record. Decoding the V8 IndexedDB binary value format
  (extracting real chat messages / tool calls) is a future stage.
- The Ollama classifier falls back to a keyword heuristic when the
  daemon is unreachable; the heuristic defaults unknown items to
  quadrant 4 (Drop / park) so the user consciously promotes them
  rather than auto-scheduling noise.
- `Trae` and `CodeBuddy CN` connectors are intentionally marked
  `planned` — the local data exists but contains no actionable
  agent artifacts yet.

## Known gaps for v0.2

- Source-link from plan_items to raw_events is implemented
  (`source_event_id` FK + helper) but no UI surface to promote an
  event into an item yet.
- Cloud providers are deliberately absent; adding one will require
  a prompt-redaction toggle that defaults to *always on* for any
  cloud backend.
- No automatic schema-version-aware upgrade for users on an older
  DB. The migration is idempotent but does not back-fill new tables
  for users who installed pre-v3.