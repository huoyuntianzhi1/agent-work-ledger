---
name: Feature request
about: A new connector, a new IPC, a new UI affordance
title: "[feature] "
labels: enhancement
assignees: ''
---

## What you want

One-sentence description of the change.

## Why

What problem does it solve? Is there a real workflow today that
the current AWL doesn't handle? If you can name a specific
connector or session that the new feature would unlock, that
helps prioritise.

## How you'd verify it

What's the minimum that proves the feature works? A new IPC
test? A screenshot of the home page card? A scan-once log
showing the new source?

## Affected modules (best guess)

- [ ] `app/src-tauri/src/connectors/` (which one?)
- [ ] `app/src-tauri/src/idb.rs` (Chromium IDB path)
- [ ] `app/src-tauri/src/redactor.rs` (privacy pre-filter)
- [ ] `app/src-tauri/src/ai.rs` (Ollama)
- [ ] `app/src-tauri/src/brief.rs` / `eisenhower.rs` / `planner.rs`
- [ ] `app/src/pages/`
- [ ] `app/src-tauri/capabilities/default.json` (Tauri 2)
- [ ] `docs/ISSUES_DRAFTS.md` (if it's another OSS project)

## Are you offering to send the PR?

Yes / no / "let me see if I can carve out a weekend". Either
is fine — even a "no, I just want it on the record" is useful
for the maintainer's prioritisation.
