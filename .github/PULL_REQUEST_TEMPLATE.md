## What

One-sentence summary of the change. Use the imperative mood
("Add X", "Fix Y", not "Added X" / "Fixed Y").

## Why

The user-visible problem this solves, or the gap it closes.
Cite an issue number if there is one.

## How

Brief description of the design choice. If you added a
connector, paste the `--scan-once` log excerpt that proves
it pulls real events. If you changed the schema, paste the
upgrade path. If you touched a Tauri 2 capability, paste the
diff to `tests/capabilities_lint.rs`.

## Test plan

- [ ] `cargo test` green
- [ ] `pnpm tsc --noEmit` (or `pnpm build`) green
- [ ] If you added a new IPC, exercised it from the
      corresponding `app/src/pages/*.tsx` page
- [ ] If you changed the bundle, re-signed the .app and
      verified it boots

## Privacy check

- [ ] Any new payload path runs through `Redactor::global()`
- [ ] No new outbound network calls (Ollama on
      `localhost:11434` only, or local-only)
- [ ] No new files written under the connector source dirs

## Issue / context

Closes #<n>. References #<n>. Screenshots / logs / session
IDs go below.
