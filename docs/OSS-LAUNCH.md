# OSS Launch — Agent Work Ledger

> Runbook for the first public push of the Agent Work Ledger repo.
> Everything in this file is reversible until the final `git push`,
> which is the one step only the maintainer (王总 / `huoyuntianzhi1`)
> should run.

## Status as of v0.4.10

The repo is OSS-ready. Everything below the `---` is the only
remaining work, and it is a one-liner plus the human decision on
visibility.

| Asset | Path | State |
|---|---|---|
| License | `LICENSE-MIT` + `LICENSE-APACHE` | ✅ dual, ~12 KB |
| README | `README.md` | ✅ 10 KB, badges + connectors + roadmap |
| Changelog | `CHANGELOG.md` | ✅ v0.4.5 → v0.4.9 captured |
| Contributing | `CONTRIBUTING.md` | ✅ build / test / commit style |
| Code of conduct | `CODE_OF_CONDUCT.md` | ✅ Contributor Covenant 2.1 |
| Security policy | `SECURITY.md` | ✅ private disclosure path |
| Issue templates | `.github/ISSUE_TEMPLATE/` | ✅ bug + feature |
| PR template | `.github/PULL_REQUEST_TEMPLATE.md` | ✅ |
| `.gitignore` | `.gitignore` | ✅ excludes `target/`, `node_modules/`, `dist/`, `ledger.db*` |
| Git history | `git log` | ✅ 3 commits at `v0.4.9` tag, clean tree |

| Privacy hardening | State |
|---|---|
| API-key regex pre-filter on every payload write | ✅ |
| Cloud provider LLM bolting enforces the same redactor | ✅ (no cloud wired yet) |
| Read-only connector source dirs | ✅ |

| Build canary | State |
|---|---|
| `cargo test` (lib + integration, 138 active) | ✅ green |
| `.app` re-signed and reinstalled on this machine | ✅ v0.4.8 (binary unchanged through v0.4.9 / v0.4.10) |
| `--scan-once` against the real data dir | ✅ 10 000+ events captured |

---

## 1. Decision points (only `huoyuntianzhi1` can run these)

### 1.1 Repo visibility

`public` is the right call for OSS. `private` makes the connector
author-outreach path harder and would force the user to sign in
to file an issue. The existing `SECURITY.md` "out-of-scope" section
already covers the only thing that would push us toward `private`
(a legitimately sensitive partner contract), and we don't have
one.

### 1.2 Repo name

Three reasonable options, in order of preference:

1. **`agent-work-ledger`** — most descriptive, matches the binary
   name, matches the Cargo `[workspace] members` reference. Slight
   risk of collision with the literal phrase (a few unrelated
   projects have the same name in the wild).
2. **`agent-work-ledger-macos`** — disambiguates the platform
   (we ship macOS-only). Wastes the suffix.
3. **Something more brand-y** (e.g. `minimax-code/agent-work-ledger`
   or a fresh handle) — more work, no payoff for a pre-alpha.

### 1.3 Org vs personal

`huoyuntianzhi1`'s personal account is the right place for a
pre-alpha / pre-revenue OSS project. Move to an org only when
there's a company to host it under, or when the maintainer
wants to dissociate.

---

## 2. The actual push (one terminal session, all reversible until the last line)

```sh
cd "/Users/apple/Desktop/Agent Work Ledger"

# 2.1 — name the remote (replace with whatever landed in step 1.2)
gh repo create agent-work-ledger \
    --public \
    --description "Local-first macOS desktop app that aggregates every artifact from every agentic CLI / IDE into one searchable timeline + Eisenhower plan." \
    --homepage "https://github.com/chrzamz/sessio" 2>/dev/null \
    || echo "Repo may already exist; continuing"

# 2.2 — set the description / topics via API (topics are not in `gh repo create`)
gh repo edit agent-work-ledger \
    --add-topic ai-tools \
    --add-topic tauri \
    --add-topic rust \
    --add-topic sqlite \
    --add-topic local-first \
    --add-topic conversation-history \
    --add-topic developer-tools \
    --add-topic macos \
    --add-topic ollama \
    --add-topic search

# 2.3 — push
git push -u origin master

# 2.4 — push the v0.4.9 tag
git push origin v0.4.9

# 2.5 — set the default branch description in the About sidebar
gh repo edit agent-work-ledger --description "..."
```

If `gh repo create` returns "name not available", do step 1.2
again — the name was taken. Don't fight it; pick another.

---

## 3. After the push

- [ ] Edit the repo's "About" sidebar in the web UI: add
      homepage, description, and the topics from step 2.2.
- [ ] Enable Issues and Discussions (default is on for
      `--public`; verify).
- [ ] Pin issue #1 (`https://github.com/<owner>/agent-work-ledger/issues/1`)
      so the Sessio cross-reference stays visible.
- [ ] Tweet / post a Show HN. Not on day one — wait until
      someone stars it. The first "external user" reaction is
      the metric, not the launch announcement.
- [ ] Add the repo URL to the README's `git clone` line at
      the top (currently placeholder) and bump to v0.4.11.

---

## 4. Things that should NOT be in the first push

- **No `ledger.db`.** The `.gitignore` excludes it; double-check
  with `git status --ignored` before pushing.
- **No `target/` or `node_modules/`.** Same check.
- **No `app/src-tauri/gen/`** (Tauri plugin generated schema
  files — already in `.gitignore`).
- **No macOS Keychain entries.** None of the connectors store
  secrets in git; the only on-disk secret is the SQLCipher key
  we'll add in v0.5.0, which won't go through git either.
- **No `app/src-tauri/Cargo.lock` change.** It is committed
  intentionally (`!Cargo.lock` rule does not apply for a
  binary project).

---

## 5. Rollback

If anything in step 2 produces an output that looks wrong, the
window to back out is between `git push` and the first external
visitor — `gh repo delete <owner>/agent-work-ledger` is the
cleanest undo, but the better undo is `Settings → Danger Zone →
Make private` (irreversible-the-other-way: once you delete a
public repo, the issues + PRs are gone).

After ~50 stars or the first external PR, treat the repo as
durable.
