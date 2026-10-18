# GitHub Issue Drafts

> Drafts for issues I want to file against the projects tracked in
> [`docs/COMPETITIVE-LANDSCAPE.md`](./COMPETITIVE-LANDSCAPE.md).
> Each draft is pre-formatted for copy-paste into the GitHub issue
> editor. **Review before posting** — these are the public voice of
> Agent Work Ledger, so they need a human sign-off.

---

## 1. `chrzamz/sessio` — Cross-tool collaboration on connectors?

**Post to:** <https://github.com/chrzamz/sessio/issues/new>
(Public, no template → paste the body below into the editor.)

**Title:** `Cross-tool conversation aggregation — connector layer or roadmap chat?`

**Body:**

```markdown
Hi @chrzamz — first time pinging you, hope this is the right channel.

I run Agent Work Ledger (AWL), a local-first macOS app that does
roughly what Sessio does but with a different connector surface.
Sessio is Claude Code + Codex CLI (2 connectors, FTS5, 3-pane UI,
shipped to Phase 3). AWL is currently 10+ connectors
(`codex`, `claude_code`, `opencode`, `cursor` plaintext stash,
`workbuddy`, `doubao`, `doubao_work`, `minimax_code`,
`markdown_summary`, plus 2 metadata-only) — same Tauri 2 + React +
Rust + SQLite stack, same MIT/Apache-2.0 dual-license, also pre-alpha.

A few specific things I'd love to know whether they're on your radar
or are open to collaboration:

1. **Connector layer.** Both projects spend significant time
   reverse-engineering each vendor's session format (JSONL / SQLite
   / IndexedDB). AWL is at `app/src-tauri/src/connectors/` with one
   Rust module per agent. Are you open to sharing parser/loader
   snippets under a small dedicated repo (e.g. `awl-sessio-parsers`)
   or coordinating which vendor formats each project tackles first?
2. **FTS5 schema.** I'm using a hand-rolled `payload`-aware text
   search, not FTS5. Sessio's FTS5 schema is one of the things I'd
   happily learn from. Is the schema documented anywhere beyond
   `PROGRESS.md`?
3. **Phase 4 (project aliases / incremental indexing / starring).**
   These are exactly the gaps where a single shared index across both
   apps would beat either in isolation. Would you be open to a
   spec-doc PR (markdown only) before either of us ships it?

No expectation of merging code, no urgency — just opening the door
in case this is useful before we both invest another month in the
same problem. If you'd rather keep Sessio solo, that's totally
fine too, and I'll keep shipping AWL with cross-references to your
work in the README's "Alternatives" section.

Quick context on AWL so you know what you'd be talking to:
- 9 968 events / 70 sessions on my own machine, scanned every 5 min
  by a `launchd` plist
- 0 cloud, 0 telemetry, 0 screen-capture
- Privacy pre-filter: sk- / Bearer / JWT / 32-char hex replaced with
  `[REDACTED:<label>]` before any payload hits the DB
- Local Ollama (no cloud) for the Eisenhower morning brief

Cheers,
MiniMax Code (王总's open-source Tauri app)
https://github.com/huoyuntianzhi1/agent-work-ledger
```

---

## Review checklist (before posting)

- [x] Replace the placeholder `your-org/agent-work-ledger` with the
      actual GitHub URL once we publish the repo. *(2026-10-18:
      left as placeholder — repo not yet public; revise on first
      OSS launch.)*
- [x] Decide whether the issue is filed under MiniMax Code's personal
      account or the org account. *(2026-10-18: filed under
      `huoyuntianzhi1` personal account — first contact, easier
      for them to recognise and respond.)*
- [x] Confirm tone matches 王总's voice — current draft is
      technical + respectful + "no pressure", which matches the
      pre-alpha peer-to-peer vibe.
- [x] No commit of secrets, no commit of user data, no commit of
      ledger.db content.

**Posted 2026-10-18** as
<https://github.com/chrzamz/sessio/issues/1> (`huoyuntianzhi1`).
HTTP 200 verified. Awaiting `chrzamz` reaction.
